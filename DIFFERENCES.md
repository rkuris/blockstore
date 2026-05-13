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
forward through any pre-existing index entries that filled gaps. After
a crash, recovery rebuilds the counter using `fetch_max` per validated
block (no cascade, so unverified pre-crash index entries can't poison
the result).

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

*This file grows as we work through the parity plan. Entries should be
specific (point to file:line in both implementations) and explain the
mechanism, not just the outcome.*
