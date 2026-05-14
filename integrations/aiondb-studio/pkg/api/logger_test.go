package api

import (
	"net/http"
	"net/url"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/stretchr/testify/assert"
)

func Test_getRequestID(t *testing.T) {
	examples := []struct {
		headers map[string]string
		result  string
	}{
		{map[string]string{}, ""},
		{map[string]string{"X-Request-ID": "foo"}, "foo"},
		{map[string]string{"x-request-id": "foo"}, "foo"},
		{map[string]string{"x-request-id": "foo"}, "foo"},
		{map[string]string{"x-request-id": "foo", "x-amzn-trace-id": "amz"}, "foo"},
		{map[string]string{"x-request-id": strings.Repeat("x", maxLogValueBytes+1)}, strings.Repeat("x", maxLogValueBytes) + truncatedLogMessage},
	}

	for _, ex := range examples {
		req := &http.Request{Header: http.Header{}}
		for k, v := range ex.headers {
			req.Header.Set(k, v)
		}

		assert.Equal(t, ex.result, getRequestID(&gin.Context{Request: req}))
	}
}

func Test_sanitizeLogRawQueryRedactsSecrets(t *testing.T) {
	raw := "_session_id=session-token&format=json&url=postgres%3A%2F%2Fuser%3Apass%40host%2Fdb&query=select+1&connect-token=backend-token"

	values, err := url.ParseQuery(sanitizeLogRawQuery(raw))
	assert.NoError(t, err)
	assert.Equal(t, "[REDACTED]", values.Get("_session_id"))
	assert.Equal(t, "[REDACTED]", values.Get("url"))
	assert.Equal(t, "[REDACTED]", values.Get("connect-token"))
	assert.Equal(t, "json", values.Get("format"))
	assert.Equal(t, "select 1", values.Get("query"))
}

func Test_sanitizeLogRawQueryFailsClosedOnMalformedQuery(t *testing.T) {
	assert.Equal(t, "[REDACTED]", sanitizeLogRawQuery("url=postgres%zz"))
}

func Test_redactLogValuesRedactsSensitiveFormFieldsWithoutMutatingOriginal(t *testing.T) {
	form := url.Values{
		"url":              {"postgres://user:pass@host/db"},
		"passfile":         {"/home/user/.pgpass"},
		"ssh_password":     {"secret"},
		"ssh_key":          {"/home/user/.ssh/id_rsa"},
		"ssh_key_password": {"key-secret"},
		"where":            {"id = 1"},
	}

	redacted := redactLogValues(form)

	assert.Equal(t, "[REDACTED]", redacted.Get("url"))
	assert.Equal(t, "[REDACTED]", redacted.Get("passfile"))
	assert.Equal(t, "[REDACTED]", redacted.Get("ssh_password"))
	assert.Equal(t, "[REDACTED]", redacted.Get("ssh_key"))
	assert.Equal(t, "[REDACTED]", redacted.Get("ssh_key_password"))
	assert.Equal(t, "id = 1", redacted.Get("where"))

	assert.Equal(t, "postgres://user:pass@host/db", form.Get("url"))
	assert.Equal(t, "secret", form.Get("ssh_password"))
}

func Test_redactLogValuesTruncatesLargeNonSensitiveValues(t *testing.T) {
	form := url.Values{
		"where": {strings.Repeat("x", maxLogValueBytes+1)},
	}

	redacted := redactLogValues(form)

	assert.Len(t, redacted.Get("where"), maxLogValueBytes+len(truncatedLogMessage))
	assert.True(t, strings.HasSuffix(redacted.Get("where"), truncatedLogMessage))
	assert.Equal(t, strings.Repeat("x", maxLogValueBytes+1), form.Get("where"))
}

func Test_redactLogValuesCapsValuesPerKey(t *testing.T) {
	form := url.Values{"tag": {"1", "2", "3", "4", "5", "6", "7", "8", "9"}}

	redacted := redactLogValues(form)

	assert.Equal(t, []string{"1", "2", "3", "4", "5", "6", "7", "8", truncatedLogMessage}, redacted["tag"])
}

func Test_sanitizeLogPathRedactsConnectBackendResource(t *testing.T) {
	assert.Equal(t, "/connect/REDACTED", sanitizeLogPath("/connect/backend-secret"))
	assert.Equal(t, "/api/query", sanitizeLogPath("/api/query"))
}
