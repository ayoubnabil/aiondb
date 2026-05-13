package client

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"
)

var (
	unsupportedDumpOptions = []string{
		"search_path",
	}
)

const defaultDumpValidateTimeout = 10 * time.Second
const maxDumpErrorOutputBytes = 64 * 1024
const maxDumpTableArgBytes = 1024

// Dump represents a database dump
type Dump struct {
	Table string
}

type cappedBuffer struct {
	buf       bytes.Buffer
	limit     int
	truncated bool
}

func newCappedBuffer(limit int) *cappedBuffer {
	return &cappedBuffer{limit: limit}
}

func (b *cappedBuffer) Write(p []byte) (int, error) {
	originalLen := len(p)
	remaining := b.limit - b.buf.Len()
	if remaining <= 0 {
		b.truncated = true
		return originalLen, nil
	}
	if len(p) > remaining {
		_, _ = b.buf.Write(p[:remaining])
		b.truncated = true
		return originalLen, nil
	}
	_, _ = b.buf.Write(p)
	return originalLen, nil
}

func (b *cappedBuffer) String() string {
	out := b.buf.String()
	if b.truncated {
		out += "... [truncated]"
	}
	return out
}

// Validate checks availability and version of pg_dump CLI
func (d *Dump) Validate(serverVersion string) error {
	ctx, cancel := context.WithTimeout(context.Background(), defaultDumpValidateTimeout)
	defer cancel()
	return d.ValidateContext(ctx, serverVersion)
}

func (d *Dump) ValidateOptions() error {
	if d.Table != "" && len(d.Table) > maxDumpTableArgBytes {
		return fmt.Errorf("dump table name exceeds maximum size of %d bytes", maxDumpTableArgBytes)
	}
	return nil
}

// ValidateContext checks availability and version of pg_dump CLI.
func (d *Dump) ValidateContext(ctx context.Context, serverVersion string) error {
	out := newCappedBuffer(maxDumpErrorOutputBytes)

	cmd := exec.CommandContext(ctx, "pg_dump", "--version")
	cmd.Stdout = out
	cmd.Stderr = out

	if err := cmd.Run(); err != nil {
		if ctx.Err() != nil {
			return fmt.Errorf("pg_dump version check canceled: %w", ctx.Err())
		}
		return fmt.Errorf("pg_dump command failed: %s", out.String())
	}

	detected, dumpVersion := detectDumpVersion(out.String())
	if detected && serverVersion != "" {
		satisfied := checkVersionRequirement(dumpVersion, serverVersion)
		if !satisfied {
			return fmt.Errorf("pg_dump version %v not compatible with server version %v", dumpVersion, serverVersion)
		}
	}

	return nil
}

// Export streams the database dump to the specified writer
func (d *Dump) Export(ctx context.Context, connstr string, writer io.Writer) error {
	if err := d.ValidateOptions(); err != nil {
		return err
	}

	var password string
	if str, pass, err := prepareDumpConnString(connstr); err != nil {
		return err
	} else {
		connstr = str
		password = pass
	}

	opts := []string{
		"--no-owner",      // skip restoration of object ownership in plain-text format
		"--clean",         // clean (drop) database objects before recreating
		"--compress", "6", // compression level for compressed formats
	}

	if d.Table != "" {
		opts = append(opts, []string{"--table", d.Table}...)
	}

	opts = append(opts, connstr)
	errOutput := newCappedBuffer(maxDumpErrorOutputBytes)

	cmd := exec.CommandContext(ctx, "pg_dump", opts...)
	cmd.Stdout = writer
	cmd.Stderr = errOutput
	if password != "" {
		cmd.Env = append(os.Environ(), "PGPASSWORD="+password)
	}

	if err := cmd.Run(); err != nil {
		return fmt.Errorf("error: %s. output: %s", err.Error(), errOutput.String())
	}
	return nil
}

func prepareDumpConnString(input string) (string, string, error) {
	cleaned, err := removeUnsupportedOptions(input)
	if err != nil {
		return "", "", err
	}

	uri, err := url.Parse(cleaned)
	if err != nil {
		return "", "", err
	}

	password := ""
	if uri.User != nil {
		username := uri.User.Username()
		if pass, ok := uri.User.Password(); ok {
			password = pass
			uri.User = url.User(username)
		}
	}

	query := uri.Query()
	for key := range query {
		normalized := strings.ToLower(key)
		if normalized != "password" && normalized != "pass" {
			continue
		}
		if password == "" {
			password = query.Get(key)
		}
		query.Del(key)
	}
	uri.RawQuery = query.Encode()

	return uri.String(), password, nil
}

// removeUnsupportedOptions removes any options unsupported for making a db dump
func removeUnsupportedOptions(input string) (string, error) {
	uri, err := url.Parse(input)
	if err != nil {
		return "", err
	}

	q := uri.Query()
	for key := range q {
		for _, opt := range unsupportedDumpOptions {
			if strings.EqualFold(key, opt) {
				q.Del(key)
				break
			}
		}
	}
	uri.RawQuery = q.Encode()

	return uri.String(), nil
}
