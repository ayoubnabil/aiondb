package client

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/sosedoff/pgweb/pkg/command"
	"github.com/sosedoff/pgweb/pkg/shared"
	"github.com/stretchr/testify/assert"
	"golang.org/x/crypto/ssh"
)

func TestMakeConfigRejectsUnknownSSHHostKey(t *testing.T) {
	privateKeyPath := writeTestPrivateKey(t)
	writeTestKnownHosts(t, "")
	_, hostPublicKey, err := generateTestSSHKey()
	assert.NoError(t, err)

	config, err := makeConfig(&shared.SSHInfo{
		User: "user",
		Host: "db.example",
		Port: "22",
		Key:  privateKeyPath,
	})
	assert.NoError(t, err)

	err = config.HostKeyCallback("db.example:22", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 22}, hostPublicKey)
	assert.Error(t, err)
}

func TestMakeConfigAcceptsKnownSSHHostKey(t *testing.T) {
	privateKeyPath := writeTestPrivateKey(t)
	_, hostPublicKey, err := generateTestSSHKey()
	assert.NoError(t, err)
	writeTestKnownHosts(t, "db.example "+strings.TrimSpace(string(ssh.MarshalAuthorizedKey(hostPublicKey)))+"\n")

	config, err := makeConfig(&shared.SSHInfo{
		User: "user",
		Host: "db.example",
		Port: "22",
		Key:  privateKeyPath,
	})
	assert.NoError(t, err)

	err = config.HostKeyCallback("db.example:22", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 22}, hostPublicKey)
	assert.NoError(t, err)
}

func TestParsePrivateKeyRejectsOversizedKeyFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "id_rsa")
	file, err := os.Create(path)
	assert.NoError(t, err)
	assert.NoError(t, file.Truncate(maxSSHPrivateKeyBytes+1))
	assert.NoError(t, file.Close())

	_, err = parsePrivateKey(path, "")

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "ssh private key file exceeds maximum size")
}

func TestMakeConfigRejectsOversizedSSHFields(t *testing.T) {
	tests := []struct {
		name string
		info *shared.SSHInfo
		err  string
	}{
		{
			name: "host",
			info: &shared.SSHInfo{Host: strings.Repeat("x", maxSSHEndpointBytes+1)},
			err:  "ssh endpoint field exceeds maximum size",
		},
		{
			name: "key path",
			info: &shared.SSHInfo{Key: strings.Repeat("x", maxSSHPathBytes+1)},
			err:  "ssh key path exceeds maximum size",
		},
		{
			name: "password",
			info: &shared.SSHInfo{Password: strings.Repeat("x", maxSSHSecretBytes+1)},
			err:  "ssh secret field exceeds maximum size",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := makeConfig(tt.info)

			assert.Error(t, err)
			assert.Contains(t, err.Error(), tt.err)
		})
	}
}

func TestMakeConfigRejectsOversizedKnownHostsFile(t *testing.T) {
	privateKeyPath := writeTestPrivateKey(t)

	home := t.TempDir()
	sshDir := filepath.Join(home, ".ssh")
	assert.NoError(t, os.MkdirAll(sshDir, 0700))
	file, err := os.Create(filepath.Join(sshDir, "known_hosts"))
	assert.NoError(t, err)
	assert.NoError(t, file.Truncate(maxSSHKnownHostsBytes+1))
	assert.NoError(t, file.Close())
	t.Setenv("HOME", home)

	_, err = makeConfig(&shared.SSHInfo{
		User: "user",
		Host: "db.example",
		Port: "22",
		Key:  privateKeyPath,
	})

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "ssh known_hosts file exceeds maximum size")
}

func TestNewFromUrlDoesNotPanicWhenTunnelCreationFails(t *testing.T) {
	disableSSH := command.Opts.DisableSSH
	command.Opts.DisableSSH = false
	t.Cleanup(func() {
		command.Opts.DisableSSH = disableSSH
	})

	assert.NotPanics(t, func() {
		client, err := NewFromUrl("://bad-url", &shared.SSHInfo{
			User: "user",
			Host: "db.example",
			Port: "22",
		})

		assert.Nil(t, client)
		assert.Error(t, err)
	})
}

func TestNewTunnelParsesIPv6Target(t *testing.T) {
	tunnel, err := NewTunnel(&shared.SSHInfo{}, "postgres://user@[::1]:55432/db?sslmode=disable")

	assert.NoError(t, err)
	assert.Equal(t, "::1", tunnel.TargetHost)
	assert.Equal(t, "55432", tunnel.TargetPort)
}

func TestNewTunnelRejectsMissingTargetHost(t *testing.T) {
	tunnel, err := NewTunnel(&shared.SSHInfo{}, "postgres:///db?sslmode=disable")

	assert.Nil(t, tunnel)
	assert.EqualError(t, err, "database host is not provided")
}

func TestTunnelHandleConnectionClosesLocalWhenRemoteDialFails(t *testing.T) {
	local, peer := net.Pipe()
	defer peer.Close()

	tunnel := &Tunnel{
		TargetHost: "db.example",
		TargetPort: "5432",
		dialTarget: func(network, addr string) (net.Conn, error) {
			assert.Equal(t, "tcp", network)
			assert.Equal(t, "db.example:5432", addr)
			return nil, errors.New("dial failed")
		},
	}

	done := make(chan struct{})
	go func() {
		tunnel.handleConnection(local)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for tunnel connection handler")
	}

	_, err := peer.Read([]byte{0})
	assert.Error(t, err)
}

func writeTestPrivateKey(t *testing.T) string {
	t.Helper()

	key, err := rsa.GenerateKey(rand.Reader, 1024)
	assert.NoError(t, err)

	block := &pem.Block{
		Type:  "RSA PRIVATE KEY",
		Bytes: x509.MarshalPKCS1PrivateKey(key),
	}

	path := filepath.Join(t.TempDir(), "id_rsa")
	assert.NoError(t, os.WriteFile(path, pem.EncodeToMemory(block), 0600))
	return path
}

func writeTestKnownHosts(t *testing.T, content string) {
	t.Helper()

	home := t.TempDir()
	sshDir := filepath.Join(home, ".ssh")
	assert.NoError(t, os.MkdirAll(sshDir, 0700))
	assert.NoError(t, os.WriteFile(filepath.Join(sshDir, "known_hosts"), []byte(content), 0600))
	t.Setenv("HOME", home)
}

func generateTestSSHKey() (ssh.Signer, ssh.PublicKey, error) {
	key, err := rsa.GenerateKey(rand.Reader, 1024)
	if err != nil {
		return nil, nil, err
	}

	signer, err := ssh.NewSignerFromKey(key)
	if err != nil {
		return nil, nil, err
	}

	return signer, signer.PublicKey(), nil
}
