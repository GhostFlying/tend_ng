package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"tailscale.com/tsnet"
)

type route struct {
	listen string
	target string
}

type routes []route

func (r *routes) String() string {
	return fmt.Sprintf("%v", []route(*r))
}

func (r *routes) Set(spec string) error {
	listen, target, ok := strings.Cut(spec, "=")
	_, _, listenErr := net.SplitHostPort(listen)
	host, _, targetErr := net.SplitHostPort(target)
	if !ok || listenErr != nil || targetErr != nil || host == "" {
		return fmt.Errorf("expected listen-address=target-address")
	}
	*r = append(*r, route{listen: listen, target: target})
	return nil
}

type configuration struct {
	dir        string
	hostname   string
	controlURL string
	inbound    routes
	inboundTLS routes
	outbound   routes
}

func execute(ctx context.Context, config configuration) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	node := &tsnet.Server{Dir: config.dir, Hostname: config.hostname, ControlURL: config.controlURL}
	defer node.Close()
	if _, err := node.Up(ctx); err != nil {
		if ctx.Err() != nil {
			return nil
		}
		return fmt.Errorf("start tsnet node: %w", err)
	}
	proxy := make([]binding, 0, len(config.inbound)+len(config.inboundTLS)+len(config.outbound))
	defer func() {
		cancel()
		for _, entry := range proxy {
			entry.listener.Close()
		}
	}()
	dialer := &net.Dialer{}
	for _, inbound := range []struct {
		routes routes
		listen func(string, string) (net.Listener, error)
	}{
		{config.inbound, node.Listen},
		{config.inboundTLS, node.ListenTLS},
	} {
		for _, entry := range inbound.routes {
			listener, err := inbound.listen("tcp", entry.listen)
			if err != nil {
				return fmt.Errorf("bind tailnet %s: %w", entry.listen, err)
			}
			proxy = append(proxy, binding{listener: listener, target: entry.target, dial: dialer.DialContext})
		}
	}
	for _, entry := range config.outbound {
		listener, err := net.Listen("tcp", entry.listen)
		if err != nil {
			return fmt.Errorf("bind local socket %s: %w", entry.listen, err)
		}
		proxy = append(proxy, binding{listener: listener, target: entry.target, dial: node.Dial})
	}
	results := make(chan error, len(proxy))
	for _, entry := range proxy {
		log.Printf("tsnet forwarding %s to %s", entry.listener.Addr(), entry.target)
		go func() { results <- entry.serve(ctx) }()
	}
	select {
	case <-ctx.Done():
		return nil
	case err := <-results:
		return err
	}
}

type binding struct {
	listener net.Listener
	target   string
	dial     func(context.Context, string, string) (net.Conn, error)
}

func (p binding) serve(ctx context.Context) error {
	stop := context.AfterFunc(ctx, func() { p.listener.Close() })
	defer stop()
	for {
		conn, err := p.listener.Accept()
		if err != nil {
			if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return err
		}
		go p.forward(ctx, conn)
	}
}

func (p binding) forward(ctx context.Context, from net.Conn) {
	defer from.Close()
	connect, cancel := context.WithTimeout(ctx, 5*time.Second)
	to, err := p.dial(connect, "tcp", p.target)
	cancel()
	if err != nil {
		return
	}
	defer to.Close()
	closeConnections := context.AfterFunc(ctx, func() { from.Close(); to.Close() })
	defer closeConnections()
	done := make(chan struct{})
	go func() {
		copyHalf(to, from)
		close(done)
	}()
	copyHalf(from, to)
	<-done
}

func copyHalf(to, from net.Conn) {
	_, err := io.Copy(to, from)
	if half, ok := to.(interface{ CloseWrite() error }); ok && err == nil {
		_ = half.CloseWrite()
		return
	}
	_ = to.Close()
}

func main() {
	config := configuration{}
	flag.StringVar(&config.dir, "state-dir", "", "persistent tsnet identity directory")
	flag.StringVar(&config.hostname, "hostname", "", "tailnet node name")
	flag.StringVar(&config.controlURL, "control-url", "", "optional Tailscale control URL")
	flag.Var(&config.inbound, "inbound", "tailnet listen address=local service address; repeatable")
	flag.Var(&config.inboundTLS, "inbound-tls", "tailnet HTTPS listen address=local HTTP service address; repeatable")
	flag.Var(&config.outbound, "outbound", "local listen address=tailnet service address; repeatable")
	flag.Parse()
	if config.dir == "" || config.hostname == "" || len(config.inbound)+len(config.inboundTLS)+len(config.outbound) == 0 {
		flag.Usage()
		os.Exit(2)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := execute(ctx, config); err != nil {
		log.Fatal(err)
	}
}
