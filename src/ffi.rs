use std::ffi::{CStr, OsStr, c_char, c_int};
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;
use std::sync::RwLock;

use super::store;
use crate::{Block, BlockId};

#[repr(C)]
#[allow(clippy::arbitrary_source_item_ordering)]
pub struct CreateOrOpenArgs {
    path: *const c_char,
    cache_size: usize,
    truncate: bool,
}

#[derive(Debug)]
pub struct FfiStore {
    /// This lock is used to ensure that the FFI functions
    /// instantiate the store from the raw pointer without
    /// violating Rust's safety guarantees. When dereferencing
    /// the raw pointer to create a mutable reference, we
    /// need to ensure that no other references to the store
    /// exist, and for immutable references, we need to ensure
    /// that no mutable references exist.
    inner: RwLock<store::Store>,
}

/// Adds a block to the store.
///
/// Returns 0 on success, or an error code on failure.
///
/// Fails if the block ID is zero.
///
/// # Safety
/// The caller must ensure that:
/// - `block` is a valid pointer to a `Block` structure
/// - The `data` field of the `Block` points to valid memory for the specified `len`
///
/// # Panics
/// Panics if `store` or `block` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_block(store: *mut FfiStore, block: Block) -> c_int {
    let store = unsafe { store.as_mut().unwrap() };
    let mut guard = store.inner.write().unwrap();

    match guard.insert(block) {
        Err(_) => 1,
        Ok(()) => 0,
    }
}

/// Retrieves a block by its ID.
///
/// If the block cannot be found, it returns a block with a
/// zero length and null pointer.
///
/// # Safety
/// The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
///
/// # Panics
/// Panics if `store` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block(store: *const FfiStore, id: BlockId) -> Block {
    let store = unsafe { store.as_ref().unwrap() };
    let guard = store.inner.read().unwrap();
    // TODO: don't swallow the error
    guard.block(id).unwrap_or(None).unwrap_or(Block::default())
}

/// Creates a new store instance. Returns a pointer to the store, or null if the store cannot be created.
///
/// # Safety
/// The caller must ensure to call `free_store` on the returned pointer when done.
///
/// # Panics
/// Panics if `args` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn new_store(args: CreateOrOpenArgs) -> *mut FfiStore {
    let path = unsafe { CStr::from_ptr(args.path) };
    let path: &Path = OsStr::from_bytes(path.to_bytes()).as_ref();

    let cache_size = args.cache_size;
    let truncate = args.truncate;
    let Some(cache_size) = NonZeroUsize::new(cache_size) else {
        return ptr::null_mut();
    };

    match store::Store::new(path, cache_size, truncate) {
        Ok(store) => Box::into_raw(Box::new(FfiStore {
            inner: store.into(),
        })),
        Err(_) => ptr::null_mut(),
    }
}

/// Frees a store instance.
///
/// # Safety
/// The caller must ensure:
/// - `store` is a valid pointer returned by `new_store`
/// - `store` has not been freed before
/// - No other references to `store` exist
///
/// # Panics
/// Panics if the safety lock cannot be acquired.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_store(store: *mut FfiStore) {
    drop(unsafe { Box::from_raw(store) });
}
