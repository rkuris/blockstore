package blockstore

type Block struct {
	Height uint64
	Data   []byte
}

type SyncMode int

const (
	Async SyncMode = iota
	Sync
)

// StoreConfig controls how a Store is opened.
type StoreConfig struct {
	// Path is the directory holding both the index file (blockdb.idx) and
	// the data files (blockdb_N.dat).
	Path string

	// Sync determines whether each write fsyncs.
	Sync SyncMode

	// Truncate, if true, wipes any existing store at Path on open.
	Truncate bool

	// MaxDataFileSize caps each blockdb_N.dat file at this many bytes,
	// rolling into a new file when a block would cross the boundary.
	// Zero means unlimited (single-file mode, blockdb_0.dat only).
	MaxDataFileSize uint64

	// MaxDataFiles bounds how many open data-file handles are cached.
	// Zero means use the Rust-side default (10).
	MaxDataFiles int

	// CacheSize is the byte budget for the in-memory LRU read cache.
	// Zero opens the store without a cache, so every read goes to the
	// index and data files. Any other value caps the cache's tracked heap
	// occupancy at that many bytes -- a budget in bytes rather than in
	// entries, so the ceiling holds whatever size the cached blocks are.
	//
	// A non-zero CacheSize costs concurrency. The LRU sits behind a single
	// mutex that every read and every write takes exclusively -- a cache
	// hit still has to update recency order -- so cached reads and writes
	// serialise against each other, while the uncached read path takes no
	// lock at all. Prefer zero when many goroutines read and write
	// distinct heights; prefer a budget when the workload re-reads a
	// working set.
	CacheSize uint64

	// MinimumHeight is the lowest height the store will accept a block at.
	// Unlike MaxDataFileSize and MaxDataFiles, zero is not a request for a
	// default: it means "the first block is height 0". Writes below this
	// height fail. Only applied when the store is created or truncated;
	// otherwise the minimum recorded on disk wins.
	MinimumHeight uint64
}

type StoreInterface interface {
	WriteBlock(block Block) error
	ReadBlock(height uint64) ([]byte, error)
	MaxContiguousHeight() uint64
	HeightHighwater() uint64
	MinBlockHeight() uint64
	Close() error
}
