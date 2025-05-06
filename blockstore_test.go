package blockstore

import (
	"crypto/rand"
	"os"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestSmoke(t *testing.T) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	assert.NoError(t, err)
	defer os.RemoveAll(dir)

	store := NewStore(dir)

	// Generate some random data, used for all blocks, size 1k
	data := make([]byte, 1024)
	_, err = rand.Read(data)
	assert.NoError(t, err)

	// write this block
	assert.Equal(t, nil, store.WriteBlock(Block{
		Height: 1,
		Data:   data,
	}))

	// read it back, and make sure it's the same
	block, err := store.ReadBlock(1)
	assert.NoError(t, err)
	assert.Equal(t, data, block)

	// check the maximum contiguous height
	assert.Equal(t, uint64(1), store.MaxContiguousHeight())
}

func TestParallel(t *testing.T) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	assert.NoError(t, err)
	defer os.RemoveAll(dir)

	store := NewStore(dir)
	data := make([]byte, 1024)
	_, err = rand.Read(data)
	assert.NoError(t, err)

	wg := sync.WaitGroup{}
	numGoroutines := runtime.NumCPU()
	wg.Add(numGoroutines)
	var height atomic.Uint64
	for i := 0; i < numGoroutines; i++ {
		go func() {
			for j := 0; j < 1024; j++ {
				h := height.Add(1)
				store.WriteBlock(Block{Height: h, Data: data})
			}
			wg.Done()
		}()
	}
	wg.Wait()
}
