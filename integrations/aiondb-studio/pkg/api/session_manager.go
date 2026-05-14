package api

import (
	"sync"
	"time"

	"github.com/sirupsen/logrus"

	"github.com/sosedoff/pgweb/pkg/client"
	"github.com/sosedoff/pgweb/pkg/metrics"
)

type SessionManager struct {
	logger      *logrus.Logger
	sessions    map[string]*client.Client
	mu          sync.Mutex
	idleTimeout time.Duration
}

func NewSessionManager(logger *logrus.Logger) *SessionManager {
	if logger == nil {
		logger = logrus.New()
	}
	return &SessionManager{
		logger:   logger,
		sessions: map[string]*client.Client{},
		mu:       sync.Mutex{},
	}
}

func (m *SessionManager) SetIdleTimeout(timeout time.Duration) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.idleTimeout = timeout
}

func (m *SessionManager) idleTimeoutValue() time.Duration {
	m.mu.Lock()
	defer m.mu.Unlock()

	return m.idleTimeout
}

func (m *SessionManager) IDs() []string {
	m.mu.Lock()
	defer m.mu.Unlock()

	ids := []string{}
	for k := range m.sessions {
		ids = append(ids, k)
	}

	return ids
}

func (m *SessionManager) Sessions() map[string]*client.Client {
	m.mu.Lock()
	defer m.mu.Unlock()

	sessions := make(map[string]*client.Client, len(m.sessions))
	for k, v := range m.sessions {
		sessions[k] = v
	}

	return sessions
}

func (m *SessionManager) Get(id string) *client.Client {
	m.mu.Lock()
	defer m.mu.Unlock()

	return m.sessions[id]
}

func (m *SessionManager) Add(id string, conn *client.Client) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.sessions[id] = conn
	metrics.SetSessionsCount(len(m.sessions))
}

func (m *SessionManager) Remove(id string) bool {
	m.mu.Lock()
	conn, ok := m.sessions[id]
	if ok {
		delete(m.sessions, id)
	}
	metrics.SetSessionsCount(len(m.sessions))
	m.mu.Unlock()

	if ok && conn != nil {
		conn.Close()
	}
	return ok
}

func (m *SessionManager) Len() int {
	m.mu.Lock()
	defer m.mu.Unlock()

	return len(m.sessions)
}

func (m *SessionManager) Cleanup() int {
	idleTimeout := m.idleTimeoutValue()
	if idleTimeout == 0 {
		return 0
	}

	removed := 0

	m.logger.Debug("starting idle sessions cleanup")
	defer func() {
		m.logger.Debug("removed idle sessions:", removed)
	}()

	for _, id := range m.staleSessions(idleTimeout) {
		m.logger.WithField("id", id).Debug("closing stale session")
		if m.Remove(id) {
			removed++
		}
	}

	return removed
}

func (m *SessionManager) RunPeriodicCleanup() {
	m.logger.WithField("timeout", m.idleTimeoutValue()).Info("session manager cleanup enabled")

	for range time.Tick(time.Minute) {
		m.Cleanup()
	}
}

func (m *SessionManager) staleSessions(idleTimeout time.Duration) []string {
	m.mu.Lock()
	defer m.mu.Unlock()

	now := time.Now()
	ids := []string{}

	for id, conn := range m.sessions {
		if conn == nil || now.Sub(conn.LastQueryTime()) > idleTimeout {
			ids = append(ids, id)
		}
	}

	return ids
}
