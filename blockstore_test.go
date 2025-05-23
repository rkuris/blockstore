package blockstore

import (
	"crypto/rand"
	"os"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSmoke(t *testing.T) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	require.NoError(t, err)
	defer os.RemoveAll(dir)

	store, err := NewRustStore(dir, Async)
	require.NoError(t, err)

	blockCount := 2
	writtenBlocks := make(map[uint64][]byte, blockCount)
	for i := 1; i <= blockCount; i++ {
		data := make([]byte, 1024)
		_, err = rand.Read(data)
		assert.NoError(t, err)
		err = store.WriteBlock(Block{
			Height: uint64(i),
			Data:   data,
		})
		require.NoError(t, err)
		writtenBlocks[uint64(i)] = data

		// check the maximum contiguous height
		assert.Equal(t, uint64(i), store.MaxContiguousHeight())
	}

	for i := 1; i <= blockCount; i++ {
		block, err := store.ReadBlock(uint64(i))
		assert.NoError(t, err)
		assert.Equal(t, writtenBlocks[uint64(i)], block)
	}
}

func TestParallel(t *testing.T) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	require.NoError(t, err)
	defer os.RemoveAll(dir)

	store, err := NewRustStore(dir, Async)
	require.NoError(t, err)
	data := make([]byte, 1024)
	_, err = rand.Read(data)
	require.NoError(t, err)

	wg := sync.WaitGroup{}
	numGoroutines := runtime.NumCPU()
	wg.Add(numGoroutines)
	var height atomic.Uint64
	for i := 0; i < numGoroutines; i++ {
		go func() {
			for j := 0; j < 1024; j++ {
				h := height.Add(1)
				err := store.WriteBlock(Block{Height: h, Data: data})
				require.NoError(t, err)
			}
			wg.Done()
		}()
	}
	wg.Wait()
}

func BenchmarkReadBlock(b *testing.B) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	require.NoError(b, err)
	defer os.RemoveAll(dir)

	store, err := NewRustStore(dir, Async)
	require.NoError(b, err)
	data := make([]byte, 1024)
	for i := range data {
		data[i] = 32
	}

	var height atomic.Uint64
	err = store.WriteBlock(Block{Height: height.Add(1), Data: data})
	require.NoError(b, err)

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		store.ReadBlock(1)
	}
}

func BenchmarkWriteBlock(b *testing.B) {
	b.Run("async", func(b *testing.B) {
		benchWrite(b, Async)
	})
	b.Run("sync", func(b *testing.B) {
		benchWrite(b, Sync)
	})
}

func benchWrite(b *testing.B, syncMode SyncMode) {
	dir, err := os.MkdirTemp("", "blockstore_test")
	require.NoError(b, err)
	defer os.RemoveAll(dir)

	store, err := NewRustStore(dir, syncMode)
	require.NoError(b, err)
	data := make([]byte, 1024)
	for i := range data {
		data[i] = 32
	}

	var height atomic.Uint64
	nthreads := runtime.NumCPU()
	b.ResetTimer()
	for i := 0; i < b.N/nthreads; i++ {
		wg := sync.WaitGroup{}
		wg.Add(nthreads)
		for j := 0; j < nthreads; j++ {
			go func() {
				defer wg.Done()
				err := store.WriteBlock(Block{Height: height.Add(1), Data: data})
				require.NoError(b, err)
			}()
		}
		wg.Wait()
	}
}
