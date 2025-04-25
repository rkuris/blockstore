# Blockstore

A Rust library with Go bindings for storing blocks of data.

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
