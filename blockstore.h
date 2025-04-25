#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>


typedef struct Store Store;

typedef struct Block {
  uint64_t id;
  size_t len;
  const uint8_t *data;
} Block;

/**
 * Adds a block to the store.
 *
 * Returns 0 on success, or an error code on failure.
 *
 * # Safety
 * The caller must ensure that:
 * - `block` is a valid pointer to a `Block` structure
 * - The `data` field of the `Block` points to valid memory for the specified `len`
 *
 * # Panics
 * Panics if `store` or `block` is a null pointer.
 */
int add_block(struct Store *store, struct Block *block);

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
 * Retrieves a block by its ID.
 *
 * If the block cannot be found, it returns a block with a
 * zero length and null pointer.
 *
 * # Safety
 * The caller must ensure that the returned `Block`'s `data` field is properly managed
 * and that the memory it points to remains valid for the duration of its use.
 *
 * # Panics
 * Panics if `store` is a null pointer.
 */
struct Block get_block(const struct Store *store, uint64_t id);

/**
 * Creates a new store instance.
 *
 * # Safety
 * The caller must ensure to call `free_store` on the returned pointer when done.
 */
struct Store *new_store(void);
