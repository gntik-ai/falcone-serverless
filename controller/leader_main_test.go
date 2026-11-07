package main

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"io/fs"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

func (f *pfAPI) leaseRequests() []pfRequest {
	var out []pfRequest
	for _, r := range f.seen() {
		if strings.Contains(r.path, "/leases") {
			out = append(out, r)
		}
	}
	return out
}

// requireNoLeaseRequestAndExit1 checks the failed-start contract: exit 1, the
// preflight report intact on stdout, no Lease request, no duty and no start hook.
func (e *pfEnv) requireNoLeaseRequestAndExit1(t *testing.T, code int) status.Report {
	t.Helper()
	r := e.report(t)
	if code != 1 {
		t.Fatalf("exit %d, want 1", code)
	}
	if n := len(e.api.leaseRequests()); n != 0 {
		t.Fatalf("%d Lease requests were sent", n)
	}
	if e.calls != 0 {
		t.Fatalf("the start hook ran %d times", e.calls)
	}
	return r
}

func TestRunSendsNoLeaseRequestWhenPodIdentityIsUnusable(t *testing.T) {
	for name, edit := range map[string]func(env map[string]string){
		"POD_NAMESPACE absent":        func(env map[string]string) { delete(env, "POD_NAMESPACE") },
		"POD_NAMESPACE empty":         func(env map[string]string) { env["POD_NAMESPACE"] = "" },
		"POD_NAMESPACE is a pool":     func(env map[string]string) { env["POD_NAMESPACE"] = "pool-a" },
		"POD_NAMESPACE differs":       func(env map[string]string) { env["POD_NAMESPACE"] = "other-system" },
		"POD_NAMESPACE is not a name": func(env map[string]string) { env["POD_NAMESPACE"] = "../kube-system" },
		"POD_NAME absent":             func(env map[string]string) { delete(env, "POD_NAME") },
		"POD_NAME invalid":            func(env map[string]string) { env["POD_NAME"] = "Not A Pod/Name" },
	} {
		t.Run(name, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(nil))
			edit(e.env)
			r := e.requireNoLeaseRequestAndExit1(t, e.run())
			if r.Preflight.Result != status.ResultPassed || len(r.Preflight.Checks) != 38 {
				t.Fatalf("the preflight report must stay intact: %s with %d checks", r.Preflight.Result, len(r.Preflight.Checks))
			}
			e.requireOnlySSARPosts(t)
			if !strings.Contains(e.errOut.String(), "leader election not started") {
				t.Fatalf("stderr = %q", e.errOut.String())
			}
		})
	}
}

func TestRunSendsNoLeaseRequestWhenALeaseCheckIsDeniedOrErrors(t *testing.T) {
	var leaseChecks []preflight.Requirement
	for _, r := range preflight.Table(pfPools, pfController) {
		if r.Resource == "leases" {
			leaseChecks = append(leaseChecks, r)
		}
	}
	if len(leaseChecks) != 10 {
		t.Fatalf("found %d Lease requirements, want 10", len(leaseChecks))
	}
	for _, removed := range leaseChecks {
		t.Run("denied "+removed.Verb+" "+removed.Name, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(nil))
			delete(e.api.granted, removed)
			r := e.requireNoLeaseRequestAndExit1(t, e.run())
			if r.Preflight.Result != status.ResultFailed {
				t.Fatalf("result = %s", r.Preflight.Result)
			}
		})
		t.Run("error "+removed.Verb+" "+removed.Name, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(nil))
			e.api.fail = func(r preflight.Requirement) bool { return r == removed }
			r := e.requireNoLeaseRequestAndExit1(t, e.run())
			if r.Preflight.Result != status.ResultFailed {
				t.Fatalf("result = %s", r.Preflight.Result)
			}
		})
	}
}

// --- a full lead, shut down and release cycle over HTTP ----------------------

type mainClock struct {
	mu     sync.Mutex
	now    time.Time
	timers []mainTimer
}

type mainTimer struct {
	at time.Time
	d  time.Duration
	ch chan time.Time
}

func (c *mainClock) Now() time.Time { c.mu.Lock(); defer c.mu.Unlock(); return c.now }

func (c *mainClock) After(d time.Duration) <-chan time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	ch := make(chan time.Time, 1)
	if d <= 0 {
		ch <- c.now
		return ch
	}
	c.timers = append(c.timers, mainTimer{c.now.Add(d), d, ch})
	return ch
}

func (c *mainClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
	var keep []mainTimer
	for _, t := range c.timers {
		if t.at.After(c.now) {
			keep = append(keep, t)
		} else {
			t.ch <- c.now
		}
	}
	c.timers = keep
}

// idle reports whether the election loop is waiting for its next retry (a pending
// timer of the retry period), so no Lease request is in flight and time may move.
func (c *mainClock) idle(retry time.Duration) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, t := range c.timers {
		if t.d == retry {
			return true
		}
	}
	return false
}

// leaseAPI answers the Lease endpoints of a fake Kubernetes API.
type leaseAPI struct {
	mu    sync.Mutex
	lease map[string]any
	n     int
	paths []string
}

func (l *leaseAPI) handle(w http.ResponseWriter, r *http.Request) bool {
	if !strings.HasPrefix(r.URL.Path, "/apis/coordination.k8s.io/") {
		return false
	}
	body, _ := io.ReadAll(r.Body)
	l.mu.Lock()
	defer l.mu.Unlock()
	l.paths = append(l.paths, r.Method+" "+r.URL.Path)
	reply := func(code int, v any) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(code)
		_ = json.NewEncoder(w).Encode(v)
	}
	var obj map[string]any
	_ = json.Unmarshal(body, &obj)
	var meta map[string]any
	if obj != nil {
		meta, _ = obj["metadata"].(map[string]any)
	}
	switch {
	case r.Method == http.MethodGet && l.lease == nil, r.Method == http.MethodPut && l.lease == nil:
		reply(404, map[string]any{"kind": "Status"})
	case r.Method == http.MethodGet:
		reply(200, l.lease)
	case r.Method == http.MethodPost && l.lease != nil:
		reply(409, map[string]any{"kind": "Status"})
	case r.Method == http.MethodPost:
		l.n++
		meta["uid"], meta["resourceVersion"] = "uid-1", "1"
		l.lease = obj
		reply(201, obj)
	default:
		cur := l.lease["metadata"].(map[string]any)
		if meta["resourceVersion"] != cur["resourceVersion"] {
			reply(409, map[string]any{"kind": "Status"})
			return true
		}
		l.n++
		meta["uid"], meta["resourceVersion"] = cur["uid"], string(rune('0'+l.n))
		l.lease = obj
		reply(200, obj)
	}
	return true
}

func (l *leaseAPI) holder() (string, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.lease == nil {
		return "", false
	}
	spec, _ := l.lease["spec"].(map[string]any)
	h, _ := spec["holderIdentity"].(string)
	return h, true
}

func TestRunLeadsThenReleasesTheLeaseOnShutdown(t *testing.T) {
	e := pfNewEnv(t, pfValues(nil))
	leases := &leaseAPI{}
	e.api.respond = leases.handle
	clock := &mainClock{now: time.Unix(1_800_000_000, 0)}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	dutyRunning := make(chan struct{})
	dutyStopped := make(chan struct{})
	duty := func(ctx context.Context, g *leader.Gate) {
		close(dutyRunning)
		<-ctx.Done()
		close(dutyStopped)
	}
	var out, errOut bytes.Buffer
	started := 0
	done := make(chan int, 1)
	go func() {
		done <- run(ctx, deps{
			getenv: func(k string) string { return e.env[k] },
			readFile: func(name string) ([]byte, error) {
				if data, ok := e.files[name]; ok {
					return data, nil
				}
				return nil, &fs.PathError{Op: "open", Path: name, Err: fs.ErrNotExist}
			},
			client: e.api.Client(), timeout: 2 * time.Second,
			start:  func() { started++ },
			clock:  clock,
			random: bytes.NewReader([]byte{1, 2, 3, 4, 5, 6, 7, 8}),
			duties: []leader.Duty{duty},
			stdout: &out, stderr: &errOut,
		})
	}()
	// Move fake time one retry period at a time, only while the loop is idle, so a
	// real HTTPS round trip never races a fake request timeout.
	deadline := 200
	for ; deadline > 0; deadline-- {
		if h, ok := leases.holder(); ok && h != "" {
			break
		}
		for spins := 0; !clock.idle(leader.DefaultTimings.RetryPeriod) && spins < 200000; spins++ {
			time.Sleep(20 * time.Microsecond)
		}
		clock.advance(leader.DefaultTimings.RetryPeriod)
	}
	if deadline == 0 {
		t.Fatalf("never became leader; stderr: %s", errOut.String())
	}
	select {
	case <-dutyRunning:
	case <-time.After(10 * time.Second):
		t.Fatal("the duty never started behind the gate")
	}
	holder, _ := leases.holder()
	if holder != "controller-7d9f6c-abcde_0102030405060708" {
		t.Fatalf("holder = %q", holder)
	}
	cancel() // SIGTERM
	select {
	case code := <-done:
		if code != 0 {
			t.Fatalf("exit %d after a graceful shutdown", code)
		}
	case <-time.After(10 * time.Second):
		clock.advance(time.Minute) // let a drain bound pass rather than hang the test
		t.Fatal("run did not return after the context ended")
	}
	<-dutyStopped
	if h, ok := leases.holder(); !ok || h != "" {
		t.Fatalf("Lease after shutdown = %q (exists %v), want released with an empty holder", h, ok)
	}
	if started != 1 {
		t.Fatalf("start hook ran %d times", started)
	}
	// Exactly one JSON report on stdout, then nothing else.
	dec := json.NewDecoder(&out)
	var report json.RawMessage
	if err := dec.Decode(&report); err != nil || dec.More() {
		t.Fatalf("stdout does not hold exactly one JSON value (err %v): %s", err, out.String())
	}
	if err := status.ValidateJSON(report); err != nil {
		t.Fatal(err)
	}
	// Every Lease request targets the release namespace and the fixed name only.
	leases.mu.Lock()
	defer leases.mu.Unlock()
	if len(leases.paths) == 0 {
		t.Fatal("no Lease requests recorded")
	}
	for _, p := range leases.paths {
		_, path, _ := strings.Cut(p, " ")
		base := "/apis/coordination.k8s.io/v1/namespaces/" + pfController + "/leases"
		if path != base && path != base+"/"+leader.LeaseName {
			t.Errorf("Lease request %q leaves the release namespace or the fixed name", p)
		}
	}
	if strings.Contains(errOut.String(), pfToken) || strings.Contains(out.String(), pfToken) {
		t.Fatal("the token reached an output stream")
	}
}
