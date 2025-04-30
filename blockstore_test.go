package blockstore

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestSmoke(t *testing.T) {
	store := NewStore("test.db")
	assert.Equal(t, nil, store.WriteBlock(Block{
		Height: 1,
		Data:   []byte("test"),
	}))

	block, err := store.ReadBlock(1)
	assert.NoError(t, err)
	assert.Equal(t, []byte("test"), block)
}
