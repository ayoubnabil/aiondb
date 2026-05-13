package api

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/sosedoff/pgweb/pkg/client"
	"github.com/sosedoff/pgweb/pkg/command"
	"github.com/sosedoff/pgweb/pkg/history"
	"github.com/sosedoff/pgweb/pkg/shared"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSetupRoutesDoesNotRegisterGetForQueryExecution(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/api/query?query=DROP+TABLE+users", nil)
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusNotFound, w.Code)
}

func TestSetupRoutesDoesNotRegisterGetForDataExport(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/api/export?table=public.users", nil)
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusNotFound, w.Code)
}

func TestSetupRoutesRejectsCrossOriginQueryPost(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(
		http.MethodPost,
		"/api/query",
		strings.NewReader("query=DROP+TABLE+users"),
	)
	req.Host = "127.0.0.1:8081"
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Origin", "http://evil.example")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusForbidden, w.Code)
}

func TestSetupRoutesRejectsFetchMetadataCrossSitePost(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(
		http.MethodPost,
		"/api/query",
		strings.NewReader("query=DROP+TABLE+users"),
	)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Sec-Fetch-Site", "cross-site")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusForbidden, w.Code)
}

func TestSetupRoutesRejectsCrossSiteConnectBackendNavigation(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{ConnectBackend: "http://backend.example/connect", ConnectToken: "token"}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/connect/resource", nil)
	req.Header.Set("Sec-Fetch-Site", "cross-site")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusForbidden, w.Code)
}

func TestSetupRoutesRejectsCrossOriginConnectBackendFetch(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{ConnectBackend: "http://backend.example/connect", ConnectToken: "token"}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/connect/resource", nil)
	req.Host = "127.0.0.1:8081"
	req.Header.Set("Origin", "http://evil.example")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusForbidden, w.Code)
}

func TestConnectWithBackendUsesGeneratedSessionID(t *testing.T) {
	prevOpts := command.Opts
	prevSessions := DbSessions
	prevClient := DbClient
	prevNewClientFromURL := newClientFromURL
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbSessions = prevSessions
		DbClient = prevClient
		newClientFromURL = prevNewClientFromURL
	})

	backend := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, http.MethodPost, r.Method)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"database_url":"postgres://localhost/db"}`))
	}))
	defer backend.Close()

	command.Opts = command.Options{Sessions: true, ConnectBackend: backend.URL, ConnectToken: "token"}
	DbClient = nil
	DbSessions = NewSessionManager(logrus.New())
	newClientFromURL = func(url string, sshInfo *shared.SSHInfo) (*client.Client, error) {
		return &client.Client{}, nil
	}

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/connect/resource", nil)
	req.Header.Set("x-session-id", "attacker-controlled")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusFound, w.Code)
	location, err := url.Parse(w.Header().Get("Location"))
	require.NoError(t, err)
	sessionID := location.Query().Get("session")
	require.NotEmpty(t, sessionID)
	assert.Nil(t, DbSessions.Get("attacker-controlled"))
	assert.NotNil(t, DbSessions.Get(sessionID))
}

func TestSetupRoutesRequiresConnectionForHistory(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/api/history", nil)

	assert.NotPanics(t, func() {
		router.ServeHTTP(w, req)
	})
	assert.Equal(t, http.StatusBadRequest, w.Code)
	assert.Contains(t, w.Body.String(), "Not connected")
}

func TestSetupRoutesHandlesMissingSessionManager(t *testing.T) {
	prevOpts := command.Opts
	prevSessions := DbSessions
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbSessions = prevSessions
	})

	command.Opts = command.Options{Sessions: true}
	DbSessions = nil

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/api/sessions", nil)
	assert.NotPanics(t, func() {
		router.ServeHTTP(w, req)
	})
	assert.Equal(t, http.StatusOK, w.Code)
	assert.Contains(t, w.Body.String(), `"sessions":0`)

	w = httptest.NewRecorder()
	req, _ = http.NewRequest(http.MethodGet, "/api/history", nil)
	req.Header.Set("x-session-id", "sid")
	assert.NotPanics(t, func() {
		router.ServeHTTP(w, req)
	})
	assert.Equal(t, http.StatusBadRequest, w.Code)
	assert.Contains(t, w.Body.String(), "Not connected")
}

func TestConnectClosesClientWhenConnectionTestFails(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	prevNewClientFromURL := newClientFromURL
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
		newClientFromURL = prevNewClientFromURL
	})

	command.Opts = command.Options{}
	DbClient = nil

	var opened *client.Client
	newClientFromURL = func(url string, sshInfo *shared.SSHInfo) (*client.Client, error) {
		cl, err := client.NewFromUrl(url, sshInfo)
		opened = cl
		return cl, err
	}

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(
		http.MethodPost,
		"/api/connect",
		strings.NewReader("url=postgres://127.0.0.1:1/db?sslmode=disable"),
	)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusBadRequest, w.Code)
	require.NotNil(t, opened)
	assert.True(t, opened.IsClosed())
}

func TestDataExportRejectsOversizedTableBeforeConnectionUse(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
	})

	command.Opts = command.Options{}
	DbClient = nil

	w := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w)
	c.Request, _ = http.NewRequest(
		http.MethodPost,
		"/api/export",
		strings.NewReader("table="+strings.Repeat("x", 2048)),
	)
	c.Request.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	assert.NotPanics(t, func() {
		DataExport(c)
	})
	assert.Equal(t, http.StatusBadRequest, w.Code)
	assert.Contains(t, w.Body.String(), "dump table name exceeds maximum size")
}

func TestSwitchDbClosesNewClientWhenConnectionTestFails(t *testing.T) {
	prevOpts := command.Opts
	prevClient := DbClient
	prevNewClientFromURL := newClientFromURL
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbClient = prevClient
		newClientFromURL = prevNewClientFromURL
	})

	command.Opts = command.Options{}
	DbClient = &client.Client{ConnectionString: "postgres://127.0.0.1:5432/current?sslmode=disable"}

	var opened *client.Client
	newClientFromURL = func(url string, sshInfo *shared.SSHInfo) (*client.Client, error) {
		cl, err := client.NewFromUrl(url, sshInfo)
		opened = cl
		return cl, err
	}

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(
		http.MethodPost,
		"/api/switchdb",
		strings.NewReader("db=next"),
	)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusBadRequest, w.Code)
	require.NotNil(t, opened)
	assert.True(t, opened.IsClosed())
	assert.False(t, DbClient.IsClosed())
}

func TestGetSessionsRedactsConnectionStringsInDebug(t *testing.T) {
	prevOpts := command.Opts
	prevSessions := DbSessions
	t.Cleanup(func() {
		command.Opts = prevOpts
		DbSessions = prevSessions
	})

	command.Opts = command.Options{Debug: true, Sessions: true}
	DbSessions = NewSessionManager(logrus.New())
	DbSessions.Add("sid", &client.Client{
		ConnectionString: "postgres://user:secret@localhost:5432/db?sslkey=/home/user/key",
		History: []history.Record{
			history.NewRecord("CREATE ROLE app PASSWORD 'secret-query-password'"),
		},
	})

	router := gin.New()
	SetupRoutes(router)

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/api/sessions", nil)
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusOK, w.Code)
	assert.NotContains(t, w.Body.String(), "secret")
	assert.NotContains(t, w.Body.String(), "/home/user/key")
	assert.NotContains(t, w.Body.String(), "secret-query-password")
	assert.Contains(t, w.Body.String(), "history_count")
	assert.Contains(t, w.Body.String(), "REDACTED")
}
