#[macro_use]
mod metrics;
mod cached_store;
mod file_set;
pub mod store;

use std::sync::Arc;

pub type BlockHeight = u64;
/// A reference-counted block payload. `Arc<[u8]>` lets the cache hand out
/// O(1) clones on hits without copying the bytes.
pub type Block = Arc<[u8]>;

pub use cached_store::CachedStore;
pub use store::{DEFAULT_MAX_DATA_FILES, Store, StoreOptions, SyncMode};
