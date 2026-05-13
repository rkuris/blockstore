use std::ptr::{self, NonNull};
use std::slice;

/// A type alias for a rust-owned byte slice.
pub type OwnedBytes = OwnedSlice<u8>;

/// A Rust-owned vector of bytes that can be passed to C code.
///
/// C callers must free this memory using the respective FFI function for the
/// concrete type (but not using the `free` function from the C standard library).
#[derive(Debug)]
#[repr(C)]
pub struct OwnedSlice<T> {
    ptr: Option<NonNull<T>>,
    len: usize,
}

impl<T> OwnedSlice<T> {
    #[must_use]
    pub const fn as_slice(&self) -> &[T] {
        match self.ptr {
            // SAFETY: pointer is valid for `len` aligned `T`s, as originally
            // produced by `Box::leak` in `From<Box<[T]>>`.
            Some(ptr) => unsafe { slice::from_raw_parts(ptr.as_ptr(), self.len) },
            None => &[],
        }
    }

    #[must_use]
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        match self.ptr {
            // SAFETY: see `as_slice`.
            Some(ptr) => unsafe { slice::from_raw_parts_mut(ptr.as_ptr(), self.len) },
            None => &mut [],
        }
    }

    #[must_use]
    pub fn into_boxed_slice(self) -> Box<[T]> {
        self.into()
    }

    fn take_box(&mut self) -> Box<[T]> {
        match self.ptr.take() {
            // SAFETY: the owned slice was created from `Box::leak`, so it can
            // be reconstituted with `Box::from_raw`.
            Some(ptr) => unsafe {
                Box::from_raw(ptr::slice_from_raw_parts_mut(ptr.as_ptr(), self.len))
            },
            None => Box::new([]),
        }
    }
}

impl<T> AsRef<[T]> for OwnedSlice<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> AsMut<[T]> for OwnedSlice<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T> From<Box<[T]>> for OwnedSlice<T> {
    fn from(data: Box<[T]>) -> Self {
        let len = data.len();
        let ptr = NonNull::from(Box::leak(data)).cast::<T>();
        Self {
            ptr: Some(ptr),
            len,
        }
    }
}

impl<T> From<Vec<T>> for OwnedSlice<T> {
    fn from(data: Vec<T>) -> Self {
        data.into_boxed_slice().into()
    }
}

impl<T> From<OwnedSlice<T>> for Box<[T]> {
    fn from(mut owned: OwnedSlice<T>) -> Self {
        owned.take_box()
    }
}

impl From<String> for OwnedBytes {
    fn from(s: String) -> Self {
        s.into_bytes().into()
    }
}

impl<T> Drop for OwnedSlice<T> {
    fn drop(&mut self) {
        drop(self.take_box());
    }
}

// SAFETY: if the value is Send/Sync, the pointer is Send/Sync. Owned data
// follows the same rules as `Box<[T]>`.
unsafe impl<T: Send> Send for OwnedSlice<T> {}
// SAFETY: see above.
unsafe impl<T: Sync> Sync for OwnedSlice<T> {}
