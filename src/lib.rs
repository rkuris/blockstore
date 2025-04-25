use std::collections::HashMap;
use std::ffi::c_int;
use std::fmt::{self, Display};
use std::sync::RwLock;

pub struct Store {
    data: HashMap<u64, Block>,
}

static SAFETY_LOCK: RwLock<()> = RwLock::new(());

#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct Block {
    pub id: u64,
    pub len: usize,
    pub data: *const u8,
}

impl Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Block {{ id: {}, len: {}, data: {:?} }}",
            self.id, self.len, self.data
        )
    }
}

impl Default for Block {
    fn default() -> Self {
        Block {
            id: 0,
            len: 0,
            data: std::ptr::null(),
        }
    }
}

/// Adds a block to the store.
///
/// Returns 0 on success, or an error code on failure.
///
/// # Safety
/// The caller must ensure that:
/// - `block` is a valid pointer to a `Block` structure
/// - The `data` field of the `Block` points to valid memory for the specified `len`
///
/// # Panics
/// Panics if `store` or `block` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_block(store: *mut Store, block: *mut Block) -> c_int {
    let _guard = SAFETY_LOCK.write().unwrap();
    let store = unsafe { store.as_mut().unwrap() };
    let block = unsafe { block.as_ref().unwrap() };
    store.data.insert(block.id, *block);
    0
}

/// Retrieves a block by its ID.
///
/// If the block cannot be found, it returns a block with a
/// zero length and null pointer.
///
/// # Safety
/// The caller must ensure that the returned `Block`'s `data` field is properly managed
/// and that the memory it points to remains valid for the duration of its use.
///
/// # Panics
/// Panics if `store` is a null pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block(store: *const Store, id: u64) -> Block {
    let _guard = SAFETY_LOCK.read().unwrap();
    let store = unsafe { store.as_ref().unwrap() };
    *store.data.get(&id).unwrap_or(&Block::default())
}

/// Creates a new store instance.
///
/// # Safety
/// The caller must ensure to call `free_store` on the returned pointer when done.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn new_store() -> *mut Store {
    Box::into_raw(Box::new(Store {
        data: HashMap::new(),
    }))
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
    let _guard = SAFETY_LOCK.write().unwrap();
    drop(unsafe { Box::from_raw(store) });
}
