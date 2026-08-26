#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


typedef enum SyncMode {
  SyncMode_Async = 0,
  SyncMode_Sync = 1,
} SyncMode;

/**
 * The opaque store handle handed out to C.
 *
 * `bs_open_store` picks the variant from [`StoreArgs::cache_size`]: zero
 * opens the store directly, any other value wraps it in the byte-budgeted
 * LRU read cache. The two have identical method surfaces, so the accessors
 * below are pure forwards. An enum rather than `Box<dyn ...>` keeps the
 * dispatch a branch on a tag instead of a vtable hop on every block read,
 * and keeps the handle one allocation.
 */
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
enum VoidResult_Tag
#if __STDC_VERSION__ >= 202311L
  : size_t
#endif // __STDC_VERSION__ >= 202311L
 {
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
#if __STDC_VERSION__ >= 202311L
typedef enum VoidResult_Tag VoidResult_Tag;
#else
typedef size_t VoidResult_Tag;
#endif // __STDC_VERSION__ >= 202311L

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
enum StoreHandleResult_Tag
#if __STDC_VERSION__ >= 202311L
  : size_t
#endif // __STDC_VERSION__ >= 202311L
 {
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
#if __STDC_VERSION__ >= 202311L
typedef enum StoreHandleResult_Tag StoreHandleResult_Tag;
#else
typedef size_t StoreHandleResult_Tag;
#endif // __STDC_VERSION__ >= 202311L

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
   * Byte budget for the LRU read cache. `0` opens the store without a
   * cache, so every read goes to the index and data files; any other
   * value caps the cache's tracked heap occupancy at that many bytes.
   *
   * A non-zero budget costs concurrency. The LRU sits behind a single
   * mutex that every read and every write takes exclusively -- a cache
   * hit still has to update recency order -- so cached reads and writes
   * serialise against each other, while the uncached read path takes no
   * lock at all. Prefer `0` when many threads read and write distinct
   * heights; prefer a budget when the workload re-reads a working set.
   */
  size_t cache_size;
  /**
   * Maximum size of a single data file in bytes. `0` means unlimited
   * (single-file mode); any other value caps each `blockdb_N.dat` file
   * at that many bytes and rolls into the next file when a block would
   * cross the boundary.
   */
  uint64_t max_data_file_size;
  /**
   * Lowest block height this store will accept. Unlike the other numeric
   * fields, `0` is *not* a request for a default: it means "the first
   * block is height 0". The value is passed through verbatim, and writes
   * below it fail. Only applied when the store is created or truncated;
   * otherwise the on-disk value wins.
   */
  uint64_t minimum_height;
  /**
   * Maximum number of open data-file handles to keep cached. `0` means
   * use the default ([`DEFAULT_MAX_DATA_FILES`]).
   */
  size_t max_data_files;
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
enum BlockResult_Tag
#if __STDC_VERSION__ >= 202311L
  : size_t
#endif // __STDC_VERSION__ >= 202311L
 {
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
#if __STDC_VERSION__ >= 202311L
typedef enum BlockResult_Tag BlockResult_Tag;
#else
typedef size_t BlockResult_Tag;
#endif // __STDC_VERSION__ >= 202311L

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
 * Returns the highest block height ever written to the store regardless of
 * contiguity, or 0 if `store` is null. Diverges from
 * [`bs_max_contiguous_height`] when blocks are written with gaps below them.
 */
BlockHeight bs_height_highwater(const struct Store *store);

/**
 * Returns the maximum contiguous block height of the store, or 0 if `store`
 * is null.
 */
BlockHeight bs_max_contiguous_height(const struct Store *store);

/**
 * Returns the store's configured first height (the lowest height it will
 * accept a block at), or 0 if `store` is null.
 */
BlockHeight bs_min_block_height(const struct Store *store);

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
 * Writes a block at `height` to the store.
 *
 * # Returns
 *
 * - [`VoidResult::NullHandlePointer`] if `store` is null.
 * - [`VoidResult::Ok`] on success.
 * - [`VoidResult::Err`] with a UTF-8 message otherwise. The caller must call
 *   [`bs_free_owned_bytes`] on the message.
 */
struct VoidResult bs_write_block(const struct Store *store, BlockHeight height, BorrowedBytes data);
