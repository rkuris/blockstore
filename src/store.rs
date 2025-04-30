//! # [Store]
//!
//! A store is a collection of blockchain blocks. Blocks can be added to the store in any order.
//!
//! The structure of the file contains a single [`IndexFileHeader`] and a sequence of [`IndexEntry`] entries.

use std::{
    fs::{File, OpenOptions},
    io::{Error, ErrorKind, Write as _},
    mem,
    num::NonZeroUsize,
    os::unix::fs::FileExt,
    path::Path,
    sync::Mutex,
};

use bytemuck::{Pod, Zeroable};

use crate::{Block, BlockHeight};

#[derive(Debug)]
pub struct Store {
    // TODO: add a recent block cache here
    // recents: crate::recents::Recents,
    index_file: File,
    // TODO: add support for multiple data files here
    data_file: File,
    highwater: Mutex<u64>,
    header: IndexFileHeader,
}

/// The size of a block in the store.
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct IndexEntry {
    pub offset: u64,
    pub size: u64,
}

/// The header of the index file.
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
}

impl IndexFileHeader {
    const INDEX_FILE_VERSION: u32 = 1;
}

impl Default for IndexFileHeader {
    fn default() -> Self {
        Self {
            version: Self::INDEX_FILE_VERSION,
            max_file_size_mb: 1024,
            lowest_block_height: 1,
            highest_contiguous_block_height: 0,
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
            let header = IndexFileHeader::default();
            index_file.write_all(bytemuck::bytes_of(&header))?;
            header
        } else {
            let mut header = IndexFileHeader::default();
            index_file.read_at(bytemuck::bytes_of_mut(&mut header), 0)?;
            header
        };

        Ok(Self {
            index_file,
            data_file,
            highwater: Mutex::new(mem::size_of::<IndexFileHeader>() as u64),
            header,
        })
    }

    /// Inserts a block into the store.
    ///
    /// # Errors
    /// Returns an error if the block cannot be written
    ///
    /// # Panics
    /// Panics if the cache lock has been poisoned.
    pub fn write_block(&self, height: BlockHeight, block: &[u8]) -> Result<(), Error> {
        // prohibit writes of zero length blocks
        if block.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "Block is empty"));
        }

        // first, check the index file offset for overflows
        // this effectively limits our block height to 2^64 / size_of::<IndexEntry>(), or 2^60 blocks
        let index_entry_offset = self
            .index_entry_offset(height)
            .ok_or(Error::new(ErrorKind::InvalidInput, "Invalid block height"))?;

        // calculate the size of the block with the header. An overflow here only happens if the block is
        // just under MAX_U64, which is a mighty big buffer to be passing to this function...
        let size_with_header = block
            .len()
            .checked_add(mem::size_of::<BlockHeader>())
            .expect("blocks will never be so large as to overflow u64")
            as u64;

        // barring running out of space or an IO error, we can now be sure the block can be written,
        // so update the highwater mark and start the write.
        // TODO: what happens if we run out of space after updating the highwater mark?
        // We can't really reduce it because someone else may already be writing past us, but we might
        // want to consider restoring it?
        let mut guard = self.highwater.lock().unwrap();
        let offset = *guard;
        *guard = guard
            .checked_add(size_with_header)
            .ok_or(Error::new(ErrorKind::InvalidInput, "Block too large"))?;
        drop(guard);

        // calculate the hash of the block
        let checksum = fxhash::hash64(&block);

        // construct and write the block header
        let header = BlockHeader {
            height,
            size: block.len() as u64,
            checksum,
        };
        self.data_file
            .write_all_at(bytemuck::bytes_of(&header), offset)?;

        // write the block data
        // safe to use wrapping_add here because we checked for overflow above
        // TODO: use a single write_all_at call to write both the header and the data
        // saves a syscall but requires copying the data around in memory
        self.data_file
            .write_all_at(block, offset.wrapping_add(mem::size_of::<BlockHeader>() as u64))?;

        // update the index file
        let index_entry = IndexEntry {
            offset: index_entry_offset,
            size: block.len() as u64,
        };
        self.index_file.write_all_at(bytemuck::bytes_of(&index_entry), index_entry_offset)?;

        // TODO: update the highest contiguous block height

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
        if let Some(offset) = self.index_entry_offset(height) {
            let mut index_entry = IndexEntry::default();
            self.index_file
                .read_at(bytemuck::bytes_of_mut(&mut index_entry), offset)?;
            let block_size = index_entry.size;
            if block_size == 0 {
                return Ok(None);
            }
            // TODO: we know the size and can read the whole header and data in one read...

            // read the block header
            let mut blockheader = BlockHeader::default();
            self.data_file
                .read_at(bytemuck::bytes_of_mut(&mut blockheader), offset)?;

            if blockheader.size != block_size {
                return Err(Error::new(ErrorKind::InvalidData, "block size in index file does not match data"));
            }

            // read the block data
            // this conversion is infallable on 64 bit systems, but not on 32 bit systems
            let block_size: usize = block_size.try_into().map_err(|_| Error::new(ErrorKind::InvalidData, "block size too large"))?;

            let mut block = vec![0; block_size];
            // checked_add should be infallable (overflowing here means that the block offset is almost at 2^64, which is insane)
            self.data_file
                .read_at(&mut block, offset.checked_add(mem::size_of::<BlockHeader>() as u64).expect("block offset overflow"))?;

            // verify the checksum
            let checksum = fxhash::hash64(&block);
            if checksum != blockheader.checksum {
                return Err(Error::new(ErrorKind::InvalidData, "checksum mismatch"));
            }

            // return the block
            return Ok(Some(block.into()));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn smoke_test() {
        let tmpdir = tempfile::tempdir().unwrap();
        let store = Store::new(tmpdir.path(), tmpdir.path(), NonZeroUsize::new(1024).unwrap(), true).unwrap();
        let block = vec![32; 1025];
        store.write_block(1, &block).unwrap();
        let block_read = store.read_block(1).unwrap().unwrap();
        assert_eq!(block.into_boxed_slice(), block_read);
    }
}

