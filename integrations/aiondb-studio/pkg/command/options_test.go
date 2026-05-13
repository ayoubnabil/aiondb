package command

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/mitchellh/go-homedir"
	"github.com/stretchr/testify/assert"
)

func TestParseOptions(t *testing.T) {
	var hdir string
	if d, err := homedir.Dir(); err == nil {
		hdir = d
	}

	t.Run("defaults", func(t *testing.T) {
		opts, err := ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, false, opts.Sessions)
		assert.Equal(t, "", opts.Prefix)
		assert.Equal(t, "", opts.ConnectToken)
		assert.Equal(t, "", opts.ConnectHeaders)
		assert.Equal(t, false, opts.DisableSSH)
		assert.Equal(t, false, opts.DisablePrettyJSON)
		assert.Equal(t, false, opts.DisableConnectionIdleTimeout)
		assert.Equal(t, 180, opts.ConnectionIdleTimeout)
		assert.Equal(t, false, opts.Cors)
		assert.Equal(t, "*", opts.CorsOrigin)
		assert.Equal(t, "", opts.Passfile)
		assert.Equal(t, filepath.Join(hdir, ".pgweb/bookmarks"), opts.BookmarksDir)
	})

	t.Run("sessions", func(t *testing.T) {
		opts, err := ParseOptions([]string{"--sessions", "1"})
		assert.NoError(t, err)
		assert.Equal(t, true, opts.Sessions)
	})

	t.Run("url prefix", func(t *testing.T) {
		opts, err := ParseOptions([]string{"--prefix", "pgweb"})
		assert.NoError(t, err)
		assert.Equal(t, "pgweb/", opts.Prefix)

		opts, err = ParseOptions([]string{"--prefix", "pgweb/"})
		assert.NoError(t, err)
		assert.Equal(t, "pgweb/", opts.Prefix)

		opts, err = ParseOptions([]string{"--prefix", "/pgweb"})
		assert.NoError(t, err)
		assert.Equal(t, "pgweb/", opts.Prefix)

		opts, err = ParseOptions([]string{"--prefix", "//evil.example"})
		assert.NoError(t, err)
		assert.Equal(t, "evil.example/", opts.Prefix)
	})

	t.Run("url prefix rejects unsafe values", func(t *testing.T) {
		_, err := ParseOptions([]string{"--prefix", "/bad?next=//evil.example"})
		assert.EqualError(t, err, "--prefix must not contain query, fragment, wildcard, or backslash characters")

		_, err = ParseOptions([]string{"--prefix", "bad\nprefix"})
		assert.EqualError(t, err, "--prefix must not contain control or whitespace characters")

		_, err = ParseOptions([]string{"--prefix", strings.Repeat("p", maxURLPrefixBytes+1)})
		assert.EqualError(t, err, "--prefix must be less than or equal to 256 bytes")
	})

	t.Run("connect backend", func(t *testing.T) {
		_, err := ParseOptions([]string{"--connect-backend", "test"})
		assert.EqualError(t, err, "--sessions flag must be set")

		_, err = ParseOptions([]string{"--connect-backend", "test", "--sessions"})
		assert.EqualError(t, err, "--connect-token flag must be set")

		_, err = ParseOptions([]string{"--connect-backend", "test", "--sessions", "--connect-token", "token"})
		assert.NoError(t, err)
	})

	t.Run("basic auth requires complete credentials", func(t *testing.T) {
		t.Setenv("AIONDB_STUDIO_AUTH_USER", "")
		t.Setenv("AIONDB_STUDIO_AUTH_PASS", "")
		t.Setenv("PGWEB_AUTH_USER", "")
		t.Setenv("PGWEB_AUTH_PASS", "")
		t.Setenv("AUTH_USER", "")
		t.Setenv("AUTH_PASS", "")

		_, err := ParseOptions([]string{"--auth-user", "admin"})
		assert.EqualError(t, err, "--auth-user and --auth-pass must be set together")

		_, err = ParseOptions([]string{"--auth-pass", "secret"})
		assert.EqualError(t, err, "--auth-user and --auth-pass must be set together")

		opts, err := ParseOptions([]string{"--auth-user", "admin", "--auth-pass", "secret"})
		assert.NoError(t, err)
		assert.Equal(t, "admin", opts.AuthUser)
		assert.Equal(t, "secret", opts.AuthPass)
	})

	t.Run("passfile", func(t *testing.T) {
		// File does not exist
		t.Setenv("PGPASSFILE", "/tmp/foo")
		opts, err := ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, "", opts.Passfile)

		// File exists and valid
		t.Setenv("PGPASSFILE", "../../data/passfile")
		opts, err = ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, "../../data/passfile", opts.Passfile)

		// Set via flag
		t.Setenv("PGPASSFILE", "")
		opts, err = ParseOptions([]string{"--passfile", "../../data/passfile"})
		assert.NoError(t, err)
		assert.Equal(t, "../../data/passfile", opts.Passfile)
	})

	t.Run("oversized passfile from env is ignored", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "pgpass")
		file, err := os.Create(path)
		assert.NoError(t, err)
		assert.NoError(t, file.Truncate(maxPassfileBytes+1))
		assert.NoError(t, file.Close())
		t.Setenv("PGPASSFILE", path)

		opts, err := ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, "", opts.Passfile)
	})

	t.Run("non-regular passfile from env is ignored", func(t *testing.T) {
		path := t.TempDir()
		t.Setenv("PGPASSFILE", path)

		opts, err := ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, "", opts.Passfile)
	})

	t.Run("duration options reject values that overflow time.Duration", func(t *testing.T) {
		_, err := ParseOptions([]string{"--open-timeout", "-1"})
		assert.EqualError(t, err, "--open-timeout must be greater than or equal to 0")

		_, err = ParseOptions([]string{"--open-timeout", "9223372037"})
		assert.EqualError(t, err, "--open-timeout must be less than or equal to 9223372036")

		_, err = ParseOptions([]string{"--query-timeout", "9223372037"})
		assert.EqualError(t, err, "--query-timeout must be less than or equal to 9223372036")

		_, err = ParseOptions([]string{"--open-retry-delay", "9223372037"})
		assert.EqualError(t, err, "--open-retry-delay must be less than or equal to 9223372036")

		_, err = ParseOptions([]string{"--idle-timeout", "-1"})
		assert.EqualError(t, err, "--idle-timeout must be greater than or equal to 0")

		_, err = ParseOptions([]string{"--idle-timeout", "153722868"})
		assert.EqualError(t, err, "--idle-timeout must be less than or equal to 153722867")
	})

	t.Run("network options reject invalid ports", func(t *testing.T) {
		_, err := ParseOptions([]string{"--port", "0"})
		assert.EqualError(t, err, "--port must be between 1 and 65535")

		_, err = ParseOptions([]string{"--port", "65536"})
		assert.EqualError(t, err, "--port must be between 1 and 65535")

		_, err = ParseOptions([]string{"--listen", "65536"})
		assert.EqualError(t, err, "--listen must be less than or equal to 65535")
	})

	t.Run("metrics path rejects invalid mux paths", func(t *testing.T) {
		_, err := ParseOptions([]string{"--metrics", "--metrics-path", ""})
		assert.EqualError(t, err, "--metrics-path must not be empty")

		_, err = ParseOptions([]string{"--metrics", "--metrics-path", "metrics"})
		assert.EqualError(t, err, "--metrics-path must start with /")

		_, err = ParseOptions([]string{"--metrics", "--metrics-path", "/metrics?token=secret"})
		assert.EqualError(t, err, "--metrics-path must not contain query or fragment delimiters")

		_, err = ParseOptions([]string{"--metrics", "--metrics-path", "/" + strings.Repeat("m", maxMetricsPathBytes+1)})
		assert.EqualError(t, err, "--metrics-path must be less than or equal to 1024 bytes")

		opts, err := ParseOptions([]string{"--metrics", "--metrics-path", "/prometheus"})
		assert.NoError(t, err)
		assert.Equal(t, "/prometheus", opts.MetricsPath)
	})

	t.Run("bookmarks dir from env var", func(t *testing.T) {
		os.Setenv("PGWEB_BOOKMARKS_DIR", "/tmp/my-bookmarks")
		defer os.Unsetenv("PGWEB_BOOKMARKS_DIR")

		opts, err := ParseOptions([]string{})
		assert.NoError(t, err)
		assert.Equal(t, "/tmp/my-bookmarks", opts.BookmarksDir)
	})

	t.Run("bookmarks dir flag takes precedence over env var", func(t *testing.T) {
		os.Setenv("PGWEB_BOOKMARKS_DIR", "/tmp/my-bookmarks")
		defer os.Unsetenv("PGWEB_BOOKMARKS_DIR")

		flagDir := t.TempDir()

		opts, err := ParseOptions([]string{"--bookmarks-dir", flagDir})
		assert.NoError(t, err)
		assert.Equal(t, flagDir, opts.BookmarksDir)
	})

	t.Run("bookmarks only mode", func(t *testing.T) {
		_, err := ParseOptions([]string{"--bookmarks-only"})
		assert.NoError(t, err)

		_, err = ParseOptions([]string{"--bookmarks-only", "--url", "test"})
		assert.EqualError(t, err, "--url not supported in bookmarks-only mode")

		_, err = ParseOptions([]string{"--bookmarks-only", "--host", "test", "--port", "5432"})
		assert.EqualError(t, err, "--host not supported in bookmarks-only mode")

		_, err = ParseOptions([]string{"--bookmarks-only", "--connect-backend", "test", "--sessions", "--connect-token", "token", "--url", "127.0.0.2"})
		assert.EqualError(t, err, "--connect-backend not supported in bookmarks-only mode")
	})
}
