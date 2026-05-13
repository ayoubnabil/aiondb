package api

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/stretchr/testify/assert"

	"github.com/sosedoff/pgweb/pkg/client"
	"github.com/sosedoff/pgweb/pkg/history"
)

func Test_desanitize64(t *testing.T) {
	examples := map[string]string{
		"test":        "test",
		"test+test+":  "test-test-",
		"test/test/":  "test_test_",
		"test=test==": "test.test..",
	}

	for expected, example := range examples {
		assert.Equal(t, expected, desanitize64(example))
	}
}

func Test_cleanQuery(t *testing.T) {
	assert.Equal(t, "a\nb\nc", cleanQuery("a\nb\nc"))
	assert.Equal(t, "", cleanQuery("--something"))
	assert.Equal(t, "test", cleanQuery("--test\ntest\n   -- test\n"))
}

func Test_sanitizeFilename(t *testing.T) {
	examples := map[string]string{
		"foo":              "foo",
		"fooBar":           "fooBar",
		"foo.bar":          "foo_bar",
		`"foo"."bar"`:      "foo_bar",
		"!@#$foo.&&*(&bar": "foo_bar",
	}

	for given, expected := range examples {
		t.Run(given, func(t *testing.T) {
			assert.Equal(t, expected, sanitizeFilename(given))
		})
	}
}

func Test_sanitizeAttachmentFilename(t *testing.T) {
	examples := map[string]string{
		"export.csv":                  "export.csv",
		"../secret.csv":               "_secret.csv",
		"db/table.csv":                "db_table.csv",
		"x\r\nSet-Cookie:session=1":   "x_Set-Cookie_session_1",
		`"quoted"; filename=evil.sql`: "_quoted_filename_evil.sql",
	}

	for given, expected := range examples {
		t.Run(given, func(t *testing.T) {
			assert.Equal(t, expected, sanitizeAttachmentFilename(given))
		})
	}

	t.Run("long filename", func(t *testing.T) {
		name := sanitizeAttachmentFilename(strings.Repeat("x", maxAttachmentFilenameBytes+1) + ".csv")

		assert.Len(t, name, maxAttachmentFilenameBytes)
	})
}

func Test_attachmentDispositionSanitizesFilename(t *testing.T) {
	header := attachmentDisposition("x\r\nSet-Cookie:session=1.csv")

	assert.NotContains(t, header, "\r")
	assert.NotContains(t, header, "\n")
	assert.NotContains(t, header, "Set-Cookie:session=1.csv")
	assert.Contains(t, header, "filename=")
}

func Test_redactHistoryMasksSQLSecretsWithoutMutatingOriginal(t *testing.T) {
	records := []history.Record{
		{Query: "CREATE ROLE app PASSWORD 'secret-password';", Timestamp: "t1"},
		{Query: `ALTER USER app WITH PASSWORD = "secret-password";`, Timestamp: "t2"},
		{Query: "CREATE USER app IDENTIFIED BY plain-secret;", Timestamp: "t3"},
		{Query: "SELECT * FROM settings WHERE token = 'secret-token';", Timestamp: "t4"},
		{Query: "SHOW password_encryption;", Timestamp: "t5"},
	}

	redacted := redactHistory(records)

	assert.NotContains(t, redacted[0].Query, "secret-password")
	assert.NotContains(t, redacted[1].Query, "secret-password")
	assert.NotContains(t, redacted[2].Query, "plain-secret")
	assert.NotContains(t, redacted[3].Query, "secret-token")
	assert.Contains(t, redacted[0].Query, "PASSWORD [REDACTED]")
	assert.Equal(t, "SHOW password_encryption;", redacted[4].Query)
	assert.Contains(t, records[0].Query, "secret-password")
}

func Test_firstFormattedRowRejectsEmptyResult(t *testing.T) {
	_, err := firstFormattedRow(&client.Result{Columns: []string{"value"}})

	assert.Equal(t, errInvalidResult, err)
}

func Test_firstFormattedRowReturnsFirstRow(t *testing.T) {
	row, err := firstFormattedRow(&client.Result{
		Columns: []string{"value"},
		Rows:    []client.Row{{"ok"}},
	})

	assert.NoError(t, err)
	assert.Equal(t, map[string]interface{}{"value": "ok"}, row)
}

func Test_firstInt64CellRejectsInvalidResult(t *testing.T) {
	_, err := firstInt64Cell(&client.Result{Rows: []client.Row{{"not-int64"}}})

	assert.Equal(t, errInvalidResult, err)
}

func Test_firstInt64CellReturnsIntegerValue(t *testing.T) {
	value, err := firstInt64Cell(&client.Result{Rows: []client.Row{{int64(42)}}})

	assert.NoError(t, err)
	assert.Equal(t, int64(42), value)
}

func Test_getSessionId(t *testing.T) {
	req := &http.Request{Header: http.Header{}}
	req.Header.Add("x-session-id", "token")
	assert.Equal(t, "token", getSessionId(req))

	req = &http.Request{}
	req.URL, _ = url.Parse("http://foobar/?_session_id=token")
	assert.Equal(t, "token", getSessionId(req))

	req, _ = http.NewRequest(http.MethodPost, "http://foobar/", strings.NewReader("_session_id=form-token"))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	assert.Equal(t, "form-token", getSessionId(req))

	req = &http.Request{Header: http.Header{}}
	req.Header.Set("x-session-id", strings.Repeat("x", maxSessionIDBytes+1))
	assert.Equal(t, "", getSessionId(req))
}

func Test_getQueryParamFallsBackToFormValue(t *testing.T) {
	c, _ := gin.CreateTestContext(httptest.NewRecorder())
	c.Request, _ = http.NewRequest(http.MethodPost, "http://foobar/", strings.NewReader("format=csv"))
	c.Request.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	assert.Equal(t, "csv", getQueryParam(c, "format"))
}

func Test_parseTableRowsPaginationRejectsUnsafeBounds(t *testing.T) {
	c, _ := gin.CreateTestContext(httptest.NewRecorder())
	c.Request, _ = http.NewRequest(http.MethodGet, "http://foobar/?offset=-1&limit=100", nil)

	_, _, err := parseTableRowsPagination(c)
	assert.EqualError(t, err, "offset must be greater than or equal to 0")

	c.Request, _ = http.NewRequest(http.MethodGet, "http://foobar/?offset=0&limit=100001", nil)
	_, _, err = parseTableRowsPagination(c)
	assert.EqualError(t, err, "limit must be less than or equal to 100000")
}

func Test_serveResult(t *testing.T) {
	server := gin.Default()
	server.GET("/good", func(c *gin.Context) {
		serveResult(c, gin.H{"foo": "bar"}, nil)
	})
	server.GET("/bad", func(c *gin.Context) {
		serveResult(c, nil, errors.New("message"))
	})
	server.GET("/nodata", func(c *gin.Context) {
		serveResult(c, nil, nil)
	})

	w := httptest.NewRecorder()
	req, _ := http.NewRequest("GET", "/good", nil)
	server.ServeHTTP(w, req)
	assert.Equal(t, 200, w.Code)
	assert.Equal(t, `{"foo":"bar"}`, w.Body.String())

	w = httptest.NewRecorder()
	req, _ = http.NewRequest("GET", "/bad", nil)
	server.ServeHTTP(w, req)
	assert.Equal(t, 400, w.Code)
	assert.Equal(t, `{"error":"message","status":400}`, w.Body.String())

	w = httptest.NewRecorder()
	req, _ = http.NewRequest("GET", "/nodata", nil)
	server.ServeHTTP(w, req)
	assert.Equal(t, 200, w.Code)
	assert.Equal(t, `null`, w.Body.String())
}
