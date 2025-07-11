pub mod store;

pub type BlockHeight = u64;
pub type Block = Box<[u8]>;

pub use store::{Store, SyncMode};
