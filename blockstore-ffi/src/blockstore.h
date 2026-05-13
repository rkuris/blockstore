#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


typedef enum SyncMode {
  SyncMode_Async = 0,
  SyncMode_Sync = 1,
} SyncMode;

typedef struct Store Store;

/**
 * A Rust-owned vector of bytes that can be passed to C code.
 *
 * C callers must free this memory using the respective FFI function for the
 * concrete type (but not using the `free` function from the C standard library).
 */
typedef struct OwnedSlice_u8 {
  uint8_t *ptr;
  size_t len;
} OwnedSlice_u8;

/**
 * A type alias for a rust-owned byte slice.
 */
typedef struct OwnedSlice_u8 OwnedBytes;

/**
 * The result type returned from an FFI function that returns no value but may
 * return an error.
 */
enum VoidResult_Tag {
  /**
   * The caller provided a null pointer to the input handle.
   */
  VoidResult_NullHandlePointer,
  /**
   * The operation was successful and no error occurred.
   */
  VoidResult_Ok,
  /**
   * An error occurred and the message is returned as an [`OwnedBytes`]. Its
   * value is guaranteed to contain only valid UTF-8.
   *
   * The caller must call [`bs_free_owned_bytes`] to free the memory
   * associated with this error.
   *
   * [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
   */
  VoidResult_Err,
};
typedef size_t VoidResult_Tag;

typedef struct VoidResult {
  VoidResult_Tag tag;
  union {
    struct {
      OwnedBytes err;
    };
  };
} VoidResult;

typedef uint64_t BlockHeight;

/**
 * The result type returned from the open store function.
 */
enum StoreHandleResult_Tag {
  /**
   * The store was opened successfully and the handle is returned as an
   * opaque pointer.
   *
   * The caller must ensure that [`bs_close_store`] is called to free
   * resources associated with this handle when it is no longer needed.
   *
   * [`bs_close_store`]: crate::bs_close_store
   */
  StoreHandleResult_Ok,
  /**
   * An error occurred and the message is returned as an [`OwnedBytes`]. Its
   * value is guaranteed to contain only valid UTF-8.
   *
   * The caller must call [`bs_free_owned_bytes`] to free the memory
   * associated with this error.
   *
   * [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
   */
  StoreHandleResult_Err,
};
typedef size_t StoreHandleResult_Tag;

typedef struct StoreHandleResult {
  StoreHandleResult_Tag tag;
  union {
    struct {
      struct Store *ok;
    };
    struct {
      OwnedBytes err;
    };
  };
} StoreHandleResult;

/**
 * A borrowed byte slice. Used to represent data that was passed in from C
 * callers and will not be freed or retained by Rust code.
 */
typedef struct BorrowedSlice_u8 {
  /**
   * A pointer to the slice of bytes. This can be null if the slice is empty.
   *
   * If the pointer is not null, it must point to a valid slice of `len`
   * elements sized and aligned for `T`.
   */
  const uint8_t *ptr;
  /**
   * The length of the slice. It is ignored if the pointer is null; however,
   * if the pointer is not null, it must be equal to the number of elements
   * pointed to by `ptr`.
   */
  size_t len;
} BorrowedSlice_u8;

/**
 * A type alias for a borrowed byte slice.
 *
 * C callers can use this to pass in a byte slice that will not be freed by Rust
 * code.
 *
 * C callers must ensure that the pointer, if not null, points to a valid slice
 * of bytes of length `len`. C callers must also ensure that the slice is valid
 * for the duration of the C function call that was passed this slice.
 */
typedef struct BorrowedSlice_u8 BorrowedBytes;

/**
 * Arguments for opening or creating a [`Store`]. Passed to [`bs_open_store`].
 */
typedef struct StoreArgs {
  /**
   * The filesystem path used for both the index and the data files. Must
   * be valid UTF-8.
   */
  BorrowedBytes path;
  /**
   * Read cache size, in bytes. Must be greater than zero.
   */
  size_t cache_size;
  /**
   * If true, the store is truncated when opened.
   */
  bool truncate;
  /**
   * Sync mode for writes.
   */
  enum SyncMode sync;
} StoreArgs;

/**
 * The result type returned from FFI functions that retrieve a single block.
 */
enum BlockResult_Tag {
  /**
   * The caller provided a null pointer to the store handle.
   */
  BlockResult_NullHandlePointer,
  /**
   * The block was not found.
   */
  BlockResult_None,
  /**
   * A block was found and is returned.
   *
   * The caller must call [`bs_free_owned_bytes`] to free the memory
   * associated with this value.
   *
   * [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
   */
  BlockResult_Some,
  /**
   * An error occurred and the message is returned as an [`OwnedBytes`]. Its
   * value is guaranteed to contain only valid UTF-8.
   *
   * The caller must call [`bs_free_owned_bytes`] to free the memory
   * associated with this error.
   *
   * [`bs_free_owned_bytes`]: crate::bs_free_owned_bytes
   */
  BlockResult_Err,
};
typedef size_t BlockResult_Tag;

typedef struct BlockResult {
  BlockResult_Tag tag;
  union {
    struct {
      OwnedBytes some;
    };
    struct {
      OwnedBytes err;
    };
  };
} BlockResult;

/**
 * Closes a [`Store`] previously returned by [`bs_open_store`].
 *
 * # Returns
 *
 * - [`VoidResult::NullHandlePointer`] if `store` is null.
 * - [`VoidResult::Ok`] otherwise.
 */
struct VoidResult bs_close_store(struct Store *store);

/**
 * Frees memory associated with an [`OwnedBytes`] previously returned from an
 * FFI call.
 */
struct VoidResult bs_free_owned_bytes(OwnedBytes bytes);

/**
 * Returns the maximum contiguous block height of the store, or 0 if `store`
 * is null.
 */
BlockHeight bs_max_contiguous_height(const struct Store *store);

/**
 * Opens (or creates) a [`Store`].
 *
 * # Returns
 *
 * - [`StoreHandleResult::Ok`] with an opaque handle on success. The caller
 *   must pass the handle to [`bs_close_store`] when done.
 * - [`StoreHandleResult::Err`] with a UTF-8 error message otherwise. The
 *   caller must call [`bs_free_owned_bytes`] on the message.
 */
struct StoreHandleResult bs_open_store(struct StoreArgs args);

/**
 * Reads the block at `height` from the store.
 *
 * # Returns
 *
 * - [`BlockResult::NullHandlePointer`] if `store` is null.
 * - [`BlockResult::None`] if no block exists at `height`.
 * - [`BlockResult::Some`] with the block bytes. The caller must call
 *   [`bs_free_owned_bytes`] on the returned data.
 * - [`BlockResult::Err`] with a UTF-8 message otherwise. The caller must call
 *   [`bs_free_owned_bytes`] on the message.
 */
struct BlockResult bs_read_block(const struct Store *store, BlockHeight height);

/**
 * Reads the block header at `height` from the store.
 *
 * See [`bs_read_block`] for the return semantics.
 */
struct BlockResult bs_read_block_header(const struct Store *store, BlockHeight height);

/**
 * Writes a block at `height` to the store.
 *
 * # Returns
 *
 * - [`VoidResult::NullHandlePointer`] if `store` is null.
 * - [`VoidResult::Ok`] on success.
 * - [`VoidResult::Err`] with a UTF-8 message otherwise. The caller must call
 *   [`bs_free_owned_bytes`] on the message.
 */
struct VoidResult bs_write_block(const struct Store *store,
                                 BlockHeight height,
                                 BorrowedBytes data,
                                 uint16_t header_size);
