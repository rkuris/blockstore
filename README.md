# Blockstore

A Rust library with Go bindings for storing blocks of data, optimized for blockchain blocks.

Blocks may be received out of order, but we still write the blocks as we receive them.

## Features

- [x] O(1) write time, preferably append-only
- [x] Low or zero data write amplification
- [x] Ability to receive blocks out of order
- [x] No garbage collection or deletion
- [x] Ability to stream/iterate over blocks efficiently either forwards or backwards
- [x] Performs basic sanity checks on blocks (such as a checksum)
- [x] Support for large and variable-sized blocks
- [x] A way of fetching the highest known contiguous height on startup
- [x] Ability to read blocks in parallel
- [x] Performance tests

## TODO

- [ ] Complete recovery code for blocks written before flushing the index file
- [ ] Circular cache for highest height blocks
- [ ] Iterators

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

- Rust library
- Go bindings (`blockstore.go`)
- Go tests (`blockstore_test.go`)

The Rust library must be built before running Go tests as the Go code links against the compiled Rust library.
