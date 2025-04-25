package blockstore

import (
	"runtime"
	"unsafe"
)

// #cgo LDFLAGS: -L${SRCDIR}/target/release -lblockstore_ffi
// #include "blockstore.h"
import "C"

type Block struct {
	Id   uint64
	Data []byte
}

type Store struct {
	handle *C.Store
}

func NewStore() *Store {
	return &Store{
		handle: C.new_store(),
	}
}

func (s *Store) AddBlock(block Block) int {
	pinner := runtime.Pinner{}
	pinner.Pin(&block.Data[0])
	defer pinner.Unpin()

	cBlock := C.Block{
		id:   C.uint64_t(block.Id),
		len:  C.size_t(len(block.Data)),
		data: (*C.uint8_t)(&block.Data[0]),
	}
	return int(C.add_block(s.handle, &cBlock))
}

func (s *Store) GetBlock(id uint64) Block {
	cBlock := C.get_block(s.handle, C.uint64_t(id))
	return Block{
		Id:   uint64(cBlock.id),
		Data: C.GoBytes(unsafe.Pointer(cBlock.data), C.int(cBlock.len)),
	}
}
