package api

import (
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"

	"github.com/sosedoff/pgweb/pkg/command"
)

var (
	logger *logrus.Logger

	reConnectToken = regexp.MustCompile("/connect/(.*)")
)

const (
	maxLogValueBytes    = 4096
	maxLogValuesPerKey  = 8
	truncatedLogMessage = "[TRUNCATED]"
)

func init() {
	if logger == nil {
		logger = logrus.New()
	}
}

// TODO: Move this into server struct when it's ready
func SetLogger(l *logrus.Logger) {
	logger = l
}

func RequestLogger(logger *logrus.Logger) gin.HandlerFunc {
	debug := logger.Level > logrus.InfoLevel
	logForwardedUser := command.Opts.LogForwardedUser

	return func(c *gin.Context) {
		start := time.Now()
		path := c.Request.URL.Path

		// Process request
		c.Next()

		if !debug {
			// Skip static assets logging
			if strings.Contains(path, "/static/") {
				return
			}
		}
		path = truncateLogValue(sanitizeLogPath(path))

		status := c.Writer.Status()
		end := time.Now()
		latency := end.Sub(start)

		fields := logrus.Fields{
			"status":      status,
			"method":      c.Request.Method,
			"remote_addr": c.ClientIP(),
			"duration":    latency.String(),
			"duration_ms": latency.Milliseconds(),
			"path":        path,
		}

		if reqID := getRequestID(c); reqID != "" {
			fields["id"] = reqID
		}

		if logForwardedUser {
			if forwardedUser := c.GetHeader("X-Forwarded-User"); forwardedUser != "" {
				fields["forwarded_user"] = truncateLogValue(forwardedUser)
			}
			if forwardedEmail := c.GetHeader("X-Forwarded-Email"); forwardedEmail != "" {
				fields["forwarded_email"] = truncateLogValue(forwardedEmail)
			}
		}

		if err := c.Errors.Last(); err != nil {
			fields["error"] = truncateLogValue(err.Error())
		}

		// Additional fields for debugging
		if debug {
			fields["raw_query"] = sanitizeLogRawQuery(c.Request.URL.RawQuery)

			if c.Request.Method != http.MethodGet {
				fields["raw_form"] = redactLogValues(c.Request.Form)
			}
		}

		entry := logger.WithFields(fields)
		msg := "http_request"

		switch {
		case status >= http.StatusBadRequest && status < http.StatusInternalServerError:
			entry.Warn(msg)
		case status >= http.StatusInternalServerError:
			entry.Error(msg)
		default:
			entry.Info(msg)
		}
	}
}

func sanitizeLogPath(str string) string {
	return reConnectToken.ReplaceAllString(str, "/connect/REDACTED")
}

func sanitizeLogRawQuery(raw string) string {
	if raw == "" {
		return ""
	}

	values, err := url.ParseQuery(raw)
	if err != nil {
		return "[REDACTED]"
	}
	return redactLogValues(values).Encode()
}

func redactLogValues(values url.Values) url.Values {
	redacted := make(url.Values, len(values))
	for key, vals := range values {
		copied := make([]string, 0, min(len(vals), maxLogValuesPerKey))
		for i, value := range vals {
			if i >= maxLogValuesPerKey {
				copied = append(copied, truncatedLogMessage)
				break
			}
			if isSensitiveLogKey(key) {
				copied = append(copied, "[REDACTED]")
				continue
			}
			copied = append(copied, truncateLogValue(value))
		}
		redacted[key] = copied
	}
	return redacted
}

func truncateLogValue(value string) string {
	if len(value) <= maxLogValueBytes {
		return value
	}
	return value[:maxLogValueBytes] + truncatedLogMessage
}

func isSensitiveLogKey(key string) bool {
	normalized := strings.ToLower(strings.ReplaceAll(key, "-", "_"))
	if normalized == "_session_id" ||
		normalized == "session_id" ||
		normalized == "session" ||
		normalized == "url" ||
		normalized == "pass" ||
		normalized == "passfile" ||
		normalized == "sslkey" ||
		normalized == "key" {
		return true
	}

	return strings.Contains(normalized, "password") ||
		strings.Contains(normalized, "token") ||
		strings.Contains(normalized, "secret") ||
		strings.HasSuffix(normalized, "_key")
}

func getRequestID(c *gin.Context) string {
	id := c.GetHeader("x-request-id")
	if id == "" {
		id = c.GetHeader("x-amzn-trace-id")
	}
	return truncateLogValue(id)
}
