#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


enum SyncMode {
  Async = 0,
  Sync = 1,
};
typedef uint8_t SyncMode;

typedef struct Store Store;

typedef struct FfiBlock {
  uint8_t *data;
  size_t len;
} FfiBlock;

typedef uint64_t BlockHeight;

typedef struct CreateOrOpenArgs {
  const char *path;
  size_t cache_size;
  bool truncate;
  SyncMode sync;
} CreateOrOpenArgs;

/**
 * Frees a previous return from `read_block`.
 *
 * # Safety
 * The caller must ensure that `data` is a valid pointer to a block.
 *
 * # Panics
 * Panics if `data` is a null pointer.
 */
void free_block(struct FfiBlock block);

/**
 * Frees a store instance.
 *
 * # Safety
 * The caller must ensure:
 * - `store` is a valid pointer returned by `new_store`
 * - `store` has not been freed before
 * - No other references to `store` exist
 *
 * # Panics
 * Panics if the safety lock cannot be acquired.
 */
void free_store(struct Store *store);

/**
 * Returns the maximum contiguous height of the store.
 *
 * # Safety
 * The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
 *
 * # Panics
 * Panics if `store` is a null pointer.
 */
BlockHeight max_contiguous_height(const struct Store *store);

/**
 * Creates a new store instance. Returns a pointer to the store, or null if the store cannot be created.
 *
 * # Safety
 * The caller must ensure to call `free_store` on the returned pointer when done.
 *
 * # Panics
 * Panics if `args` is a null pointer.
 */
const struct Store *new_store(struct CreateOrOpenArgs args);

/**
 * Retrieves a block by its ID.
 *
 * If the block cannot be found, it returns a block with a
 * zero length and null pointer. If an error occurs, it returns
 * a C string containing the error message and a zero size.
 *
 * # Safety
 * The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
 *
 * # Panics
 * Panics if `store` is a null pointer.
 */
struct FfiBlock read_block(const struct Store *store, BlockHeight id);

/**
 * Adds a block to the store.
 *
 * Returns 0 on success, or an error code on failure.
 *
 * Fails if the block ID is zero.
 *
 * # Safety
 * The caller must ensure that:
 * - `block` is a valid pointer to a `Block` structure
 * - The `data` field of the `Block` points to valid memory for the specified `len`
 *
 * # Panics
 * Panics if `store` or `block` is a null pointer.
 */
const char *write_block(const struct Store *store,
                        BlockHeight height,
                        size_t block_len,
                        const uint8_t *block_data);
