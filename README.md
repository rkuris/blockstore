# Blockstore

A Rust library with Go bindings for storing blocks of data, optimized for blockchain blocks.

Blocks may be received out of order, but we still write the blocks as we receive them.

To find blocks, we maintain a LRU cache. We add to the cache whenever we write a block to disk,
as it's likely someone will be requesting that block soon. However, there are two cases to
consider:

- A new process starts up and wants to start streaming blocks from some recent point in time.
   The cache should work fine here, because entries created much later will disappear from the
   cache sooner.

- A new process starts up and wants blocks from the beginning of time. Since these will be read
  from disk and they will be out of order, we should be caching entries that are larger than the
  on requested, since they are likely to be requested soon. We handle this by caching blocks we
  happen to see while reading a chunk.

We could do a lot better here. Since the chunk we're reading probably contains some additional blocks
we are likely to want, we should keep reading the chunk and cache some additional entries, or at least
remember where we left off so we can resume from there on the next read.

## Building the Rust Library

1. Build the Rust library:

```bash
cargo build --release
```

## Running Go Tests

### Install Go dependencies

```bash
go mod tidy
```

### Run the tests

```bash
go test -v
```

## Development

The project consists of:

- Rust library (`src/lib.rs`)
- Go bindings (`blockstore.go`)
- Go tests (`blockstore_test.go`)

The Rust library must be built before running Go tests as the Go code links against the compiled Rust library.
