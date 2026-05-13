mod borrowed;
mod owned;
mod results;

pub use self::borrowed::{BorrowedBytes, BorrowedSlice};
pub use self::owned::{OwnedBytes, OwnedSlice};
pub use self::results::{BlockResult, StoreHandleResult, VoidResult};
pub(crate) use self::results::{CResult, NullHandleResult};
