package connect

import (
	"context"
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
)

func TestBackendFetchCredential(t *testing.T) {
	examples := []struct {
		name         string
		backend      Backend
		resourceName string
		cred         *Credential
		headers      http.Header
		ctx          func() (context.Context, context.CancelFunc)
		err          error
	}{
		{
			name:    "Bad auth token",
			backend: Backend{Endpoint: "http://localhost:5555/unauthorized"},
			err:     errors.New("backend credential fetch received HTTP status code 401"),
		},
		{
			name:    "Backend timeout",
			backend: Backend{Endpoint: "http://localhost:5555/timeout"},
			ctx: func() (context.Context, context.CancelFunc) {
				return context.WithTimeout(context.Background(), time.Millisecond*100)
			},
			err: errors.New("unable to connect to the auth backend"),
		},
		{
			name:    "Empty response",
			backend: Backend{Endpoint: "http://localhost:5555/empty-response"},
			err:     errors.New("connection string is required"),
		},
		{
			name:    "Oversized response",
			backend: Backend{Endpoint: "http://localhost:5555/oversized-response"},
			err:     errors.New("backend credential response exceeds maximum size"),
		},
		{
			name:    "Missing header",
			backend: Backend{Endpoint: "http://localhost:5555/pass-header"},
			err:     errors.New("backend credential fetch received HTTP status code 400"),
		},
		{
			name: "Require header",
			backend: Backend{
				Endpoint:    "http://localhost:5555/pass-header",
				PassHeaders: []string{"x-foo"},
			},
			headers: http.Header{
				"X-Foo": []string{"bar"},
			},
			cred: &Credential{DatabaseURL: "postgres://hostname/bar"},
		},
		{
			name:    "Success",
			backend: Backend{Endpoint: "http://localhost:5555/success"},
			cred:    &Credential{DatabaseURL: "postgres://hostname/dbname"},
		},
	}

	srvCtx, srvCancel := context.WithTimeout(context.Background(), time.Minute)
	defer srvCancel()

	startTestBackend(srvCtx, "localhost:5555")

	for _, ex := range examples {
		ex.backend.logger = logrus.StandardLogger()

		t.Run(ex.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			if ex.ctx != nil {
				ctx, cancel = ex.ctx()
			}
			defer cancel()

			cred, err := ex.backend.FetchCredential(ctx, ex.resourceName, ex.headers)
			assert.Equal(t, ex.err, err)
			assert.Equal(t, ex.cred, cred)
		})
	}
}

func TestBackendFetchCredentialDoesNotFollowRedirects(t *testing.T) {
	redirected := false
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		redirected = true
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"database_url":"postgres://redirected/db"}`))
	}))
	defer target.Close()

	source := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL, http.StatusTemporaryRedirect)
	}))
	defer source.Close()

	backend := NewBackend(source.URL, "backend-token")
	cred, err := backend.FetchCredential(context.Background(), "resource", http.Header{})

	assert.Nil(t, cred)
	if assert.Error(t, err) {
		assert.Contains(t, err.Error(), "307")
	}
	assert.False(t, redirected)
}

func TestBackendFetchCredentialRejectsOversizedRequestFields(t *testing.T) {
	t.Run("resource", func(t *testing.T) {
		backend := NewBackend("http://127.0.0.1:1", "token")
		cred, err := backend.FetchCredential(context.Background(), strings.Repeat("x", maxBackendResourceBytes+1), http.Header{})

		assert.Nil(t, cred)
		assert.Equal(t, errBackendRequestTooLarge, err)
	})

	t.Run("token", func(t *testing.T) {
		backend := NewBackend("http://127.0.0.1:1", strings.Repeat("x", maxBackendTokenBytes+1))
		cred, err := backend.FetchCredential(context.Background(), "resource", http.Header{})

		assert.Nil(t, cred)
		assert.Equal(t, errBackendRequestTooLarge, err)
	})

	t.Run("header value", func(t *testing.T) {
		backend := NewBackend("http://127.0.0.1:1", "token")
		backend.PassHeaders = []string{"x-token"}

		cred, err := backend.FetchCredential(context.Background(), "resource", http.Header{
			"X-Token": []string{strings.Repeat("x", maxBackendHeaderValueBytes+1)},
		})

		assert.Nil(t, cred)
		assert.Equal(t, errBackendRequestTooLarge, err)
	})

	t.Run("header name", func(t *testing.T) {
		backend := NewBackend("http://127.0.0.1:1", "token")
		backend.PassHeaders = []string{strings.Repeat("x", maxBackendHeaderNameBytes+1)}

		cred, err := backend.FetchCredential(context.Background(), "resource", http.Header{})

		assert.Nil(t, cred)
		assert.Equal(t, errBackendRequestTooLarge, err)
	})
}

func startTestBackend(ctx context.Context, listenAddr string) {
	router := gin.New()

	router.Use(func(c *gin.Context) {
		if c.GetHeader("content-type") != "application/json" {
			c.AbortWithStatus(http.StatusBadRequest)
		}
	})

	router.POST("/unauthorized", func(c *gin.Context) {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "Unauthorized"})
	})

	router.POST("/timeout", func(c *gin.Context) {
		time.Sleep(time.Second)
		c.JSON(http.StatusOK, gin.H{})
	})

	router.POST("/empty-response", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{})
	})

	router.POST("/oversized-response", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"database_url": strings.Repeat("x", 2*1024*1024),
		})
	})

	router.POST("/pass-header", func(c *gin.Context) {
		req := Request{}
		if err := c.BindJSON(&req); err != nil {
			panic(err)
		}

		header := req.Headers["x-foo"]
		if header == "" {
			c.AbortWithStatus(http.StatusBadRequest)
			return
		}

		c.JSON(http.StatusOK, gin.H{
			"database_url": "postgres://hostname/" + header,
		})
	})

	router.POST("/success", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"database_url": "postgres://hostname/dbname",
		})
	})

	server := &http.Server{Addr: listenAddr, Handler: router}
	mustStartServer(server)

	go func() {
		<-ctx.Done()
		if err := server.Shutdown(context.Background()); err != nil && err != http.ErrServerClosed {
			panic(err)
		}
	}()
}

func mustStartServer(server *http.Server) {
	go func() {
		err := server.ListenAndServe()
		if err != nil && err != http.ErrServerClosed {
			panic(err)
		}
	}()

	if err := waitForServer(server.Addr, 5); err != nil {
		panic(err)
	}
}

func waitForServer(addr string, n int) error {
	var lastErr error

	for i := 0; i < n; i++ {
		conn, err := net.Dial("tcp", addr)
		if err == nil {
			conn.Close()
			return nil
		}

		lastErr = err
		time.Sleep(time.Millisecond * 100)
	}

	return lastErr
}
