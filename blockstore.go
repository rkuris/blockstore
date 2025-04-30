package blockstore

import (
	"errors"
	"runtime"
	"unsafe"
)

// #cgo LDFLAGS: -L${SRCDIR}/target/release -lblockstore_ffi
// #include <stdlib.h>
// #include "blockstore.h"
import "C"

type Block struct {
	Height uint64
	Data   []byte
}

type Store struct {
	handle *C.struct_Store
}

func NewStore(path string) *Store {
	createOrOpenArgs := C.struct_CreateOrOpenArgs{
		path:       C.CString(path),
		cache_size: C.size_t(64 * 1024 * 1024), // 64MB cache
		truncate:   C._Bool(true),
	}

	return &Store{
		handle: C.new_store(createOrOpenArgs),
	}
}

func (s *Store) WriteBlock(block Block) error {
	pinner := runtime.Pinner{}
	pinner.Pin(&block.Data[0])
	defer pinner.Unpin()

	result := C.write_block(s.handle, C.uint64_t(block.Height), C.size_t(len(block.Data)), (*C.uchar)(unsafe.SliceData(block.Data)))
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

func (s *Store) Close() {
	C.free_store(s.handle)
}
