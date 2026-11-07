package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

const leasesPath = "/apis/coordination.k8s.io/v1/namespaces/" + releaseNS + "/leases"

// fakeAPI is a Kubernetes API server's Lease endpoints, recording every request.
type fakeAPI struct {
	*httptest.Server
	mu       sync.Mutex
	paths    []string
	tokens   []string
	bodies   []string
	lease    map[string]any
	counter  int
	override func(w http.ResponseWriter, r *http.Request) bool
}

func newFakeAPI(t *testing.T) *fakeAPI {
	t.Helper()
	f := &fakeAPI{}
	f.Server = httptest.NewServer(http.HandlerFunc(f.serve))
	t.Cleanup(f.Close)
	return f
}

func (f *fakeAPI) serve(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	f.mu.Lock()
	defer f.mu.Unlock()
	f.paths = append(f.paths, r.Method+" "+r.URL.Path)
	f.tokens = append(f.tokens, r.Header.Get("Authorization"))
	f.bodies = append(f.bodies, string(body))
	if f.override != nil && f.override(w, r) {
		return
	}
	reply := func(code int, v any) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(code)
		_ = json.NewEncoder(w).Encode(v)
	}
	var obj map[string]any
	_ = json.Unmarshal(body, &obj)
	meta, _ := obj["metadata"].(map[string]any)
	switch r.Method {
	case http.MethodGet:
		if f.lease == nil {
			reply(404, map[string]any{"kind": "Status"})
			return
		}
		reply(200, f.lease)
	case http.MethodPost:
		if f.lease != nil {
			reply(409, map[string]any{"kind": "Status"})
			return
		}
		f.counter++
		meta["uid"], meta["resourceVersion"] = "uid-1", itoa(f.counter)
		f.lease = obj
		reply(201, obj)
	case http.MethodPut:
		cur, _ := f.lease["metadata"].(map[string]any)
		if f.lease == nil {
			reply(404, map[string]any{"kind": "Status"})
			return
		}
		if meta["resourceVersion"] != cur["resourceVersion"] {
			reply(409, map[string]any{"kind": "Status"})
			return
		}
		f.counter++
		meta["uid"], meta["resourceVersion"] = cur["uid"], itoa(f.counter)
		f.lease = obj
		reply(200, obj)
	default:
		reply(405, nil)
	}
}

func (f *fakeAPI) seen() ([]string, []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.paths...), append([]string(nil), f.tokens...)
}

func httpLeaseClient(t *testing.T, api *fakeAPI, ns string, token func() (string, error)) *leader.HTTPClient {
	t.Helper()
	c, err := leader.NewHTTPClient(api.URL, ns, token, api.Client(), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func constToken(s string) func() (string, error) { return func() (string, error) { return s, nil } }

// TestElectionOverHTTPTouchesOnlyTheReleaseNamespace runs two electors through the
// real HTTP client against a fake API, with a release namespace distinct from the
// pool namespaces, and checks every request target.
func TestElectionOverHTTPTouchesOnlyTheReleaseNamespace(t *testing.T) {
	api := newFakeAPI(t)
	clock := newFakeClock()
	mk := func(name string) *leader.Elector {
		el, err := leader.New(leader.Config{Namespace: releaseNS, Identity: name, Timings: defaults,
			Client: httpLeaseClient(t, api, releaseNS, constToken("sa-token-xyz")), Clock: clock})
		if err != nil {
			t.Fatal(err)
		}
		return el
	}
	a, b := mk("a-0123abcd"), mk("b-0123abcd")
	for s := 0; s < 40; s += 2 {
		a.Tick(context.Background())
		b.Tick(context.Background())
		clock.Advance(2 * sec)
	}
	_, aOpen := a.Token()
	_, bOpen := b.Token()
	if aOpen == bOpen {
		t.Fatalf("a open %v, b open %v; want exactly one", aOpen, bOpen)
	}
	paths, tokens := api.seen()
	if len(paths) == 0 {
		t.Fatal("no requests recorded")
	}
	for i, p := range paths {
		method, path, _ := strings.Cut(p, " ")
		if path != leasesPath && path != leasesPath+"/"+leader.LeaseName {
			t.Errorf("request %q targets %s", method, path)
		}
		if strings.Contains(path, poolA) || strings.Contains(path, poolB) {
			t.Errorf("request reached a pool namespace: %s", p)
		}
		if tokens[i] != "Bearer sa-token-xyz" {
			t.Errorf("request %d carries authorization %q", i, tokens[i])
		}
	}
	var created map[string]any
	for i, p := range paths {
		if strings.HasPrefix(p, "POST ") {
			api.mu.Lock()
			_ = json.Unmarshal([]byte(api.bodies[i]), &created)
			api.mu.Unlock()
		}
	}
	meta, _ := created["metadata"].(map[string]any)
	if created["apiVersion"] != "coordination.k8s.io/v1" || created["kind"] != "Lease" || meta["namespace"] != releaseNS || meta["name"] != leader.LeaseName {
		t.Fatalf("created object = %v", created)
	}
}

func TestHTTPClientRejectsAnInvalidNamespace(t *testing.T) {
	api := newFakeAPI(t)
	for _, ns := range []string{"", "Bad", "a/b", "../kube-system", "a b", strings.Repeat("a", 64)} {
		if _, err := leader.NewHTTPClient(api.URL, ns, constToken("t"), api.Client(), time.Second); err == nil {
			t.Errorf("namespace %q was accepted", ns)
		}
	}
	if _, err := leader.NewHTTPClient(api.URL, releaseNS, constToken("t"), api.Client(), 0); err == nil {
		t.Error("a zero timeout was accepted")
	}
}

func TestHTTPClientMapsOnlyDefiniteAnswers(t *testing.T) {
	api := newFakeAPI(t)
	c := httpLeaseClient(t, api, releaseNS, constToken("secret-token-value"))
	ctx := context.Background()
	if _, err := c.Get(ctx); !errors.Is(err, leader.ErrNotFound) {
		t.Fatalf("empty store Get = %v, want ErrNotFound", err)
	}
	created, err := c.Create(ctx, leader.Lease{Holder: "me", LeaseDurationSeconds: 15, Labels: map[string]string{"k": "v"}})
	if err != nil || created.ResourceVersion == "" || created.UID == "" || created.Holder != "me" || created.LeaseDurationSeconds != 15 {
		t.Fatalf("create = %+v, %v", created, err)
	}
	if _, err := c.Create(ctx, leader.Lease{Holder: "me"}); !errors.Is(err, leader.ErrConflict) {
		t.Fatalf("second create = %v, want ErrConflict", err)
	}
	stale := created
	stale.ResourceVersion = "999"
	if _, err := c.Update(ctx, stale); !errors.Is(err, leader.ErrConflict) {
		t.Fatalf("stale update = %v, want ErrConflict", err)
	}
	if _, err := c.Update(ctx, leader.Lease{}); err == nil {
		t.Fatal("an update without resourceVersion was sent")
	}
	created.Holder = ""
	released, err := c.Update(ctx, created)
	if err != nil || released.Holder != "" || released.ResourceVersion == created.ResourceVersion {
		t.Fatalf("update = %+v, %v", released, err)
	}
	api.mu.Lock()
	api.lease = nil
	api.mu.Unlock()
	if _, err := c.Update(ctx, released); !errors.Is(err, leader.ErrNotFound) {
		t.Fatalf("update of a deleted Lease = %v, want ErrNotFound", err)
	}

	// Everything else is an API failure that opens nothing, and leaks no secret.
	big := strings.Repeat("x", 2<<20)
	for name, h := range map[string]func(w http.ResponseWriter, r *http.Request){
		"401":       func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(401) },
		"403":       func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(403) },
		"500":       func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(500) },
		"redirect":  func(w http.ResponseWriter, r *http.Request) { http.Redirect(w, r, "/elsewhere", http.StatusFound) },
		"malformed": func(w http.ResponseWriter, r *http.Request) { _, _ = io.WriteString(w, "{not json") },
		"oversized": func(w http.ResponseWriter, r *http.Request) { _, _ = io.WriteString(w, `{"pad":"`+big+`"}`) },
		"wrong kind": func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, `{"kind":"Pod","metadata":{"name":"falcone-pool-controller-leader","uid":"u","resourceVersion":"1"}}`)
		},
		"wrong name": func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, `{"kind":"Lease","metadata":{"name":"other","uid":"u","resourceVersion":"1"}}`)
		},
		"wrong namespace": func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, `{"kind":"Lease","metadata":{"name":"falcone-pool-controller-leader","namespace":"pool-a","uid":"u","resourceVersion":"1"}}`)
		},
	} {
		api.mu.Lock()
		api.override = func(w http.ResponseWriter, r *http.Request) bool { h(w, r); return true }
		api.mu.Unlock()
		_, err := c.Get(ctx)
		if err == nil || errors.Is(err, leader.ErrNotFound) || errors.Is(err, leader.ErrConflict) {
			t.Errorf("%s: Get = %v, want a plain failure", name, err)
		} else if strings.Contains(err.Error(), "secret-token-value") {
			t.Errorf("%s: error leaks the token: %v", name, err)
		}
	}
	paths, _ := api.seen()
	for _, p := range paths {
		if strings.Contains(p, "elsewhere") {
			t.Error("the client followed a redirect")
		}
	}
}

func TestHTTPClientRereadsTheRotatingToken(t *testing.T) {
	api := newFakeAPI(t)
	n := 0
	c := httpLeaseClient(t, api, releaseNS, func() (string, error) { n++; return "tok-" + itoa(n), nil })
	for i := 0; i < 3; i++ {
		_, _ = c.Get(context.Background())
	}
	_, tokens := api.seen()
	for i, tok := range tokens {
		if tok != "Bearer tok-"+itoa(i+1) {
			t.Errorf("request %d used %q", i, tok)
		}
	}
	bad := httpLeaseClient(t, api, releaseNS, func() (string, error) { return "", errors.New("unreadable") })
	before, _ := api.seen()
	if _, err := bad.Get(context.Background()); err == nil {
		t.Fatal("a request went out without a token")
	}
	if after, _ := api.seen(); len(after) != len(before) {
		t.Fatal("a request was sent without a token")
	}
}

func TestHTTPClientBoundsAHungRequest(t *testing.T) {
	api := newFakeAPI(t)
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })
	api.override = func(w http.ResponseWriter, r *http.Request) bool {
		api.mu.Unlock()
		defer api.mu.Lock()
		<-release
		return true
	}
	c, err := leader.NewHTTPClient(api.URL, releaseNS, constToken("t"), api.Client(), 50*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { _, err := c.Get(context.Background()); done <- err }()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("a hung request succeeded")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the request was not bounded by its timeout")
	}
}
