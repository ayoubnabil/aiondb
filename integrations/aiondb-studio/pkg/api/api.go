package api

import (
	"context"
	"encoding/base64"
	"fmt"
	"net/http"
	neturl "net/url"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/tuvistavie/securerandom"

	"github.com/sosedoff/pgweb/pkg/bookmarks"
	"github.com/sosedoff/pgweb/pkg/client"
	"github.com/sosedoff/pgweb/pkg/command"
	"github.com/sosedoff/pgweb/pkg/connect"
	"github.com/sosedoff/pgweb/pkg/connection"
	"github.com/sosedoff/pgweb/pkg/metrics"
	"github.com/sosedoff/pgweb/pkg/queries"
	"github.com/sosedoff/pgweb/pkg/shared"
	"github.com/sosedoff/pgweb/static"
)

const (
	maxHTTPRequestQueryBytes = 16 * 1024 * 1024
	maxHTTPRequestBodyBytes  = 16 * 1024 * 1024
	maxTableRowsLimit        = 100000
	maxTableRowsOffset       = 10000000
)

var (
	// DbClient represents the active database connection in a single-session mode
	DbClient *client.Client

	// DbSessions represents the mapping for client connections
	DbSessions *SessionManager

	// QueryStore reads the SQL queries stores in the home directory
	QueryStore *queries.Store

	newClientFromURL      = client.NewFromUrl
	newClientFromBookmark = client.NewFromBookmark
)

// DB returns a database connection from the client context
func DB(c *gin.Context) *client.Client {
	if command.Opts.Sessions {
		if DbSessions == nil {
			return nil
		}
		return DbSessions.Get(getSessionId(c.Request))
	}
	return DbClient
}

// setClient sets the database client connection for the sessions
func setClient(c *gin.Context, newClient *client.Client) error {
	currentClient := DB(c)
	if currentClient != nil {
		currentClient.Close()
	}

	if !command.Opts.Sessions {
		DbClient = newClient
		return nil
	}

	sid := getSessionId(c.Request)
	if sid == "" {
		return errSessionRequired
	}
	if DbSessions == nil {
		return errNotConnected
	}

	DbSessions.Add(sid, newClient)
	return nil
}

// GetHome renders the home page
func GetHome(prefix string) http.Handler {
	if prefix != "" {
		prefix = "/" + prefix
	}
	return http.StripPrefix(prefix, static.GetHandler())
}

func GetAssets(prefix string) http.Handler {
	if prefix != "" {
		prefix = "/" + prefix + "static/"
	} else {
		prefix = "/static/"
	}
	return http.StripPrefix(prefix, static.GetHandler())
}

// GetSessions renders the number of active sessions
func GetSessions(c *gin.Context) {
	if DbSessions == nil {
		if command.Opts.Debug {
			successResponse(c, gin.H{})
			return
		}
		successResponse(c, gin.H{"sessions": 0})
		return
	}

	// In debug mode the endpoint returns extra session diagnostics, but
	// connection strings stay redacted to avoid leaking credentials.
	if command.Opts.Debug {
		successResponse(c, redactDebugSessions(DbSessions.Sessions()))
		return
	}
	successResponse(c, gin.H{"sessions": DbSessions.Len()})
}

func redactDebugSessions(sessions map[string]*client.Client) map[string]gin.H {
	redacted := make(map[string]gin.H, len(sessions))
	for id, conn := range sessions {
		if conn == nil {
			continue
		}
		redacted[id] = gin.H{
			"external":          conn.External,
			"history_count":     conn.HistoryCount(),
			"connection_string": connection.RedactURL(conn.ConnectionString),
		}
	}
	return redacted
}

// ConnectWithBackend creates a new connection based on backend resource
func ConnectWithBackend(c *gin.Context) {
	if strings.EqualFold(c.Request.Header.Get("Sec-Fetch-Site"), "cross-site") {
		errorResponse(c, http.StatusForbidden, "cross-origin request blocked")
		return
	}
	origin := c.Request.Header.Get("Origin")
	if origin != "" && !originMatchesRequestHost(c.Request, origin) {
		errorResponse(c, http.StatusForbidden, "cross-origin request blocked")
		return
	}

	backend := connect.NewBackend(command.Opts.ConnectBackend, command.Opts.ConnectToken)
	backend.SetLogger(logger)

	if command.Opts.ConnectHeaders != "" {
		backend.SetPassHeaders(strings.Split(command.Opts.ConnectHeaders, ","))
	}

	ctx, cancel := context.WithTimeout(c.Request.Context(), time.Second*10)
	defer cancel()

	// Fetch connection credentials
	cred, err := backend.FetchCredential(ctx, c.Param("resource"), c.Request.Header)
	if err != nil {
		badRequest(c, err)
		return
	}

	// Make the new session
	sid, err := securerandom.Uuid()
	if err != nil {
		badRequest(c, err)
		return
	}
	c.Request.Header.Set("x-session-id", sid)

	// Connect to the database
	cl, err := newClientFromURL(cred.DatabaseURL, nil)
	if err != nil {
		badRequest(c, err)
		return
	}
	cl.External = true

	// Finalize session seetup
	_, err = cl.Info()
	if err == nil {
		err = setClient(c, cl)
	}
	if err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}

	redirectURI := fmt.Sprintf("/%s?session=%s", command.Opts.Prefix, sid)
	c.Redirect(302, redirectURI)
}

// Connect creates a new client connection
func Connect(c *gin.Context) {
	if command.Opts.LockSession {
		badRequest(c, errSessionLocked)
		return
	}

	var (
		cl  *client.Client
		err error
	)

	if bookmarkID := c.Request.FormValue("bookmark_id"); bookmarkID != "" {
		cl, err = ConnectWithBookmark(bookmarkID)
	} else if command.Opts.BookmarksOnly {
		err = errNotPermitted
	} else {
		cl, err = ConnectWithURL(c)
	}
	if err != nil {
		badRequest(c, err)
		return
	}

	err = cl.Test()
	if err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}

	info, err := cl.Info()
	var formattedInfo map[string]interface{}
	if err == nil {
		formattedInfo, err = firstFormattedRow(info)
	}
	if err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}
	if err = setClient(c, cl); err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}

	successResponse(c, formattedInfo)
}

func ConnectWithURL(c *gin.Context) (*client.Client, error) {
	url := c.Request.FormValue("url")
	if url == "" {
		return nil, errURLRequired
	}

	url, err := connection.FormatURL(command.Options{
		URL:      url,
		Passfile: command.Opts.Passfile,
	})
	if err != nil {
		return nil, err
	}

	var sshInfo *shared.SSHInfo
	if c.Request.FormValue("ssh") != "" {
		sshInfo = parseSshInfo(c)
	}

	return newClientFromURL(url, sshInfo)
}

func ConnectWithBookmark(id string) (*client.Client, error) {
	manager := bookmarks.NewManager(command.Opts.BookmarksDir)

	bookmark, err := manager.Get(id)
	if err != nil {
		return nil, err
	}

	return newClientFromBookmark(bookmark)
}

// SwitchDb perform database switch for the client connection
func SwitchDb(c *gin.Context) {
	if command.Opts.LockSession {
		badRequest(c, errSessionLocked)
		return
	}

	name := c.Request.URL.Query().Get("db")
	if name == "" {
		name = c.Request.FormValue("db")
	}
	if name == "" {
		badRequest(c, errDatabaseNameRequired)
		return
	}

	conn := DB(c)
	if conn == nil {
		badRequest(c, errNotConnected)
		return
	}

	// Do not allow switching databases for connections from third-party backends
	if conn.External {
		badRequest(c, errSessionLocked)
		return
	}

	currentURL, err := neturl.Parse(conn.ConnectionString)
	if err != nil {
		badRequest(c, errInvalidConnString)
		return
	}
	currentURL.Path = name

	cl, err := newClientFromURL(currentURL.String(), nil)
	if err != nil {
		badRequest(c, err)
		return
	}

	err = cl.Test()
	if err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}

	info, err := cl.Info()
	var formattedInfo map[string]interface{}
	if err == nil {
		formattedInfo, err = firstFormattedRow(info)
	}
	if err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}
	if err = setClient(c, cl); err != nil {
		cl.Close()
		badRequest(c, err)
		return
	}

	conn.Close()

	successResponse(c, formattedInfo)
}

// Disconnect closes the current database connection
func Disconnect(c *gin.Context) {
	if command.Opts.LockSession {
		badRequest(c, errSessionLocked)
		return
	}

	if command.Opts.Sessions {
		if DbSessions == nil {
			successResponse(c, gin.H{"success": false})
			return
		}
		result := DbSessions.Remove(getSessionId(c.Request))
		successResponse(c, gin.H{"success": result})
		return
	}

	conn := DB(c)
	if conn == nil {
		badRequest(c, errNotConnected)
		return
	}

	err := conn.Close()
	if err != nil {
		badRequest(c, err)
		return
	}

	DbClient = nil
	successResponse(c, gin.H{"success": true})
}

// RunQuery executes the query
func RunQuery(c *gin.Context) {
	query := cleanQuery(c.Request.FormValue("query"))

	if query == "" {
		badRequest(c, errQueryRequired)
		return
	}

	HandleQuery(query, c)
}

// ExplainQuery renders query explain plan
func ExplainQuery(c *gin.Context) {
	query := cleanQuery(c.Request.FormValue("query"))

	if query == "" {
		badRequest(c, errQueryRequired)
		return
	}

	HandleQuery(fmt.Sprintf("EXPLAIN %s", query), c)
}

// AnalyzeQuery renders query explain plan and analyze profile
func AnalyzeQuery(c *gin.Context) {
	query := cleanQuery(c.Request.FormValue("query"))

	if query == "" {
		badRequest(c, errQueryRequired)
		return
	}

	HandleQuery(fmt.Sprintf("EXPLAIN ANALYZE %s", query), c)
}

// GetDatabases renders a list of all databases on the server
func GetDatabases(c *gin.Context) {
	if command.Opts.LockSession {
		serveResult(c, []string{}, nil)
		return
	}
	conn := DB(c)
	if conn.External {
		errorResponse(c, 403, errNotPermitted)
		return
	}

	names, err := DB(c).Databases()
	serveResult(c, names, err)
}

// GetObjects renders a list of database objects
func GetObjects(c *gin.Context) {
	result, err := DB(c).Objects()
	if err != nil {
		badRequest(c, err)
		return
	}
	successResponse(c, client.ObjectsFromResult(result))
}

// GetSchemas renders list of available schemas
func GetSchemas(c *gin.Context) {
	res, err := DB(c).Schemas()
	serveResult(c, res, err)
}

// GetTable renders table information
func GetTable(c *gin.Context) {
	var (
		res *client.Result
		err error
	)

	db := DB(c)
	tableName := c.Params.ByName("table")

	switch c.Request.FormValue("type") {
	case client.ObjTypeMaterializedView:
		res, err = db.MaterializedView(tableName)
	case client.ObjTypeFunction:
		res, err = db.Function(tableName)
	default:
		res, err = db.Table(tableName)
	}

	serveResult(c, res, err)
}

// GetTableRows renders table rows
func GetTableRows(c *gin.Context) {
	offset, limit, err := parseTableRowsPagination(c)
	if err != nil {
		badRequest(c, err)
		return
	}

	opts := client.RowsOptions{
		Limit:      limit,
		Offset:     offset,
		SortColumn: c.Request.FormValue("sort_column"),
		SortOrder:  c.Request.FormValue("sort_order"),
		Where:      c.Request.FormValue("where"),
	}

	res, err := DB(c).TableRows(c.Params.ByName("table"), opts)
	if err != nil {
		badRequest(c, err)
		return
	}

	countRes, err := DB(c).TableRowsCount(c.Params.ByName("table"), opts)
	if err != nil {
		badRequest(c, err)
		return
	}

	numFetch := int64(opts.Limit)
	numOffset := int64(opts.Offset)
	numRows, err := firstInt64Cell(countRes)
	if err != nil {
		badRequest(c, err)
		return
	}
	numPages := numRows / numFetch

	if numPages*numFetch < numRows {
		numPages++
	}

	res.Pagination = &client.Pagination{
		Rows:    numRows,
		Page:    (numOffset / numFetch) + 1,
		Pages:   numPages,
		PerPage: numFetch,
	}

	serveResult(c, res, err)
}

// GetTableInfo renders a selected table information
func GetTableInfo(c *gin.Context) {
	res, err := DB(c).TableInfo(c.Params.ByName("table"))
	if err != nil {
		badRequest(c, err)
		return
	}

	formattedInfo, err := firstFormattedRow(res)
	if err != nil {
		badRequest(c, err)
		return
	}
	successResponse(c, formattedInfo)
}

// GetHistory renders a list of recent queries
func GetHistory(c *gin.Context) {
	successResponse(c, redactHistory(DB(c).HistoryRecords()))
}

// GetConnectionInfo renders information about current connection
func GetConnectionInfo(c *gin.Context) {
	conn := DB(c)

	if err := conn.TestWithTimeout(5 * time.Second); err != nil {
		badRequest(c, err)
		return
	}

	res, err := conn.Info()
	if err != nil {
		badRequest(c, err)
		return
	}

	info, err := firstFormattedRow(res)
	if err != nil {
		badRequest(c, err)
		return
	}
	info["session_lock"] = command.Opts.LockSession

	successResponse(c, info)
}

// GetServerSettings renders a list of all server settings
func GetServerSettings(c *gin.Context) {
	res, err := DB(c).ServerSettings()
	serveResult(c, res, err)
}

// GetActivity renders a list of running queries
func GetActivity(c *gin.Context) {
	res, err := DB(c).Activity()
	serveResult(c, res, err)
}

// GetTableIndexes renders a list of database table indexes
func GetTableIndexes(c *gin.Context) {
	res, err := DB(c).TableIndexes(c.Params.ByName("table"))
	serveResult(c, res, err)
}

// GetTableConstraints renders a list of database constraints
func GetTableConstraints(c *gin.Context) {
	res, err := DB(c).TableConstraints(c.Params.ByName("table"))
	serveResult(c, res, err)
}

// GetTablesStats renders data sizes and estimated rows for all tables in the database
func GetTablesStats(c *gin.Context) {
	db := DB(c)

	connCtx, err := db.GetConnContext()
	if err != nil {
		badRequest(c, err)
		return
	}

	res, err := db.TablesStats()
	if err != nil {
		badRequest(c, err)
		return
	}

	format := getQueryParam(c, "format")
	if format == "" {
		format = "json"
	}

	// Save as attachment if exporting parameter is set
	if getQueryParam(c, "export") == "true" {
		ts := time.Now().Format(time.DateOnly)

		filename := fmt.Sprintf("pgweb-dbstats-%s-%s.%s", connCtx.Database, ts, format)
		c.Writer.Header().Set("Content-Disposition", attachmentDisposition(filename))
	}

	switch format {
	case "json":
		c.JSON(http.StatusOK, res)
	case "csv":
		c.Data(http.StatusOK, "text/csv", res.CSV())
	case "xml":
		c.XML(200, res)
	default:
		badRequest(c, "invalid format")
	}
}

// HandleQuery runs the database query
func HandleQuery(query string, c *gin.Context) {
	metrics.IncrementQueriesCount()

	if len(query) > maxHTTPRequestQueryBytes {
		badRequest(c, fmt.Errorf("query exceeds maximum size of %d bytes", maxHTTPRequestQueryBytes))
		return
	}
	rawQuery, err := base64.StdEncoding.DecodeString(desanitize64(query))
	if err == nil {
		if len(rawQuery) > maxHTTPRequestQueryBytes {
			badRequest(c, fmt.Errorf("query exceeds maximum size of %d bytes", maxHTTPRequestQueryBytes))
			return
		}
		query = string(rawQuery)
	}

	result, err := DB(c).Query(query)
	if err != nil {
		badRequest(c, err)
		return
	}

	format := getQueryParam(c, "format")
	filename := getQueryParam(c, "filename")

	if filename == "" {
		filename = fmt.Sprintf("pgweb-%v.%v", time.Now().Unix(), format)
	} else {
		filename = sanitizeAttachmentFilename(filename)
		if filename == "" {
			filename = fmt.Sprintf("pgweb-%v.%v", time.Now().Unix(), format)
		}
	}

	if format != "" {
		c.Writer.Header().Set("Content-Disposition", attachmentDisposition(filename))
	}

	switch format {
	case "csv":
		c.Data(200, "text/csv", result.CSV())
	case "json":
		c.Data(200, "application/json", result.JSON())
	case "xml":
		c.XML(200, result)
	default:
		c.JSON(200, result)
	}
}

// GetBookmarks renders the list of available bookmarks
func GetBookmarks(c *gin.Context) {
	manager := bookmarks.NewManager(command.Opts.BookmarksDir)
	ids, err := manager.ListIDs()
	serveResult(c, ids, err)
}

// GetInfo renders the pgweb system information
func GetInfo(c *gin.Context) {
	successResponse(c, gin.H{
		"app": command.Info,
		"features": gin.H{
			"session_lock":   command.Opts.LockSession,
			"query_timeout":  command.Opts.QueryTimeout,
			"local_queries":  QueryStore != nil,
			"bookmarks_only": command.Opts.BookmarksOnly,
		},
	})
}

// DataExport performs database table export
func DataExport(c *gin.Context) {
	db := DB(c)

	dump := client.Dump{
		Table: strings.TrimSpace(c.Request.FormValue("table")),
	}
	if err := dump.ValidateOptions(); err != nil {
		badRequest(c, err)
		return
	}

	info, err := db.Info()
	if err != nil {
		badRequest(c, err)
		return
	}

	exportCtx := c.Request.Context()
	if command.Opts.QueryTimeout > 0 {
		var cancel context.CancelFunc
		exportCtx, cancel = context.WithTimeout(exportCtx, time.Duration(command.Opts.QueryTimeout)*time.Second)
		defer cancel()
	}
	validateCtx, validateCancel := context.WithTimeout(exportCtx, 10*time.Second)
	defer validateCancel()

	// Perform validation of pg_dump command availability and compatibility.
	// Must be done before the actual command is executed to display errors.
	if err := dump.ValidateContext(validateCtx, db.ServerVersion()); err != nil {
		badRequest(c, err)
		return
	}

	formattedInfo, err := firstFormattedRow(info)
	if err != nil {
		badRequest(c, err)
		return
	}

	filename, err := stringResultField(formattedInfo, "current_database")
	if err != nil {
		badRequest(c, err)
		return
	}
	if dump.Table != "" {
		filename = filename + "_" + dump.Table
	}

	filename = sanitizeFilename(filename)
	filename = fmt.Sprintf("%s_%s", filename, time.Now().Format("20060102_150405"))

	c.Header(
		"Content-Disposition",
		attachmentDisposition(fmt.Sprintf("%s.sql.gz", filename)),
	)

	err = dump.Export(exportCtx, db.ConnectionString, c.Writer)
	if err != nil {
		logger.WithError(err).Error("pg_dump command failed")
		badRequest(c, err)
	}
}

// GetFunction renders function information
func GetFunction(c *gin.Context) {
	res, err := DB(c).Function(c.Param("id"))
	serveResult(c, res, err)
}

func GetLocalQueries(c *gin.Context) {
	connCtx, err := DB(c).GetConnContext()
	if err != nil {
		badRequest(c, err)
		return
	}

	storeQueries, err := QueryStore.ReadAll()
	if err != nil {
		badRequest(c, err)
		return
	}

	queries := []localQuery{}
	for _, q := range storeQueries {
		if !q.IsPermitted(connCtx.Host, connCtx.User, connCtx.Database, connCtx.Mode) {
			continue
		}

		queries = append(queries, localQuery{
			ID:          q.ID,
			Title:       q.Meta.Title,
			Description: q.Meta.Description,
			Query:       cleanQuery(q.Data),
		})
	}

	successResponse(c, queries)
}

func RunLocalQuery(c *gin.Context) {
	query, err := QueryStore.Read(c.Param("id"))
	if err != nil {
		if err == queries.ErrQueryFileNotExist {
			query = nil
		} else {
			badRequest(c, err)
			return
		}
	}
	if query == nil {
		errorResponse(c, 404, "query not found")
		return
	}

	connCtx, err := DB(c).GetConnContext()
	if err != nil {
		badRequest(c, err)
		return
	}

	if !query.IsPermitted(connCtx.Host, connCtx.User, connCtx.Database, connCtx.Mode) {
		errorResponse(c, 404, "query not found")
		return
	}

	if c.Request.Method == http.MethodGet {
		successResponse(c, localQuery{
			ID:          query.ID,
			Title:       query.Meta.Title,
			Description: query.Meta.Description,
			Query:       query.Data,
		})
		return
	}

	statement := cleanQuery(query.Data)
	if statement == "" {
		badRequest(c, errQueryRequired)
		return
	}

	HandleQuery(statement, c)
}
