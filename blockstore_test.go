package blockstore

import (
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestAddBlock(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "test.db")
	store := NewStore(dbPath)
	assert.Equal(t, 0, store.AddBlock(Block{
		Id:   1,
		Data: []byte("test"),
	}))

	assert.Equal(t, []byte("test"), store.GetBlock(1))
}
