use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr;
use std::slice;
use std::str::{self, Utf8Error};

/// A type alias for a borrowed byte slice.
///
/// C callers can use this to pass in a byte slice that will not be freed by Rust
/// code.
///
/// C callers must ensure that the pointer, if not null, points to a valid slice
/// of bytes of length `len`. C callers must also ensure that the slice is valid
/// for the duration of the C function call that was passed this slice.
pub type BorrowedBytes<'a> = BorrowedSlice<'a, u8>;

/// A borrowed byte slice. Used to represent data that was passed in from C
/// callers and will not be freed or retained by Rust code.
#[derive(Debug)]
#[repr(C)]
pub struct BorrowedSlice<'a, T> {
    /// A pointer to the slice of bytes. This can be null if the slice is empty.
    ///
    /// If the pointer is not null, it must point to a valid slice of `len`
    /// elements sized and aligned for `T`.
    ptr: *const T,
    /// The length of the slice. It is ignored if the pointer is null; however,
    /// if the pointer is not null, it must be equal to the number of elements
    /// pointed to by `ptr`.
    len: usize,
    /// Tracks the lifetime of the slice passed in to C functions.
    marker: PhantomData<&'a [T]>,
}

impl<T> Clone for BorrowedSlice<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for BorrowedSlice<'_, T> {}

impl<'a, T> BorrowedSlice<'a, T> {
    #[must_use]
    pub const fn as_slice(&self) -> &'a [T] {
        if self.ptr.is_null() {
            &[]
        } else {
            // SAFETY: the caller has upheld the invariant that the pointer is
            // valid for `len` aligned elements of `T`. The phantom marker ties
            // the lifetime of the returned slice to this `BorrowedSlice`.
            unsafe { slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    #[must_use]
    pub const fn from_slice(slice: &'a [T]) -> Self {
        let len = slice.len();
        Self {
            ptr: ptr::from_ref(slice).cast(),
            len,
            marker: PhantomData,
        }
    }

    /// Returns true if the pointer is null.
    /// This is used to differentiate between a nil slice and an empty slice.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.ptr.is_null()
    }
}

impl<T> Deref for BorrowedSlice<'_, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> AsRef<[T]> for BorrowedSlice<'_, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'a> BorrowedBytes<'a> {
    /// Creates a new [`str`] from this borrowed byte slice.
    ///
    /// # Errors
    ///
    /// If the slice is not valid UTF-8, an error is returned.
    pub const fn as_str(&self) -> Result<&'a str, Utf8Error> {
        str::from_utf8(self.as_slice())
    }
}

// SAFETY: the pointer is send/sync iff the value is sync. The value does not
// need to be `Send` for the pointer to be `Send` because the pointer is not
// moving the value across threads, only the reference.
unsafe impl<T: Sync> Send for BorrowedSlice<'_, T> {}
// SAFETY: see above.
unsafe impl<T: Sync> Sync for BorrowedSlice<'_, T> {}
