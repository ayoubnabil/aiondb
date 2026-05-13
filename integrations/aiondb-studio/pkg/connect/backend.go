package connect

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/sirupsen/logrus"
)

type Backend struct {
	Endpoint    string
	Token       string
	PassHeaders []string

	logger *logrus.Logger
}

const (
	maxCredentialResponseBytes = 1024 * 1024
	maxBackendResourceBytes    = 4 * 1024
	maxBackendTokenBytes       = 4 * 1024
	maxBackendPassHeaders      = 32
	maxBackendHeaderNameBytes  = 1024
	maxBackendHeaderValueBytes = 8 * 1024
	redactedBackendResource    = "[REDACTED]"
)

var backendHTTPClient = &http.Client{
	CheckRedirect: func(req *http.Request, via []*http.Request) error {
		return http.ErrUseLastResponse
	},
}

func NewBackend(endpoint string, token string) Backend {
	return Backend{
		Endpoint: endpoint,
		Token:    token,
		logger:   logrus.StandardLogger(),
	}
}

func (be *Backend) SetLogger(logger *logrus.Logger) {
	be.logger = logger
}

func (be *Backend) SetPassHeaders(headers []string) {
	be.PassHeaders = headers
}

func (be *Backend) FetchCredential(ctx context.Context, resource string, headers http.Header) (*Credential, error) {
	be.logger.WithField("resource", redactedBackendResource).Debug("fetching database credential")
	if len(resource) > maxBackendResourceBytes ||
		len(be.Token) > maxBackendTokenBytes ||
		len(be.PassHeaders) > maxBackendPassHeaders {
		return nil, errBackendRequestTooLarge
	}

	request := Request{
		Resource: resource,
		Token:    be.Token,
		Headers:  map[string]string{},
	}

	// Pass allow-listed client headers to the backend request
	for _, name := range be.PassHeaders {
		name = strings.TrimSpace(name)
		if name == "" {
			continue
		}
		if len(name) > maxBackendHeaderNameBytes {
			return nil, errBackendRequestTooLarge
		}
		value := headers.Get(name)
		if len(value) > maxBackendHeaderValueBytes {
			return nil, errBackendRequestTooLarge
		}
		request.Headers[strings.ToLower(name)] = value
	}

	body, err := json.Marshal(request)
	if err != nil {
		be.logger.WithField("resource", redactedBackendResource).Error("backend request serialization error:", err)
		return nil, err
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, be.Endpoint, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("content-type", "application/json")

	resp, err := backendHTTPClient.Do(req)
	if err != nil {
		be.logger.WithField("resource", redactedBackendResource).Error("backend credential fetch failed:", err)
		return nil, errBackendConnectError
	}
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		err = fmt.Errorf("backend credential fetch received HTTP status code %v", resp.StatusCode)

		be.logger.
			WithField("resource", redactedBackendResource).
			WithField("status", resp.StatusCode).
			Error(err)

		return nil, err
	}

	body, err = io.ReadAll(io.LimitReader(resp.Body, maxCredentialResponseBytes+1))
	if err != nil {
		return nil, err
	}
	if len(body) > maxCredentialResponseBytes {
		return nil, errBackendResponseTooLarge
	}

	cred := &Credential{}
	if err := json.Unmarshal(body, cred); err != nil {
		return nil, err
	}

	if cred.DatabaseURL == "" {
		return nil, errConnStringRequired
	}

	return cred, nil
}
