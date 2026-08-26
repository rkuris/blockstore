#![allow(clippy::cargo_common_metadata)]

mod value;

use std::ffi::OsStr;
use std::io::{Error as IoError, ErrorKind};
use std::num::{NonZeroU64, NonZeroUsize};
use std::os::unix::ffi::OsStrExt as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use blockstore::{BlockHeight, DEFAULT_MAX_DATA_FILES, Store, StoreOptions, SyncMode};

pub use crate::value::*;
use crate::value::{CResult, NullHandleResult};

/// Invokes a closure and returns the result as a [`CResult`].
///
/// If the closure panics, it returns [`CResult::from_panic`] with the panic
/// information.
#[inline]
fn invoke<T: CResult, V: Into<T>>(once: impl FnOnce() -> V) -> T {
    #[cfg(panic = "unwind")]
    match catch_unwind(AssertUnwindSafe(once)) {
        Ok(result) => result.into(),
        Err(panic) => T::from_panic(panic),
    }

    #[cfg(not(panic = "unwind"))]
    {
        once().into()
    }
}

/// Invokes a closure with a non-null handle, dispatching to the null-handle
/// error variant if the handle is `None`.
#[inline]
fn invoke_with_handle<H, T: NullHandleResult, V: Into<T>>(
    handle: Option<H>,
    once: impl FnOnce(H) -> V,
) -> T {
    match handle {
        Some(handle) => invoke(move || once(handle)),
        None => T::null_handle_pointer_error(),
    }
}

/// Arguments for opening or creating a [`Store`]. Passed to [`bs_open_store`].
#[repr(C)]
#[allow(clippy::arbitrary_source_item_ordering)]
pub struct StoreArgs<'a> {
    /// The filesystem path used for both the index and the data files. Must
    /// be valid UTF-8.
    pub path: BorrowedBytes<'a>,
    /// Read cache size, in bytes. Must be greater than zero.
    pub cache_size: usize,
    /// Maximum size of a single data file in bytes. `0` means unlimited
    /// (single-file mode); any other value caps each `blockdb_N.dat` file
    /// at that many bytes and rolls into the next file when a block would
    /// cross the boundary.
    pub max_data_file_size: u64,
    /// Lowest block height this store will accept. Unlike the other numeric
    /// fields, `0` is *not* a request for a default: it means "the first
    /// block is height 0". The value is passed through verbatim, and writes
    /// below it fail. Only applied when the store is created or truncated;
    /// otherwise the on-disk value wins.
    pub minimum_height: u64,
    /// Maximum number of open data-file handles to keep cached. `0` means
    /// use the default ([`DEFAULT_MAX_DATA_FILES`]).
    pub max_data_files: usize,
    /// If true, the store is truncated when opened.
    pub truncate: bool,
    /// Sync mode for writes.
    pub sync: SyncMode,
}

/// Opens (or creates) a [`Store`].
///
/// # Returns
///
/// - [`StoreHandleResult::Ok`] with an opaque handle on success. The caller
///   must pass the handle to [`bs_close_store`] when done.
/// - [`StoreHandleResult::Err`] with a UTF-8 error message otherwise. The
///   caller must call [`bs_free_owned_bytes`] on the message.
#[unsafe(no_mangle)]
pub extern "C" fn bs_open_store(args: StoreArgs<'_>) -> StoreHandleResult {
    invoke(move || -> Result<Store, IoError> {
        let path_str = args
            .path
            .as_str()
            .map_err(|e| IoError::new(ErrorKind::InvalidData, e))?;
        let path: &Path = OsStr::from_bytes(path_str.as_bytes()).as_ref();
        let cache_size = NonZeroUsize::new(args.cache_size)
            .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "cache_size must be > 0"))?;
        let max_data_files = if args.max_data_files == 0 {
            DEFAULT_MAX_DATA_FILES
        } else {
            args.max_data_files
        };
        Store::open(
            path,
            path,
            StoreOptions {
                cache_size,
                truncate: args.truncate,
                sync: args.sync,
                minimum_height: args.minimum_height,
                max_data_file_size: NonZeroU64::new(args.max_data_file_size),
                max_data_files,
            },
        )
    })
}

/// Closes a [`Store`] previously returned by [`bs_open_store`].
///
/// # Returns
///
/// - [`VoidResult::NullHandlePointer`] if `store` is null.
/// - [`VoidResult::Ok`] otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn bs_close_store(store: Option<Box<Store>>) -> VoidResult {
    invoke_with_handle(store, |store| {
        drop(store);
    })
}

/// Writes a block at `height` to the store.
///
/// # Returns
///
/// - [`VoidResult::NullHandlePointer`] if `store` is null.
/// - [`VoidResult::Ok`] on success.
/// - [`VoidResult::Err`] with a UTF-8 message otherwise. The caller must call
///   [`bs_free_owned_bytes`] on the message.
#[unsafe(no_mangle)]
pub extern "C" fn bs_write_block(
    store: Option<&Store>,
    height: BlockHeight,
    data: BorrowedBytes<'_>,
) -> VoidResult {
    invoke_with_handle(store, move |store| {
        store.write_block(height, data.as_slice())
    })
}

/// Reads the block at `height` from the store.
///
/// # Returns
///
/// - [`BlockResult::NullHandlePointer`] if `store` is null.
/// - [`BlockResult::None`] if no block exists at `height`.
/// - [`BlockResult::Some`] with the block bytes. The caller must call
///   [`bs_free_owned_bytes`] on the returned data.
/// - [`BlockResult::Err`] with a UTF-8 message otherwise. The caller must call
///   [`bs_free_owned_bytes`] on the message.
#[unsafe(no_mangle)]
pub extern "C" fn bs_read_block(store: Option<&Store>, height: BlockHeight) -> BlockResult {
    invoke_with_handle(store, move |store| store.read_block(height))
}

/// Returns the maximum contiguous block height of the store, or 0 if `store`
/// is null.
#[unsafe(no_mangle)]
pub extern "C" fn bs_max_contiguous_height(store: Option<&Store>) -> BlockHeight {
    match store {
        Some(store) => store.max_contiguous_height(),
        None => 0,
    }
}

/// Returns the highest block height ever written to the store regardless of
/// contiguity, or 0 if `store` is null. Diverges from
/// [`bs_max_contiguous_height`] when blocks are written with gaps below them.
#[unsafe(no_mangle)]
pub extern "C" fn bs_height_highwater(store: Option<&Store>) -> BlockHeight {
    match store {
        Some(store) => store.height_highwater(),
        None => 0,
    }
}

/// Returns the store's configured first height (the lowest height it will
/// accept a block at), or 0 if `store` is null.
#[unsafe(no_mangle)]
pub extern "C" fn bs_min_block_height(store: Option<&Store>) -> BlockHeight {
    match store {
        Some(store) => store.min_block_height(),
        None => 0,
    }
}

/// Frees memory associated with an [`OwnedBytes`] previously returned from an
/// FFI call.
#[unsafe(no_mangle)]
pub extern "C" fn bs_free_owned_bytes(bytes: OwnedBytes) -> VoidResult {
    invoke(move || drop(bytes))
}
