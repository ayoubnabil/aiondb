package queries

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

var (
	ErrQueryDirNotExist  = errors.New("queries directory does not exist")
	ErrQueryFileNotExist = errors.New("query file does not exist")
)

const (
	maxLocalQueryFileBytes  = 16 * 1024 * 1024
	maxLocalQueryFiles      = 1000
	maxLocalQueryTotalBytes = int64(64 * 1024 * 1024)
)

type Store struct {
	dir string
}

func NewStore(dir string) *Store {
	return &Store{
		dir: dir,
	}
}

func (s Store) Read(id string) (*Query, error) {
	if !isLocalQueryID(id) {
		return nil, ErrQueryFileNotExist
	}
	path := filepath.Join(s.dir, fmt.Sprintf("%s.sql", id))
	return readQuery(path)
}

func isLocalQueryID(id string) bool {
	if id == "" || id == "." || id == ".." || filepath.IsAbs(id) {
		return false
	}
	return !strings.ContainsAny(id, `/\`)
}

func (s Store) ReadAll() ([]Query, error) {
	dir, err := os.Open(s.dir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			err = ErrQueryDirNotExist
		}
		return nil, err
	}
	defer dir.Close()

	queries := []Query{}

	matchedFiles := 0
	totalBytes := int64(0)
	for {
		entries, err := dir.ReadDir(100)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, err
		}

		for _, entry := range entries {
			name := entry.Name()
			if filepath.Ext(name) != ".sql" {
				continue
			}
			matchedFiles++
			if matchedFiles > maxLocalQueryFiles {
				return nil, fmt.Errorf("queries directory exceeds maximum file count of %d", maxLocalQueryFiles)
			}

			path := filepath.Join(s.dir, name)
			query, err := readQuery(path)
			if err != nil {
				fmt.Fprintf(os.Stderr, "[WARN] skipping %q query file due to error: %v\n", name, err)
				continue
			}
			if query == nil {
				continue
			}
			if localQueryTotalWouldExceed(totalBytes, query, maxLocalQueryTotalBytes) {
				return nil, fmt.Errorf("queries directory exceeds maximum total size of %d bytes", maxLocalQueryTotalBytes)
			}
			totalBytes += estimateLocalQueryBytes(query)

			queries = append(queries, *query)
		}
	}

	sort.Slice(queries, func(i, j int) bool {
		return queries[i].ID < queries[j].ID
	})

	return queries, nil
}

func estimateLocalQueryBytes(query *Query) int64 {
	if query == nil {
		return 0
	}
	total := int64(len(query.ID) + len(query.Path) + len(query.Data))
	if query.Meta != nil {
		total += int64(len(query.Meta.Title) + len(query.Meta.Description))
	}
	return total
}

func localQueryTotalWouldExceed(current int64, query *Query, limit int64) bool {
	queryBytes := estimateLocalQueryBytes(query)
	if queryBytes > limit {
		return true
	}
	return current > limit-queryBytes
}

func readQuery(path string) (*Query, error) {
	info, err := os.Lstat(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, ErrQueryFileNotExist
		}
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("query file must be a regular file")
	}
	if info.Size() > maxLocalQueryFileBytes {
		return nil, fmt.Errorf("query file exceeds maximum size of %d bytes", maxLocalQueryFileBytes)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, ErrQueryFileNotExist
		}
		return nil, err
	}
	if len(data) > maxLocalQueryFileBytes {
		return nil, fmt.Errorf("query file exceeds maximum size of %d bytes", maxLocalQueryFileBytes)
	}
	dataStr := string(data)

	meta, err := parseMetadata(dataStr)
	if err != nil {
		return nil, err
	}
	if meta == nil {
		return nil, nil
	}

	return &Query{
		ID:   strings.Replace(filepath.Base(path), ".sql", "", 1),
		Path: path,
		Meta: meta,
		Data: sanitizeMetadata(dataStr),
	}, nil
}
