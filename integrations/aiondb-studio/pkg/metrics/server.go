package metrics

import (
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/sirupsen/logrus"
)

const maxMetricsPathBytes = 1024

func StartServer(logger *logrus.Logger, path string, addr string) error {
	logger.WithField("addr", addr).WithField("path", path).Info("starting prometheus metrics server")
	if err := validateServerPath(path); err != nil {
		return err
	}

	mux := http.NewServeMux()
	mux.Handle(path, NewHandler())

	server := &http.Server{
		Addr:              addr,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      30 * time.Second,
		IdleTimeout:       60 * time.Second,
	}

	return server.ListenAndServe()
}

func validateServerPath(path string) error {
	if path == "" {
		return errors.New("metrics path must not be empty")
	}
	if len(path) > maxMetricsPathBytes {
		return fmt.Errorf("metrics path must be less than or equal to %d bytes", maxMetricsPathBytes)
	}
	if !strings.HasPrefix(path, "/") {
		return errors.New("metrics path must start with /")
	}
	if strings.ContainsAny(path, "?#") {
		return errors.New("metrics path must not contain query or fragment delimiters")
	}
	return nil
}
