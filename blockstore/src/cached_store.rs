//! A read-through block cache wrapping [`Store`].
//!
//! Mirrors the role of blockdb's `cacheDB`: a bounded LRU over the
//! underlying store's blocks. Unlike blockdb, the cache is sized by
//! **bytes of heap occupancy** rather than entry count, using `lru-mem`'s
//! `HeapSize`-aware accounting. Block sizes on Avalanche vary wildly
//! (small P-chain blocks vs. multi-MiB C-chain blocks); a fixed entry
//! count gives no memory-pressure guarantee, while a byte budget does.

use std::io::Error;
use std::path::Path;
use std::sync::Arc;

use lru_mem::{HeapSize, LruCache};
use parking_lot::Mutex;

use crate::{Block, BlockHeight, Store, StoreOptions};

/// Newtype wrapper around [`Block`] (an `Arc<[u8]>`) so lru-mem can
/// account for the slot's bytes. Each cached entry is accounted as if
/// it owns the payload; that over-counts when callers hold outstanding
/// `Arc` clones, but the cache stays strictly under its byte budget.
#[derive(Debug, Clone)]
struct CacheEntry(Block);

impl HeapSize for CacheEntry {
    fn heap_size(&self) -> usize {
        self.0.len()
    }
}

/// A [`Store`] with an in-memory LRU read cache in front of `read_block`.
///
/// Operations match `Store`'s API. Writes go through to the underlying
/// store and populate the cache; reads check the cache first.
#[derive(Debug)]
pub struct CachedStore {
    inner: Store,
    cache: Mutex<LruCache<BlockHeight, CacheEntry>>,
}

impl CachedStore {
    /// Opens (or creates) a cached store. `options.cache_size` is
    /// interpreted as the cache's byte budget.
    ///
    /// # Errors
    /// Forwards any error from [`Store::open`].
    pub fn open(index_path: &Path, data_path: &Path, options: StoreOptions) -> Result<Self, Error> {
        let cache_bytes = options.cache_size.get();
        let inner = Store::open(index_path, data_path, options)?;
        Ok(Self {
            inner,
            cache: Mutex::new(LruCache::new(cache_bytes)),
        })
    }

    /// Reads a block, consulting the cache first. Cache misses fall
    /// through to the underlying store and populate the cache on success.
    ///
    /// # Errors
    /// Forwards errors from [`Store::read_block`].
    pub fn read_block(&self, height: BlockHeight) -> Result<Option<Block>, Error> {
        if let Some(cached) = self.cache.lock().get(&height) {
            return Ok(Some(Arc::clone(&cached.0)));
        }
        let block = self.inner.read_block(height)?;
        if let Some(ref b) = block {
            // `insert` can fail if a single entry exceeds the cache's
            // byte budget; in that case we just skip caching and the
            // next read will miss again. Not an error from the caller's
            // perspective.
            let _ = self.cache.lock().insert(height, CacheEntry(Arc::clone(b)));
        }
        Ok(block)
    }

    /// Writes a block and caches it for future reads.
    ///
    /// # Errors
    /// Forwards errors from [`Store::write_block`]. If the write fails,
    /// the cache is not touched.
    pub fn write_block(&self, height: BlockHeight, data: &[u8]) -> Result<(), Error> {
        self.inner.write_block(height, data)?;
        let block: Block = data.into();
        let _ = self.cache.lock().insert(height, CacheEntry(block));
        Ok(())
    }

    pub fn max_contiguous_height(&self) -> BlockHeight {
        self.inner.max_contiguous_height()
    }

    pub fn min_block_height(&self) -> BlockHeight {
        self.inner.min_block_height()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use crate::SyncMode;

    use super::*;

    fn open_cached(path: &Path, cache_bytes: usize) -> CachedStore {
        CachedStore::open(
            path,
            path,
            StoreOptions {
                truncate: true,
                sync: SyncMode::Sync,
                cache_size: NonZeroUsize::new(cache_bytes).expect("nonzero"),
                ..StoreOptions::default()
            },
        )
        .expect("open")
    }

    /// Reading a block populates the cache, so a subsequent read returns
    /// the cached `Arc<[u8]>` (same pointer identity).
    #[test]
    fn read_populates_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_cached(dir.path(), 1 << 20);
        let block = vec![0xABu8; 256];
        store.write_block(1, &block).unwrap();

        let first = store.read_block(1).unwrap().unwrap();
        let second = store.read_block(1).unwrap().unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "second read must hit the cache",
        );
        assert_eq!(&block[..], &*first);
    }

    /// Writing a block at an existing height invalidates the prior cache
    /// entry — subsequent reads return the new payload.
    #[test]
    fn write_overrides_cache_at_same_height() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_cached(dir.path(), 1 << 20);
        let first = vec![0x11u8; 128];
        let second = vec![0x22u8; 128];

        store.write_block(1, &first).unwrap();
        let read_first = store.read_block(1).unwrap().unwrap();
        assert_eq!(&first[..], &*read_first);

        store.write_block(1, &second).unwrap();
        let read_second = store.read_block(1).unwrap().unwrap();
        assert_eq!(&second[..], &*read_second);
        assert!(
            !Arc::ptr_eq(&read_first, &read_second),
            "cache must hold the new value, not the old",
        );
    }

    /// A block too large for the cache budget is still written and
    /// readable through the underlying store; the cache silently skips it.
    #[test]
    fn oversize_block_bypasses_cache() {
        let dir = tempfile::tempdir().unwrap();
        // Cache budget < block size + Arc<[u8]> overhead.
        let store = open_cached(dir.path(), 64);
        let block = vec![0xCDu8; 2048];
        store.write_block(1, &block).unwrap();
        let read = store.read_block(1).unwrap().unwrap();
        assert_eq!(&block[..], &*read);
    }

    /// A tight cache budget forces eviction of older entries as new ones
    /// are inserted, but reads of evicted blocks still succeed (via disk).
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::indexing_slicing,
        reason = "test code with known-small values"
    )]
    fn small_cache_evicts_but_reads_still_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_cached(dir.path(), 4096);
        // ~50 blocks of 256 bytes each ≫ 4 KiB cache budget.
        for h in 1..=50u64 {
            let block = vec![h as u8; 256];
            store.write_block(h, &block).unwrap();
        }
        // The oldest block must still be readable, just not from cache.
        let block = store.read_block(1).unwrap().unwrap();
        assert_eq!(256, block.len());
        assert_eq!(1u8, block[0]);
    }
}
