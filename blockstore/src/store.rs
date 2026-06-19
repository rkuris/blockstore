//!
//! A store is a collection of blockchain blocks. Blocks can be added to the store in any order.
//! # [Store]
//!
//! The structure of the file contains a single [`IndexFileHeader`] and a sequence of [`IndexEntry`] entries.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Write as _};
use std::mem;
use std::num::{NonZeroU64, NonZeroUsize};
use std::os::unix::fs::FileExt;
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "metrics")]
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use parking_lot::Mutex;
use xxhash_rust::xxh64::xxh64;

use crate::file_set::{self, FileSet};
use crate::{Block, BlockHeight};

/// Default upper bound on cached open data-file handles. Matches blockdb's
/// `DefaultMaxDataFiles`.
pub const DEFAULT_MAX_DATA_FILES: usize = 10;

/// Options controlling how a [`Store`] is opened. Use [`StoreOptions::default`]
/// and the `with_*` helpers to override individual fields.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// Block-cache size in bytes (currently unused; reserved for a future
    /// in-memory read cache).
    pub cache_size: NonZeroUsize,
    /// If true, any existing files are truncated and a fresh header is
    /// written. If false, the store is opened and recovery runs.
    pub truncate: bool,
    /// fsync policy for writes.
    pub sync: SyncMode,
    /// Lowest block height this store will accept. Used only on truncate.
    pub minimum_height: u64,
    /// Maximum size of a single data file in bytes. `None` disables
    /// splitting (single-file mode). Round-A only supports `None`;
    /// `Some(_)` is reserved for Round B.
    pub max_data_file_size: Option<NonZeroU64>,
    /// Maximum number of open data-file handles to keep cached.
    pub max_data_files: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            cache_size: NonZeroUsize::new(1024).expect("nonzero literal"),
            truncate: false,
            sync: SyncMode::Async,
            minimum_height: 1,
            max_data_file_size: None,
            max_data_files: DEFAULT_MAX_DATA_FILES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, strum::Display)]
#[strum(serialize_all = "lowercase")]
#[repr(C)]
pub enum SyncMode {
    Async = 0,
    Sync = 1,
}

#[derive(Debug)]
pub struct Store {
    // TODO: add a recent block cache here
    // recents: crate::recents::Recents,
    index_file: File,
    files: FileSet,
    data_highwater: Mutex<u64>,
    header: IndexFileHeader,
    sync: SyncMode,
    max_contiguous_height: AtomicU64,
    height_highwater: AtomicU64,
    /// When set, `Drop` skips the final checkpoint and lets files close
    /// normally. Used by tests to simulate an unclean shutdown without
    /// leaking handles (which would also retain advisory locks). See
    /// `simulate_crash` in this file's tests.
    #[cfg(test)]
    crashed: AtomicBool,
}

/// The size of a block in the store.
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct IndexEntry {
    pub offset: u64,
    pub size: u32,
    pub reserved: [u8; 4],
}

/// The header of the index file.
///
/// This is written to the index file and is used to recover the store from a crash.
/// This MUST be a multiple of the [`IndexEntry`] size.
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct IndexFileHeader {
    // The version of the index file format
    pub version: u64,
    // Matches Go's MaxDataFileSize field.
    pub max_data_file_size: u64,
    // The lowest block height tracked by this index.
    pub min_height: u64,
    // The highest block height written.
    pub max_height: u64,
    // The next write offset in the data file.
    pub next_write_offset: u64,
    // The highest height with no gaps below it (the contiguity floor) as of
    // the last checkpoint. Persisted so reopen can resume the contiguity scan
    // from here instead of rescanning the whole index. A value of 0 means
    // "unknown" — an old on-disk format, or a DB last written by avalanchego
    // (which leaves these bytes zeroed since it doesn't track contiguity) —
    // and triggers a full scan. Occupies the first 8 bytes of Go's reserved
    // area; avalanchego keeps a matching reserved field at this offset.
    pub highest_contiguous_block_height: u64,
    // Reserved bytes to keep header size equal to Go implementation (64 bytes total).
    pub reserved: [u8; 16],
}

impl IndexFileHeader {
    const INDEX_FILE_VERSION: u64 = 1;

    fn with_lowest_block_height(self, lowest_block_height: u64) -> Self {
        Self {
            min_height: lowest_block_height,
            ..self
        }
    }
}

impl Default for IndexFileHeader {
    fn default() -> Self {
        Self {
            version: Self::INDEX_FILE_VERSION,
            max_data_file_size: u64::MAX,
            min_height: 1,
            max_height: 0,
            next_write_offset: 0,
            highest_contiguous_block_height: 0,
            reserved: [0; 16],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BlockHeader {
    height: u64,
    size: u32,
    checksum: u64,
    version: u16,
}

impl BlockHeader {
    /// 1 GiB — sanity bound to reject corrupt headers during recovery.
    const MAX_BLOCK_SIZE: u32 = 1 << 30;
    const BLOCK_ENTRY_VERSION: u16 = 1;
    const SERIALIZED_SIZE: usize = 8 + 4 + 8 + 2;

    fn serialize(self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut buf = [0u8; Self::SERIALIZED_SIZE];
        buf[0..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.checksum.to_le_bytes());
        buf[20..22].copy_from_slice(&self.version.to_le_bytes());
        buf
    }

    fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        fn take<const N: usize>(
            buf: &[u8],
            offset: usize,
            field: &'static str,
        ) -> Result<[u8; N], Error> {
            offset
                .checked_add(N)
                .and_then(|end| buf.get(offset..end))
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, field))
        }

        if buf.len() != Self::SERIALIZED_SIZE {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid block header size",
            ));
        }

        let height = u64::from_le_bytes(take(buf, 0, "invalid block header height field")?);
        let size = u32::from_le_bytes(take(buf, 8, "invalid block header size field")?);
        let checksum = u64::from_le_bytes(take(buf, 12, "invalid block header checksum field")?);
        let version = u16::from_le_bytes(take(buf, 20, "invalid block header version field")?);

        Ok(Self {
            height,
            size,
            checksum,
            version,
        })
    }
}

/// Compresses the given data using Snappy compression.
///
/// # Arguments
/// * `data` - The data to compress
///
/// # Returns
/// A `Vec<u8>` containing the compressed data
///
/// # Panics
/// Panics if compression fails
fn compress(data: &[u8]) -> Vec<u8> {
    #[cfg(all(feature = "snappy", not(feature = "zstd")))]
    {
        let mut encoder = snap::write::FrameEncoder::new(Vec::new());
        encoder.write_all(data).expect("Failed to write to encoder");
        encoder
            .into_inner()
            .expect("Failed to finalize compression")
    }
    #[cfg(feature = "zstd")]
    {
        const ZSTD_COMPRESSION_LEVEL: i32 = 3;
        zstd::encode_all(data, ZSTD_COMPRESSION_LEVEL).expect("all in memory")
    }

    #[cfg(not(any(feature = "zstd", feature = "snappy")))]
    data.into()
}

/// Decompresses the given data using Snappy decompression.
///
/// # Arguments
/// * `compressed_data` - The compressed data to decompress
///
/// # Returns
/// A `Vec<u8>` containing the decompressed data
///
/// # Panics
/// Panics if decompression fails
fn decompress(compressed_data: &[u8]) -> Vec<u8> {
    #[cfg(all(feature = "snappy", not(feature = "zstd")))]
    {
        let mut decoder = snap::read::FrameDecoder::new(compressed_data);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .expect("Failed to decompress data");
        decompressed
    }
    #[cfg(feature = "zstd")]
    {
        zstd::decode_all(compressed_data).expect("all in memory")
    }
    #[cfg(not(any(feature = "zstd", feature = "snappy")))]
    compressed_data.into()
}

/// Removes any `blockdb_N.dat` files from `dir`. Used when opening a store
/// with `truncate = true` so a fresh store does not inherit stale data
/// files from a previous incarnation.
fn remove_existing_data_files(dir: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if let Some(s) = name.to_str()
            && file_set::parse_data_file_index(s).is_some()
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Computes the global byte offset at the start of the file after
/// `current_file_idx`, given the file-size cap. Errors on overflow.
fn advance_to_next_file(current_file_idx: u32, cap: u64) -> Result<u64, Error> {
    current_file_idx
        .checked_add(1)
        .map(u64::from)
        .and_then(|i| i.checked_mul(cap))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "data offset overflow during recovery: cannot advance past file {current_file_idx} with cap {cap}",
                ),
            )
        })
}

impl Store {
    const INDEX_FILE_NAME: &'static str = "blockdb.idx";

    /// We write the header to the index file every 1024 blocks
    const CHECKPOINT_INTERVAL: u64 = 1024;

    /// Opens (or creates) a store with the given options.
    ///
    /// Both `index_path` and `data_path` must be directories. The index
    /// file lives at `<index_path>/blockdb.idx`; data files live at
    /// `<data_path>/blockdb_N.dat`. The split lets callers put the
    /// index on faster storage than the data.
    ///
    /// # Errors
    /// Returns an error if files cannot be opened, truncated, or if the
    /// on-disk state is inconsistent with the provided options.
    pub fn open(index_path: &Path, data_path: &Path, options: StoreOptions) -> Result<Self, Error> {
        if options.max_data_files == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "max_data_files must be > 0",
            ));
        }

        let mut opts = OpenOptions::new();
        let index_filename = index_path.join(Self::INDEX_FILE_NAME);
        let data_dir = data_path.to_path_buf();
        let opts = opts
            .create(options.truncate)
            .truncate(options.truncate)
            .write(true)
            .read(true);
        let mut index_file = opts.open(&index_filename)?;

        // Advisory lock the index file for the lifetime of the Store. This
        // prevents two processes from opening the same store directory
        // concurrently — without it, both processes would happily interleave
        // writes to the index header and data files, silently corrupting
        // layout invariants (next_write_offset, contiguity, etc.). The lock
        // is released when `index_file` is dropped (i.e., when the Store is
        // dropped or the process exits).
        //
        // Same-process opens are also rejected, which is by design: if a
        // caller wants two handles to the same store within one process they
        // should share a `Store` via `Arc`, not open twice.
        index_file.try_lock().map_err(|e| {
            Error::new(
                ErrorKind::WouldBlock,
                format!(
                    "unable to acquire advisory lock on {}: store may be open in another process ({e})",
                    index_filename.display(),
                ),
            )
        })?;

        // On truncate, also remove any pre-existing blockdb_N.dat files
        // from the data directory. Otherwise old data would haunt a "fresh"
        // store and confuse `recover` on the next open.
        if options.truncate {
            remove_existing_data_files(&data_dir)?;
        }

        let header = if options.truncate {
            let mut header =
                IndexFileHeader::default().with_lowest_block_height(options.minimum_height);
            // On disk we keep the Go-compatible representation: `u64::MAX`
            // means "unlimited / single file", any other value is the cap.
            header.max_data_file_size =
                options.max_data_file_size.map_or(u64::MAX, NonZeroU64::get);
            index_file.write_all(bytemuck::bytes_of(&header))?;
            header
        } else {
            let mut header = IndexFileHeader::default();
            index_file.read_at(bytemuck::bytes_of_mut(&mut header), 0)?;
            header
        };

        // The on-disk header is the source of truth for the file-size cap.
        // For truncate opens this matches options; for re-opens it may
        // differ (the user can't change the splitting policy of an existing
        // store).
        let effective_max = if header.max_data_file_size == u64::MAX {
            None
        } else {
            NonZeroU64::new(header.max_data_file_size)
        };
        let files = FileSet::new(data_dir, effective_max, options.max_data_files);
        // Data files are created lazily on first write. We must NOT
        // eagerly create file 0 here: that would mask a missing-file
        // corruption case (deleted-file recovery test) by silently
        // re-creating file 0 before `recover` inspects the layout.

        let mut result = Self {
            index_file,
            files,
            data_highwater: Mutex::new(0),
            header,
            sync: options.sync,
            max_contiguous_height: AtomicU64::new(options.minimum_height.saturating_sub(1)),
            height_highwater: AtomicU64::new(options.minimum_height.saturating_sub(1)),
            #[cfg(test)]
            crashed: AtomicBool::default(),
        };
        if !options.truncate {
            result.recover()?;
        }
        Ok(result)
    }

    /// Convenience constructor matching the pre-`StoreOptions` API.
    ///
    /// # Errors
    /// See [`Store::open`].
    pub fn new(
        index_path: &Path,
        data_path: &Path,
        cache_size: NonZeroUsize,
        truncate: bool,
        sync: SyncMode,
        minimum_height: u64,
    ) -> Result<Self, Error> {
        let options = StoreOptions {
            cache_size,
            truncate,
            sync,
            minimum_height,
            ..StoreOptions::default()
        };
        Self::open(index_path, data_path, options)
    }

    fn recover(&mut self) -> Result<(), Error> {
        let data_files = self.files.list_on_disk()?;
        let calculated_next_write_offset = self.files.validate_layout(&data_files)?;

        if calculated_next_write_offset < self.header.next_write_offset {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "data files contain {calculated_next_write_offset} bytes but index header claims next_write_offset={next_write_offset}",
                    next_write_offset = self.header.next_write_offset,
                ),
            ));
        }

        // `header.max_height` is the persisted highwater, NOT the contiguity
        // floor. Seed `height_highwater` from it. The true
        // `max_contiguous_height` is recomputed below by scanning the index
        // for the first zeroed entry, since gaps below the highwater may
        // have existed before the crash (e.g. out-of-order writes during
        // bootstrap).
        self.height_highwater
            .store(self.header.max_height, Ordering::Relaxed);

        if calculated_next_write_offset > self.header.next_write_offset {
            self.recover_unindexed_blocks(calculated_next_write_offset, &data_files)?;
        }

        // Resume the contiguity scan from the persisted floor. It is a
        // monotonic lower bound (heights at or below it were contiguous at
        // the last checkpoint, and index entries are never cleared), so the
        // scan need only walk any blocks recovered above it by
        // `recover_unindexed_blocks`. A persisted 0 means the floor is
        // unknown (old format / avalanchego-written), and the scan falls back
        // to walking the whole index from `min_height`.
        let contiguous_floor =
            self.scan_contiguous_floor(self.header.highest_contiguous_block_height)?;
        self.max_contiguous_height
            .store(contiguous_floor, Ordering::Relaxed);

        *self.data_highwater.lock() = self.header.next_write_offset;
        Ok(())
    }

    /// Bulk-scans the index file for the first zeroed (`offset == 0 &&
    /// size == 0`) entry at or above `resume_floor` (bounded above by
    /// `height_highwater`) and returns the height of the last populated slot
    /// before it — i.e. the highest contiguous height. Returns
    /// `min_height - 1` if nothing is present.
    ///
    /// `resume_floor` is the persisted contiguity floor (0 if unknown). It is
    /// a monotonic lower bound: every height at or below it was contiguous at
    /// the last checkpoint and index entries are never cleared, so the scan
    /// can start just above it rather than at `min_height`. After a clean
    /// shutdown (floor == highwater) this returns without reading the index
    /// at all. A `resume_floor` of 0 — or any value at/below the empty floor,
    /// `min_height - 1` — collapses to a full scan from `min_height`; that is
    /// safe because the full scan independently reproduces the correct floor,
    /// so the only cost of the overloaded 0 is recomputing a floor that is
    /// already the cheapest possible to recompute (the scan stops at the
    /// first height).
    ///
    /// This matches the recovery algorithm in the block-store design doc:
    /// "Once we know the first zeroed offset, we can know the highest
    /// contiguous height." The scan is bounded by `height_highwater`
    /// (which only ever rises from validated data — either the persisted
    /// checkpoint or the data-file scan in `recover_unindexed_blocks`), so
    /// zombie index entries past a corrupt block do not get counted as
    /// contiguous. Reads are issued in 64 KiB chunks so the cost is
    /// dominated by sequential I/O, not per-entry syscalls.
    fn scan_contiguous_floor(&self, resume_floor: BlockHeight) -> Result<BlockHeight, Error> {
        const ENTRY_SIZE: usize = mem::size_of::<IndexEntry>();
        const CHUNK_ENTRIES: usize = 4096;
        const CHUNK_BYTES: usize = CHUNK_ENTRIES * ENTRY_SIZE;

        let header_size = mem::size_of::<IndexFileHeader>() as u64;
        let min_height = self.header.min_height;
        let upper_bound = self.height_highwater.load(Ordering::Relaxed);
        let floor_if_empty = min_height.saturating_sub(1);
        if upper_bound < min_height {
            return Ok(floor_if_empty);
        }

        // Clamp the resume point into [floor_if_empty, upper_bound]. A value
        // below `floor_if_empty` (including the unknown sentinel 0) starts the
        // scan at `min_height`; a value at the highwater short-circuits the
        // whole scan.
        let start_floor = resume_floor.max(floor_if_empty).min(upper_bound);
        if start_floor >= upper_bound {
            return Ok(start_floor);
        }

        let overflow = || {
            Error::new(
                ErrorKind::InvalidData,
                "index offset overflow during contiguity scan",
            )
        };

        let mut buf = vec![0u8; CHUNK_BYTES];
        // Every height at or below `start_floor` is already known present.
        let mut height_cursor = start_floor.checked_add(1).ok_or_else(overflow)?;
        let mut last_present = start_floor;

        loop {
            let relative = height_cursor.checked_sub(min_height).ok_or_else(overflow)?;
            let file_offset = relative
                .checked_mul(ENTRY_SIZE as u64)
                .and_then(|o| o.checked_add(header_size))
                .ok_or_else(overflow)?;
            let n = self.index_file.read_at(&mut buf, file_offset)?;
            if n == 0 {
                return Ok(last_present);
            }
            if n % ENTRY_SIZE != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "index file has trailing partial entry",
                ));
            }
            let entries_read = n / ENTRY_SIZE;
            let chunk = buf.get(..n).ok_or_else(overflow)?;
            for (i, entry_bytes) in chunk.chunks_exact(ENTRY_SIZE).enumerate() {
                let height = height_cursor.checked_add(i as u64).ok_or_else(overflow)?;
                if height > upper_bound {
                    return Ok(last_present);
                }
                let entry: &IndexEntry = bytemuck::from_bytes(entry_bytes);
                if entry.offset == 0 && entry.size == 0 {
                    return Ok(last_present);
                }
                last_present = height;
            }
            height_cursor = height_cursor
                .checked_add(entries_read as u64)
                .ok_or_else(overflow)?;
            if entries_read < CHUNK_ENTRIES {
                return Ok(last_present);
            }
        }
    }

    /// Walks from `self.header.next_write_offset` up to `end_offset`,
    /// validating each block and writing the missing index entries.
    /// Handles cross-file EOF transitions: if the current scan position
    /// hits the actual byte-end of a data file in multi-file mode, we
    /// advance to the start of the next data file.
    fn recover_unindexed_blocks(
        &mut self,
        end_offset: u64,
        data_files: &BTreeMap<u32, u64>,
    ) -> Result<(), Error> {
        let cap = self.files.max_data_file_size().map(NonZeroU64::get);

        loop {
            let scan = self.header.next_write_offset;
            if scan >= end_offset {
                break;
            }
            let (file_idx, local_offset) = self.files.split_offset(scan);
            let Some(&file_size) = data_files.get(&file_idx) else {
                // The validate_layout pre-check ensures contiguous indices;
                // a miss here would mean a file disappeared between listing
                // and scanning. Treat as end-of-data.
                break;
            };

            // If the next block's header wouldn't fit in this file, we hit
            // the tail-leak: skip to the start of the next file. In
            // single-file mode there is no next file, so we're done.
            let header_end = local_offset.saturating_add(BlockHeader::SERIALIZED_SIZE as u64);
            if header_end > file_size {
                match cap {
                    None => break,
                    Some(cap) => {
                        self.header.next_write_offset = advance_to_next_file(file_idx, cap)?;
                        continue;
                    }
                }
            }

            let block_header = self.read_block_header_at(scan)?;
            let height = block_header.height;
            let size = block_header.size;
            if height < self.header.min_height
                || size == 0
                || size > BlockHeader::MAX_BLOCK_SIZE
                || block_header.version != BlockHeader::BLOCK_ENTRY_VERSION
            {
                break;
            }

            // The full block (header + payload) must fit in this same file
            // — we never write a block that crosses a file boundary.
            let block_end_local = header_end.checked_add(u64::from(size)).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "recovery: block end overflow at file {file_idx} local offset {local_offset} (size={size})",
                    ),
                )
            })?;
            if block_end_local > file_size {
                break;
            }

            #[allow(clippy::cast_possible_truncation)]
            let mut compressed_block = vec![0; size as usize];
            let data_file = self.files.get_or_open(file_idx)?;
            data_file.read_at(&mut compressed_block, header_end)?;

            let block = decompress(&compressed_block);
            if xxh64(&block, 0) != block_header.checksum {
                break;
            }

            let index_entry = IndexEntry {
                offset: scan,
                size,
                reserved: [0; 4],
            };
            self.update_index(height, index_entry)?;
            self.update_highwater(height);
            // During recovery we walk strictly forward, so the cascade in
            // `advance_max_contiguous_height` is unneeded and harmful: the
            // existing index file may still contain entries from pre-crash
            // writes for blocks we're about to reject (e.g. corrupt block
            // headers in the scan range). `fetch_max` updates only based
            // on what this loop has actually validated.
            self.max_contiguous_height
                .fetch_max(height, Ordering::AcqRel);

            self.header.next_write_offset = scan
                .checked_add(BlockHeader::SERIALIZED_SIZE as u64)
                .and_then(|p| p.checked_add(u64::from(size)))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!(
                            "recovery: next write offset overflow after block at global offset {scan} (size={size})",
                        ),
                    )
                })?;
        }
        Ok(())
    }

    // update the highwater mark if the new height is higher
    // returns true if the highwater mark was updated
    fn update_highwater(&self, height: BlockHeight) -> bool {
        self.height_highwater
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                if old < height { Some(height) } else { None }
            })
            .is_ok()
    }

    /// Inserts a block into the store.
    ///
    /// # Errors
    /// Returns an error if the block cannot be written
    ///
    /// # Panics
    /// Panics if the cache lock has been poisoned.
    pub fn write_block(&self, height: BlockHeight, block: &[u8]) -> Result<(), Error> {
        #[cfg(feature = "metrics")]
        let start = Instant::now();

        // prohibit writes of zero length blocks
        if block.is_empty() {
            counter!("blockstore.write_block", "outcome" => "empty").increment(1);
            return Err(Error::new(ErrorKind::InvalidInput, "Block is empty"));
        }

        let _: u32 = block.len().try_into().map_err(|_| {
            counter!("blockstore.write_block", "outcome" => "block_too_large").increment(1);
            Error::new(ErrorKind::InvalidInput, "Block too large")
        })?;

        // compress the block
        let compressed_block = compress(block);
        let compressed_block_len: u32 = compressed_block.len().try_into().unwrap();

        // check the index file offset for overflows
        // this limits our block height to 2^64 / size_of::<IndexEntry>(), or 2^60 blocks
        let index_entry_offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.write_block", "outcome" => "invalid_block_height").increment(1);
            Error::new(ErrorKind::InvalidInput, "Invalid block height")
        })?;

        // calculate the size of the compressed block with the header. An overflow here only happens if the block is
        // just under MAX_U64, which is a mighty big buffer to be passing to this function...
        let size_with_header = compressed_block
            .len()
            .checked_add(BlockHeader::SERIALIZED_SIZE)
            .expect("blocks will never be so large as to overflow u64")
            as u64;

        // calculate the hash of the original (uncompressed) block
        let checksum = xxh64(block, 0);

        // construct the block header
        let header = BlockHeader {
            height,
            size: compressed_block_len,
            checksum,
            version: BlockHeader::BLOCK_ENTRY_VERSION,
        };

        // Reserve space in the data file(s). In multi-file mode, this may
        // skip past the tail of the current data file if the block would
        // cross a file boundary; the skipped bytes are never read.
        // TODO: what happens if we run out of space after updating the
        // highwater mark? We can't really reduce it because someone else may
        // already be writing past us, but we might want to consider restoring it?
        let (offset, saved_offset) = self.reserve_space(size_with_header)?;

        let (data_file, local_offset, _file_index) = self.files.resolve(offset)?;
        data_file
            .write_all_at(&header.serialize(), local_offset)
            .inspect_err(|_| {
                counter!("blockstore.write_block", "outcome" => "write_header_failed").increment(1);
            })?;

        // write the compressed block data
        // safe to use wrapping_add here because we checked for overflow above
        // TODO: use a single write_all_at call to write both the header and the data
        // saves a syscall but requires copying the data around in memory
        data_file
            .write_all_at(
                &compressed_block,
                local_offset.wrapping_add(BlockHeader::SERIALIZED_SIZE as u64),
            )
            .inspect_err(|_| {
                counter!("blockstore.write_block", "outcome" => "write_data_failed").increment(1);
            })?;

        if self.sync == SyncMode::Sync {
            #[cfg(feature = "metrics")]
            let sync_start = Instant::now();
            data_file.sync_all()?;
            record_duration!(sync_start, "blockstore.write_block.sync.duration_seconds");
        }

        // update the index file
        self.update_index_at(
            index_entry_offset,
            IndexEntry {
                offset,
                size: compressed_block_len,
                reserved: [0; 4],
            },
        )?;

        self.advance_max_contiguous_height(height);

        if self.update_highwater(height) && height.is_multiple_of(Self::CHECKPOINT_INTERVAL) {
            self.checkpoint(saved_offset)?;
        }

        if self
            .height_highwater
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                if old < height { Some(height) } else { None }
            })
            .is_ok()
            && height.is_multiple_of(Self::CHECKPOINT_INTERVAL)
        {
            self.checkpoint(saved_offset)?;
        }

        counter!("blockstore.write_block", "outcome" => "success").increment(1);
        record_duration!(start, "blockstore.write_block.duration_seconds");

        Ok(())
    }

    /// Reserves `size_with_header` bytes in the data file(s).
    ///
    /// Returns `(write_offset, new_highwater)`. In multi-file mode, if the
    /// block would cross a file boundary, `write_offset` is advanced to the
    /// start of the next data file (the bytes from `data_highwater` up to
    /// that point are leaked and will never be read). Mirrors blockdb's
    /// `allocateBlockSpace`.
    fn reserve_space(&self, size_with_header: u64) -> Result<(u64, u64), Error> {
        let max = self.files.max_data_file_size().map(NonZeroU64::get);

        // Reject blocks that can't fit in a single file at all.
        if let Some(cap) = max
            && size_with_header > cap
        {
            counter!("blockstore.write_block", "outcome" => "block_exceeds_file_size").increment(1);
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("block size {size_with_header} exceeds max_data_file_size {cap}"),
            ));
        }

        let mut guard = self.data_highwater.lock();
        let current = *guard;

        let write_offset = match max {
            None => current,
            Some(cap) => {
                let offset_within_file = current.checked_rem(cap).expect("cap is non-zero");
                let end_within_file =
                    offset_within_file.checked_add(size_with_header).ok_or_else(|| {
                        counter!("blockstore.write_block", "outcome" => "block_too_large").increment(1);
                        Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "block end overflow: in-file offset {offset_within_file} + size {size_with_header} > u64::MAX",
                            ),
                        )
                    })?;
                if end_within_file <= cap {
                    current
                } else {
                    // Skip to the start of the next data file.
                    let current_file_idx = current.checked_div(cap).expect("cap is non-zero");
                    let current_file_idx_u32 = u32::try_from(current_file_idx).map_err(|_| {
                        counter!("blockstore.write_block", "outcome" => "offset_overflow")
                            .increment(1);
                        Error::new(
                            ErrorKind::InvalidInput,
                            format!("file index {current_file_idx} exceeds u32::MAX (cap={cap})"),
                        )
                    })?;
                    advance_to_next_file(current_file_idx_u32, cap).inspect_err(|_| {
                        counter!("blockstore.write_block", "outcome" => "offset_overflow")
                            .increment(1);
                    })?
                }
            }
        };

        let new_highwater = write_offset.checked_add(size_with_header).ok_or_else(|| {
            counter!("blockstore.write_block", "outcome" => "block_too_large").increment(1);
            Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "data highwater overflow: write_offset {write_offset} + size {size_with_header} > u64::MAX",
                ),
            )
        })?;
        *guard = new_highwater;
        Ok((write_offset, new_highwater))
    }

    fn advance_max_contiguous_height(&self, height: BlockHeight) {
        // optimize for the case where the block height is the next contiguous height
        // if the highest contiguous height is the one block before us, then we update it
        // and start looking for the next gap
        let prev = height.saturating_sub(1);
        let _ = self
            .max_contiguous_height
            .compare_exchange(prev, height, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                counter!("blockstore.write_block", "outcome" => "out_of_order").increment(1);
            })
            .map(|mut prev| {
                // if the current_highwater is higher than the height we just wrote,
                // we need to check if it can be increased
                // we will stop at the highwater as of right now, and not increase it
                // TODO: is there a race condition here? I think not, but it could use
                // more thought.
                loop {
                    // overflowing_add is safe because read_index_entry will return None on overflow
                    prev = prev.wrapping_add(1);
                    let next = prev.wrapping_add(1);
                    if let Ok(Some(entry)) = self.read_index_entry(next) {
                        if entry.offset == 0 && entry.size == 0 {
                            // we found a gap, so we can stop here
                            break;
                        }

                        if self
                            .max_contiguous_height
                            .compare_exchange(prev, next, Ordering::AcqRel, Ordering::Acquire)
                            .is_err()
                        {
                            // someone else is also doing this, so we stop here
                            break;
                        }
                    } else {
                        // the index is out of range, so we can stop here
                        break;
                    }
                }
            });
    }

    fn update_index(&self, height: BlockHeight, index_entry: IndexEntry) -> Result<(), Error> {
        let offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.update_index", "outcome" => "invalid_block_height").increment(1);
            Error::new(ErrorKind::InvalidInput, "Invalid block height")
        })?;
        self.update_index_at(offset, index_entry)
    }
    fn update_index_at(&self, offset: u64, index_entry: IndexEntry) -> Result<(), Error> {
        self.index_file
            .write_all_at(bytemuck::bytes_of(&index_entry), offset)
    }

    /// Writes the header to the index file.
    ///
    /// Caution: the sync must be done before writing the header.
    /// Why? We are guaranteeing that the data in the index file is correct up
    /// to the point where the last block was written. If we write the header before
    /// syncing, then it could be that the data in the index file has not been flushed
    /// even though the offset has been updated.
    ///
    /// # Errors
    /// Returns an error if the header cannot be written.
    fn checkpoint(&self, saved_offset: u64) -> Result<(), Error> {
        if self.sync == SyncMode::Sync {
            self.index_file.sync_all()?;
        }
        let mut header = self.header;
        header.next_write_offset = saved_offset;
        header.max_height = self.height_highwater.load(Ordering::Relaxed);
        header.highest_contiguous_block_height = self.max_contiguous_height.load(Ordering::Relaxed);
        self.index_file
            .write_all_at(bytemuck::bytes_of(&header), 0)?;
        Ok(())
    }

    /// Returns the offset of the index entry for the given block height.
    ///
    /// # Returns
    /// Returns the offset of the index entry for the given block height.
    /// Returns None if the block height is before the first block in the store,
    /// or if an overflow occurs.
    fn index_entry_offset(&self, height: BlockHeight) -> Option<u64> {
        height
            .checked_sub(self.header.min_height)?
            .checked_mul(mem::size_of::<IndexEntry>() as u64)?
            .checked_add(mem::size_of::<IndexFileHeader>() as u64)
    }

    fn read_block_header_at(&self, offset: u64) -> Result<BlockHeader, Error> {
        let mut buf = [0u8; BlockHeader::SERIALIZED_SIZE];
        let (data_file, local_offset, _file_index) = self.files.resolve(offset)?;
        data_file.read_at(&mut buf, local_offset)?;
        BlockHeader::deserialize(&buf)
    }

    fn read_index_entry(&self, height: BlockHeight) -> Result<Option<IndexEntry>, Error> {
        let offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.read_index_entry", "outcome" => "invalid_block_height")
                .increment(1);
            Error::new(ErrorKind::InvalidInput, "Invalid block height")
        })?;
        let mut index_entry = IndexEntry::default();
        self.index_file
            .read_at(bytemuck::bytes_of_mut(&mut index_entry), offset)?;
        Ok(Some(index_entry))
    }

    /// Retrieves a block by its ID.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The file cannot be read
    /// - The block header cannot be decoded
    /// - The block data cannot be read
    ///
    /// # Panics
    /// Panics if:
    /// - The cache lock has been poisoned.
    /// - The file offset somehow overflows
    pub fn read_block(&self, height: BlockHeight) -> Result<Option<Block>, Error> {
        #[cfg(feature = "metrics")]
        let start = Instant::now();

        let entry = self.read_index_entry(height).inspect_err(|_| {
            counter!("blockstore.read_block", "outcome" => "read_index_entry_failed").increment(1);
        })?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        let block_size = entry.size;
        if block_size == 0 {
            counter!("blockstore.read_block", "outcome" => "not_found").increment(1);
            return Ok(None);
        }
        // TODO: we know the size and can read the whole header and data in one read...

        // read the block header
        let blockheader = self.read_block_header_at(entry.offset).inspect_err(|_| {
            counter!("blockstore.read_block", "outcome" => "read_header_failed").increment(1);
        })?;

        if blockheader.size != block_size {
            counter!("blockstore.read_block", "outcome" => "block_size_mismatch").increment(1);
            return Err(Error::new(
                ErrorKind::InvalidData,
                "block size in index file does not match data",
            ));
        }

        // read the compressed block data
        // this conversion is infallable on 64 bit systems, but not on 32 bit systems
        let block_size: usize = block_size
            .try_into()
            .inspect_err(|_| {
                counter!("blockstore.read_block", "outcome" => "block_size_too_large").increment(1);
            })
            .map_err(|_| Error::new(ErrorKind::InvalidData, "block size too large"))?;

        let mut compressed_block = vec![0; block_size];
        // checked_add should be infallable (overflowing here means that the block offset is almost at 2^64, which is insane)
        let block_global_offset = entry
            .offset
            .checked_add(BlockHeader::SERIALIZED_SIZE as u64)
            .expect("block offset overflow");
        let (data_file, local_offset, _file_index) = self.files.resolve(block_global_offset)?;
        data_file.read_at(&mut compressed_block, local_offset)?;

        // decompress the block data
        let block = decompress(&compressed_block);

        // verify the checksum (checksum is calculated on the original uncompressed data)
        let checksum = xxh64(&block, 0);
        if checksum != blockheader.checksum {
            counter!("blockstore.read_block", "outcome" => "checksum_mismatch").increment(1);
            return Err(Error::new(ErrorKind::InvalidData, "checksum mismatch"));
        }

        let block = Some(block.into());
        counter!("blockstore.read_block", "outcome" => "success").increment(1);
        record_duration!(start, "blockstore.read_block.duration_seconds");
        Ok(block)
    }

    pub fn max_contiguous_height(&self) -> BlockHeight {
        self.max_contiguous_height.load(Ordering::Relaxed)
    }

    /// Highest block height ever written to this store, regardless of
    /// contiguity. Diverges from [`Self::max_contiguous_height`] whenever
    /// blocks are written with gaps below them: contiguous tracking stalls at
    /// the floor of the first gap, while the highwater advances on every
    /// successful write.
    pub fn height_highwater(&self) -> BlockHeight {
        self.height_highwater.load(Ordering::Relaxed)
    }

    pub fn min_block_height(&self) -> BlockHeight {
        self.header.min_height
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        #[cfg(test)]
        if self.crashed.load(Ordering::Relaxed) {
            // Simulated crash: skip the final checkpoint so the next open
            // exercises the recovery path. Files still close normally,
            // releasing OS advisory locks — that's the whole point of
            // using this over `mem::forget`.
            return;
        }
        if self.sync == SyncMode::Sync {
            self.index_file.sync_all().unwrap();
        }
        // if this fails, no biggie, we'll just have to do recovery at startup
        let _ = self.checkpoint(*self.data_highwater.lock());
    }
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::thread::{available_parallelism, scope};

    use super::*;

    /// Simulate an unclean shutdown: skip the checkpoint but still run
    /// `Drop` so files close and advisory locks are released. Unlike
    /// `mem::forget(store)`, which would keep the locks held and block
    /// the next `Store::open` in this test.
    fn simulate_crash(store: Store) {
        store.crashed.store(true, Ordering::Relaxed);
        drop(store);
    }

    #[test]
    fn second_open_of_same_dir_is_rejected() {
        let tmpdir = tempfile::tempdir().unwrap();
        let _first = Store::open(
            tmpdir.path(),
            tmpdir.path(),
            StoreOptions {
                truncate: true,
                ..StoreOptions::default()
            },
        )
        .unwrap();

        // Second open against the same directory must fail because the
        // index file is held with an exclusive advisory lock.
        let err = Store::open(
            tmpdir.path(),
            tmpdir.path(),
            StoreOptions {
                truncate: false,
                ..StoreOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(ErrorKind::WouldBlock, err.kind(), "got: {err}");
    }

    #[test]
    fn lock_released_after_drop() {
        let tmpdir = tempfile::tempdir().unwrap();
        let first = Store::open(
            tmpdir.path(),
            tmpdir.path(),
            StoreOptions {
                truncate: true,
                ..StoreOptions::default()
            },
        )
        .unwrap();
        drop(first);

        // After a clean drop the index-file lock is released, so a
        // subsequent open succeeds.
        let _second = Store::open(
            tmpdir.path(),
            tmpdir.path(),
            StoreOptions {
                truncate: false,
                ..StoreOptions::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn header_size_test() {
        let header_size = mem::size_of::<IndexFileHeader>();
        let entry_size = mem::size_of::<IndexEntry>();
        assert_eq!(64, header_size, "index header must match Go format");
        assert_eq!(16, entry_size, "index entry must match Go format");
        assert!(
            header_size.is_multiple_of(entry_size),
            "header size must be a multiple of entry size"
        );
        assert_eq!(
            mem::align_of::<IndexFileHeader>(),
            mem::align_of::<IndexEntry>(),
            "header and entry must have same alignment for zero-copy serialization"
        );
        assert_eq!(
            22,
            BlockHeader::SERIALIZED_SIZE,
            "block entry header must match Go format"
        );
    }

    #[test]
    fn smoke_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Async,
            1,
        )
        .unwrap();
        let block = vec![32; 1024];
        store.write_block(1, &block).unwrap();
        let block_read = store.read_block(1).unwrap().unwrap();
        assert_eq!(&block[..], &*block_read);

        // check the maximum contiguous height
        assert_eq!(1, store.max_contiguous_height());
    }

    #[test]
    fn height_highwater_diverges_with_gaps() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Async,
            1,
        )
        .unwrap();
        let block = vec![7u8; 64];

        store.write_block(1, &block).unwrap();
        assert_eq!(1, store.max_contiguous_height());
        assert_eq!(1, store.height_highwater());

        // Skip height 2, write 3: highwater advances, contiguous does not.
        store.write_block(3, &block).unwrap();
        assert_eq!(1, store.max_contiguous_height());
        assert_eq!(3, store.height_highwater());

        // Fill the gap: contiguous catches up to the highwater.
        store.write_block(2, &block).unwrap();
        assert_eq!(3, store.max_contiguous_height());
        assert_eq!(3, store.height_highwater());
    }

    #[test]
    fn parallel_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Async,
            1,
        )
        .unwrap();

        let height = AtomicU64::new(1);
        let data = vec![32; 1024];
        scope(|s| {
            let handles = (0..available_parallelism()
                .unwrap_or(NonZeroUsize::new(1).unwrap())
                .get())
                .map(|_| {
                    s.spawn(|| {
                        for _ in 0..100 {
                            let i = height.fetch_add(1, Ordering::Relaxed);
                            store.write_block(i, &data).unwrap();
                        }
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.join().unwrap();
            }
        });
        assert_eq!(
            height.load(Ordering::Relaxed) - 1,
            store.max_contiguous_height()
        );
    }

    #[test]
    fn recover_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Sync,
            1,
        )
        .unwrap();
        store.write_block(1, &vec![32; 1024]).unwrap();
        assert_eq!(1, store.max_contiguous_height());

        // simulate a crash
        simulate_crash(store);

        // recover the store
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            1,
        )
        .unwrap();
        assert_eq!(1, store.max_contiguous_height());
        assert_eq!(1, store.height_highwater.load(Ordering::Relaxed));

        // force a checkpoint
        store.checkpoint(*store.data_highwater.lock()).unwrap();

        // simulate a crash
        simulate_crash(store);

        // recover the store
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            2048, // bogus height, should be ignored
        )
        .unwrap();
        assert_eq!(1, store.max_contiguous_height());
        assert_eq!(1, store.height_highwater.load(Ordering::Relaxed));
    }

    /// Regression test for the bug where `recover()` set
    /// `max_contiguous_height` to the checkpointed highwater, hiding any
    /// gap that existed below it before the crash. Reopen must report the
    /// true contiguity floor, not the highwater.
    #[test]
    fn recover_preserves_gap_below_checkpointed_highwater() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Sync,
            1,
        )
        .unwrap();

        let block = vec![42u8; 128];
        store.write_block(1, &block).unwrap();
        store.write_block(2, &block).unwrap();
        // Skip height 3, jump ahead — leaves a gap that contiguity must reflect.
        store.write_block(4, &block).unwrap();
        store.write_block(5, &block).unwrap();

        assert_eq!(2, store.max_contiguous_height());
        assert_eq!(5, store.height_highwater());

        // Force a checkpoint so header.max_height = 5 (the highwater).
        store.checkpoint(*store.data_highwater.lock()).unwrap();
        simulate_crash(store);

        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            1,
        )
        .unwrap();
        assert_eq!(5, store.height_highwater());
        assert_eq!(
            2,
            store.max_contiguous_height(),
            "max_contiguous_height must reflect the gap at height 3, not the checkpointed highwater"
        );

        // Filling the gap should now advance contiguity all the way.
        store.write_block(3, &block).unwrap();
        assert_eq!(5, store.max_contiguous_height());
    }

    /// The contiguity floor is persisted in the header and restored on
    /// reopen, so recovery no longer rescans the whole index from
    /// `min_height` after a clean checkpoint.
    #[test]
    fn recover_restores_persisted_contiguous_floor() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Sync,
            1,
        )
        .unwrap();

        let block = vec![7u8; 64];
        for height in 1..=4 {
            store.write_block(height, &block).unwrap();
        }
        assert_eq!(4, store.max_contiguous_height());

        store.checkpoint(*store.data_highwater.lock()).unwrap();
        simulate_crash(store);

        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            1,
        )
        .unwrap();
        // Floor recovered, and it came from the persisted header field.
        assert_eq!(4, store.max_contiguous_height());
        assert_eq!(4, store.header.highest_contiguous_block_height);
    }

    /// With `min_height == 0` the contiguity floor of a single block at
    /// height 0 is itself 0 — the same value the header uses to mean
    /// "unknown". Reopen must still recover correctly: the fallback full
    /// scan reproduces the floor of 0, and a subsequent in-order write
    /// advances contiguity.
    #[test]
    fn recover_handles_zero_min_height_floor() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Sync,
            0,
        )
        .unwrap();

        let block = vec![9u8; 64];
        store.write_block(0, &block).unwrap();
        assert_eq!(0, store.max_contiguous_height());

        store.checkpoint(*store.data_highwater.lock()).unwrap();
        // Persisted floor is 0, indistinguishable from the "unknown" sentinel.
        assert_eq!(0, store.header.highest_contiguous_block_height);
        simulate_crash(store);

        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            0,
        )
        .unwrap();
        assert_eq!(0, store.max_contiguous_height());

        store.write_block(1, &block).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }

    #[test]
    fn default_filename_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Async,
            1,
        )
        .unwrap();

        store.write_block(1, &[1, 2, 3, 4]).unwrap();

        assert!(tmpdir.path().join(Store::INDEX_FILE_NAME).exists());
        assert!(tmpdir.path().join("blockdb_0.dat").exists());
    }

    #[test]
    fn height_zero_regression_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let first = vec![7; 256];
        let second = vec![9; 128];

        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            SyncMode::Sync,
            0,
        )
        .unwrap();

        store.write_block(0, &first).unwrap();
        store.write_block(1, &second).unwrap();

        assert_eq!(Some(first.clone().into()), store.read_block(0).unwrap());
        assert_eq!(Some(second.clone().into()), store.read_block(1).unwrap());
        assert_eq!(1, store.max_contiguous_height());

        simulate_crash(store);

        let recovered = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            999,
        )
        .unwrap();

        assert_eq!(Some(first.into()), recovered.read_block(0).unwrap());
        assert_eq!(Some(second.into()), recovered.read_block(1).unwrap());
        assert_eq!(1, recovered.max_contiguous_height());
    }

    // ---- multi-file (data-splitting) tests -----------------------------

    fn open_with_cap(
        path: &Path,
        cap: u64,
        truncate: bool,
        minimum_height: u64,
    ) -> Result<Store, Error> {
        Store::open(
            path,
            path,
            StoreOptions {
                truncate,
                sync: SyncMode::Sync,
                minimum_height,
                max_data_file_size: Some(NonZeroU64::new(cap).expect("nonzero")),
                ..StoreOptions::default()
            },
        )
    }

    /// Pseudo-random bytes from a seed via xxh64. Hash output compresses
    /// no better than random, so byte counts in tests stay close to the
    /// declared payload size regardless of zstd/snappy/none.
    fn incompressible(seed: u64, len: usize) -> Vec<u8> {
        (0..len as u64)
            .map(|i| xxh64(&i.to_le_bytes(), seed).to_le_bytes()[0])
            .collect()
    }

    /// Blocks that don't quite fit in the current file should skip to the
    /// start of the next data file. Mirrors blockdb's `TestDataSplitting`.
    #[test]
    fn data_splitting_writes_into_new_file() {
        let tmpdir = tempfile::tempdir().unwrap();
        // cap=400 + two ~322-byte (300 payload + ~22 header + minimal
        // compression overhead) blocks → second block must split.
        let cap = 400u64;
        let store = open_with_cap(tmpdir.path(), cap, true, 1).unwrap();

        let block_a = incompressible(1, 300);
        let block_b = incompressible(2, 300);
        store.write_block(1, &block_a).unwrap();
        store.write_block(2, &block_b).unwrap();

        // Both blocks should round-trip.
        assert_eq!(Some(block_a.clone().into()), store.read_block(1).unwrap());
        assert_eq!(Some(block_b.clone().into()), store.read_block(2).unwrap());

        // Both data files should exist; file 1 holds the second block.
        let file0 = tmpdir.path().join("blockdb_0.dat");
        let file1 = tmpdir.path().join("blockdb_1.dat");
        assert!(file0.exists(), "file 0 must exist");
        assert!(file1.exists(), "file 1 must exist after split");
        // file 0 should be at most `cap` bytes (it has a tail leak).
        assert!(fs::metadata(&file0).unwrap().len() <= cap);
    }

    /// A block bigger than the file cap is rejected.
    #[test]
    fn block_larger_than_cap_is_rejected() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = open_with_cap(tmpdir.path(), 256, true, 1).unwrap();
        // 4 KiB of incompressible data > 256 byte cap regardless of
        // compression.
        let huge = incompressible(0xDEAD_BEEF, 4096);
        let err = store.write_block(1, &huge).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    /// Reopen of a multi-file store must read both files correctly.
    #[test]
    fn multi_file_reopen_round_trips() {
        let tmpdir = tempfile::tempdir().unwrap();
        let cap = 400u64;
        let block_a = incompressible(11, 300);
        let block_b = incompressible(22, 300);
        let block_c = incompressible(33, 300);

        {
            let store = open_with_cap(tmpdir.path(), cap, true, 1).unwrap();
            store.write_block(1, &block_a).unwrap();
            store.write_block(2, &block_b).unwrap();
            store.write_block(3, &block_c).unwrap();
            // Drop closes cleanly; checkpoint runs.
        }

        let reopened = open_with_cap(tmpdir.path(), cap, false, 1).unwrap();
        assert_eq!(Some(block_a.into()), reopened.read_block(1).unwrap());
        assert_eq!(Some(block_b.into()), reopened.read_block(2).unwrap());
        assert_eq!(Some(block_c.into()), reopened.read_block(3).unwrap());
        assert_eq!(3, reopened.max_contiguous_height());
    }

    /// A missing intermediate data file is detected as corruption on open.
    /// Mirrors blockdb's `TestDataSplitting_DeletedFile`.
    #[test]
    fn missing_intermediate_data_file_is_corruption() {
        let tmpdir = tempfile::tempdir().unwrap();
        let cap = 400u64;
        {
            let store = open_with_cap(tmpdir.path(), cap, true, 1).unwrap();
            for h in 1..=4u64 {
                store.write_block(h, &incompressible(h, 300)).unwrap();
            }
        }

        // Confirm multiple expected files exist, then delete file 0.
        let file0 = tmpdir.path().join("blockdb_0.dat");
        let file1 = tmpdir.path().join("blockdb_1.dat");
        assert!(file0.exists() && file1.exists());
        fs::remove_file(&file0).unwrap();

        let err = open_with_cap(tmpdir.path(), cap, false, 1).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    /// Crash mid-write of a block in file 1 should recover the indexed
    /// blocks and skip the partially-written one.
    #[test]
    fn multi_file_recovery_skips_partial_tail() {
        let tmpdir = tempfile::tempdir().unwrap();
        let cap = 400u64;
        let block_a = incompressible(0xAA, 300);
        let block_b = incompressible(0xBB, 300);

        {
            let store = open_with_cap(tmpdir.path(), cap, true, 1).unwrap();
            store.write_block(1, &block_a).unwrap();
            store.write_block(2, &block_b).unwrap();
            simulate_crash(store);
        }

        // Both files should exist.
        assert!(tmpdir.path().join("blockdb_0.dat").exists());
        assert!(tmpdir.path().join("blockdb_1.dat").exists());

        // Recover and verify both blocks come back.
        let reopened = open_with_cap(tmpdir.path(), cap, false, 1).unwrap();
        assert_eq!(Some(block_a.into()), reopened.read_block(1).unwrap());
        assert_eq!(Some(block_b.into()), reopened.read_block(2).unwrap());
        assert_eq!(2, reopened.max_contiguous_height());
    }

    // ---- recovery / corruption-detection tests -------------------------
    //
    // Ported from blockdb's `TestRecovery_CorruptionDetection`. Each test
    // writes blocks, simulates a crash via `simulate_crash(store)` (so the
    // index header stays stale and recovery does a fresh scan), corrupts a byte
    // on disk, and asserts that the corrupted block (and all that follow)
    // are not picked up by recovery.

    /// Reads the BlockHeader.size field at `local_offset` in `blockdb_0.dat`.
    /// Used to compute the on-disk offset of subsequent blocks.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "test helper with known-small offsets"
    )]
    fn read_block_size_field(dir: &Path, local_offset: u64) -> u32 {
        let path = dir.join("blockdb_0.dat");
        let file = File::open(path).unwrap();
        let mut buf = [0u8; 4];
        // BlockHeader layout: height(8) || size(4) || checksum(8) || version(2)
        file.read_at(&mut buf, local_offset + 8).unwrap();
        u32::from_le_bytes(buf)
    }

    /// Writes a single byte at `local_offset` in `blockdb_0.dat`. Used to
    /// flip specific fields of an on-disk block header or payload.
    fn poke_byte(dir: &Path, local_offset: u64, value: u8) {
        let path = dir.join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&[value], local_offset).unwrap();
        file.sync_all().unwrap();
    }

    /// Writes `n` incompressible blocks at heights `1..=n` via the store,
    /// then `forget`s the store so the index header stays stale and the
    /// next open triggers a full data-file scan.
    fn write_blocks_and_crash(dir: &Path, n: u64, block_size: usize) {
        let store = Store::open(
            dir,
            dir,
            StoreOptions {
                truncate: true,
                sync: SyncMode::Sync,
                minimum_height: 1,
                ..StoreOptions::default()
            },
        )
        .unwrap();
        for h in 1..=n {
            store
                .write_block(h, &incompressible(h, block_size))
                .unwrap();
        }
        simulate_crash(store);
    }

    fn reopen(dir: &Path) -> Result<Store, Error> {
        Store::open(
            dir,
            dir,
            StoreOptions {
                truncate: false,
                sync: SyncMode::Sync,
                minimum_height: 1,
                ..StoreOptions::default()
            },
        )
    }

    /// A bit-flip in the compressed payload changes the xxh64 of the
    /// decompressed bytes, so recovery rejects the block.
    #[test]
    fn recovery_detects_checksum_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        // Corrupt one byte inside block 2's payload. Block 1's total size
        // on disk is HEADER + compressed_size_of_block_1; that's where
        // block 2's header starts. Flip a byte 32 into block 2's payload.
        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        let block_2_payload_offset = block_2_offset + BlockHeader::SERIALIZED_SIZE as u64;
        poke_byte(dir.path(), block_2_payload_offset + 32, 0xFF);

        let store = reopen(dir.path()).unwrap();
        // Block 1 was recovered; block 2 onward was rejected by the scan.
        assert_eq!(1, store.max_contiguous_height());
        assert!(store.read_block(1).unwrap().is_some());
        // Block 2's index entry survives from before the crash, so a
        // direct read goes to disk and surfaces the corruption as
        // `InvalidData` rather than `None`.
        let err = store.read_block(2).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    /// Setting the block's `size` field above `MAX_BLOCK_SIZE` makes the
    /// header fail sanity, so recovery stops at it.
    #[test]
    fn recovery_detects_invalid_block_size() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        // Write a huge size value (4 bytes at block_2_offset + 8).
        let huge: u32 = BlockHeader::MAX_BLOCK_SIZE + 1;
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&huge.to_le_bytes(), block_2_offset + 8)
            .unwrap();
        file.sync_all().unwrap();

        let store = reopen(dir.path()).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }

    /// Recovery rejects blocks whose version doesn't match the writer's
    /// expected version constant.
    #[test]
    fn recovery_detects_invalid_version() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        // BlockHeader version is at offset 20 (u16 LE).
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&99u16.to_le_bytes(), block_2_offset + 20)
            .unwrap();
        file.sync_all().unwrap();

        let store = reopen(dir.path()).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }

    /// Recovery rejects blocks with `size == 0` in the header.
    #[test]
    fn recovery_detects_zero_block_size() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&0u32.to_le_bytes(), block_2_offset + 8)
            .unwrap();
        file.sync_all().unwrap();

        let store = reopen(dir.path()).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }

    /// Recovery rejects blocks whose height falls below `min_height`.
    #[test]
    fn recovery_detects_invalid_height() {
        let dir = tempfile::tempdir().unwrap();
        // Open with min_height=5; corrupt block 2's height to 1.
        let store = Store::open(
            dir.path(),
            dir.path(),
            StoreOptions {
                truncate: true,
                sync: SyncMode::Sync,
                minimum_height: 5,
                ..StoreOptions::default()
            },
        )
        .unwrap();
        for h in 5..=8 {
            store.write_block(h, &incompressible(h, 256)).unwrap();
        }
        simulate_crash(store);

        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        // Write height=1 (below min_height of 5).
        file.write_all_at(&1u64.to_le_bytes(), block_2_offset)
            .unwrap();
        file.sync_all().unwrap();

        let store = Store::open(
            dir.path(),
            dir.path(),
            StoreOptions {
                truncate: false,
                sync: SyncMode::Sync,
                minimum_height: 5,
                ..StoreOptions::default()
            },
        )
        .unwrap();
        // Only block 5 (the first one) is recovered.
        assert_eq!(5, store.max_contiguous_height());
    }

    /// A data file truncated mid-payload (the block header is intact but
    /// the payload runs past EOF) is detected: that block and any after
    /// are not recovered.
    #[test]
    fn recovery_handles_partial_block_at_eof() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        // Truncate the file 8 bytes into block 2's payload.
        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        let truncate_to = block_2_offset + BlockHeader::SERIALIZED_SIZE as u64 + 8;
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(truncate_to).unwrap();
        file.sync_all().unwrap();

        let store = reopen(dir.path()).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }

    /// Garbage bytes where a block header should be (all 0xFF) fails the
    /// sanity check on the size field (>= `MAX_BLOCK_SIZE`) and on the
    /// version field, so recovery stops cleanly.
    #[test]
    fn recovery_handles_garbage_block_header() {
        let dir = tempfile::tempdir().unwrap();
        write_blocks_and_crash(dir.path(), 4, 256);

        let block_1_compressed = read_block_size_field(dir.path(), 0);
        let block_2_offset = u64::from(block_1_compressed) + BlockHeader::SERIALIZED_SIZE as u64;
        let path = dir.path().join("blockdb_0.dat");
        let file = OpenOptions::new().write(true).open(path).unwrap();
        let garbage = [0xFFu8; BlockHeader::SERIALIZED_SIZE];
        file.write_all_at(&garbage, block_2_offset).unwrap();
        file.sync_all().unwrap();

        let store = reopen(dir.path()).unwrap();
        assert_eq!(1, store.max_contiguous_height());
    }
}
