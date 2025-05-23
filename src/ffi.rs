use std::ffi::{CStr, CString, OsStr, c_char};
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::{ptr, slice};

use crate::{BlockHeight, Store, SyncMode};

#[repr(C)]
#[allow(clippy::arbitrary_source_item_ordering)]
pub struct CreateOrOpenArgs {
    path: *const c_char,
    cache_size: usize,
    truncate: bool,
    sync: SyncMode,
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
pub unsafe extern "C" fn write_block(
    store: *const Store,
    height: BlockHeight,
    block_len: usize,
    block_data: *const u8,
) -> *const c_char {
    let store = unsafe { store.as_ref().unwrap() };
    let block = unsafe { slice::from_raw_parts(block_data, block_len) };
    match store.write_block(height, block) {
        Ok(()) => ptr::null(),
        // TODO: error strings leak memory :()
        Err(e) => CString::new(e.to_string()).unwrap().into_raw(),
    }
}

#[repr(C)]
pub struct FfiBlock {
    data: *mut u8,
    len: usize,
}

/// Retrieves a block by its ID.
///
/// If the block cannot be found, it returns a block with a
/// zero length and null pointer. If an error occurs, it returns
/// a C string containing the error message and a zero size.
///
/// # Safety
/// The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
///
/// # Panics
/// Panics if `store` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read_block(store: *const Store, id: BlockHeight) -> FfiBlock {
    let store = unsafe { store.as_ref().unwrap() };
    match store.read_block(id) {
        Ok(Some(block)) => {
            let leaked = Box::leak(block);
            FfiBlock {
                data: leaked.as_mut_ptr(),
                len: leaked.len(),
            }
        }
        Ok(None) => FfiBlock {
            data: ptr::null_mut(),
            len: 0,
        },
        Err(e) => FfiBlock {
            data: CString::new(e.to_string()).unwrap().into_raw().cast::<u8>(),
            len: 0,
        },
    }
}

/// Frees a previous return from `read_block`.
///
/// # Safety
/// The caller must ensure that `data` is a valid pointer to a block.
///
/// # Panics
/// Panics if `data` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_block(block: FfiBlock) {
    let slice = unsafe { slice::from_raw_parts_mut(block.data, block.len) };
    let boxed = unsafe { Box::from_raw(slice) };
    drop(boxed);
}

/// Creates a new store instance. Returns a pointer to the store, or null if the store cannot be created.
///
/// # Safety
/// The caller must ensure to call `free_store` on the returned pointer when done.
///
/// # Panics
/// Panics if `args` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn new_store(args: CreateOrOpenArgs) -> *const Store {
    let path = unsafe { CStr::from_ptr(args.path) };
    let path: &Path = OsStr::from_bytes(path.to_bytes()).as_ref();

    let cache_size = args.cache_size;
    let truncate = args.truncate;
    let Some(cache_size) = NonZeroUsize::new(cache_size) else {
        return ptr::null_mut();
    };

    match Store::new(path, path, cache_size, truncate, args.sync, 1) {
        Ok(store) => Box::into_raw(Box::new(store)),
        Err(_) => ptr::null_mut(),
    }
}

/// Returns the maximum contiguous height of the store.
///
/// # Safety
/// The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
///
/// # Panics
/// Panics if `store` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn max_contiguous_height(store: *const Store) -> BlockHeight {
    let store = unsafe { store.as_ref().unwrap() };
    store.max_contiguous_height()
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
pub unsafe extern "C" fn free_store(store: *mut Store) {
    drop(unsafe { Box::from_raw(store) });
}
