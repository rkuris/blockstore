# Differences from the Go `x/blockdb` implementation

A running list of places where the Rust port deviates from blockdb because
Rust's type system or ownership model let us delete code, simplify a
contract, or strengthen an invariant.

Each entry: what blockdb does → what we do → why it's better.

---

## 1. Evicted file handles don't strand in-flight operations

**blockdb:** The LRU cache's on-evict callback calls `f.Close()` immediately
(`database.go:221`). If another goroutine is mid-`WriteAt`/`ReadAt` on that
file, it gets `os.ErrClosed`. The read/write paths carry a retry loop
(`database.go:1083-1102`) that catches `ErrClosed`, re-evicts, and retries.

**Rust:** `FileSet::get_or_open` returns `Arc<File>`. When an entry is
evicted from the cache, the cache drops its `Arc`, but any in-flight
operations still hold their own clone — the `File` only closes when the
last `Arc` drops. Use-after-close is impossible by construction.

**Why it's better:** The retry loop disappears entirely. blockdb pays for
strict-bounded open fds with a correctness footgun; we pay for simpler
read/write paths with a tiny transient fd-count spike (bounded by
concurrent-operation count, not by anything unbounded). Better failure
mode either way.

## 2. No `Mutex<()>` to serialise file opens

**blockdb:** Has `fileOpenMu sync.Mutex` plus double-checked locking
(`database.go:1027-1034`) to prevent N goroutines from each opening the
same file index on a cache miss.

**Rust:** `get_or_open` opens optimistically without a lock, then takes
the single inner cache lock to insert. If another thread won the race, we
discard our freshly-opened handle and return theirs. The cache invariant
("each index maps to one cached handle") is preserved through the single
existing lock.

**Why it's better:** No bare `Mutex<()>`, no double-checked-locking
pattern. Cost on contention is one wasted `open(2)` syscall — and opens
are rare (only on fresh index / LRU miss). Eliminates a synchronization
primitive without weakening any guarantee.

## 3. `Option<NonZeroU64>` instead of a `u64::MAX` sentinel

**blockdb:** `MaxDataFileSize uint64` where `math.MaxUint64` means
"unlimited / single-file mode". Every consumer must remember to check the
sentinel; the recovery code has an explicit guard
(`database.go:691`: `if s.header.MaxDataFileSize == math.MaxUint64 && len(dataFiles) > 1`).

**Rust:** `StoreOptions::max_data_file_size: Option<NonZeroU64>`. `None`
is unlimited, `Some(n)` is the cap. The `None` case must be handled at
each use site (the compiler enforces it); the `Some(n)` case carries
proof that `n > 0`, so division and modulo are safe without
`#[expect(clippy::arithmetic_side_effects)]`.

The on-disk representation stays `u64` with `u64::MAX = unlimited` for
Go-format compatibility; conversion is one line at the open/truncate
boundary.

**Why it's better:** No magic constant. The type carries the
"non-zero" invariant — `FileSet::split_offset` can use plain `/` and `%`
with no validation, because the type already proved them safe. The
"unlimited" branch is forced into the type system, not lurking behind a
runtime value comparison.

## 4. Safe zero-copy serialisation via `bytemuck::Pod`

**blockdb:** Custom `BinaryMarshaler`/`BinaryUnmarshaler` per struct,
with manual `binary.LittleEndian.PutUint64`/`Uint64` calls and explicit
offset arithmetic (e.g. `database.go:148-164` for `indexFileHeader`).
Each field's offset must be tracked by hand; getting it wrong silently
mis-reads on-disk data.

**Rust:** `IndexFileHeader` and `IndexEntry` derive `Pod` and `Zeroable`.
Reads use `bytemuck::bytes_of_mut(&mut header)` to deserialise in place;
writes use `bytemuck::bytes_of(&header)`. The compiler verifies the
struct is bit-pattern-safe to transmute (no padding bytes, no enums,
`#[repr(C)]`).

**Why it's better:** Layout is enforced by the compiler. Adding a field
in the wrong place is a compile error (padding/alignment) instead of a
silent on-disk format break. Zero hand-written offset arithmetic in the
serialiser.

## 5. Automatic cleanup via `Drop`

**blockdb:** `Close()` must be called explicitly. Forgetting to call it
leaks file handles and skips the final index-header checkpoint, forcing
recovery on next open.

**Rust:** `Store` implements `Drop`: on the way out it syncs the index
file (if `SyncMode::Sync`) and writes a final checkpoint. Even on panic
or early return, the destructor runs. The user can still call an
explicit close if they need to surface errors, but they can't silently
forget.

**Why it's better:** A category of bug ("forgot to call Close()") is
removed. Recovery still works if `Drop` is skipped via `mem::forget` or
process crash — but the common-case shutdown path is automatic.

---

*This file grows as we work through the parity plan. Entries should be
specific (point to file:line in both implementations) and explain the
mechanism, not just the outcome.*
