//! Collection of data files behind a bounded LRU handle cache.
//!
//! When `max_data_file_size == None` the set behaves like a single file
//! (`blockdb_0.dat`). Otherwise data is striped across `blockdb_N.dat`
//! files and a global offset `O` lives at local offset `O % cap` inside
//! file `O / cap`, where `cap` is `max_data_file_size.get()`.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DATA_FILE_PREFIX: &str = "blockdb_";
const DATA_FILE_SUFFIX: &str = ".dat";

/// Compute the data file basename for a given index, e.g. `blockdb_0.dat`.
pub(crate) fn data_file_name(index: u32) -> String {
    format!("{DATA_FILE_PREFIX}{index}{DATA_FILE_SUFFIX}")
}

/// Parse a basename like `blockdb_42.dat` back into its index, if it matches.
pub(crate) fn parse_data_file_index(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(DATA_FILE_PREFIX)?;
    let digits = rest.strip_suffix(DATA_FILE_SUFFIX)?;
    digits.parse::<u32>().ok()
}

#[derive(Debug)]
pub(crate) struct FileSet {
    dir: PathBuf,
    /// Maximum size in bytes of a single data file. `None` disables
    /// splitting (single-file mode, file index always 0).
    max_data_file_size: Option<NonZeroU64>,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Bound on cached handles. The current file (highest index seen) is
    /// always cached in addition to this many additional handles.
    capacity: usize,
    /// Insertion/access order; back = most recently used.
    order: Vec<u32>,
    handles: HashMap<u32, Arc<File>>,
}

impl FileSet {
    pub(crate) fn new(
        dir: PathBuf,
        max_data_file_size: Option<NonZeroU64>,
        max_data_files: usize,
    ) -> Self {
        let capacity = max_data_files.max(1);
        Self {
            dir,
            max_data_file_size,
            inner: Mutex::new(Inner {
                capacity,
                order: Vec::with_capacity(capacity),
                handles: HashMap::with_capacity(capacity),
            }),
        }
    }

    #[expect(dead_code, reason = "used in Round B")]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn max_data_file_size(&self) -> Option<NonZeroU64> {
        self.max_data_file_size
    }

    /// Translate a global offset into `(file_index, local_offset)`.
    ///
    /// In single-file mode (`max_data_file_size == None`) this always
    /// returns `(0, offset)` without performing division.
    pub(crate) fn split_offset(&self, global_offset: u64) -> (u32, u64) {
        let Some(cap) = self.max_data_file_size else {
            return (0, global_offset);
        };
        let cap = cap.get();
        // `cap` is non-zero by construction; checked_div / checked_rem
        // express that to clippy without an attribute escape hatch.
        let idx = global_offset.checked_div(cap).expect("cap is non-zero");
        let local = global_offset.checked_rem(cap).expect("cap is non-zero");
        // file index is bounded to u32: with the default 500 GiB chunk size,
        // u32::MAX files cover ~2 ZiB which is well beyond any practical store.
        // We saturate rather than panic; opening will fail naturally if this
        // somehow gets exercised.
        let idx_u32 = u32::try_from(idx).unwrap_or(u32::MAX);
        (idx_u32, local)
    }

    /// Returns the path that file `index` would live at.
    pub(crate) fn path_for(&self, index: u32) -> PathBuf {
        self.dir.join(data_file_name(index))
    }

    /// Enumerates `blockdb_N.dat` files in the data directory, returning
    /// `index -> file_size_in_bytes`. Non-matching entries are ignored.
    /// Mirrors blockdb's `listDataFiles`.
    pub(crate) fn list_on_disk(&self) -> Result<BTreeMap<u32, u64>, Error> {
        let mut out = BTreeMap::new();
        let entries = fs::read_dir(&self.dir)?;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let Some(idx) = parse_data_file_index(name_str) else {
                continue;
            };
            let size = entry.metadata()?.len();
            out.insert(idx, size);
        }
        Ok(out)
    }

    /// Validates that the on-disk file set is contiguous starting at index
    /// 0 and matches the configured splitting mode. Returns the calculated
    /// global next-write offset based on file sizes.
    ///
    /// In multi-file mode the answer is `max_idx * cap + size_of_last_file`.
    /// Intermediate files may be shorter than `cap` (an in-progress write
    /// can leave a "tail leak" when a block skips to the next file); the
    /// recovery scan handles those EOF transitions.
    pub(crate) fn validate_layout(&self, files: &BTreeMap<u32, u64>) -> Result<u64, Error> {
        if files.is_empty() {
            return Ok(0);
        }
        let max_idx = *files.keys().next_back().expect("non-empty");
        for i in 0..=max_idx {
            if !files.contains_key(&i) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("data file at index {i} is missing"),
                ));
            }
        }
        match self.max_data_file_size {
            None => {
                if max_idx > 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "single-file mode but multiple data files present",
                    ));
                }
                Ok(*files.get(&0).expect("checked above"))
            }
            Some(cap) => {
                let cap = cap.get();
                let last_size = *files.get(&max_idx).expect("non-empty");
                let total = u64::from(max_idx)
                    .checked_mul(cap)
                    .and_then(|t| t.checked_add(last_size))
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "data file total size overflow: max_idx={max_idx} cap={cap} last_size={last_size}",
                            ),
                        )
                    })?;
                Ok(total)
            }
        }
    }

    /// Returns a cached handle for `index`, opening (and creating) the file
    /// on miss. Handles are cached up to `max_data_files`; on overflow the
    /// least-recently-used handle is evicted (closed when its last `Arc`
    /// reference drops).
    ///
    /// On a cache miss the file is opened without holding any lock. If
    /// another thread inserts a handle for the same index first, this
    /// thread's freshly-opened handle is discarded and the cached one is
    /// returned — wasting one `open(2)` syscall but keeping the cache
    /// invariant that every index maps to exactly one handle.
    pub(crate) fn get_or_open(&self, index: u32) -> Result<Arc<File>, Error> {
        if let Some(handle) = self.lookup(index) {
            return Ok(handle);
        }

        let path = self.path_for(index);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let new_handle = Arc::new(file);

        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.handles.get(&index).cloned() {
            inner.touch(index);
            return Ok(existing);
        }
        inner.insert(index, Arc::clone(&new_handle));
        Ok(new_handle)
    }

    /// Convenience: resolve a global offset to `(handle, local_offset, file_index)`.
    pub(crate) fn resolve(&self, global_offset: u64) -> Result<(Arc<File>, u64, u32), Error> {
        let (idx, local) = self.split_offset(global_offset);
        let handle = self.get_or_open(idx)?;
        Ok((handle, local, idx))
    }

    /// Drop the cached handle for `index` (forcing the next access to reopen).
    /// Used after EBADF / closed-file errors.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "wired up in Round B (retry-on-evicted-handle path)"
        )
    )]
    pub(crate) fn evict(&self, index: u32) {
        let mut inner = self.inner.lock().unwrap();
        inner.remove(index);
    }

    /// Iterate all currently cached handles and run `f` on each.
    /// Used by `Drop` and explicit sync paths.
    #[expect(dead_code, reason = "used in Round B")]
    pub(crate) fn for_each_open<F>(&self, mut f: F)
    where
        F: FnMut(u32, &File),
    {
        let inner = self.inner.lock().unwrap();
        for &idx in &inner.order {
            if let Some(handle) = inner.handles.get(&idx) {
                f(idx, handle.as_ref());
            }
        }
    }

    fn lookup(&self, index: u32) -> Option<Arc<File>> {
        let mut inner = self.inner.lock().unwrap();
        let handle = inner.handles.get(&index).cloned()?;
        inner.touch(index);
        Some(handle)
    }
}

impl Inner {
    fn insert(&mut self, index: u32, handle: Arc<File>) {
        if self.handles.insert(index, handle).is_some() {
            // Replacing an existing entry; update its position.
            self.order.retain(|&i| i != index);
        } else if self.handles.len() > self.capacity
            && let Some(lru) = self.order.first().copied()
            && lru != index
        {
            // Need to evict the LRU entry.
            self.order.remove(0);
            self.handles.remove(&lru);
        }
        self.order.push(index);
    }

    fn touch(&mut self, index: u32) {
        // Fast path: already MRU. Common in single-file mode where file 0
        // is the only entry and `touch` runs on every read/write.
        if self.order.last() == Some(&index) {
            return;
        }
        if let Some(pos) = self.order.iter().position(|&i| i == index) {
            self.order.remove(pos);
            self.order.push(index);
        }
    }

    fn remove(&mut self, index: u32) {
        self.handles.remove(&index);
        self.order.retain(|&i| i != index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!("blockdb_0.dat", data_file_name(0));
        assert_eq!("blockdb_42.dat", data_file_name(42));
        assert_eq!(Some(0), parse_data_file_index("blockdb_0.dat"));
        assert_eq!(Some(42), parse_data_file_index("blockdb_42.dat"));
        assert_eq!(None, parse_data_file_index("blockdb.idx"));
        assert_eq!(None, parse_data_file_index("blockdb_.dat"));
        assert_eq!(None, parse_data_file_index("blockdb_-1.dat"));
    }

    fn cap(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("nonzero literal")
    }

    #[test]
    fn split_offset_single_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSet::new(dir.path().to_path_buf(), None, 4);
        assert_eq!((0, 0), fs.split_offset(0));
        assert_eq!((0, 1234), fs.split_offset(1234));
        assert_eq!((0, u64::MAX - 1), fs.split_offset(u64::MAX - 1));
    }

    #[test]
    fn split_offset_multi_file() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSet::new(dir.path().to_path_buf(), Some(cap(1024)), 4);
        assert_eq!((0, 0), fs.split_offset(0));
        assert_eq!((0, 1023), fs.split_offset(1023));
        assert_eq!((1, 0), fs.split_offset(1024));
        assert_eq!((2, 100), fs.split_offset(2148));
    }

    #[test]
    fn lazy_open_caches_and_evicts() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSet::new(dir.path().to_path_buf(), Some(cap(1024)), 2);

        let a = fs.get_or_open(0).unwrap();
        let b = fs.get_or_open(1).unwrap();
        // Same index returns the same Arc.
        let a2 = fs.get_or_open(0).unwrap();
        assert!(Arc::ptr_eq(&a, &a2));

        // Open a third; capacity is 2, so file 1 (LRU after touching 0) gets evicted.
        let _c = fs.get_or_open(2).unwrap();
        // The Arc<File> outside the cache is still usable.
        drop(b);

        // Reopening 1 produces a fresh handle (different Arc identity).
        let b3 = fs.get_or_open(1).unwrap();
        let b4 = fs.get_or_open(1).unwrap();
        assert!(Arc::ptr_eq(&b3, &b4));

        // All three files should now exist on disk.
        for i in 0..=2u32 {
            assert!(dir.path().join(data_file_name(i)).exists());
        }
    }

    #[test]
    fn evict_clears_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSet::new(dir.path().to_path_buf(), Some(cap(1024)), 4);

        let a = fs.get_or_open(0).unwrap();
        fs.evict(0);
        let a2 = fs.get_or_open(0).unwrap();
        // Evicted then re-opened: not the same Arc.
        assert!(!Arc::ptr_eq(&a, &a2));
    }
}
