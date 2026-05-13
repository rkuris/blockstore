mod file_set;
pub mod store;

pub type BlockHeight = u64;
pub type Block = Box<[u8]>;

pub use store::{DEFAULT_MAX_DATA_FILES, Store, StoreOptions, SyncMode};
