# Differences from the Go `x/blockdb` implementation

A running list of places where the Rust port deviates from blockdb.
Three categories:

- **Additional features** — APIs we provide that blockdb does not.
- **Performance improvements** — places where the Rust path is faster or
  has a lower memory ceiling than the equivalent in blockdb.
- **Safety and code-deletion wins** — places where Rust's type system or
  ownership model let us delete code blockdb has to carry, or strengthen
  an invariant.

Each entry: what blockdb does → what we do → why it's better.

---

## Additional features

### Byte-budgeted block cache

**blockdb:** `cacheDB` is sized by *number of cached entries* —
`BlockCacheSize uint16` defaults to 256 (`config.go:14-15`). The cache
holds up to 256 `BlockData` entries regardless of their size. Total
memory usage is `256 × whatever_block_sizes_happen_to_be`, which on
Avalanche can mean anywhere from a few hundred KiB (small P-chain
blocks) to multiple GiB (large C-chain blocks). No memory-pressure
guarantee.

**Rust:** `CachedStore` uses [`lru-mem`](https://crates.io/crates/lru-mem),
which evicts oldest entries until the total tracked heap occupancy
stays under a *byte budget* (`StoreOptions::cache_size: NonZeroUsize`,
in bytes). Block sizes vary; the byte budget gives a hard
memory-pressure bound regardless of which blocks are hot.

**Why it's useful:** Operators can size the cache by "how much RAM I'm
willing to spend on block caching" — a number they can actually reason
about — instead of "how many entries, given block sizes I have to
guess". For Avalanche specifically, where C-chain block sizes can be
1000× larger than P-chain blocks, this is the difference between a
predictable memory footprint and a per-workload surprise.

**Implementation note:** `Block` is `Arc<[u8]>` rather than `Box<[u8]>`
so cache hits are O(1) reference-count clones, not memcpy. A small
newtype `CacheEntry` lets `Arc<[u8]>` participate in lru-mem's
`HeapSize`-aware accounting; the cache over-counts when callers hold
outstanding `Arc` clones, but always stays *under* its budget
(conservative eviction, never an overflow).

### Metrics surface

**blockdb:** ships no internal counters or gauges. The cache, the
recovery scan, and the read/write paths emit zap log lines for
notable events but no machine-readable telemetry. Operators wanting
to know "is the cache doing work?" or "how many block reads are
hitting the slow path?" must instrument the consumer side.

**Rust:** every read/write path and every cache lookup emits a
`metrics` counter, gated behind `feature = "metrics"` (no-op when
off, so there's no runtime cost in builds that don't want them). The
pattern mirrors firewood's: a single counter name with structured
labels distinguishing the variants.

Current counters (under the `blockstore.` prefix):

| Counter | Labels | Where |
|---|---|---|
| `read_block.success` | — | per successful read |
| `read_block.not_found` | — | block doesn't exist |
| `read_block.checksum_mismatch` | — | corrupt block detected on read |
| `read_block.read_header_failed` | — | header read I/O error |
| `read_block.read_index_entry_failed` | — | index lookup failure |
| `read_block.block_size_mismatch` | — | index/header size disagree |
| `read_block.block_size_too_large` | — | reject oversized block |
| `read_block.success.duration_ms` | — | latency histogram |
| `write_block.success` | — | per successful write |
| `write_block.empty` | — | reject zero-length block |
| `write_block.block_too_large` | — | reject oversized block |
| `write_block.invalid_block_height` | — | reject height < min |
| `write_block.block_exceeds_file_size` | — | reject block > max_data_file_size |
| `write_block.offset_overflow` | — | u64 offset arithmetic overflow |
| `write_block.out_of_order` | — | write filled a gap, not the next height |
| `write_block.write_header_failed` | — | data-file header write failed |
| `write_block.write_data_failed` | — | data-file payload write failed |
| `write_block.success.duration_ms` | — | latency histogram |
| `write_block.sync_duration_ms` | — | `fsync` latency under sync mode |
| `cache.read` | `result=hit\|miss` | every cache lookup |
| `cache.populate` | `outcome=ok\|oversize` | post-miss cache insert |
| `cache.populate_on_write` | `outcome=ok\|oversize` | write-side cache insert |

**Why it's useful:** the cache counters in particular answer the
"is my cache budget reasonable?" question directly:
`cache.read{result=hit}/cache.read{result=miss}` is the hit ratio,
and `cache.populate{outcome=oversize}` flags entries the budget
silently refused. The pattern (single counter name, structured
labels) follows firewood — cheaper at query time than parallel
hit/miss counter names, and easier to chart in Grafana/Prometheus.

### Command-line tool (`blockstore-cli`)

**blockdb:** is a library only — no `main.go`, no `cmd/` directory,
no binary. Inspecting a store on disk, importing from another
backend, or copying blocks between stores all require writing a
one-off Go program that links the package and uses its public API.

**Rust:** ships `blockstore-cli` as a workspace crate with three
subcommands:

| Subcommand | What it does |
|---|---|
| `get --height N` | Read and hex-dump the block at height N. |
| `import --leveldb DB` | Import blocks from a LevelDB database (the historical avalanchego format) into a fresh blockstore. |
| `copy --target DIR` | Copy/migrate blocks from one blockstore directory to another. |

**Why it's useful (maintenance angle):**

Every CLI subcommand replaces an ad-hoc Go program that blockdb
operators would otherwise have to write each time. Concretely:

- **Post-incident forensics.** When a node misbehaves and you suspect
  the block store, "what's actually at height N?" is one CLI
  invocation, no avalanchego binary or Go toolchain required. With
  blockdb you'd write `main.go`, `go run` it, and probably re-write
  it next time because you didn't commit it anywhere.
- **Migration and format work.** `copy` and `import` exercise the
  full open → read → write → recovery pipeline against real data,
  which doubles as integration testing for the library itself. Any
  format change immediately shows up if `copy` round-trips fail.
- **Reproducing bugs.** When a user reports a corrupted store, you
  can `get` specific heights and inspect the raw bytes without
  setting up a node. The CLI accepts the same paths the FFI does, so
  there's no behavioral skew between "what the CLI sees" and "what
  avalanchego sees".
- **No version drift.** The CLI lives in the same workspace as the
  library, builds in the same CI, and tracks the same `Store` API.
  An out-of-tree debug script in Go drifts the moment the library
  changes signature.

The CLI is small (~150 lines of `clap` glue + one import module) and
the maintenance cost is dwarfed by the saved one-off scripting.

### `max_contiguous_height` — incremental contiguity tracking

**blockdb:** tracks `maxBlockHeight` (`database.go:189`) — the highest
height ever written — via an `atomic.Uint64`. It says nothing about
whether `[min_height, maxBlockHeight]` is contiguous; gaps are allowed
and silently ignored. A consumer that needs "the highest height H such
that all of `min..=H` are present" must call `Has(h)` for each
candidate or walk by `Get(h)` until it hits `ErrNotFound`.

**Rust:** `Store::max_contiguous_height()` returns exactly that value
in O(1). It's maintained incrementally on write via a CAS-based
fast-path: a write at height `prev + 1` bumps the counter and cascades
forward through any pre-existing index entries that filled gaps.

The floor is persisted at each checkpoint in the index header field
`highest_contiguous_block_height`, which occupies the first 8 bytes of
the area blockdb reserves after `next_write_offset` (so the 64-byte
header size and every preceding field offset are unchanged, and a
blockdb-written index still parses). On reopen the persisted value seeds
the counter and the contiguity scan resumes just above it — after a
clean shutdown this is O(1), with no index reads. A persisted `0` means
"unknown": an index written by blockdb (which zeroes the reserved area
and doesn't track contiguity) or by an older revision of this crate. In
that case recovery falls back to a full index scan from `min_height`,
bounded by the highwater so unverified pre-crash entries past a corrupt
block can't poison the result. The scan independently reproduces the
correct floor, so a database can round-trip blockdb ↔ blockstore and
only pays the one-time catch-up scan on the next blockstore open.

**Why it's useful:** consensus engines frequently need this exact
guarantee — "what's the highest height I can hand to a downstream
consumer without doing per-block existence checks?". Computing it
lazily on every read is expensive (probe-until-gap). Tracking it
incrementally on write is cheap. Avalanche specifically needs this when
serving blocks to bootstrapping peers.

**History:** this API was requested in the original blockstore design
document but was never implemented in blockdb. The Rust port picks it
up.

---

## Performance improvements

### Evicted file handles don't strand in-flight operations

**blockdb:** The LRU cache's on-evict callback calls `f.Close()`
immediately (`database.go:221`). If another goroutine is mid-`WriteAt`
or `ReadAt` on that file, it gets `os.ErrClosed`. The read/write paths
carry a retry loop (`database.go:1083-1102`) that catches `ErrClosed`,
re-evicts the entry, and retries.

**Rust:** `FileSet::get_or_open` returns `Arc<File>`. When an entry is
evicted from the cache, the cache drops its `Arc`, but any in-flight
operations still hold their own clone — the `File` only closes when the
last `Arc` drops. Use-after-close is impossible by construction.

**Why it's better:** The retry loop disappears entirely — we never pay
its latency under cache contention, and we never carry the code. blockdb
pays for strict-bounded open fds with a correctness footgun; we pay for
simpler hot paths with a tiny transient fd-count spike (bounded by
concurrent-operation count). Better failure mode either way.

### No mutex on the file-open path

**blockdb:** Has `fileOpenMu sync.Mutex` plus double-checked locking
(`database.go:1027-1034`) to prevent N goroutines from each opening the
same file index on a cache miss. Every cache miss takes a lock.

**Rust:** `get_or_open` opens optimistically without a lock, then takes
the single inner cache lock to insert. If another thread won the race,
we discard our freshly-opened handle and return theirs. Cache miss
contention costs one wasted `open(2)` syscall in the worst case rather
than serialising all opens through a global mutex.

**Why it's better:** Eliminates lock contention on the first-open path
without weakening any guarantee. The Rust cache invariant ("each index
maps to one cached handle at a time") is preserved through the
existing inner lock, so no new synchronization primitive is added.

### Safe zero-copy serialisation via `bytemuck::Pod`

**blockdb:** Custom `BinaryMarshaler`/`BinaryUnmarshaler` per struct,
with manual `binary.LittleEndian.PutUint64`/`Uint64` calls and explicit
offset arithmetic (e.g. `database.go:148-164` for `indexFileHeader`).
Each field's offset is tracked by hand; getting it wrong silently
mis-reads on-disk data. Every read allocates a buffer and copies bytes
out; every write does the reverse.

**Rust:** `IndexFileHeader` and `IndexEntry` derive `Pod` and
`Zeroable`. Reads use `bytemuck::bytes_of_mut(&mut header)` to
deserialise in place; writes use `bytemuck::bytes_of(&header)`. The
compiler verifies the struct is bit-pattern-safe to transmute (no
padding bytes, no enums, `#[repr(C)]`). No allocation, no copy.

**Why it's better:** Layout is enforced by the compiler — adding a
field in the wrong place is a compile error (padding/alignment) instead
of a silent on-disk format break. Index header reads and writes happen
directly against the file buffer with zero intermediate allocations,
which matters on hot startup paths.

---

## Safety and code-deletion wins

### `Option<NonZeroU64>` instead of a `u64::MAX` sentinel

**blockdb:** `MaxDataFileSize uint64` where `math.MaxUint64` means
"unlimited / single-file mode". Every consumer must remember to check
the sentinel; the recovery code has an explicit guard
(`database.go:691`: `if s.header.MaxDataFileSize == math.MaxUint64 && len(dataFiles) > 1`).

**Rust:** `StoreOptions::max_data_file_size: Option<NonZeroU64>`.
`None` is unlimited, `Some(n)` is the cap. The `None` case must be
handled at each use site (the compiler enforces it); the `Some(n)`
case carries proof that `n > 0`, so division and modulo are safe
without `#[expect(clippy::arithmetic_side_effects)]`.

The on-disk representation stays `u64` with `u64::MAX = unlimited` for
Go-format compatibility; conversion is one line at the open/truncate
boundary.

**Why it's better:** No magic constant. The type carries the
"non-zero" invariant — `FileSet::split_offset` can use plain `/` and
`%` with no validation, because the type already proved them safe. The
"unlimited" branch is forced into the type system, not lurking behind
a runtime value comparison.

### Advisory locking on `Store::open`

**blockdb:** the Go implementation takes no advisory lock when opening a
store directory. Two processes that point at the same directory both
succeed, and both proceed to write — interleaving updates to the index
header, data files, and next-write-offset. The on-disk layout silently
diverges from any consistent state. There is no in-process or
cross-process guard against this; consumers are expected to coordinate
externally (typically by relying on the surrounding service being a
singleton).

**Rust:** `Store::open` takes an exclusive advisory lock
(`File::try_lock`, stable stdlib since 1.89) on the index file and holds
it for the `Store`'s lifetime. A second open against the same directory
fails fast with `io::ErrorKind::WouldBlock` instead of corrupting data.
Every R/W-opened data file likewise takes an advisory lock when added
to the `FileSet`; the lock is released when the file handle's last
`Arc<File>` clone drops.

**Why it's better:** an entire class of corruption — "I accidentally
ran two writers against the same store" — becomes a clean, immediate
error at open time instead of slow, silent damage that only surfaces
on the next reopen. The mechanism is OS-native (`flock` on Unix,
`LockFileEx` on Windows via stdlib), advisory (no impact on processes
that don't open via `Store::open`), and costs one syscall at open and
one at drop.

**Known limitation:** the data-file lock travels with the cached
`Arc<File>`. If a sealed data file is evicted from the `FileSet`'s LRU
cache, its lock is briefly released until the file is re-opened. The
active (most-recently-touched) data file is always MRU so it doesn't
get evicted in practice, but tightening this gap by pinning the active
file in a dedicated slot is tracked as future work.

### Automatic cleanup via `Drop`

**blockdb:** `Close()` must be called explicitly. Forgetting to call
it leaks file handles and skips the final index-header checkpoint,
forcing recovery on next open.

**Rust:** `Store` implements `Drop`: on the way out it syncs the index
file (if `SyncMode::Sync`) and writes a final checkpoint. Even on
panic or early return, the destructor runs. The user can still call an
explicit close if they need to surface errors, but they can't silently
forget.

**Why it's better:** A category of bug ("forgot to call Close()") is
removed. Recovery still works if `Drop` is skipped via `mem::forget`
or process crash — but the common-case shutdown path is automatic.

---

## Constraints

Honest list of places where the Rust port is *less capable* than
blockdb. These are tradeoffs we made deliberately — usually in
exchange for one of the wins above — not oversights.

### Little-endian only

**blockdb:** uses `binary.LittleEndian.PutUint64` / `Uint64`
throughout (`database.go:148-164` etc.). The serialisation is
portable: a blockdb store written on a big-endian host is readable
on a little-endian host and vice versa.

**Rust:** `IndexFileHeader` and `IndexEntry` are serialised
zero-copy via `bytemuck::bytes_of` — the on-disk bytes are the
in-memory representation, which is **host-endian**. On every
target Rust ships with by default (x86_64, aarch64, RISC-V LE, WASM)
that's little-endian and matches blockdb. On a big-endian target
(`powerpc-unknown-linux-gnu`, `s390x-unknown-linux-gnu`, MIPS BE)
the on-disk bytes would be reversed and incompatible with blockdb.

`BlockHeader` itself is serialised with explicit `to_le_bytes` /
`from_le_bytes` and is portable, but the index structures are not.

We defend this with a compile-time guard in `blockstore/src/lib.rs`:

```rust
#[cfg(not(target_endian = "little"))]
compile_error!("blockstore requires a little-endian target: ...");
```

A BE build fails loudly instead of silently producing files that
neither blockdb nor a future LE Rust build could read.

**Why we accept this:** zero-copy serialisation buys us
compiler-verified layout (a Pod-derive failure is a compile error)
and zero-allocation read/write of the index header — both real wins
on the LE platforms we actually ship to. Every realistic deployment
target (x86_64 servers, ARM64 macOS / Linux, ARM64 cloud) is LE; no
Avalanche operator is running a node on a System Z mainframe or
PowerPC 64BE. The cost of the constraint is theoretical.

If a BE target ever became load-bearing, the fix is mechanical:
switch the index structures to the same `to_le_bytes` /
`from_le_bytes` style `BlockHeader` already uses. We'd lose the
zero-copy path; we'd keep the compatibility. We'd take that trade if
we had to — we just don't have to today.

---

*This file grows as we work through the parity plan. Entries should be
specific (point to file:line in both implementations) and explain the
mechanism, not just the outcome.*
