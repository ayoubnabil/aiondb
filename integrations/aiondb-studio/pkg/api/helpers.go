package api

import (
	"fmt"
	"mime"
	"net/http"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"

	"github.com/sosedoff/pgweb/pkg/client"
	"github.com/sosedoff/pgweb/pkg/history"
	"github.com/sosedoff/pgweb/pkg/shared"
)

var (
	// Mime types definitions
	extraMimeTypes = map[string]string{
		".icon": "image-x-icon",
		".ttf":  "application/x-font-ttf",
		".woff": "application/x-font-woff",
		".eot":  "application/vnd.ms-fontobject",
		".svg":  "image/svg+xml",
		".html": "text/html; charset-utf-8",
	}

	// Paths that dont require database connection
	allowedPaths = map[string]bool{
		"/api/sessions":  true,
		"/api/info":      true,
		"/api/connect":   true,
		"/api/bookmarks": true,
	}

	// List of characters replaced by javascript code to make queries url-safe.
	base64subs = map[string]string{
		"-": "+",
		"_": "/",
		".": "=",
	}

	// Regular expression to remove unwanted characters in filenames
	regexCleanFilename           = regexp.MustCompile(`[^\w]+`)
	regexCleanAttachmentFilename = regexp.MustCompile(`[^\w.\-]+`)
	historySecretPatterns        = []*regexp.Regexp{
		regexp.MustCompile(`(?i)(\bpassword(?:\s+(?:=|to)?\s*|\s*=\s*))('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
		regexp.MustCompile(`(?i)(\bidentified\s+by\s*)('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
		regexp.MustCompile(`(?i)(\b(?:secret|token|api[_-]?key)\s*=\s*)('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
	}
)

const (
	maxAttachmentFilenameBytes = 255
	maxSessionIDBytes          = 256
)

type Error struct {
	Message string `json:"error"`
}

func NewError(err error) Error {
	return Error{err.Error()}
}

// Returns a clean query without any comment statements
func cleanQuery(query string) string {
	lines := []string{}

	for _, line := range strings.Split(query, "\n") {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, "--") {
			continue
		}
		lines = append(lines, line)
	}

	return strings.TrimSpace(strings.Join(lines, "\n"))
}

func desanitize64(query string) string {
	// Before feeding the string into decoded, we must "reconstruct" the base64 data.
	// Javascript replaces a few characters to be url-safe.
	for olds, news := range base64subs {
		query = strings.Replace(query, olds, news, -1)
	}

	return query
}

func sanitizeFilename(str string) string {
	str = strings.ReplaceAll(str, ".", "_")
	return regexCleanFilename.ReplaceAllString(str, "")
}

func sanitizeAttachmentFilename(str string) string {
	str = strings.ReplaceAll(str, `/`, "_")
	str = strings.ReplaceAll(str, `\`, "_")
	str = regexCleanAttachmentFilename.ReplaceAllString(str, "_")
	str = strings.Trim(str, ". ")
	if len(str) > maxAttachmentFilenameBytes {
		str = strings.Trim(str[:maxAttachmentFilenameBytes], ". ")
	}
	return str
}

func attachmentDisposition(filename string) string {
	filename = sanitizeAttachmentFilename(filename)
	if filename == "" {
		filename = "download"
	}
	return mime.FormatMediaType("attachment", map[string]string{"filename": filename})
}

func redactHistory(records []history.Record) []history.Record {
	redacted := make([]history.Record, len(records))
	copy(redacted, records)
	for i := range redacted {
		redacted[i].Query = redactHistoryQuery(redacted[i].Query)
	}
	return redacted
}

func redactHistoryQuery(query string) string {
	for _, pattern := range historySecretPatterns {
		query = pattern.ReplaceAllString(query, "${1}[REDACTED]")
	}
	return query
}

func firstFormattedRow(result *client.Result) (map[string]interface{}, error) {
	if result == nil || len(result.Rows) == 0 {
		return nil, errInvalidResult
	}

	formatted := result.Format()
	if len(formatted) == 0 {
		return nil, errInvalidResult
	}
	return formatted[0], nil
}

func firstInt64Cell(result *client.Result) (int64, error) {
	if result == nil || len(result.Rows) == 0 || len(result.Rows[0]) == 0 {
		return 0, errInvalidResult
	}

	switch val := result.Rows[0][0].(type) {
	case int64:
		return val, nil
	case int:
		return int64(val), nil
	default:
		return 0, errInvalidResult
	}
}

func stringResultField(row map[string]interface{}, key string) (string, error) {
	value, ok := row[key].(string)
	if !ok {
		return "", errInvalidResult
	}
	return value, nil
}

func getSessionId(req *http.Request) string {
	id := req.Header.Get("x-session-id")
	if id == "" {
		id = req.URL.Query().Get("_session_id")
	}
	if id == "" {
		id = req.FormValue("_session_id")
	}
	if len(id) > maxSessionIDBytes {
		return ""
	}
	return id
}

func getQueryParam(c *gin.Context, name string) string {
	result := ""
	q := c.Request.URL.Query()

	if len(q[name]) > 0 {
		result = q[name][0]
	}
	if result == "" {
		result = c.Request.FormValue(name)
	}

	return result
}

func parseIntFormValue(c *gin.Context, name string, defValue int) (int, error) {
	val := c.Request.FormValue(name)

	if val == "" {
		return defValue, nil
	}

	num, err := strconv.Atoi(val)
	if err != nil {
		return defValue, fmt.Errorf("%s must be a number", name)
	}

	if num < 0 {
		return defValue, fmt.Errorf("%s must be greater than or equal to 0", name)
	}

	if num < 1 && defValue != 0 {
		return defValue, fmt.Errorf("%s must be greater than 0", name)
	}

	return num, nil
}

func parseTableRowsPagination(c *gin.Context) (int, int, error) {
	offset, err := parseIntFormValue(c, "offset", 0)
	if err != nil {
		return 0, 0, err
	}

	limit, err := parseIntFormValue(c, "limit", 100)
	if err != nil {
		return 0, 0, err
	}
	if limit > maxTableRowsLimit {
		return 0, 0, fmt.Errorf("limit must be less than or equal to %d", maxTableRowsLimit)
	}

	return offset, limit, nil
}

func parseSshInfo(c *gin.Context) *shared.SSHInfo {
	info := shared.SSHInfo{
		Host:        c.Request.FormValue("ssh_host"),
		Port:        c.Request.FormValue("ssh_port"),
		User:        c.Request.FormValue("ssh_user"),
		Password:    c.Request.FormValue("ssh_password"),
		Key:         c.Request.FormValue("ssh_key"),
		KeyPassword: c.Request.FormValue("ssh_key_password"),
	}

	if info.Port == "" {
		info.Port = "22"
	}

	return &info
}

func assetContentType(name string) string {
	ext := filepath.Ext(name)
	result := mime.TypeByExtension(ext)

	if result == "" {
		result = extraMimeTypes[ext]
	}

	if result == "" {
		result = "text/plain; charset=utf-8"
	}

	return result
}

// Send a query result to client
func serveResult(c *gin.Context, result interface{}, err interface{}) {
	if err != nil {
		badRequest(c, err)
		return
	}

	successResponse(c, result)
}

// Send successful response back to client
func successResponse(c *gin.Context, data interface{}) {
	c.JSON(200, data)
}

// Send an error response back to client
func errorResponse(c *gin.Context, status int, err interface{}) {
	var message interface{}

	switch v := err.(type) {
	case error:
		message = v.Error()
	case string:
		message = v
	default:
		message = v
	}

	c.AbortWithStatusJSON(status, gin.H{"status": status, "error": message})
}

// Send a bad request (http 400) back to client
func badRequest(c *gin.Context, err interface{}) {
	errorResponse(c, 400, err)
}
