package client

import (
	"context"
	"errors"
	"fmt"
	"log"
	neturl "net/url"
	"reflect"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/jmoiron/sqlx"
	_ "github.com/lib/pq"

	"github.com/sosedoff/pgweb/pkg/bookmarks"
	"github.com/sosedoff/pgweb/pkg/command"
	"github.com/sosedoff/pgweb/pkg/connection"
	"github.com/sosedoff/pgweb/pkg/history"
	"github.com/sosedoff/pgweb/pkg/shared"
	"github.com/sosedoff/pgweb/pkg/statements"
)

var (
	regexErrAuthFailed        = regexp.MustCompile(`(authentication failed|role "(.*)" does not exist)`)
	regexErrConnectionRefused = regexp.MustCompile(`(connection|actively) refused`)
	regexErrDatabaseNotExist  = regexp.MustCompile(`database "(.*)" does not exist`)
	regexQueryLogSecrets      = []*regexp.Regexp{
		regexp.MustCompile(`(?i)(\bpassword(?:\s+(?:=|to)?\s*|\s*=\s*))('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
		regexp.MustCompile(`(?i)(\bidentified\s+by\s*)('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
		regexp.MustCompile(`(?i)(\b(?:secret|token|api[_-]?key)\s*=\s*)('(?:''|[^'])*'|"(?:[^"]|"")*"|[^\s;]+)`),
	}
)

var (
	ErrAuthFailed        = errors.New("authentication failed")
	ErrConnectionRefused = errors.New("connection refused")
	ErrDatabaseNotExist  = errors.New("database does not exist")
)

const (
	maxQueryResultRows       = 100000
	maxQueryResultBytes      = int64(64 * 1024 * 1024)
	maxHistoryRecords        = 100
	maxHistoryQueryBytes     = 64 * 1024
	maxConnectionStringBytes = 32 * 1024
)

type Client struct {
	mu               sync.Mutex
	db               *sqlx.DB
	tunnel           *Tunnel
	serverVersion    string
	serverType       string
	lastQueryTime    time.Time
	queryTimeout     time.Duration
	readonly         bool
	closed           bool
	External         bool             `json:"external"`
	History          []history.Record `json:"history"`
	ConnectionString string           `json:"connection_string"`
}

func getSchemaAndTable(str string) (string, string) {
	chunks := strings.Split(str, ".")
	if len(chunks) == 1 {
		return "public", chunks[0]
	}
	return chunks[0], chunks[1]
}

func quoteIdentifier(identifier string) string {
	return `"` + strings.ReplaceAll(identifier, `"`, `""`) + `"`
}

func redactQueryForLog(query string) string {
	for _, pattern := range regexQueryLogSecrets {
		query = pattern.ReplaceAllString(query, "${1}[REDACTED]")
	}
	return query
}

func validateRowsOptions(opts RowsOptions) error {
	if opts.Where != "" {
		if strings.Contains(opts.Where, ";") ||
			strings.Contains(opts.Where, "--") ||
			strings.Contains(opts.Where, "/*") ||
			strings.Contains(opts.Where, "*/") ||
			containsRestrictedKeywords(opts.Where) {
			return errors.New("unsafe table filter")
		}
	}

	if opts.SortOrder != "" {
		order := strings.ToUpper(strings.TrimSpace(opts.SortOrder))
		if order != "ASC" && order != "DESC" {
			return errors.New("invalid sort order")
		}
	}

	return nil
}

func estimateResultValueBytes(value interface{}) int64 {
	switch val := value.(type) {
	case nil:
		return 0
	case string:
		return int64(len(val))
	case []byte:
		return int64(len(val))
	case time.Time:
		return int64(len(time.RFC3339Nano))
	default:
		// Numeric and boolean values marshal to short JSON/CSV strings. Use
		// a conservative fixed cost without invoking fmt on arbitrary values.
		return 64
	}
}

func estimateResultRowBytes(row Row) int64 {
	total := int64(2) // row delimiters/overhead
	for _, item := range row {
		total += estimateResultValueBytes(item) + 1
	}
	return total
}

func resultBytesWouldExceed(current int64, row Row, limit int64) bool {
	rowBytes := estimateResultRowBytes(row)
	if rowBytes > limit {
		return true
	}
	return current > limit-rowBytes
}

func resultRowsBytesWouldExceed(rows []Row, limit int64) bool {
	var total int64
	for _, row := range rows {
		if resultBytesWouldExceed(total, row, limit) {
			return true
		}
		total += estimateResultRowBytes(row)
	}
	return false
}

func New() (*Client, error) {
	str, err := connection.BuildStringFromOptions(command.Opts)

	if command.Opts.Debug && str != "" {
		fmt.Println("Creating a new client for:", connection.RedactURL(str))
	}

	if err != nil {
		return nil, err
	}

	db, err := sqlx.Open("postgres", str)
	if err != nil {
		return nil, err
	}

	client := Client{
		db:               db,
		ConnectionString: str,
		History:          history.New(),
	}

	client.init()
	return &client, nil
}

func NewFromUrl(url string, sshInfo *shared.SSHInfo) (*Client, error) {
	if len(url) > maxConnectionStringBytes {
		return nil, fmt.Errorf("connection string exceeds maximum size of %d bytes", maxConnectionStringBytes)
	}

	var (
		tunnel *Tunnel
		err    error
	)

	if sshInfo != nil {
		if command.Opts.DisableSSH {
			return nil, fmt.Errorf("ssh connections are disabled")
		}
		if command.Opts.Debug {
			fmt.Println("Opening SSH tunnel for:", sshInfo)
		}

		tunnel, err = NewTunnel(sshInfo, url)
		if err != nil {
			if tunnel != nil {
				tunnel.Close()
			}
			return nil, err
		}

		err = tunnel.Configure()
		if err != nil {
			tunnel.Close()
			return nil, err
		}

		go tunnel.Start()

		uri, err := neturl.Parse(url)
		if err != nil {
			tunnel.Close()
			return nil, err
		}

		// Override remote postgres port with local proxy port
		url = strings.Replace(url, uri.Host, fmt.Sprintf("127.0.0.1:%v", tunnel.Port), 1)
	}

	if command.Opts.Debug {
		fmt.Println("Creating a new client for:", connection.RedactURL(url))
	}

	uri, err := neturl.Parse(url)
	if err == nil && uri.Path == "" {
		return nil, fmt.Errorf("Database name is not provided")
	}

	db, err := sqlx.Open("postgres", url)
	if err != nil {
		return nil, err
	}

	client := Client{
		db:               db,
		tunnel:           tunnel,
		serverType:       postgresType,
		ConnectionString: url,
		History:          history.New(),
	}

	client.init()
	return &client, nil
}

func NewFromBookmark(bookmark *bookmarks.Bookmark) (*Client, error) {
	var (
		connStr string
		err     error
	)

	options := bookmark.ConvertToOptions()
	if options.URL != "" {
		connStr = options.URL
	} else {
		connStr, err = connection.BuildStringFromOptions(options)
		if err != nil {
			return nil, err
		}
	}

	var sshInfo *shared.SSHInfo
	if !bookmark.SSHInfoIsEmpty() {
		sshInfo = bookmark.SSH
	}

	client, err := NewFromUrl(connStr, sshInfo)
	if err != nil {
		return nil, err
	}

	if bookmark.ReadOnly {
		client.readonly = true
	}

	return client, nil
}

func (client *Client) init() {
	if command.Opts.QueryTimeout > 0 {
		client.queryTimeout = time.Second * time.Duration(command.Opts.QueryTimeout)
	}

	client.setServerVersion()
}

func (client *Client) setServerVersion() {
	res, err := client.query("SELECT version()")
	if err != nil {
		return
	}

	version, err := serverVersionFromResult(res)
	if err != nil {
		return
	}
	match, serverType, serverVersion := detectServerTypeAndVersion(version)
	if match {
		client.serverType = serverType
		client.serverVersion = serverVersion
	}
}

func serverVersionFromResult(result *Result) (string, error) {
	if result == nil || len(result.Rows) == 0 || len(result.Rows[0]) == 0 {
		return "", errors.New("invalid server version result")
	}
	version, ok := result.Rows[0][0].(string)
	if !ok {
		return "", errors.New("invalid server version result")
	}
	return version, nil
}

func (client *Client) Test() error {
	// NOTE: This is a different timeout defined in CLI OpenTimeout
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	err := client.db.PingContext(ctx)
	if err == nil {
		return nil
	}

	errMsg := err.Error()

	if regexErrConnectionRefused.MatchString(errMsg) {
		return ErrConnectionRefused
	}
	if regexErrAuthFailed.MatchString(errMsg) {
		return ErrAuthFailed
	}
	if regexErrDatabaseNotExist.MatchString(errMsg) {
		return ErrDatabaseNotExist
	}

	return err
}

func (client *Client) TestWithTimeout(timeout time.Duration) (result error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	// Check connection status right away without waiting for the ticker to kick in.
	// We're expecting to get "connection refused" here for the most part.
	if err := client.db.PingContext(ctx); err == nil {
		return nil
	}

	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			result = client.db.PingContext(ctx)
			if result == nil {
				return
			}
		case <-ctx.Done():
			return
		}
	}
}

func (client *Client) Info() (*Result, error) {
	result, err := client.query(statements.Info)
	if err != nil {
		msg := err.Error()
		if strings.Contains(msg, "inet_") && (strings.Contains(msg, "not supported") || strings.Contains(msg, "permission denied")) {
			// Fetch client information without inet_ function calls
			result, err = client.query(statements.InfoSimple)
		}
	}
	return result, err
}

func (client *Client) Databases() ([]string, error) {
	return client.fetchRows(statements.Databases)
}

func (client *Client) Schemas() ([]string, error) {
	return client.fetchRows(statements.Schemas)
}

func (client *Client) Objects() (*Result, error) {
	return client.query(statements.Objects)
}

func (client *Client) Table(table string) (*Result, error) {
	schema, table := getSchemaAndTable(table)
	return client.query(statements.TableSchema, schema, table)
}

func (client *Client) MaterializedView(name string) (*Result, error) {
	return client.query(statements.MaterializedView, name)
}

func (client *Client) Function(id string) (*Result, error) {
	return client.query(statements.Function, id)
}

func (client *Client) TableRows(table string, opts RowsOptions) (*Result, error) {
	if err := validateRowsOptions(opts); err != nil {
		return nil, err
	}
	schema, table := getSchemaAndTable(table)
	sql := fmt.Sprintf(
		"SELECT * FROM %s.%s",
		quoteIdentifier(schema),
		quoteIdentifier(table),
	)

	if opts.Where != "" {
		sql += fmt.Sprintf(" WHERE %s", opts.Where)
	}

	if opts.SortColumn != "" {
		if opts.SortOrder == "" {
			opts.SortOrder = "ASC"
		}
		opts.SortOrder = strings.ToUpper(strings.TrimSpace(opts.SortOrder))

		sql += fmt.Sprintf(` ORDER BY %s %s`, quoteIdentifier(opts.SortColumn), opts.SortOrder)
	}

	if opts.Limit > 0 {
		sql += fmt.Sprintf(" LIMIT %d", opts.Limit)
	}

	if opts.Offset > 0 {
		sql += fmt.Sprintf(" OFFSET %d", opts.Offset)
	}

	return client.query(sql)
}

func (client *Client) EstimatedTableRowsCount(table string, opts RowsOptions) (*Result, error) {
	schema, table := getSchemaAndTable(table)
	result, err := client.query(statements.EstimatedTableRowCount, schema, table)
	if err != nil {
		return nil, err
	}
	estimatedRowsCount, err := estimatedRowsCountFromResult(result)
	if err != nil {
		return nil, err
	}
	result.Rows[0] = Row{estimatedRowsCount}

	return result, nil
}

func estimatedRowsCountFromResult(result *Result) (int64, error) {
	if result == nil || len(result.Rows) == 0 || len(result.Rows[0]) == 0 {
		return 0, errors.New("invalid estimated row count result")
	}

	switch value := result.Rows[0][0].(type) {
	case float64:
		return int64(value), nil
	case int64:
		return value, nil
	case int:
		return int64(value), nil
	default:
		return 0, errors.New("invalid estimated row count result")
	}
}

func (client *Client) TableRowsCount(table string, opts RowsOptions) (*Result, error) {
	if err := validateRowsOptions(opts); err != nil {
		return nil, err
	}
	// Return postgres estimated rows count on empty filter
	if opts.Where == "" && client.serverType == postgresType {
		res, err := client.EstimatedTableRowsCount(table, opts)
		if err != nil {
			return nil, err
		}
		n := res.Rows[0][0].(int64)
		if n >= 100000 {
			return res, nil
		}
	}

	schema, tableName := getSchemaAndTable(table)
	sql := fmt.Sprintf(
		"SELECT COUNT(1) FROM %s.%s",
		quoteIdentifier(schema),
		quoteIdentifier(tableName),
	)

	if opts.Where != "" {
		sql += fmt.Sprintf(" WHERE %s", opts.Where)
	}

	return client.query(sql)
}

func (client *Client) TableInfo(table string) (*Result, error) {
	if client.serverType == cockroachType {
		return client.query(statements.TableInfoCockroach)
	}
	schema, table := getSchemaAndTable(table)
	return client.query(statements.TableInfo, fmt.Sprintf(`"%s"."%s"`, schema, table))
}

func (client *Client) TableIndexes(table string) (*Result, error) {
	schema, table := getSchemaAndTable(table)
	res, err := client.query(statements.TableIndexes, schema, table)

	if err != nil {
		return nil, err
	}

	return res, err
}

func (client *Client) TableConstraints(table string) (*Result, error) {
	schema, table := getSchemaAndTable(table)
	res, err := client.query(statements.TableConstraints, schema, table)

	if err != nil {
		return nil, err
	}

	return res, err
}

func (client *Client) TablesStats() (*Result, error) {
	return client.query(statements.TablesStats)
}

func (client *Client) ServerSettings() (*Result, error) {
	return client.query(statements.Settings)
}

// Returns all active queriers on the server
func (client *Client) Activity() (*Result, error) {
	if client.serverType == cockroachType {
		return client.query("SHOW QUERIES")
	}

	version := getMajorMinorVersionString(client.serverVersion)
	query := statements.Activity[version]
	if query == "" {
		query = statements.Activity["default"]
	}

	return client.query(query)
}

func (client *Client) Query(query string) (*Result, error) {
	res, err := client.query(query)

	// Save history records only if query did not fail
	if err == nil {
		client.addHistoryRecord(query)
	}

	return res, err
}

func (client *Client) SetReadOnlyMode() error {
	var value string
	if err := client.db.Get(&value, "SHOW default_transaction_read_only;"); err != nil {
		return err
	}

	if value == "off" {
		_, err := client.db.Exec("SET default_transaction_read_only=on;")
		return err
	}

	return nil
}

func (client *Client) ServerVersionInfo() string {
	return fmt.Sprintf("%s %s", client.serverType, client.serverVersion)
}

func (client *Client) ServerVersion() string {
	return client.serverVersion
}

func (client *Client) context() (context.Context, context.CancelFunc) {
	if client.queryTimeout > 0 {
		return context.WithTimeout(context.Background(), client.queryTimeout)
	}
	return context.Background(), func() {}
}

func (client *Client) exec(query string, args ...interface{}) (*Result, error) {
	ctx, cancel := client.context()
	defer cancel()

	queryStart := time.Now()
	res, err := client.db.ExecContext(ctx, query, args...)
	queryFinish := time.Now()
	if err != nil {
		return nil, err
	}

	affected, err := res.RowsAffected()
	if err != nil {
		return nil, err
	}

	result := Result{
		Columns: []string{"Rows Affected"},
		Rows: []Row{
			{affected},
		},
		Stats: &ResultStats{
			ColumnsCount:    1,
			RowsCount:       1,
			QueryStartTime:  queryStart.UTC(),
			QueryFinishTime: queryFinish.UTC(),
			QueryDuration:   queryFinish.Sub(queryStart).Milliseconds(),
		},
	}

	return &result, nil
}

func (client *Client) query(query string, args ...interface{}) (*Result, error) {
	if client.db == nil {
		return nil, nil
	}

	// Update the last usage time
	defer func() {
		client.setLastQueryTime(time.Now().UTC())
	}()

	// We're going to force-set transaction mode on every query.
	// This is needed so that default mode could not be changed by user.
	if command.Opts.ReadOnly || client.readonly {
		if err := client.SetReadOnlyMode(); err != nil {
			return nil, err
		}
		if containsRestrictedKeywords(query) {
			return nil, errors.New("query contains keywords not allowed in read-only mode")
		}
	}

	action := strings.ToLower(strings.Split(query, " ")[0])
	hasReturnValues := strings.Contains(strings.ToLower(query), " returning ")

	if (action == "update" || action == "delete") && !hasReturnValues {
		return client.exec(query, args...)
	}

	ctx, cancel := client.context()
	defer cancel()

	queryStart := time.Now()
	rows, err := client.db.QueryxContext(ctx, query, args...)
	queryFinish := time.Now()
	if err != nil {
		if command.Opts.Debug {
			log.Println("Failed query:", redactQueryForLog(query), "\nArgs:", args)
		}
		return nil, err
	}
	defer rows.Close()

	cols, err := rows.Columns()
	if err != nil {
		return nil, err
	}

	// Make sure to never return null columns
	if cols == nil {
		cols = []string{}
	}

	result := Result{
		Columns: cols,
		Rows:    []Row{},
	}
	var resultBytes int64

	for rows.Next() {
		if len(result.Rows) >= maxQueryResultRows {
			return nil, fmt.Errorf("query result exceeded maximum row count (%d)", maxQueryResultRows)
		}
		obj, err := rows.SliceScan()
		if err != nil {
			return nil, err
		}

		for i, item := range obj {
			if item == nil {
				obj[i] = nil
			} else {
				t := reflect.TypeOf(item).Kind().String()

				if t == "slice" {
					obj[i] = string(item.([]byte))
				}
			}
		}

		row := Row(obj)
		if resultBytesWouldExceed(resultBytes, row, maxQueryResultBytes) {
			return nil, fmt.Errorf("query result exceeded maximum size (%d bytes)", maxQueryResultBytes)
		}
		resultBytes += estimateResultRowBytes(row)
		result.Rows = append(result.Rows, row)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	result.Stats = &ResultStats{
		ColumnsCount:    len(cols),
		RowsCount:       len(result.Rows),
		QueryStartTime:  queryStart.UTC(),
		QueryFinishTime: queryFinish.UTC(),
		QueryDuration:   queryFinish.Sub(queryStart).Milliseconds(),
	}

	result.PostProcess()
	if resultRowsBytesWouldExceed(result.Rows, maxQueryResultBytes) {
		return nil, fmt.Errorf("query result exceeded maximum size (%d bytes)", maxQueryResultBytes)
	}

	return &result, nil
}

// Close database connection
func (client *Client) Close() error {
	client.mu.Lock()
	if client.closed {
		client.mu.Unlock()
		return nil
	}
	client.closed = true
	tunnel := client.tunnel
	db := client.db
	client.tunnel = nil
	client.mu.Unlock()

	if tunnel != nil {
		tunnel.Close()
	}

	if db != nil {
		return db.Close()
	}

	return nil
}

func (c *Client) IsClosed() bool {
	c.mu.Lock()
	defer c.mu.Unlock()

	return c.closed
}

func (c *Client) LastQueryTime() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()

	return c.lastQueryTime
}

func (c *Client) setLastQueryTime(ts time.Time) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.lastQueryTime = ts
}

func (client *Client) IsIdle() bool {
	mins := int(time.Since(client.LastQueryTime()).Minutes())

	if command.Opts.ConnectionIdleTimeout > 0 {
		return mins >= command.Opts.ConnectionIdleTimeout
	}

	return false
}

// Fetch all rows as strings for a single column
func (client *Client) fetchRows(q string) ([]string, error) {
	res, err := client.query(q)

	if err != nil {
		return nil, err
	}
	return stringsFromFirstColumn(res)
}

func stringsFromFirstColumn(res *Result) ([]string, error) {
	// Init empty slice so json.Marshal will encode it to "[]" instead of "null"
	results := make([]string, 0)
	if res == nil {
		return results, nil
	}

	for _, row := range res.Rows {
		if len(row) == 0 {
			return nil, errors.New("invalid row result")
		}
		value, ok := row[0].(string)
		if !ok {
			return nil, errors.New("invalid row result")
		}
		results = append(results, value)
	}

	return results, nil
}

func (client *Client) hasHistoryRecord(query string) bool {
	client.mu.Lock()
	defer client.mu.Unlock()

	return client.hasHistoryRecordLocked(query)
}

func (client *Client) hasHistoryRecordLocked(query string) bool {
	for _, record := range client.History {
		if record.Query == query {
			return true
		}
	}

	return false
}

func (client *Client) addHistoryRecord(query string) {
	query = historyQueryForStorage(query)
	client.mu.Lock()
	defer client.mu.Unlock()

	if client.hasHistoryRecordLocked(query) {
		return
	}

	client.History = append(client.History, history.NewRecord(query))
	if len(client.History) > maxHistoryRecords {
		client.History = client.History[len(client.History)-maxHistoryRecords:]
	}
}

func (client *Client) HistoryRecords() []history.Record {
	client.mu.Lock()
	defer client.mu.Unlock()

	records := make([]history.Record, len(client.History))
	copy(records, client.History)
	return records
}

func (client *Client) HistoryCount() int {
	client.mu.Lock()
	defer client.mu.Unlock()

	return len(client.History)
}

func historyQueryForStorage(query string) string {
	if len(query) <= maxHistoryQueryBytes {
		return query
	}
	return query[:maxHistoryQueryBytes] + "\n-- truncated by pgweb history limit"
}

type ConnContext struct {
	Host     string
	User     string
	Database string
	Mode     string
}

func (c ConnContext) String() string {
	return fmt.Sprintf(
		"host=%q user=%q database=%q mode=%q",
		c.Host, c.User, c.Database, c.Mode,
	)
}

// ConnContext returns information about current database connection
func (client *Client) GetConnContext() (*ConnContext, error) {
	url, err := neturl.Parse(client.ConnectionString)
	if err != nil {
		return nil, err
	}

	ctx, cancel := context.WithTimeout(context.Background(), time.Second*10)
	defer cancel()

	connCtx := ConnContext{
		Host: url.Hostname(),
		Mode: "default",
	}

	if command.Opts.ReadOnly {
		connCtx.Mode = "readonly"
	}

	row := client.db.QueryRowContext(ctx, "SELECT current_user, current_database()")
	if err := row.Scan(&connCtx.User, &connCtx.Database); err != nil {
		return nil, err
	}

	return &connCtx, nil
}
