use std::any::Any;
#[cfg(panic = "unwind")]
use std::error::Error as StdError;
use std::fmt;

use blockstore::Store;

use crate::value::OwnedBytes;

/// The result type returned from an FFI function that returns no value but may
/// return an error.
#[derive(Debug)]
#[repr(C, usize)]
pub enum VoidResult {
    /// The caller provided a null pointer to the input handle.
    NullHandlePointer,
    /// The operation was successful and no error occurred.
    Ok,
    /// An error occurred and the message is returned as an [`OwnedBytes`]. Its
    /// value is guaranteed to contain only valid UTF-8.
    ///
    /// The caller must call [`bs_free_owned_bytes`] to free the memory
    /// associated with this error.
    ///
    /// [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
    Err(OwnedBytes),
}

impl From<()> for VoidResult {
    fn from((): ()) -> Self {
        VoidResult::Ok
    }
}

impl<E: fmt::Display> From<Result<(), E>> for VoidResult {
    fn from(value: Result<(), E>) -> Self {
        match value {
            Ok(()) => VoidResult::Ok,
            Err(err) => VoidResult::Err(err.to_string().into_bytes().into()),
        }
    }
}

/// The result type returned from the open store function.
#[derive(Debug)]
#[repr(C, usize)]
pub enum StoreHandleResult {
    /// The store was opened successfully and the handle is returned as an
    /// opaque pointer.
    ///
    /// The caller must ensure that [`bs_close_store`] is called to free
    /// resources associated with this handle when it is no longer needed.
    ///
    /// [`bs_close_store`]: crate::bs_close_store
    Ok(Box<Store>),
    /// An error occurred and the message is returned as an [`OwnedBytes`]. Its
    /// value is guaranteed to contain only valid UTF-8.
    ///
    /// The caller must call [`bs_free_owned_bytes`] to free the memory
    /// associated with this error.
    ///
    /// [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
    Err(OwnedBytes),
}

impl<E: fmt::Display> From<Result<Store, E>> for StoreHandleResult {
    fn from(value: Result<Store, E>) -> Self {
        match value {
            Ok(store) => StoreHandleResult::Ok(Box::new(store)),
            Err(err) => StoreHandleResult::Err(err.to_string().into_bytes().into()),
        }
    }
}

/// The result type returned from FFI functions that retrieve a single block.
#[derive(Debug)]
#[repr(C, usize)]
pub enum BlockResult {
    /// The caller provided a null pointer to the store handle.
    NullHandlePointer,
    /// The block was not found.
    None,
    /// A block was found and is returned.
    ///
    /// The caller must call [`bs_free_owned_bytes`] to free the memory
    /// associated with this value.
    ///
    /// [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
    Some(OwnedBytes),
    /// An error occurred and the message is returned as an [`OwnedBytes`]. Its
    /// value is guaranteed to contain only valid UTF-8.
    ///
    /// The caller must call [`bs_free_owned_bytes`] to free the memory
    /// associated with this error.
    ///
    /// [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
    Err(OwnedBytes),
}

impl<E: fmt::Display> From<Result<Option<Box<[u8]>>, E>> for BlockResult {
    fn from(value: Result<Option<Box<[u8]>>, E>) -> Self {
        match value {
            Ok(None) => BlockResult::None,
            Ok(Some(data)) => BlockResult::Some(data.into()),
            Err(err) => BlockResult::Err(err.to_string().into_bytes().into()),
        }
    }
}

/// Helper trait used by `invoke_with_handle` to construct the
/// null-handle-pointer error variant of a result enum.
pub(crate) trait NullHandleResult: CResult {
    fn null_handle_pointer_error() -> Self;
}

/// Helper trait to convert errors and panics into the result-enum's error
/// variant.
pub(crate) trait CResult: Sized {
    #[cfg(panic = "unwind")]
    fn from_err(err: impl ToString) -> Self;

    #[cfg(panic = "unwind")]
    fn from_panic(panic: Box<dyn Any + Send>) -> Self
    where
        Self: Sized,
    {
        Self::from_err(Panic::from(panic))
    }
}

macro_rules! impl_null_handle_result {
    ($($Enum:ty),* $(,)?) => {
        $(
            impl NullHandleResult for $Enum {
                fn null_handle_pointer_error() -> Self {
                    Self::NullHandlePointer
                }
            }
        )*
    };
}

macro_rules! impl_cresult {
    ($($Enum:ty),* $(,)?) => {
        $(
            impl CResult for $Enum {
                #[cfg(panic = "unwind")]
                fn from_err(err: impl ToString) -> Self {
                    Self::Err(err.to_string().into_bytes().into())
                }
            }
        )*
    };
}

impl_null_handle_result!(VoidResult, BlockResult);
impl_cresult!(VoidResult, BlockResult, StoreHandleResult);

#[cfg(panic = "unwind")]
enum Panic {
    Static(&'static str),
    Formatted(String),
    SendSyncErr(Box<dyn StdError + Send + Sync>),
    SendErr(Box<dyn StdError + Send>),
    Unknown(#[expect(unused)] Box<dyn Any + Send>),
}

#[cfg(panic = "unwind")]
impl From<Box<dyn Any + Send>> for Panic {
    fn from(panic: Box<dyn Any + Send>) -> Self {
        macro_rules! downcast {
            ($Variant:ident($panic:ident)) => {
                let $panic = match $panic.downcast() {
                    Ok(panic) => return Panic::$Variant(*panic),
                    Err(panic) => panic,
                };
            };
        }

        downcast!(Static(panic));
        downcast!(Formatted(panic));
        downcast!(SendSyncErr(panic));
        downcast!(SendErr(panic));

        Self::Unknown(panic)
    }
}

#[cfg(panic = "unwind")]
impl fmt::Display for Panic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Panic::Static(msg) => f.pad(msg),
            Panic::Formatted(msg) => f.pad(msg),
            Panic::SendSyncErr(err) => err.fmt(f),
            Panic::SendErr(err) => err.fmt(f),
            Panic::Unknown(_) => f.pad("unknown panic type recovered"),
        }
    }
}
