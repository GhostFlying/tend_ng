package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

const tlsFixtureDomain = "incoming.test.ts.net"

func TestForwardingAndIdentityResume(t *testing.T) {
	netns.SetEnabled(false)
	control := &testcontrol.Server{
		DERPMap:        integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"),
		DNSConfig:      &tailcfg.DNSConfig{Proxied: true},
		MagicDNSDomain: "test.ts.net",
		Logf:           logger.Discard,
	}
	control.HTTPTestServer = httptest.NewServer(control)
	t.Cleanup(control.HTTPTestServer.Close)

	backends := make([]*httptest.Server, 2)
	for index := range backends {
		backends[index] = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
			if request.Header.Get("Authorization") != "Bearer fixture" {
				http.Error(w, "missing auth", http.StatusUnauthorized)
				return
			}
			fmt.Fprintf(w, "runner-%d %s", index, request.URL.Path)
		}))
		t.Cleanup(backends[index].Close)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Second)
	defer cancel()
	inbound := configuration{
		dir: t.TempDir(), hostname: "incoming", controlURL: control.HTTPTestServer.URL,
		inboundTLS: routes{{listen: ":443", target: backends[0].Listener.Addr().String()}},
	}
	writeCachedCertificate(t, inbound.dir)
	for index, backend := range backends {
		inbound.inbound = append(inbound.inbound, route{
			listen: fmt.Sprintf(":%d", 8080+index), target: backend.Listener.Addr().String(),
		})
	}
	node := &tsnet.Server{
		Dir: inbound.dir, Hostname: inbound.hostname, ControlURL: inbound.controlURL,
		Logf: logger.Discard, UserLogf: logger.Discard,
	}
	if _, err := node.Up(ctx); err != nil {
		node.Close()
		t.Fatal(err)
	}
	ip, _ := node.TailscaleIPs()
	if err := node.Close(); err != nil {
		t.Fatal(err)
	}
	startInbound := func() func() {
		runCtx, stop := context.WithCancel(ctx)
		done := make(chan error, 1)
		go func() { done <- execute(runCtx, inbound) }()
		closeInbound := sync.OnceFunc(func() {
			stop()
			if err := <-done; err != nil {
				t.Error(err)
			}
		})
		t.Cleanup(closeInbound)
		return closeInbound
	}

	closeInbound := startInbound()
	outbound := configuration{
		dir: t.TempDir(), hostname: "outgoing", controlURL: control.HTTPTestServer.URL,
	}
	forwardPorts := []int{8080, 8081, 443}
	for _, port := range forwardPorts {
		reservation, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		addr := reservation.Addr().String()
		reservation.Close()
		outbound.outbound = append(outbound.outbound, route{listen: addr, target: fmt.Sprintf("%s:%d", ip, port)})
	}
	if err := ctx.Err(); err != nil {
		t.Fatal(err)
	}
	proxy := make(chan error, 1)
	go func() { proxy <- execute(ctx, outbound) }()

	query := func(index int, scheme string, backend int) {
		t.Helper()
		client := &http.Client{Timeout: 2 * time.Second, Transport: &http.Transport{
			DisableKeepAlives: true,
			TLSClientConfig:   &tls.Config{ServerName: tlsFixtureDomain},
		}}
		want := fmt.Sprintf("runner-%d /health", backend)
		deadline := time.Now().Add(15 * time.Second)
		var last error
		for time.Now().Before(deadline) {
			request, err := http.NewRequestWithContext(ctx, http.MethodGet, scheme+"://"+outbound.outbound[index].listen+"/health", nil)
			if err != nil {
				t.Fatal(err)
			}
			request.Header.Set("Authorization", "Bearer fixture")
			response, err := client.Do(request)
			if err == nil {
				body, err := io.ReadAll(response.Body)
				response.Body.Close()
				if err == nil && response.StatusCode == http.StatusOK && string(body) == want {
					return
				}
				err = fmt.Errorf("response %d %q: %v", response.StatusCode, body, err)
			}
			last = err
			select {
			case err := <-proxy:
				t.Fatalf("forwarder exited: %v", err)
			default:
			}
			time.Sleep(50 * time.Millisecond)
		}
		t.Fatalf("forward %d: %v", index, last)
	}

	query(0, "http", 0)
	query(1, "http", 1)
	query(2, "https", 0)
	closeInbound()
	closeInbound = startInbound()
	query(0, "http", 0)
	query(1, "http", 1)
	query(2, "https", 0)
	closeInbound()
	cancel()
	if err := <-proxy; err != nil {
		t.Fatal(err)
	}
}

type certificatePEM struct {
	cert []byte
	key  []byte
}

// Go caches system roots for the lifetime of the test process. Reuse the same
// certificate when the test runs several times in that process.
var fixtureCertificate = sync.OnceValues(func() (certificatePEM, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return certificatePEM{}, err
	}
	fixture := &x509.Certificate{
		SerialNumber: big.NewInt(1), DNSNames: []string{tlsFixtureDomain},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(60 * 24 * time.Hour),
		BasicConstraintsValid: true, IsCA: true,
		KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
	}
	certDER, err := x509.CreateCertificate(rand.Reader, fixture, fixture, &key.PublicKey, key)
	if err != nil {
		return certificatePEM{}, err
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		return certificatePEM{}, err
	}
	return certificatePEM{
		cert: pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certDER}),
		key:  pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER}),
	}, nil
})

func writeCachedCertificate(t *testing.T, stateDir string) {
	t.Helper()
	fixture, err := fixtureCertificate()
	if err != nil {
		t.Fatal(err)
	}
	certDir := filepath.Join(stateDir, "certs")
	if err := os.Mkdir(certDir, 0o700); err != nil {
		t.Fatal(err)
	}
	cert := filepath.Join(certDir, tlsFixtureDomain+".crt")
	if err := os.WriteFile(cert, fixture.cert, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(certDir, tlsFixtureDomain+".key"), fixture.key, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("SSL_CERT_FILE", cert)
}
