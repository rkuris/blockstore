package blockstore

// #cgo LDFLAGS: -L${SRCDIR}/../target/release -L${SRCDIR}/../target/debug -lblockstore_ffi
// #include <stdlib.h>
// #include "src/blockstore.h"
// #cgo noescape bs_write_block
// #cgo nocallback bs_write_block
// #cgo noescape bs_read_block
// #cgo nocallback bs_read_block
// #cgo noescape bs_max_contiguous_height
// #cgo nocallback bs_max_contiguous_height
import "C"

import (
	"runtime"
)

type Store struct {
	handle *C.struct_Store
}

// OpenRustStore opens (or creates) a Rust-backed store. Unset fields on
// cfg take their natural defaults: MaxDataFileSize=0 selects single-file
// mode, MaxDataFiles=0 selects the Rust-side default.
func OpenRustStore(cfg StoreConfig) (*Store, error) {
	pinner := runtime.Pinner{}
	defer pinner.Unpin()

	pathBytes := []byte(cfg.Path)
	args := C.struct_StoreArgs{
		path:               newBorrowedBytes(pathBytes, &pinner),
		cache_size:         C.size_t(64 * 1024 * 1024),
		max_data_file_size: C.uint64_t(cfg.MaxDataFileSize),
		max_data_files:     C.size_t(cfg.MaxDataFiles),
		truncate:           C._Bool(cfg.Truncate),
		sync:               uint32(cfg.Sync),
	}

	return getStoreFromHandleResult(C.bs_open_store(args))
}

// NewRustStore is a thin shim over OpenRustStore that opens a
// single-file, truncate-on-open store with the given sync mode.
// Kept for callers that don't need to set advanced options.
func NewRustStore(path string, sync SyncMode) (*Store, error) {
	return OpenRustStore(StoreConfig{
		Path:     path,
		Sync:     sync,
		Truncate: true,
	})
}

func (s *Store) WriteBlock(block Block) error {
	pinner := runtime.Pinner{}
	defer pinner.Unpin()

	data := newBorrowedBytes(block.Data, &pinner)
	return getErrorFromVoidResult(
		C.bs_write_block(s.handle, C.uint64_t(block.Height), data),
	)
}

func (s *Store) ReadBlock(height uint64) ([]byte, error) {
	return getBytesFromBlockResult(C.bs_read_block(s.handle, C.uint64_t(height)))
}

func (s *Store) MaxContiguousHeight() uint64 {
	return uint64(C.bs_max_contiguous_height(s.handle))
}

func (s *Store) Close() error {
	if s.handle == nil {
		return nil
	}
	err := getErrorFromVoidResult(C.bs_close_store(s.handle))
	s.handle = nil
	return err
}
