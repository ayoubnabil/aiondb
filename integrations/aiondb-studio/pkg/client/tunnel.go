package client

import (
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/ScaleFT/sshkeys"
	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/knownhosts"

	"github.com/sosedoff/pgweb/pkg/connection"
	"github.com/sosedoff/pgweb/pkg/shared"
)

const (
	portStart             = 29168
	portLimit             = 500
	maxSSHPrivateKeyBytes = 1024 * 1024
	maxSSHKnownHostsBytes = 4 * 1024 * 1024
	maxSSHEndpointBytes   = 1024
	maxSSHPathBytes       = 4096
	maxSSHSecretBytes     = 8 * 1024
)

// Tunnel represents the connection between local and remote server
type Tunnel struct {
	TargetHost string
	TargetPort string
	Port       int
	SSHInfo    *shared.SSHInfo
	Config     *ssh.ClientConfig
	Client     *ssh.Client
	Listener   *net.TCPListener
	dialTarget func(network, addr string) (net.Conn, error)
}

func defaultKeyPath() string {
	return filepath.Join(os.Getenv("HOME"), ".ssh/id_rsa")
}

func expandKeyPath(path string) string {
	home := os.Getenv("HOME")
	if home == "" {
		return path
	}
	return strings.Replace(path, "~", home, 1)
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func parsePrivateKey(keyPath string, keyPass string) (ssh.Signer, error) {
	info, err := os.Stat(keyPath)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("ssh private key file must be a regular file")
	}
	if info.Size() > maxSSHPrivateKeyBytes {
		return nil, fmt.Errorf("ssh private key file exceeds maximum size of %d bytes", maxSSHPrivateKeyBytes)
	}

	buff, err := os.ReadFile(keyPath)
	if err != nil {
		return nil, err
	}

	signer, err := ssh.ParsePrivateKey(buff)
	if _, ok := err.(*ssh.PassphraseMissingError); ok {
		if keyPass == "" {
			return nil, errors.New("ssh key password is not provided")
		}
		return sshkeys.ParseEncryptedPrivateKey(buff, []byte(keyPass))
	}

	return signer, err
}

func defaultKnownHostsPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return "", fmt.Errorf("could not determine home directory for ssh known_hosts")
	}
	return filepath.Join(home, ".ssh", "known_hosts"), nil
}

func makeHostKeyCallback() (ssh.HostKeyCallback, error) {
	path, err := defaultKnownHostsPath()
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, fmt.Errorf("ssh known_hosts verification is required: %w", err)
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("ssh known_hosts file must be a regular file")
	}
	if info.Size() > maxSSHKnownHostsBytes {
		return nil, fmt.Errorf("ssh known_hosts file exceeds maximum size of %d bytes", maxSSHKnownHostsBytes)
	}
	callback, err := knownhosts.New(path)
	if err != nil {
		return nil, fmt.Errorf("ssh known_hosts verification is required: %w", err)
	}
	return callback, nil
}

func makeConfig(info *shared.SSHInfo) (*ssh.ClientConfig, error) {
	if err := validateSSHInfo(info); err != nil {
		return nil, err
	}

	methods := []ssh.AuthMethod{}

	// Try to use user-provided key, fallback to system default key
	keyPath := info.Key
	if keyPath == "" {
		keyPath = defaultKeyPath()
	} else {
		keyPath = expandKeyPath(keyPath)
	}

	if !fileExists(keyPath) {
		return nil, fmt.Errorf("ssh public key not found at path %q", keyPath)
	}

	// Append public key authentication method
	key, err := parsePrivateKey(keyPath, info.KeyPassword)
	if err != nil {
		return nil, err
	}
	methods = append(methods, ssh.PublicKeys(key))

	hostKeyCallback, err := makeHostKeyCallback()
	if err != nil {
		return nil, err
	}

	// Append password authentication method
	if info.Password != "" {
		methods = append(methods, ssh.Password(info.Password))
	}

	cfg := &ssh.ClientConfig{
		User:            info.User,
		Auth:            methods,
		Timeout:         time.Second * 10,
		HostKeyCallback: hostKeyCallback,
	}

	return cfg, nil
}

func validateSSHInfo(info *shared.SSHInfo) error {
	if info == nil {
		return errors.New("ssh configuration is required")
	}
	if len(info.Host) > maxSSHEndpointBytes ||
		len(info.Port) > maxSSHEndpointBytes ||
		len(info.User) > maxSSHEndpointBytes {
		return errors.New("ssh endpoint field exceeds maximum size")
	}
	if len(info.Key) > maxSSHPathBytes {
		return errors.New("ssh key path exceeds maximum size")
	}
	if len(info.Password) > maxSSHSecretBytes ||
		len(info.KeyPassword) > maxSSHSecretBytes {
		return errors.New("ssh secret field exceeds maximum size")
	}
	return nil
}

func (tunnel *Tunnel) sshEndpoint() string {
	return fmt.Sprintf("%s:%v", tunnel.SSHInfo.Host, tunnel.SSHInfo.Port)
}

func (tunnel *Tunnel) targetEndpoint() string {
	return fmt.Sprintf("%v:%v", tunnel.TargetHost, tunnel.TargetPort)
}

func (tunnel *Tunnel) copy(wg *sync.WaitGroup, writer, reader net.Conn) {
	defer wg.Done()
	if _, err := io.Copy(writer, reader); err != nil {
		log.Println("Tunnel copy error:", err)
	}
}

func (tunnel *Tunnel) handleConnection(local net.Conn) {
	defer local.Close()

	remote, err := tunnel.dialRemote("tcp", tunnel.targetEndpoint())
	if err != nil {
		return
	}
	defer remote.Close()

	wg := &sync.WaitGroup{}
	wg.Add(2)

	go tunnel.copy(wg, local, remote)
	go tunnel.copy(wg, remote, local)

	wg.Wait()
}

func (tunnel *Tunnel) dialRemote(network, addr string) (net.Conn, error) {
	if tunnel.dialTarget != nil {
		return tunnel.dialTarget(network, addr)
	}
	return tunnel.Client.Dial(network, addr)
}

// Close closes the tunnel connection
func (tunnel *Tunnel) Close() {
	if tunnel.Client != nil {
		tunnel.Client.Close()
	}

	if tunnel.Listener != nil {
		tunnel.Listener.Close()
	}
}

// Configure establishes the tunnel between localhost and remote machine
func (tunnel *Tunnel) Configure() error {
	config, err := makeConfig(tunnel.SSHInfo)
	if err != nil {
		return err
	}
	tunnel.Config = config

	client, err := ssh.Dial("tcp", tunnel.sshEndpoint(), config)
	if err != nil {
		return err
	}
	tunnel.Client = client

	listener, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%v", tunnel.Port))
	if err != nil {
		return err
	}
	tunnel.Listener = listener.(*net.TCPListener)

	return nil
}

// Start starts the connection handler loop
func (tunnel *Tunnel) Start() {
	defer tunnel.Close()

	for {
		conn, err := tunnel.Listener.Accept()
		if err != nil {
			return
		}

		go tunnel.handleConnection(conn)
	}
}

// NewTunnel instantiates a new tunnel struct from given ssh info
func NewTunnel(sshInfo *shared.SSHInfo, dbUrl string) (*Tunnel, error) {
	uri, err := url.Parse(dbUrl)
	if err != nil {
		return nil, err
	}

	host := uri.Hostname()
	if host == "" {
		return nil, errors.New("database host is not provided")
	}
	port := uri.Port()
	if port == "" {
		port = "5432"
	}

	listenPort, err := connection.FindAvailablePort(portStart, portLimit)
	if err != nil {
		return nil, err
	}

	tunnel := &Tunnel{
		Port:       listenPort,
		SSHInfo:    sshInfo,
		TargetHost: host,
		TargetPort: port,
	}

	return tunnel, nil
}
