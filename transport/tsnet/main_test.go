package main

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

func TestForwardingAndIdentityResume(t *testing.T) {
	netns.SetEnabled(false)
	control := &testcontrol.Server{
		DERPMap:        integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"),
		DNSConfig:      &tailcfg.DNSConfig{},
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
	dir := t.TempDir()
	startInbound := func() (*tsnet.Server, string) {
		node := &tsnet.Server{
			Dir: dir, Hostname: "incoming", ControlURL: control.HTTPTestServer.URL,
			Logf: logger.Discard, UserLogf: logger.Discard,
		}
		if _, err := node.Up(ctx); err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { node.Close() })
		for index, backend := range backends {
			listener, err := node.Listen("tcp", fmt.Sprintf(":%d", 8080+index))
			if err != nil {
				t.Fatal(err)
			}
			b := binding{listener: listener, target: backend.Listener.Addr().String(), dial: (&net.Dialer{}).DialContext}
			go b.serve(ctx)
		}
		ip, _ := node.TailscaleIPs()
		return node, ip.String()
	}

	inbound, ip := startInbound()
	outbound := configuration{
		dir: t.TempDir(), hostname: "outgoing", controlURL: control.HTTPTestServer.URL,
	}
	for index := range backends {
		reservation, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		addr := reservation.Addr().String()
		reservation.Close()
		outbound.outbound = append(outbound.outbound, route{listen: addr, target: fmt.Sprintf("%s:%d", ip, index+8080)})
	}
	if err := ctx.Err(); err != nil {
		t.Fatal(err)
	}
	proxy := make(chan error, 1)
	go func() { proxy <- execute(ctx, outbound) }()

	query := func(index int) {
		t.Helper()
		client := &http.Client{Timeout: 2 * time.Second, Transport: &http.Transport{DisableKeepAlives: true}}
		want := fmt.Sprintf("runner-%d /health", index)
		deadline := time.Now().Add(15 * time.Second)
		var last error
		for time.Now().Before(deadline) {
			request, err := http.NewRequestWithContext(ctx, http.MethodGet, "http://"+outbound.outbound[index].listen+"/health", nil)
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

	query(0)
	query(1)
	if err := inbound.Close(); err != nil {
		t.Fatal(err)
	}
	_, restored := startInbound()
	if restored != ip {
		t.Fatalf("node address changed from %s to %s", ip, restored)
	}
	query(0)
	query(1)
	cancel()
	if err := <-proxy; err != nil {
		t.Fatal(err)
	}
}
