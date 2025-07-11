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

type StoreInterface interface {
	WriteBlock(block Block) error
	ReadBlock(id uint64) ([]byte, error)
	MaxContiguousHeight() uint64
	Close()
}
