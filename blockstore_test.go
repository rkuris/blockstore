package blockstore

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestAddBlock(t *testing.T) {
	store := NewStore()
	assert.Equal(t, 0, store.AddBlock(Block{
		Id:   1,
		Data: []byte("test"),
	}))

	assert.Equal(t, Block{
		Id:   1,
		Data: []byte("test"),
	}, store.GetBlock(1))
}
