// `IndexFileHeader` and `IndexEntry` are serialised via bytemuck's
// zero-copy `bytes_of` / `bytes_of_mut`, which writes the in-memory
// representation. That representation is host-endian: on a little-
// endian target the on-disk bytes match blockdb's (which always uses
// `binary.LittleEndian`); on a big-endian target the bytes would be
// reversed and stores would be unreadable by blockdb and vice versa.
// We accept this constraint — every Rust target we ship is LE — and
// fail the build loudly on anything else rather than silently produce
// incompatible files. See DIFFERENCES.md "Constraints" for context.
#[cfg(not(target_endian = "little"))]
compile_error!(
    "blockstore requires a little-endian target: IndexFileHeader and \
     IndexEntry are serialised host-endian via bytemuck for byte-level \
     compatibility with blockdb on LE machines."
);

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
