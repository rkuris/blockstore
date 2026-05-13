//!
//! A store is a collection of blockchain blocks. Blocks can be added to the store in any order.
//! # [Store]
//!
//! The structure of the file contains a single [`IndexFileHeader`] and a sequence of [`IndexEntry`] entries.

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Write as _};
use std::num::NonZeroUsize;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{fmt, mem};

use bytemuck::{Pod, Zeroable};

use xxhash_rust::xxh64::xxh64;

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

#[derive(Clone, Debug, PartialEq)]
#[repr(C)]
pub enum SyncMode {
    Async = 0,
    Sync = 1,
}

impl Display for SyncMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncMode::Async => write!(f, "async"),
            SyncMode::Sync => write!(f, "sync"),
        }
    }
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
    sync: SyncMode,
    max_contiguous_height: AtomicU64,
    height_highwater: AtomicU64,
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
    // Reserved bytes to keep header size equal to Go implementation (64 bytes total).
    pub reserved: [u8; 24],
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
            reserved: [0; 24],
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

impl Store {
    const INDEX_FILE_NAME: &'static str = "blockdb.idx";
    const DATA_FILE_NAME: &'static str = "blockdb_0.dat";

    /// We write the header to the index file every 1024 blocks
    const CHECKPOINT_INTERVAL: u64 = 1024;

    fn resolve_index_filename(path: &Path) -> PathBuf {
        if path.is_dir() {
            path.join(Self::INDEX_FILE_NAME)
        } else {
            path.to_path_buf()
        }
    }

    fn resolve_data_filename(path: &Path) -> PathBuf {
        if path.is_dir() {
            path.join(Self::DATA_FILE_NAME)
        } else {
            path.to_path_buf()
        }
    }

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
        sync: SyncMode,
        minimum_height: u64,
    ) -> Result<Self, Error> {
        let mut opts = OpenOptions::new();

        let index_filename = Self::resolve_index_filename(index_path);
        let data_filename = Self::resolve_data_filename(data_path);
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
            data_highwater: Mutex::new(0),
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

        // if the data file is smaller than the index file, then we need to truncate the data file
        if data_file_actual_size < self.header.next_write_offset {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "data file is smaller than the index file indicates",
            ));
        }

        self.max_contiguous_height
            .store(self.header.max_height, Ordering::Relaxed);
        self.height_highwater
            .store(self.header.max_height, Ordering::Relaxed);

        // if the data file is larger than the index file, then we need to read the data and
        // see if we can apply any of it
        if data_file_actual_size > self.header.next_write_offset {
            // start reading the data file until we reach the end
            while let Ok(block_header) = self.read_block_header_at(self.header.next_write_offset) {
                let height = block_header.height;
                let size = block_header.size;
                // sanity checks
                if height < self.header.min_height {
                    break;
                }
                if size == 0 || size > BlockHeader::MAX_BLOCK_SIZE {
                    break;
                }
                if self.header.next_write_offset.saturating_add(size.into()) > data_file_actual_size
                {
                    break;
                }

                // block looks okay, lets read it. We know it's smaller than MAX_BLOCK_SIZE which
                // will not overflow usize

                self.header.next_write_offset = self
                    .header
                    .next_write_offset
                    .wrapping_add(BlockHeader::SERIALIZED_SIZE as u64);

                #[allow(clippy::cast_possible_truncation)]
                let mut compressed_block = vec![0; size as usize];
                self.data_file
                    .read_at(&mut compressed_block, self.header.next_write_offset)?;

                // decompress the block data
                let block = decompress(&compressed_block);

                // verify the checksum (checksum is calculated on the original uncompressed data)
                let checksum = xxh64(&block, 0);
                if checksum != block_header.checksum {
                    break;
                }

                let index_entry = IndexEntry {
                    offset: self
                        .header
                        .next_write_offset
                        .saturating_sub(BlockHeader::SERIALIZED_SIZE as u64),
                    size,
                    reserved: [0; 4],
                };
                self.update_index(height, index_entry)?;
                self.update_highwater(height);
                self.advance_max_contiguous_height(height);

                self.header.next_write_offset =
                    self.header.next_write_offset.wrapping_add(size.into());
            }
        }

        *self.data_highwater.lock().unwrap() = self.header.next_write_offset;

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
    pub fn write_block(
        &self,
        height: BlockHeight,
        block: &[u8],
        _header_size: u16,
    ) -> Result<(), Error> {
        #[cfg(feature = "metrics")]
        let start = coarsetime::Instant::now();

        // prohibit writes of zero length blocks
        if block.is_empty() {
            counter!("blockstore.write_block.empty").increment(1);
            return Err(Error::new(ErrorKind::InvalidInput, "Block is empty"));
        }

        let _: u32 = block.len().try_into().map_err(|_| {
            counter!("blockstore.write_block.block_too_large").increment(1);
            Error::new(ErrorKind::InvalidInput, "Block too large")
        })?;

        // compress the block
        let compressed_block = compress(block);
        let compressed_block_len: u32 = compressed_block.len().try_into().unwrap();

        // check the index file offset for overflows
        // this limits our block height to 2^64 / size_of::<IndexEntry>(), or 2^60 blocks
        let index_entry_offset = self.index_entry_offset(height).ok_or_else(|| {
            counter!("blockstore.write_block.invalid_block_height").increment(1);
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
            .write_all_at(&header.serialize(), offset)
            .inspect_err(|_| {
                counter!("blockstore.write_block.write_header_failed").increment(1);
            })?;

        // write the compressed block data
        // safe to use wrapping_add here because we checked for overflow above
        // TODO: use a single write_all_at call to write both the header and the data
        // saves a syscall but requires copying the data around in memory
        self.data_file
            .write_all_at(
                &compressed_block,
                offset.wrapping_add(BlockHeader::SERIALIZED_SIZE as u64),
            )
            .inspect_err(|_| {
                counter!("blockstore.write_block.write_data_failed").increment(1);
            })?;

        if self.sync == SyncMode::Sync {
            #[cfg(feature = "metrics")]
            let sync_start = coarsetime::Instant::now();
            self.data_file.sync_all()?;
            record_duration!(sync_start, "blockstore.write_block.sync_duration_ms");
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

        counter!("blockstore.write_block.success").increment(1);
        record_duration!(start, "blockstore.write_block.success.duration_ms");

        Ok(())
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
            counter!("blockstore.update_index.invalid_block_height").increment(1);
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
        self.data_file.read_at(&mut buf, offset)?;
        BlockHeader::deserialize(&buf)
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

        let entry = self.read_index_entry(height).inspect_err(|_| {
            counter!("blockstore.read_block.read_index_entry_failed").increment(1);
        })?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        let block_size = entry.size;
        if block_size == 0 {
            counter!("blockstore.read_block.not_found").increment(1);
            return Ok(None);
        }
        // TODO: we know the size and can read the whole header and data in one read...

        // read the block header
        let blockheader = self.read_block_header_at(entry.offset).inspect_err(|_| {
            counter!("blockstore.read_block.read_header_failed").increment(1);
        })?;

        if blockheader.size != block_size {
            counter!("blockstore.read_block.block_size_mismatch").increment(1);
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
                counter!("blockstore.read_block.block_size_too_large").increment(1);
            })
            .map_err(|_| Error::new(ErrorKind::InvalidData, "block size too large"))?;

        let mut compressed_block = vec![0; block_size];
        // checked_add should be infallable (overflowing here means that the block offset is almost at 2^64, which is insane)
        self.data_file.read_at(
            &mut compressed_block,
            entry
                .offset
                .checked_add(BlockHeader::SERIALIZED_SIZE as u64)
                .expect("block offset overflow"),
        )?;

        // decompress the block data
        let block = decompress(&compressed_block);

        // verify the checksum (checksum is calculated on the original uncompressed data)
        let checksum = xxh64(&block, 0);
        if checksum != blockheader.checksum {
            counter!("blockstore.read_block.checksum_mismatch").increment(1);
            return Err(Error::new(ErrorKind::InvalidData, "checksum mismatch"));
        }

        let block = Some(block.into());
        counter!("blockstore.read_block.success").increment(1);
        record_duration!(start, "blockstore.read_block.success.duration_ms");
        Ok(block)
    }

    pub fn max_contiguous_height(&self) -> BlockHeight {
        self.max_contiguous_height.load(Ordering::Relaxed)
    }

    pub fn min_block_height(&self) -> BlockHeight {
        self.header.min_height
    }

    /// The Go-compatible format does not store a separate per-block header size,
    /// so extracting a logical "block header" slice is not supported.
    ///
    /// # Errors
    /// Always returns `ErrorKind::Unsupported`.
    pub fn read_block_header(&self, height: BlockHeight) -> Result<Option<Block>, Error> {
        let _ = height;
        Err(Error::new(
            ErrorKind::Unsupported,
            "read_block_header is not supported by Go-compatible on-disk format",
        ))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.sync == SyncMode::Sync {
            self.index_file.sync_all().unwrap();
        }
        // if this fails, no biggie, we'll just have to do recovery at startup
        let _ = self.checkpoint(*self.data_highwater.lock().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::mem::forget;
    use std::thread::{available_parallelism, scope};

    use super::*;

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
        store.write_block(1, &block, 0).unwrap();
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
                            store.write_block(i, &data, 0).unwrap();
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
        store.write_block(1, &vec![32; 1024], 0).unwrap();
        assert_eq!(1, store.max_contiguous_height());

        // simulate a crash
        forget(store);

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
        store
            .checkpoint(*store.data_highwater.lock().unwrap())
            .unwrap();

        // simulate a crash
        forget(store);

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

        store.write_block(1, &[1, 2, 3, 4], 0).unwrap();

        assert!(tmpdir.path().join(Store::INDEX_FILE_NAME).exists());
        assert!(tmpdir.path().join(Store::DATA_FILE_NAME).exists());
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

        store.write_block(0, &first, 0).unwrap();
        store.write_block(1, &second, 0).unwrap();

        assert_eq!(
            Some(first.clone().into_boxed_slice()),
            store.read_block(0).unwrap()
        );
        assert_eq!(
            Some(second.clone().into_boxed_slice()),
            store.read_block(1).unwrap()
        );
        assert_eq!(1, store.max_contiguous_height());

        forget(store);

        let recovered = Store::new(
            tmpdir.path(),
            tmpdir.path(),
            NonZeroUsize::new(1024).unwrap(),
            false,
            SyncMode::Sync,
            999,
        )
        .unwrap();

        assert_eq!(
            Some(first.into_boxed_slice()),
            recovered.read_block(0).unwrap()
        );
        assert_eq!(
            Some(second.into_boxed_slice()),
            recovered.read_block(1).unwrap()
        );
        assert_eq!(1, recovered.max_contiguous_height());
    }
}
