package api

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/stretchr/testify/assert"
)

func Test_securityHeadersMiddlewareSetsDefensiveHeaders(t *testing.T) {
	router := gin.New()
	router.Use(securityHeadersMiddleware())
	router.GET("/ok", func(c *gin.Context) {
		c.String(http.StatusOK, "ok")
	})

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodGet, "/ok", nil)
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusOK, w.Code)
	assert.Equal(t, contentSecurityPolicy, w.Header().Get("Content-Security-Policy"))
	assert.Contains(t, w.Header().Get("Content-Security-Policy"), "object-src 'none'")
	assert.Contains(t, w.Header().Get("Content-Security-Policy"), "base-uri 'none'")
	assert.Contains(t, w.Header().Get("Content-Security-Policy"), "form-action 'self'")
	assert.Equal(t, "same-origin", w.Header().Get("Cross-Origin-Resource-Policy"))
	assert.Equal(t, "camera=(), geolocation=(), microphone=()", w.Header().Get("Permissions-Policy"))
	assert.Equal(t, "no-referrer", w.Header().Get("Referrer-Policy"))
	assert.Equal(t, "nosniff", w.Header().Get("X-Content-Type-Options"))
	assert.Equal(t, "DENY", w.Header().Get("X-Frame-Options"))
}

func Test_requestBodyLimitMiddlewareRejectsDeclaredOversizedBody(t *testing.T) {
	router := gin.New()
	router.Use(requestBodyLimitMiddleware(8))
	router.POST("/ok", func(c *gin.Context) {
		c.String(http.StatusOK, "ok")
	})

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodPost, "/ok", strings.NewReader("012345678"))
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusRequestEntityTooLarge, w.Code)
	assert.Contains(t, w.Body.String(), "request body exceeds maximum size")
}

func Test_requestBodyLimitMiddlewareCapsUnknownLengthBody(t *testing.T) {
	router := gin.New()
	router.Use(requestBodyLimitMiddleware(8))
	router.POST("/ok", func(c *gin.Context) {
		_, err := io.ReadAll(c.Request.Body)
		if err != nil {
			c.String(http.StatusRequestEntityTooLarge, "blocked")
			return
		}
		c.String(http.StatusOK, "ok")
	})

	w := httptest.NewRecorder()
	req, _ := http.NewRequest(http.MethodPost, "/ok", strings.NewReader("012345678"))
	req.ContentLength = -1
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusRequestEntityTooLarge, w.Code)
	assert.Equal(t, "blocked", w.Body.String())
}
