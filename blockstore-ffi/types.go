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
}

type StoreInterface interface {
	WriteBlock(block Block) error
	ReadBlock(height uint64) ([]byte, error)
	MaxContiguousHeight() uint64
	Close() error
}
