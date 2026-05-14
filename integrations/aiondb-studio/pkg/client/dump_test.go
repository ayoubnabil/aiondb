package client

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

func TestDumpValidateContextCancelsHungVersionCheck(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("POSIX shell test")
	}

	dir := t.TempDir()
	fakePgDump := filepath.Join(dir, "pg_dump")
	assert.NoError(t, os.WriteFile(fakePgDump, []byte("#!/bin/sh\nsleep 1\n"), 0o700))
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()

	err := (&Dump{}).ValidateContext(ctx, "")
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "pg_dump version check canceled")
}

func TestDumpValidateContextCapsVersionOutput(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("POSIX shell test")
	}

	dir := t.TempDir()
	fakePgDump := filepath.Join(dir, "pg_dump")
	assert.NoError(t, os.WriteFile(fakePgDump, []byte("#!/bin/sh\nprintf '%*s' 70000 '' | tr ' ' x\nexit 1\n"), 0o700))
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))

	err := (&Dump{}).ValidateContext(context.Background(), "")

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "[truncated]")
	assert.LessOrEqual(t, len(err.Error()), maxDumpErrorOutputBytes+128)
}

func TestDumpExportCapsErrorOutput(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("POSIX shell test")
	}

	dir := t.TempDir()
	fakePgDump := filepath.Join(dir, "pg_dump")
	assert.NoError(t, os.WriteFile(fakePgDump, []byte("#!/bin/sh\nprintf '%*s' 70000 '' | tr ' ' x >&2\nexit 1\n"), 0o700))
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))

	err := (&Dump{}).Export(context.Background(), "postgres://localhost/db", io.Discard)

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "[truncated]")
	assert.LessOrEqual(t, len(err.Error()), maxDumpErrorOutputBytes+128)
}

func TestDumpExportRejectsOversizedTableArg(t *testing.T) {
	err := (&Dump{Table: strings.Repeat("x", maxDumpTableArgBytes+1)}).Export(
		context.Background(),
		"postgres://localhost/db",
		io.Discard,
	)

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "dump table name exceeds maximum size")
}

func TestDumpValidateOptionsRejectsOversizedTableArg(t *testing.T) {
	err := (&Dump{Table: strings.Repeat("x", maxDumpTableArgBytes+1)}).ValidateOptions()

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "dump table name exceeds maximum size")
}

func TestPrepareDumpConnStringRemovesPasswordFromCommandArg(t *testing.T) {
	connstr, password, err := prepareDumpConnString(
		"postgres://alice:s3cr3t@localhost:5432/db?sslmode=disable&search_path=private",
	)

	assert.NoError(t, err)
	assert.Equal(t, "s3cr3t", password)
	assert.Equal(t, "postgres://alice@localhost:5432/db?sslmode=disable", connstr)
	assert.NotContains(t, connstr, "s3cr3t")
}

func TestPrepareDumpConnStringRemovesPasswordQueryParam(t *testing.T) {
	connstr, password, err := prepareDumpConnString(
		"postgres://alice@localhost:5432/db?Password=s3cr3t&sslmode=disable",
	)

	assert.NoError(t, err)
	assert.Equal(t, "s3cr3t", password)
	assert.Equal(t, "postgres://alice@localhost:5432/db?sslmode=disable", connstr)
	assert.NotContains(t, connstr, "s3cr3t")
}

func TestRemoveUnsupportedOptionsIsCaseInsensitive(t *testing.T) {
	connstr, err := removeUnsupportedOptions(
		"postgres://alice@localhost:5432/db?Search_Path=private&sslmode=disable",
	)

	assert.NoError(t, err)
	assert.Equal(t, "postgres://alice@localhost:5432/db?sslmode=disable", connstr)
}

func testDumpExport(t *testing.T) {
	url := fmt.Sprintf("postgres://%s@%s:%s/%s?sslmode=disable", serverUser, serverHost, serverPort, serverDatabase)

	savePath := "/tmp/dump.sql.gz"
	os.Remove(savePath)

	saveFile, err := os.Create(savePath)
	if err != nil {
		t.Fatal(err.Error())
	}

	defer func() {
		saveFile.Close()
		os.Remove(savePath)
	}()

	dump := Dump{}

	// Test for pg_dump presence
	assert.NoError(t, dump.Validate("10.0"))
	assert.NoError(t, dump.Validate(""))
	assert.Contains(t, dump.Validate("20").Error(), "not compatible with server version 20")

	// Test full db dump
	err = dump.Export(context.Background(), url, saveFile)
	assert.NoError(t, err)

	// Test nonexistent database
	invalidURL := fmt.Sprintf("postgres://%s@%s:%s/%s?sslmode=disable", serverUser, serverHost, serverPort, "foobar")
	err = dump.Export(context.Background(), invalidURL, saveFile)
	assert.Contains(t, err.Error(), `database "foobar" does not exist`)

	// Test dump of non existent db
	dump = Dump{Table: "foobar"}
	err = dump.Export(context.Background(), url, saveFile)
	assert.NotNil(t, err)
	assert.Contains(t, err.Error(), "no matching tables were found")

	// Should drop "search_path" param from URI
	dump = Dump{}
	searchPathURL := fmt.Sprintf("postgres://%s@%s:%s/%s?sslmode=disable&search_path=private", serverUser, serverHost, serverPort, serverDatabase)
	err = dump.Export(context.Background(), searchPathURL, saveFile)
	assert.NoError(t, err)
}
