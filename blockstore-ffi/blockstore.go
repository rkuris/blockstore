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

func NewRustStore(path string, sync SyncMode) (*Store, error) {
	pinner := runtime.Pinner{}
	defer pinner.Unpin()

	pathBytes := []byte(path)
	args := C.struct_StoreArgs{
		path:       newBorrowedBytes(pathBytes, &pinner),
		cache_size: C.size_t(64 * 1024 * 1024),
		truncate:   C._Bool(true),
		sync:       uint32(sync),
	}

	return getStoreFromHandleResult(C.bs_open_store(args))
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
