//go:build !windows

package queries

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestStoreReadAll(t *testing.T) {
	t.Run("valid dir", func(t *testing.T) {
		queries, err := NewStore("../../data").ReadAll()
		assert.NoError(t, err)
		assert.Equal(t, 2, len(queries))
	})

	t.Run("invalid dir", func(t *testing.T) {
		queries, err := NewStore("../../data2").ReadAll()
		assert.Equal(t, err.Error(), "queries directory does not exist")
		assert.Equal(t, 0, len(queries))
	})
}

func TestStoreReadAllRejectsTooManyQueryFiles(t *testing.T) {
	dir := t.TempDir()
	for i := 0; i <= maxLocalQueryFiles; i++ {
		path := filepath.Join(dir, "query_"+strconv.Itoa(i)+".sql")
		assert.NoError(t, os.WriteFile(path, []byte("-- pgweb: host=\"localhost\"\nselect 1"), 0o600))
	}

	queries, err := NewStore(dir).ReadAll()

	assert.Nil(t, queries)
	assert.EqualError(t, err, "queries directory exceeds maximum file count of 1000")
}

func TestLocalQueryTotalWouldExceed(t *testing.T) {
	query := &Query{ID: "q", Path: "q.sql", Data: "select 1", Meta: &Metadata{Title: "title"}}

	assert.False(t, localQueryTotalWouldExceed(0, query, 1024))
	assert.True(t, localQueryTotalWouldExceed(1020, query, 1024))
	assert.True(t, localQueryTotalWouldExceed(0, &Query{Data: strings.Repeat("x", 1025)}, 1024))
}

func TestStoreRead(t *testing.T) {
	examples := []struct {
		id    string
		err   string
		check func(*testing.T, *Query)
	}{
		{id: "foo", err: "query file does not exist"},
		{id: "lc_no_meta"},
		{id: "lc_invalid_meta", err: `invalid "mode" field value: "foo"`},
		{
			id: "lc_example1",
			check: func(t *testing.T, q *Query) {
				assert.Equal(t, "lc_example1", q.ID)
				assert.Equal(t, "../../data/lc_example1.sql", q.Path)
				assert.Equal(t, "select 'foo'", q.Data)
				assert.Equal(t, "localhost", q.Meta.Host.String())
				assert.Equal(t, "*", q.Meta.User.String())
				assert.Equal(t, "*", q.Meta.Database.String())
			},
		},
		{
			id: "lc_example2",
			check: func(t *testing.T, q *Query) {
				assert.Equal(t, "lc_example2", q.ID)
				assert.Equal(t, "../../data/lc_example2.sql", q.Path)
				assert.Equal(t, "-- some comment\nselect 'foo'", q.Data)
				assert.Equal(t, "localhost", q.Meta.Host.String())
				assert.Equal(t, "foo", q.Meta.User.String())
				assert.Equal(t, "*", q.Meta.Database.String())
			},
		},
	}

	store := NewStore("../../data")

	for _, ex := range examples {
		t.Run(ex.id, func(t *testing.T) {
			query, err := store.Read(ex.id)
			if ex.err != "" || err != nil {
				assert.Equal(t, ex.err, err.Error())
			}
			if ex.check != nil {
				ex.check(t, query)
			}
		})
	}
}

func TestStoreReadRejectsPathTraversal(t *testing.T) {
	root := t.TempDir()
	queryDir := filepath.Join(root, "queries")
	assert.NoError(t, os.Mkdir(queryDir, 0o700))
	assert.NoError(t, os.WriteFile(
		filepath.Join(root, "outside.sql"),
		[]byte("-- pgweb: host=\"localhost\"\nselect 'outside'"),
		0o600,
	))

	query, err := NewStore(queryDir).Read("../outside")

	assert.ErrorIs(t, err, ErrQueryFileNotExist)
	assert.Nil(t, query)
}

func TestStoreReadRejectsSymlink(t *testing.T) {
	root := t.TempDir()
	queryDir := filepath.Join(root, "queries")
	assert.NoError(t, os.Mkdir(queryDir, 0o700))

	outsidePath := filepath.Join(root, "outside.sql")
	assert.NoError(t, os.WriteFile(
		outsidePath,
		[]byte("-- pgweb: host=\"localhost\"\nselect 'outside'"),
		0o600,
	))
	assert.NoError(t, os.Symlink(outsidePath, filepath.Join(queryDir, "link.sql")))

	query, err := NewStore(queryDir).Read("link")

	if !assert.Error(t, err) {
		return
	}
	assert.Nil(t, query)
	assert.Contains(t, err.Error(), "regular file")
}
