//! # [Store]
//!
//! A store is a collection of blocks. Blocks can be added to the store in any order,
//! but the store will be more efficient if blocks are added in order.
//!
//! The structure of the file is as follows:
//!
//! ## [`FileHeader`]
//!
//! The header is a fixed size header at the beginning of the file
//! that contains the following information:
//!
//! - A set of region maps including
//!   - the lowest block id in the region
//!   - the highest block id in the region
//!   - the start offset of the region,
//!   - the number of items in the region.
//!   - TODO: we could add a id->offset map here to avoid scanning the region for the offset of a given id
//!
//! The number of region maps is computed to fit within a single disk block
//!
//! ## Regions
//!
//! Regions are a set of blocks that are stored contiguously in the file.
//!
//! ## Block
//!
//! Blocks are blocks on the blockchain.
//!
//!

use std::{
    array,
    cmp::{Ordering, max, min},
    fs::{File, OpenOptions},
    io::{Error, Write as _},
    mem,
    num::NonZeroUsize,
    ops::RangeInclusive,
    os::unix::fs::FileExt,
    path::Path,
    sync::Mutex,
};

use bincode::{Decode, Encode};
use lru::LruCache;

use crate::{Block, BlockHeader, BlockId};

#[derive(Debug)]
pub struct Store {
    cache: Mutex<LruCache<u64, Block>>,
    checkpoint: usize,
    current_region: Region,
    file: File,
    file_header: FileHeader,
    highwater: u64,
}

/// The size of a block in the store.
pub const BLOCK_SIZE: usize = 4096;

pub const NREGION_MAPS: usize = (BLOCK_SIZE - mem::size_of::<Magic>()) / mem::size_of::<Region>();

#[derive(Debug, Encode, Decode)]
pub struct Magic([u8; 16]);

impl Default for Magic {
    fn default() -> Self {
        Self(*b"BlockStore 0.1\0\0")
    }
}
#[derive(Debug, Encode, Decode)]
#[repr(C)]
pub struct FileHeader {
    magic: Magic,
    region_maps: [Region; NREGION_MAPS],
}

impl FileHeader {
    /// Encodes the file header into bytes.
    ///
    /// # Panics
    /// Returns an error if:
    /// - The header data exceeds bincode's size limits (should never happen)
    /// - The header contains invalid data that cannot be serialized
    /// - The region maps contain invalid data
    #[must_use]
    pub fn as_bytes(&self) -> Vec<u8> {
        bincode::encode_to_vec(self, crate::BINCODE_CONFIG).unwrap()
    }

    /// Decodes a file header from bytes.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The input bytes are not a valid file header
    /// - The input bytes are too short
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Ok(
            bincode::borrow_decode_from_slice::<Self, _>(bytes, crate::BINCODE_CONFIG)
                .map_err(Error::other)?
                .0,
        )
    }

    /// Returns the regions that contain the given block id.
    #[must_use]
    pub fn regions_for_id(&self, id: BlockId) -> Vec<&Region> {
        self.region_maps
            .iter()
            .filter(|region| region.id_range.as_ref().is_some_and(|r| r.contains(&id)))
            .collect()
    }
}

impl Default for FileHeader {
    fn default() -> Self {
        Self {
            magic: Magic::default(),
            region_maps: array::from_fn(|_| Region::default()),
        }
    }
}

#[derive(Clone, Default, Debug, Encode, Decode)]
pub struct Region {
    id_range: Option<RangeInclusive<BlockId>>,
    item_count: usize,
    start_offset: u64,
}

impl Store {
    /// Creates a new store.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The file cannot be opened
    /// - The file cannot be truncated
    pub fn new(path: &Path, cache_size: NonZeroUsize, truncate: bool) -> Result<Self, Error> {
        let mut opts = OpenOptions::new();
        let opts = opts
            .create(truncate)
            .truncate(truncate)
            .write(true)
            .read(true);
        let mut file = opts.open(path)?;
        let cache = LruCache::new(cache_size).into();

        if truncate {
            let header = FileHeader::default();
            file.write_all(header.as_bytes().as_ref())?;
        }

        Ok(Self {
            file,
            cache,
            checkpoint: 10, // TODO: make this configurable
            current_region: Region::default(),
            file_header: FileHeader::default(),
            highwater: BLOCK_SIZE as u64,
        })
    }

    /// Inserts a block into the store.
    ///
    /// # Errors
    /// Returns an error if the block cannot be written
    ///
    /// # Panics
    /// Panics if the cache lock has been poisoned.
    pub fn insert(&mut self, block: Block) -> Result<(), Error> {
        self.cache.lock().unwrap().put(block.header.id.get(), block);
        let to_write = block.as_bytes();
        self.file.write_all_at(&to_write, self.highwater)?;
        let id = block.header.id;
        if let (Some(region), item_count) = (
            &mut self.current_region.id_range,
            &mut self.current_region.item_count,
        ) {
            *region = min(*region.start(), id)..=max(*region.end(), id);
            *item_count = item_count.checked_add(1).unwrap();
        } else {
            let current_region = &mut self.current_region;
            debug_assert!(current_region.item_count == 0);
            current_region.id_range = Some(id..=id);
            current_region.item_count = 1;
            current_region.start_offset = self.highwater;
        }
        // This shouldn't overflow, but we check and panic if it does
        self.highwater = self.highwater.checked_add(to_write.len() as u64).unwrap();

        if self.current_region.item_count == self.checkpoint {
            // update the file header with the new region and reset it
            // TODO: this should not be maps[0], compute the correct index
            // and coalesce regions here
            self.file_header.region_maps[0] = self.current_region.clone();
            self.current_region.id_range = None;
            self.current_region.item_count = 0;
        }

        Ok(())
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
    pub fn block(&self, id: BlockId) -> Result<Option<Block>, Error> {
        let mut guard = self.cache.lock().unwrap();
        let cached = guard.get(&id.get());
        if let Some(block) = cached {
            return Ok(Some(*block));
        }

        for region in self.file_header.regions_for_id(id) {
            // read the block header at this offset
            let mut offset = region.start_offset;
            let mut buf = vec![0; mem::size_of::<BlockHeader>()];
            for _ in 0..region.item_count {
                self.file.read_at(buf.as_mut(), offset)?;
                offset = offset.checked_add(mem::size_of::<BlockHeader>() as u64).unwrap();
                let header = BlockHeader::from_bytes(&buf)?;

                match header.id.cmp(&id) {
                    Ordering::Equal => {
                        let block = self.read_block_from_header(header, offset)?;
                        self.cache.lock().unwrap().put(id.get(), block);
                        return self.block(id);
                    }
                    Ordering::Greater => {
                        let block = self.read_block_from_header(header, offset)?;
                        self.cache.lock().unwrap().put(id.get(), block);
                    }
                    Ordering::Less => {}
                }
            }
        }
        Ok(None)
    }
    fn read_block_from_header(&self, header: BlockHeader, offset: u64) -> Result<Block, Error> {
        let mut buf = vec![0; header.len];
        self.file.read_at(buf.as_mut(), offset)?;
        Ok(Block {
            header,
            data: buf.as_ptr(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use more_asserts::*;

    #[test]
    fn test_region_map_size() {
        assert_le!(mem::size_of::<FileHeader>(), BLOCK_SIZE);
        assert_ge!(
            mem::size_of::<FileHeader>(),
            BLOCK_SIZE - mem::size_of::<Magic>()
        );
    }
}
