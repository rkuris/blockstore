#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


/**
 * The size of a block in the store.
 */
#define BLOCK_SIZE 4096

typedef struct FfiStore FfiStore;

typedef uint64_t BlockId;

typedef struct BlockHeader {
  BlockId id;
  size_t len;
} BlockHeader;

typedef struct Block {
  struct BlockHeader header;
  const uint8_t *data;
} Block;

typedef struct CreateOrOpenArgs {
  const char *path;
  size_t cache_size;
  bool truncate;
} CreateOrOpenArgs;

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
int add_block(struct FfiStore *store, struct Block block);

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
void free_store(struct FfiStore *store);

/**
 * Retrieves a block by its ID.
 *
 * If the block cannot be found, it returns a block with a
 * zero length and null pointer.
 *
 * # Safety
 * The caller must ensure that `store` is a valid pointer to a `FfiStore` instance.
 *
 * # Panics
 * Panics if `store` is a null pointer.
 */
struct Block get_block(const struct FfiStore *store, BlockId id);

/**
 * Creates a new store instance. Returns a pointer to the store, or null if the store cannot be created.
 *
 * # Safety
 * The caller must ensure to call `free_store` on the returned pointer when done.
 *
 * # Panics
 * Panics if `args` is a null pointer.
 */
struct FfiStore *new_store(struct CreateOrOpenArgs args);
