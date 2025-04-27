package blockstore

import (
	"runtime"
	"unsafe"
)

// #cgo LDFLAGS: -L${SRCDIR}/target/release -lblockstore_ffi
// #include <stdlib.h>
// #include "blockstore.h"
import "C"

type Block struct {
	Id   uint64
	Data []byte
}

type Store struct {
	handle *C.FfiStore
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

func (s *Store) AddBlock(block Block) int {
	pinner := runtime.Pinner{}
	pinner.Pin(&block.Data[0])
	defer pinner.Unpin()

	cBlock := C.Block{
		header: C.BlockHeader{
			id:   C.uint64_t(block.Id),
			len:  C.size_t(len(block.Data)),
		},
		data: (*C.uint8_t)(&block.Data[0]),
	}
	return int(C.add_block(s.handle, cBlock))
}

func (s *Store) GetBlock(id uint64) []byte {
	cBlock := C.get_block(s.handle, C.uint64_t(id))
	return C.GoBytes(unsafe.Pointer(cBlock.data), C.int(cBlock.header.len))
}

func (s *Store) Close() {
	C.free_store(s.handle)
}
