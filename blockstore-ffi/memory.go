package blockstore

// #include <stdlib.h>
// #include "src/blockstore.h"
// #cgo noescape bs_free_owned_bytes
// #cgo nocallback bs_free_owned_bytes
import "C"

import (
	"bytes"
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

var (
	errStoreClosed  = errors.New("store handle is null")
	errFreeingValue = errors.New("unexpected error while freeing value")
)

// Borrower is an interface for types that can borrow or copy bytes returned
// from FFI methods.
type Borrower interface {
	// BorrowedBytes returns a slice of bytes that borrows the data from the
	// Borrower's internal memory.
	//
	// The returned slice is valid only as long as the Borrower is valid.
	BorrowedBytes() []byte

	// CopiedBytes returns a slice of bytes that is a copy of the Borrower's
	// internal memory.
	CopiedBytes() []byte

	// Free releases the memory associated with the Borrower's data.
	//
	// It is safe to call Free multiple times. Subsequent calls do nothing if
	// the data has already been freed.
	//
	// It is not safe to call Free concurrently from multiple goroutines, nor
	// while outstanding references to the slice returned by BorrowedBytes
	// exist.
	Free() error
}

var _ Borrower = (*ownedBytes)(nil)

// newBorrowedBytes creates a new BorrowedBytes from a Go byte slice.
//
// Provide a Pinner to ensure the memory is pinned while the BorrowedBytes is in use.
func newBorrowedBytes(slice []byte, pinner *runtime.Pinner) C.BorrowedBytes {
	ptr := unsafe.SliceData(slice)
	sliceLen := len(slice)

	if ptr == nil {
		return C.BorrowedBytes{ptr: nil, len: 0}
	}

	if sliceLen > 0 {
		pinner.Pin(ptr)
	}

	return C.BorrowedBytes{
		ptr: (*C.uint8_t)(ptr),
		len: C.size_t(sliceLen),
	}
}

// ownedBytes is a wrapper around C.OwnedBytes that provides a Go interface
// for Rust-owned byte slices.
type ownedBytes struct {
	owned C.OwnedBytes
}

func newOwnedBytes(owned C.OwnedBytes) *ownedBytes {
	return &ownedBytes{owned: owned}
}

// Free releases the memory associated with the Borrower's data.
func (b *ownedBytes) Free() error {
	if b.owned.ptr == nil {
		return nil
	}

	if err := getErrorFromVoidResult(C.bs_free_owned_bytes(b.owned)); err != nil {
		return fmt.Errorf("%w: %w", errFreeingValue, err)
	}

	b.owned = C.OwnedBytes{}

	return nil
}

// BorrowedBytes returns the underlying byte slice. The slice is valid only
// while the ownedBytes is valid; freeing invalidates the slice.
func (b *ownedBytes) BorrowedBytes() []byte {
	if b.owned.ptr == nil {
		return nil
	}

	return unsafe.Slice((*byte)(b.owned.ptr), b.owned.len)
}

// CopiedBytes returns a copy of the underlying byte slice, valid independently
// of the ownedBytes.
//
// Uses unsafe.Slice + bytes.Clone instead of C.GoBytes because C.GoBytes takes
// a C.int length and silently truncates for buffers > MaxInt32 (Rust permits
// blocks up to u32::MAX bytes).
func (b *ownedBytes) CopiedBytes() []byte {
	if b.owned.ptr == nil {
		return nil
	}

	return bytes.Clone(unsafe.Slice((*byte)(b.owned.ptr), b.owned.len))
}

// intoError copies the bytes into a Go error and frees the underlying memory.
// Returns nil if the ownedBytes is empty.
func (b *ownedBytes) intoError() error {
	if b.owned.ptr == nil {
		return nil
	}

	err := errors.New(string(b.CopiedBytes()))

	if err2 := b.Free(); err2 != nil {
		return fmt.Errorf("%w: %w (original error: %w)", errFreeingValue, err, err2)
	}

	return err
}

// getErrorFromVoidResult converts a C.VoidResult to an error.
//
// Returns nil if the result is Ok.
func getErrorFromVoidResult(result C.VoidResult) error {
	switch result.tag {
	case C.VoidResult_NullHandlePointer:
		return errStoreClosed
	case C.VoidResult_Ok:
		return nil
	case C.VoidResult_Err:
		return newOwnedBytes(*(*C.OwnedBytes)(unsafe.Pointer(&result.anon0))).intoError()
	default:
		return fmt.Errorf("unknown C.VoidResult tag: %d", result.tag)
	}
}

// getBytesFromBlockResult converts a C.BlockResult to a Go-owned byte slice or
// error. The returned slice is independent of the FFI memory, which is freed
// inline.
//
// Returns (nil, nil) when the block is absent.
func getBytesFromBlockResult(result C.BlockResult) ([]byte, error) {
	switch result.tag {
	case C.BlockResult_NullHandlePointer:
		return nil, errStoreClosed
	case C.BlockResult_None:
		return nil, nil
	case C.BlockResult_Some:
		owned := newOwnedBytes(*(*C.OwnedBytes)(unsafe.Pointer(&result.anon0)))
		bytes := owned.CopiedBytes()
		if err := owned.Free(); err != nil {
			return nil, fmt.Errorf("%w: %w", errFreeingValue, err)
		}
		return bytes, nil
	case C.BlockResult_Err:
		return nil, newOwnedBytes(*(*C.OwnedBytes)(unsafe.Pointer(&result.anon0))).intoError()
	default:
		return nil, fmt.Errorf("unknown C.BlockResult tag: %d", result.tag)
	}
}

// getStoreFromHandleResult converts a C.StoreHandleResult to a Store or error.
func getStoreFromHandleResult(result C.StoreHandleResult) (*Store, error) {
	switch result.tag {
	case C.StoreHandleResult_Ok:
		ptr := *(**C.Store)(unsafe.Pointer(&result.anon0))
		return &Store{handle: ptr}, nil
	case C.StoreHandleResult_Err:
		return nil, newOwnedBytes(*(*C.OwnedBytes)(unsafe.Pointer(&result.anon0))).intoError()
	default:
		return nil, fmt.Errorf("unknown C.StoreHandleResult tag: %d", result.tag)
	}
}
