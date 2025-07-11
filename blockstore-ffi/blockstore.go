package blockstore

import (
	"errors"
	"runtime"
	"unsafe"
)

// #cgo LDFLAGS: -L${SRCDIR}/../target/release -lblockstore_ffi
// #include <stdlib.h>
// #include "src/blockstore.h"
import "C"
type Store struct {
	handle *C.struct_Store
}

func NewRustStore(path string, sync SyncMode) (*Store, error) {
	createOrOpenArgs := C.struct_CreateOrOpenArgs{
		path:       C.CString(path),
		cache_size: C.size_t(64 * 1024 * 1024), // 64MB cache
		truncate:   C._Bool(true),
		sync:       uint32(sync),
	}

	return &Store{
		handle: C.new_store(createOrOpenArgs),
	}, nil
}

func (s *Store) WriteBlock(block Block, header_size uint16) error {
	pinner := runtime.Pinner{}
	pinner.Pin(&block.Data[0])
	defer pinner.Unpin()

	result := C.write_block(s.handle, C.uint64_t(block.Height), C.size_t(len(block.Data)), (*C.uchar)(unsafe.SliceData(block.Data)), C.uint16_t(header_size))
	if result != nil {
		return errors.New(C.GoString(result))
	}
	return nil
}

func (s *Store) ReadBlock(id uint64) ([]byte, error) {
	cBlock := C.read_block(s.handle, C.uint64_t(id))
	if cBlock.len == 0 {
		if cBlock.data == nil {
			return nil, nil
		}
		return nil, errors.New(C.GoString((*C.char)(unsafe.Pointer(cBlock.data))))
	}
	return C.GoBytes(unsafe.Pointer(cBlock.data), C.int(cBlock.len)), nil
}

func (s *Store) ReadBlockHeader(id uint64) ([]byte, error) {
	cBlock := C.read_block_header(s.handle, C.uint64_t(id))
	if cBlock.len == 0 {
		if cBlock.data == nil {
			return nil, nil
		}
		return nil, errors.New(C.GoString((*C.char)(unsafe.Pointer(cBlock.data))))
	}
	return C.GoBytes(unsafe.Pointer(cBlock.data), C.int(cBlock.len)), nil
}

func (s *Store) MaxContiguousHeight() uint64 {
	return uint64(C.max_contiguous_height(s.handle))
}

func (s *Store) Close() {
	C.free_store(s.handle)
}
