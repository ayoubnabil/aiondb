package bookmarks

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/BurntSushi/toml"
)

type Manager struct {
	dir string
}

const (
	maxBookmarkFileBytes  = 1024 * 1024
	maxBookmarkFiles      = 1000
	maxBookmarkTotalBytes = int64(64 * 1024 * 1024)
)

func NewManager(dir string) Manager {
	return Manager{
		dir: dir,
	}
}

func (m Manager) Get(id string) (*Bookmark, error) {
	bookmarks, err := m.list()
	if err != nil {
		return nil, err
	}

	for _, b := range bookmarks {
		if b.ID == id {
			return &b, nil
		}
	}

	return nil, fmt.Errorf("bookmark %v not found", id)
}

func (m Manager) List() ([]Bookmark, error) {
	return m.list()
}

func (m Manager) ListIDs() ([]string, error) {
	bookmarks, err := m.list()
	if err != nil {
		return nil, err
	}

	ids := make([]string, len(bookmarks))
	for i, bookmark := range bookmarks {
		ids[i] = bookmark.ID
	}

	return ids, nil
}

func (m Manager) list() ([]Bookmark, error) {
	result := []Bookmark{}

	if m.dir == "" {
		return result, nil
	}

	info, err := os.Stat(m.dir)
	if err != nil {
		// Do not fail if base dir does not exists: it's not created by default
		if errors.Is(err, os.ErrNotExist) {
			fmt.Fprintf(os.Stderr, "[WARN] bookmarks dir %s does not exist\n", m.dir)
			return result, nil
		}
		return nil, err
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("path %s is not a directory", m.dir)
	}

	dir, err := os.Open(m.dir)
	if err != nil {
		return nil, err
	}
	defer dir.Close()

	matchedFiles := 0
	totalBytes := int64(0)
	for {
		dirEntries, err := dir.ReadDir(100)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, err
		}

		for _, entry := range dirEntries {
			name := entry.Name()
			if filepath.Ext(name) != ".toml" {
				continue
			}
			matchedFiles++
			if matchedFiles > maxBookmarkFiles {
				return nil, fmt.Errorf("bookmarks directory exceeds maximum file count of %d", maxBookmarkFiles)
			}

			path := filepath.Join(m.dir, name)
			size, err := bookmarkFileSize(path)
			if err != nil {
				fmt.Fprintf(os.Stderr, "[WARN] bookmark file %s is invalid: %s\n", name, err)
				continue
			}
			if bookmarkTotalWouldExceed(totalBytes, size, maxBookmarkTotalBytes) {
				return nil, fmt.Errorf("bookmarks directory exceeds maximum total size of %d bytes", maxBookmarkTotalBytes)
			}
			totalBytes += size

			bookmark, err := readBookmark(path)
			if err != nil {
				// Do not fail if one of the bookmarks is invalid
				fmt.Fprintf(os.Stderr, "[WARN] bookmark file %s is invalid: %s\n", name, err)
				continue
			}

			result = append(result, bookmark)
		}
	}

	sort.Slice(result, func(i, j int) bool {
		return result[i].ID < result[j].ID
	})

	return result, nil
}

func bookmarkFileSize(path string) (int64, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return 0, err
	}
	if !info.Mode().IsRegular() {
		return 0, errors.New("bookmark file must be a regular file")
	}
	if info.Size() > maxBookmarkFileBytes {
		return 0, fmt.Errorf("bookmark file exceeds maximum size of %d bytes", maxBookmarkFileBytes)
	}
	return info.Size(), nil
}

func bookmarkTotalWouldExceed(current int64, fileSize int64, limit int64) bool {
	if fileSize > limit {
		return true
	}
	return current > limit-fileSize
}

func readBookmark(path string) (Bookmark, error) {
	bookmark := Bookmark{
		ID: fileBasename(path),
	}

	info, err := os.Lstat(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			err = fmt.Errorf("bookmark file %s does not exist", path)
		}
		return bookmark, err
	}
	if !info.Mode().IsRegular() {
		return bookmark, errors.New("bookmark file must be a regular file")
	}

	file, err := os.Open(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			err = fmt.Errorf("bookmark file %s does not exist", path)
		}
		return bookmark, err
	}
	defer file.Close()

	buff, err := io.ReadAll(io.LimitReader(file, maxBookmarkFileBytes+1))
	if err != nil {
		return bookmark, err
	}
	if len(buff) > maxBookmarkFileBytes {
		return bookmark, fmt.Errorf("bookmark file exceeds maximum size of %d bytes", maxBookmarkFileBytes)
	}

	_, err = toml.Decode(string(buff), &bookmark)

	if bookmark.Port == 0 {
		bookmark.Port = 5432
	}

	// List of all supported postgres modes
	modes := []string{"disable", "allow", "prefer", "require", "verify-ca", "verify-full"}
	valid := false

	for _, mode := range modes {
		if bookmark.SSLMode == mode {
			valid = true
			break
		}
	}

	// Fall back to a default mode if mode is not set or invalid
	// Typical typo: ssl mode set to "disabled"
	if bookmark.SSLMode == "" || !valid {
		bookmark.SSLMode = "disable"
	}

	// Set default SSH port if it's not provided by user
	if bookmark.SSH != nil && bookmark.SSH.Port == "" {
		bookmark.SSH.Port = "22"
	}

	return bookmark, err
}

func fileBasename(path string) string {
	filename := filepath.Base(path)
	return strings.Replace(filename, filepath.Ext(path), "", 1)
}
