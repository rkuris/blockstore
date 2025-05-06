//!
//! A store is a collection of blockchain blocks. Blocks can be added to the store in any order.
//! # [Store]
//!
//! The structure of the file contains a single [`IndexFileHeader`] and a sequence of [`IndexEntry`] entries.

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Write as _};
use std::mem;
use std::num::NonZeroUsize;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use bytemuck::{Pod, Zeroable};

use crate::{Block, BlockHeight};

#[cfg(feature = "metrics")]
use metrics::counter;

#[cfg(not(feature = "metrics"))]
macro_rules! counter {
    ($key:expr) => {{
        struct FakeCounter;
        impl FakeCounter {
            pub fn increment(&self, _: u64) {
                // do nothing
            }
        }
        FakeCounter {}
    }};
}

#[cfg(feature = "metrics")]
macro_rules! record_duration {
    ($start:expr, $key:expr) => {
        let duration = coarsetime::Instant::now().duration_since($start);
        counter!($key).increment(duration.as_millis() as u64);
    };
}
#[cfg(not(feature = "metrics"))]
macro_rules! record_duration {
    ($start:expr, $key:expr) => {
        // do nothing
    };
}

#[derive(Debug)]
pub struct Store {
    // TODO: add a recent block cache here
    // recents: crate::recents::Recents,
    index_file: File,
    // TODO: add support for multiple data files here
    data_file: File,
    data_highwater: Mutex<u64>,
    header: IndexFileHeader,
    sync: bool,
    max_contiguous_height: AtomicU64,
    height_highwater: AtomicU64,
}

/// The size of a block in the store.
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct IndexEntry {
    pub offset: u64,
    pub size: u64,
}

/// The header of the index file.
///
/// This is written to the index file and is used to recover the store from a crash.
/// This MUST be a multiple of the [`IndexEntry`] size.
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct IndexFileHeader {
    // The version of the index file format
    pub version: u32,
    // The maximum size of the index file in MB
    pub max_file_size_mb: u32,
    // The lowest block height in the index file
    // TODO: this should be a BlockHeight
    pub lowest_block_height: u64,
    // The highest block height in the index file
    pub highest_contiguous_block_height: u64,
    // The size of the data file in bytes
    pub data_file_size: u64,
}

impl IndexFileHeader {
    const INDEX_FILE_VERSION: u32 = 1;

    fn with_lowest_block_height(self, lowest_block_height: u64) -> Self {
        Self {
            lowest_block_height,
            ..self
        }
    }
}

impl Default for IndexFileHeader {
    fn default() -> Self {
        Self {
            version: Self::INDEX_FILE_VERSION,
            max_file_size_mb: 1024,
            lowest_block_height: 1,
            highest_contiguous_block_height: 0,
            data_file_size: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
#[repr(C)]
struct BlockHeader {
    height: u64,
    size: u64,
    checksum: u64,
}

impl Store {
    /// We write the header to the index file every 1024 blocks
    const CHECKPOINT_INTERVAL: u64 = 1024;

    /// Creates a new store.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The file cannot be opened
    /// - The file cannot be truncated
    pub fn new(
        index_path: &Path,
        data_path: &Path,
        _cache_size: NonZeroUsize,
        truncate: bool,
        sync: bool,
        minimum_height: u64,
    ) -> Result<Self, Error> {
        let mut opts = OpenOptions::new();

        let index_filename = index_path.with_extension("idx");
        let data_filename = data_path.with_extension("dat");
        let opts = opts
            .create(truncate)
            .truncate(truncate)
            .write(true)
            .read(true);
        let mut index_file = opts.open(index_filename)?;
        let data_file = opts.open(data_filename)?;

        let header = if truncate {
            let header = IndexFileHeader::default().with_lowest_block_height(minimum_height);
            index_file.write_all(bytemuck::bytes_of(&header))?;
            header
        } else {
            let mut header = IndexFileHeader::default();
            index_file.read_at(bytemuck::bytes_of_mut(&mut header), 0)?;
            header
        };
        let mut result = Self {
            index_file,
            data_file,
            data_highwater: Mutex::new(mem::size_of::<IndexFileHeader>() as u64),
            header,
            sync,
            max_contiguous_height: AtomicU64::new(minimum_height.saturating_sub(1)),
            height_highwater: AtomicU64::new(minimum_height.saturating_sub(1)),
        };
        if !truncate {
            result.recover()?;
        }

        // we use saturating_sub here, so that if you use 0, we're actually off by 1
        // until you write the first block
        Ok(result)
    }

    fn recover(&mut self) -> Result<(), Error> {
        // see if the index file contains the correct block file size. This happens
        // if the database was closed cleanly.
        let data_file_actual_size = self.data_file.metadata()?.len();
        if data_file_actual_size == self.header.data_file_size {
            return Ok(());
        }
        // if the data file is larger than the index file, then we need to read the data and
        // see if we can apply any of it
        if data_file_actual_size > self.header.data_file_size {
            todo!()
        }

        // if the data file is smaller than the index file, then we need to truncate the data file
        if data_file_actual_size < self.header.data_file_size {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "data file is smaller than the index file indicates",
            ));
        }

        Ok(())
    }

    /// Inserts a block into the store.
    ///
    /// # Errors
    /// Returns an error if the block cannot be written
    ///
    /// # Panics
    /// Panics if the cache lock has been poisoned.
    #[allow(clippy::too_many_lines)]
    pub fn write_block(&self, height: BlockHeight, block: &[u8]) -> Result<(), Error> {
        #[cfg(feature = "metrics")]
        let start = coarsetime::Instant::now();

        // prohibit writes of zero length blocks
        if block.is_empty() {
            counter!("blockstore.write_block.empty").increment(1);
            return Err(Error::new(ErrorKind::InvalidInput, "Block is empty"));
        }

        // check the index file offset for overflows
        // this limits our block height to 2^64 / size_of::<IndexEntry>(), or 2^60 blocks
        let index_entry_offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.write_block.invalid_block_height").increment(1);
            Error::new(ErrorKind::InvalidInput, "Invalid block height")
        })?;

        // calculate the size of the block with the header. An overflow here only happens if the block is
        // just under MAX_U64, which is a mighty big buffer to be passing to this function...
        let size_with_header = block
            .len()
            .checked_add(mem::size_of::<BlockHeader>())
            .expect("blocks will never be so large as to overflow u64")
            as u64;

        // calculate the hash of the block
        let checksum = fxhash::hash64(&block);

        // construct the block header
        let header = BlockHeader {
            height,
            size: block.len() as u64,
            checksum,
        };

        // barring running out of space or an IO error, we can now be sure the block can be written,
        // so grab and update the highwater mark and start the write.
        // TODO: what happens if we run out of space after updating the highwater mark?
        // We can't really reduce it because someone else may already be writing past us, but we might
        // want to consider restoring it?
        let mut guard = self.data_highwater.lock().unwrap();
        let offset = *guard;
        *guard = guard.checked_add(size_with_header).ok_or_else(|| {
            counter!("blockstore.write_block.block_too_large").increment(1);
            Error::new(ErrorKind::InvalidInput, "Block too large")
        })?;
        let saved_offset = *guard;
        // drop the guard here, this allows for parallel writes
        drop(guard);

        self.data_file
            .write_all_at(bytemuck::bytes_of(&header), offset)
            .inspect_err(|_| {
                counter!("blockstore.write_block.write_header_failed").increment(1);
            })?;

        // write the block data
        // safe to use wrapping_add here because we checked for overflow above
        // TODO: use a single write_all_at call to write both the header and the data
        // saves a syscall but requires copying the data around in memory
        self.data_file
            .write_all_at(
                block,
                offset.wrapping_add(mem::size_of::<BlockHeader>() as u64),
            )
            .inspect_err(|_| {
                counter!("blockstore.write_block.write_data_failed").increment(1);
            })?;

        if self.sync {
            #[cfg(feature = "metrics")]
            let sync_start = coarsetime::Instant::now();
            self.data_file.sync_all()?;
            record_duration!(sync_start, "blockstore.write_block.sync_duration_ms");
        }

        // update the index file
        let index_entry = IndexEntry {
            offset: index_entry_offset,
            size: block.len() as u64,
        };
        self.index_file
            .write_all_at(bytemuck::bytes_of(&index_entry), index_entry_offset)
            .inspect_err(|_| counter!("blockstore.write_block.write_index_failed").increment(1))?;

        // optimize for the case where the block height is the next contiguous height
        // if the highest contiguous height is the one block before us, then we update it
        // and start looking for the next gap
        let prev = height.saturating_sub(1);
        let _ = self
            .max_contiguous_height
            .compare_exchange(prev, height, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                counter!("blockstore.write_block.out_of_order").increment(1);
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
                        if entry.offset == 0 {
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

        if self
            .height_highwater
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                if old < height { Some(height) } else { None }
            })
            .is_ok()
            && height % Self::CHECKPOINT_INTERVAL == 0
        {
            self.checkpoint(saved_offset)?;
        }

        counter!("blockstore.write_block.success").increment(1);
        record_duration!(start, "blockstore.write_block.success.duration_ms");

        Ok(())
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
        if self.sync {
            self.index_file.sync_all()?;
        }
        let mut header = self.header;
        header.data_file_size = saved_offset;
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
            .checked_sub(self.header.lowest_block_height)?
            .checked_mul(mem::size_of::<IndexEntry>() as u64)?
            .checked_add(mem::size_of::<IndexFileHeader>() as u64)
    }

    fn read_index_entry(&self, height: BlockHeight) -> Result<Option<IndexEntry>, Error> {
        let offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.read_index_entry.invalid_block_height").increment(1);
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
        let start = coarsetime::Instant::now();

        let index_entry = self.read_index_entry(height).inspect_err(|_| {
            counter!("blockstore.read_block.read_index_entry_failed").increment(1);
        })?;
        if let Some(index_entry) = index_entry {
            let block_size = index_entry.size;
            if block_size == 0 {
                counter!("blockstore.read_block.not_found").increment(1);
                return Ok(None);
            }
            // TODO: we know the size and can read the whole header and data in one read...

            // read the block header
            let mut blockheader = BlockHeader::default();
            self.data_file
                .read_at(bytemuck::bytes_of_mut(&mut blockheader), index_entry.offset)
                .inspect_err(|_| {
                    counter!("blockstore.read_block.read_header_failed").increment(1);
                })?;

            if blockheader.size != block_size {
                counter!("blockstore.read_block.block_size_mismatch").increment(1);
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "block size in index file does not match data",
                ));
            }

            // read the block data
            // this conversion is infallable on 64 bit systems, but not on 32 bit systems
            let block_size: usize = block_size
                .try_into()
                .inspect_err(|_| {
                    counter!("blockstore.read_block.block_size_too_large").increment(1);
                })
                .map_err(|_| Error::new(ErrorKind::InvalidData, "block size too large"))?;

            let mut block = vec![0; block_size];
            // checked_add should be infallable (overflowing here means that the block offset is almost at 2^64, which is insane)
            self.data_file.read_at(
                &mut block,
                index_entry
                    .offset
                    .checked_add(mem::size_of::<BlockHeader>() as u64)
                    .expect("block offset overflow"),
            )?;

            // verify the checksum
            let checksum = fxhash::hash64(&block);
            if checksum != blockheader.checksum {
                counter!("blockstore.read_block.checksum_mismatch").increment(1);
                return Err(Error::new(ErrorKind::InvalidData, "checksum mismatch"));
            }

            let block = Some(block.into());
            counter!("blockstore.read_block.success").increment(1);
            record_duration!(start, "blockstore.read_block.success.duration_ms");
            return Ok(block);
        }
        counter!("blockstore.read_block.not_found").increment(1);
        Ok(None)
    }

    pub fn max_contiguous_height(&self) -> BlockHeight {
        self.max_contiguous_height.load(Ordering::Relaxed)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.sync {
            self.index_file.sync_all().unwrap();
        }
        // if this fails, no biggie, we'll just have to do recovery at startup
        let _ = self.checkpoint(*self.data_highwater.lock().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use std::thread::{available_parallelism, scope};

    use super::*;

    #[test]
    fn header_size_test() {
        let header_size = mem::size_of::<IndexFileHeader>();
        let entry_size = mem::size_of::<IndexEntry>();
        assert!(
            header_size % entry_size == 0,
            "header size must be a multiple of the entry size"
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
            false,
            1,
        )
        .unwrap();
        let block = vec![32; 1024];
        store.write_block(1, &block).unwrap();
        let block_read = store.read_block(1).unwrap().unwrap();
        assert_eq!(block.into_boxed_slice(), block_read);

        // check the maximum contiguous height
        assert_eq!(1, store.max_contiguous_height());
    }

    #[test]
    fn parallel_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            true,
            false,
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
}
