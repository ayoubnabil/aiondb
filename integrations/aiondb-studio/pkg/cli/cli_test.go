package cli

import (
	"net/http"
	"testing"
	"time"

	"github.com/sosedoff/pgweb/pkg/command"
	"github.com/stretchr/testify/assert"
)

func TestNewAppHTTPServerSetsTimeouts(t *testing.T) {
	handler := http.NewServeMux()
	server := newAppHTTPServer(handler, command.Options{
		HTTPHost: "127.0.0.1",
		HTTPPort: 8081,
	})

	assert.Equal(t, "127.0.0.1:8081", server.Addr)
	assert.Same(t, handler, server.Handler)
	assert.Equal(t, 10*time.Second, server.ReadHeaderTimeout)
	assert.Equal(t, 30*time.Second, server.ReadTimeout)
	assert.Equal(t, 30*time.Second, server.WriteTimeout)
	assert.Equal(t, 60*time.Second, server.IdleTimeout)
}
