package api

import (
	"fmt"
	"net/http"
	"net/url"
	"strings"

	"github.com/gin-gonic/gin"

	"github.com/sosedoff/pgweb/pkg/command"
)

const contentSecurityPolicy = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; worker-src 'self' blob:; frame-ancestors 'none'"

func securityHeadersMiddleware() gin.HandlerFunc {
	return func(c *gin.Context) {
		c.Header("Content-Security-Policy", contentSecurityPolicy)
		c.Header("Cross-Origin-Resource-Policy", "same-origin")
		c.Header("Permissions-Policy", "camera=(), geolocation=(), microphone=()")
		c.Header("Referrer-Policy", "no-referrer")
		c.Header("X-Content-Type-Options", "nosniff")
		c.Header("X-Frame-Options", "DENY")
		c.Next()
	}
}

func requestBodyLimitMiddleware(limit int64) gin.HandlerFunc {
	return func(c *gin.Context) {
		if isSafeMethod(c.Request.Method) {
			c.Next()
			return
		}

		if c.Request.ContentLength > limit {
			errorResponse(c, http.StatusRequestEntityTooLarge, fmt.Sprintf("request body exceeds maximum size of %d bytes", limit))
			return
		}
		if c.Request.Body != nil {
			c.Request.Body = http.MaxBytesReader(c.Writer, c.Request.Body, limit)
		}

		c.Next()
	}
}

func csrfOriginMiddleware() gin.HandlerFunc {
	return func(c *gin.Context) {
		if isSafeMethod(c.Request.Method) || command.Opts.Cors {
			c.Next()
			return
		}

		if strings.EqualFold(c.Request.Header.Get("Sec-Fetch-Site"), "cross-site") {
			errorResponse(c, http.StatusForbidden, "cross-origin request blocked")
			return
		}

		origin := c.Request.Header.Get("Origin")
		if origin == "" {
			c.Next()
			return
		}

		if !originMatchesRequestHost(c.Request, origin) {
			errorResponse(c, http.StatusForbidden, "cross-origin request blocked")
			return
		}

		c.Next()
	}
}

func isSafeMethod(method string) bool {
	switch method {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		return true
	default:
		return false
	}
}

func originMatchesRequestHost(req *http.Request, origin string) bool {
	parsed, err := url.Parse(origin)
	if err != nil || parsed.Host == "" {
		return false
	}

	host := req.Host
	if host == "" {
		host = req.URL.Host
	}

	return strings.EqualFold(parsed.Host, host)
}

// Middleware to check database connection status before running queries
func dbCheckMiddleware() gin.HandlerFunc {
	return func(c *gin.Context) {
		path := strings.Replace(c.Request.URL.Path, command.Opts.Prefix, "", -1)

		// Allow whitelisted paths
		if allowedPaths[path] {
			c.Next()
			return
		}

		// Check if session exists in single-session mode
		if !command.Opts.Sessions {
			if DbClient == nil {
				badRequest(c, errNotConnected)
				return
			}

			c.Next()
			return
		}

		// Determine session ID from the client request
		if DbSessions == nil {
			badRequest(c, errNotConnected)
			return
		}
		sid := getSessionId(c.Request)
		if sid == "" {
			badRequest(c, errSessionRequired)
			return
		}

		// Determine the database connection handle for the session
		conn := DbSessions.Get(sid)
		if conn == nil {
			badRequest(c, errNotConnected)
			return
		}

		c.Next()
	}
}

// Middleware to inject CORS headers
func corsMiddleware() gin.HandlerFunc {
	return func(c *gin.Context) {
		c.Header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		c.Header("Access-Control-Expose-Headers", "*")
		c.Header("Access-Control-Allow-Origin", command.Opts.CorsOrigin)
	}
}

func requireLocalQueries() gin.HandlerFunc {
	return func(c *gin.Context) {
		if QueryStore == nil {
			badRequest(c, "local queries are disabled")
			return
		}

		c.Next()
	}
}
