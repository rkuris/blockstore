# blockstore

A Rust library (with Go bindings) for storing blockchain blocks
keyed by height. On-disk-format-compatible with avalanchego's
[`x/blockdb`](https://github.com/ava-labs/avalanchego/tree/master/x/blockdb),
intended as a candidate replacement for it.

Blocks can arrive out of order. The store appends data as it receives
it, keeps a sparse index keyed by height, and recovers gracefully
from unclean shutdown by scanning data files and rebuilding any
missing index entries.

## What's here

This is a Cargo workspace with four crates plus a Go FFI wrapper:

| Crate | What it does |
|---|---|
| [`blockstore`](./blockstore) | The core library — `Store`, `CachedStore`, recovery, multi-file data splitting. |
| [`blockstore-cli`](./blockstore-cli) | `clap`-based CLI: `get`, `import`, `copy`. Useful for forensics and migration. |
| [`blockstore-ffi`](./blockstore-ffi) | C ABI + a thin Go wrapper (`OpenRustStore`) so Go callers can use the store. |
| [`parser`](./parser) | Block-format parser used by the CLI's `import` subcommand. |

## Why this exists alongside `x/blockdb`

avalanchego already ships a working Go block store. The Rust port is
not motivated by raw speed — both implementations are I/O-bound and
on realistic workloads the bottleneck is `pwrite(2)` / `fsync(2)`,
not the language runtime.

What the Rust port buys is **lower long-term maintenance cost**,
**better observability**, and **a few features blockdb doesn't have**.
Everything else — features, on-disk format, crash recovery,
durability — is at parity by design.

### Concrete arguments for the Rust port

1. **Smaller maintenance surface.** `x/blockdb` is coupled to four
   internal avalanchego packages (`cache/lru`, `database`,
   `utils/compression`, `utils/logging`). Refactoring it touches the
   wider monorepo. The Rust port is a self-contained workspace
   depending on a small set of community crates — readable in an
   afternoon, refactorable without touching anything else.

2. **Built-in metrics.** blockdb ships no internal counters.
   blockstore has tagged counters at every hot path
   (`blockstore.cache.read{result=hit|miss}`,
   `blockstore.read_block.*`, `blockstore.write_block.*`), gated
   behind `feature = "metrics"` so non-metrics builds pay nothing.
   The pattern follows firewood's: one counter name, structured
   labels.

3. **CLI.** blockdb is library-only. Inspecting a store on disk or
   migrating data requires writing a one-off Go program. blockstore
   ships `blockstore-cli` with `get` / `import` / `copy`. Concrete
   maintenance wins for post-incident forensics and format work.

4. **Surplus features.** Documented in [DIFFERENCES.md](./DIFFERENCES.md):
   - `max_contiguous_height()` — O(1) answer to "what's the highest
     contiguous height present?". Requested in the original design
     doc but never landed in blockdb.
   - Byte-budgeted block cache via `lru-mem` — operators size by
     memory budget, not entry count, so block-size variance can't
     blow up RAM usage.

5. **Type-system enforcement.** A handful of bug-classes that
   blockdb has to remember to avoid are structurally impossible
   here: `Option<NonZeroU64>` instead of a `u64::MAX` sentinel,
   `bytemuck::Pod` for compile-checked on-disk layout, `Arc<File>`
   eviction without `ErrClosed` retry loops, `Drop` for automatic
   checkpoint.

6. **Less Go GC pressure.** The hot paths in blockstore allocate on
   the Rust heap: index entries, block headers, compressed
   buffers, file handles, and — importantly — the entire block
   cache, with all its `Arc<[u8]>` entries. None of these touch
   Go's GC. Compare to `x/blockdb`, where every cached `[]byte`,
   every per-block compression buffer, and every internal map
   participates in Go's GC cycle. Replacing blockdb with the
   FFI'd Rust store moves a substantial chunk of allocation
   traffic off the Go heap entirely, reducing GC pause frequency
   and shrinking the Go heap working set on busy nodes.

### What we are not claiming

- **Not faster than blockdb** in any way that matters for I/O-bound
  workloads.
- **Not smaller in raw line count.** The two libraries are within a
  few percent of each other; the difference is in *what they
  stand on* (avalanchego internals vs community crates), not in
  size.
- **Not yet battle-tested.** Neither blockstore nor `x/blockdb` is
  in full production today — blockdb runs on canary nodes and
  blockstore hasn't been rolled out. Both deserve measured rollouts.
  The argument here is "given we're picking between two early-stage
  implementations, which one has the better forward trajectory?",
  not "displace a proven system with a new one".

### Costs we accept

Documented in [DIFFERENCES.md](./DIFFERENCES.md#constraints):

- **Little-endian only.** Index structures are serialised
  host-endian via `bytemuck` for the zero-copy benefit; a
  compile-time guard fails the build on big-endian targets. Every
  realistic deployment target is LE.
- **FFI overhead per call.** Crossing cgo costs ~100 ns plus a
  memory pin. In practice this is dwarfed by the disk I/O every
  block operation does — a single `pwrite` or `pread` is orders of
  magnitude slower than the FFI hop, so the overhead is invisible
  on real workloads.

See [DIFFERENCES.md](./DIFFERENCES.md) for the detailed item-by-item
technical comparison.

## Building

The Rust workspace is the source of truth. The Go wrapper links
against the compiled Rust static library.

```bash
# build the FFI library Go links against (release recommended)
cargo build --release -p blockstore-ffi

# core library tests
cargo test -p blockstore

# the CLI
cargo build --release -p blockstore-cli
./target/release/blockstore-cli --help
```

For Go consumers:

```bash
cd blockstore-ffi
go test ./...   # exercises the FFI smoke + parallel tests + example
```

## CLI examples

```bash
# Read block at height 42 from a store
blockstore-cli --db-path /var/avalanche/blocks get --height 42

# Migrate blocks between stores
blockstore-cli --db-path /old/store copy --target /new/store

# Import from a LevelDB-formatted block database
blockstore-cli --db-path /new/store import --leveldb /old/leveldb
```

## Documentation

- [DIFFERENCES.md](./DIFFERENCES.md) — item-by-item technical
  comparison with `x/blockdb`. Categorised into *Additional
  features*, *Performance improvements*, *Safety and code-deletion
  wins*, and *Constraints* (what we give up).
- [`blockstore/src/store.rs`](./blockstore/src/store.rs) — core
  `Store` API. Doc-comments on `Store::open` and `StoreOptions`
  cover the typical use cases.
- [`blockstore-ffi/blockstore.go`](./blockstore-ffi/blockstore.go) —
  Go-side `OpenRustStore(StoreConfig)` entry point.
  `ExampleOpenRustStore` in the test file is the canonical usage
  example.

## License

See [LICENSE.md](./LICENSE.md).
