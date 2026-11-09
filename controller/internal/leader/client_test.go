package leader

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

const leaseURL = "/apis/coordination.k8s.io/v1/namespaces/" + relNS + "/leases"

type httpRequest struct {
	method, path, contentType, auth, body string
}

// httpLeaseServer is a minimal Lease API: GET and PATCH on the fixed name, POST on the
// collection, with resourceVersion preconditions, UIDs and bearer-token checks.
type httpLeaseServer struct {
	*httptest.Server
	mu       sync.Mutex
	token    string // the only token the server accepts
	lease    *wireLease
	rv, uids int
	requests []httpRequest
	respond  func(w http.ResponseWriter, r *http.Request) bool
}

func newHTTPLeaseServer(t *testing.T, token string) *httpLeaseServer {
	t.Helper()
	s := &httpLeaseServer{token: token}
	s.Server = httptest.NewServer(http.HandlerFunc(s.handle))
	t.Cleanup(s.Close)
	return s
}

func (s *httpLeaseServer) setToken(token string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.token = token
}

func (s *httpLeaseServer) seen() []httpRequest {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]httpRequest(nil), s.requests...)
}

func (s *httpLeaseServer) handle(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.requests = append(s.requests, httpRequest{r.Method, r.URL.Path, r.Header.Get("Content-Type"), r.Header.Get("Authorization"), string(body)})
	if s.respond != nil && s.respond(w, r) {
		return
	}
	if r.Header.Get("Authorization") != "Bearer "+s.token {
		w.WriteHeader(http.StatusUnauthorized)
		return
	}
	reply := func(code int, l *wireLease) {
		w.WriteHeader(code)
		_ = json.NewEncoder(w).Encode(l)
	}
	named := leaseURL + "/" + LeaseName
	switch {
	case r.Method == http.MethodGet && r.URL.Path == named:
		if s.lease == nil {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		reply(http.StatusOK, s.lease)
	case r.Method == http.MethodPost && r.URL.Path == leaseURL:
		if s.lease != nil {
			w.WriteHeader(http.StatusConflict)
			return
		}
		var l wireLease
		if json.Unmarshal(body, &l) != nil {
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		s.uids++
		s.rv++
		l.Metadata.UID, l.Metadata.ResourceVersion = fmt.Sprintf("uid-%d", s.uids), strconv.Itoa(s.rv)
		s.lease = &l
		reply(http.StatusCreated, s.lease)
	case r.Method == http.MethodPatch && r.URL.Path == named:
		var p wireLease
		if s.lease == nil {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		if json.Unmarshal(body, &p) != nil || p.Metadata.ResourceVersion != s.lease.Metadata.ResourceVersion {
			w.WriteHeader(http.StatusConflict)
			return
		}
		for k, v := range p.Metadata.Annotations {
			s.lease.Metadata.Annotations[k] = v
		}
		if p.Spec.HolderIdentity != nil {
			s.lease.Spec.HolderIdentity = p.Spec.HolderIdentity
		}
		if p.Spec.RenewTime != nil {
			s.lease.Spec.RenewTime = p.Spec.RenewTime
		}
		if p.Spec.AcquireTime != nil {
			s.lease.Spec.AcquireTime = p.Spec.AcquireTime
		}
		if p.Spec.LeaseDurationSeconds != nil {
			s.lease.Spec.LeaseDurationSeconds = p.Spec.LeaseDurationSeconds
		}
		if p.Spec.LeaseTransitions != nil {
			s.lease.Spec.LeaseTransitions = p.Spec.LeaseTransitions
		}
		s.rv++
		s.lease.Metadata.ResourceVersion = strconv.Itoa(s.rv)
		reply(http.StatusOK, s.lease)
	default:
		w.WriteHeader(http.StatusNotFound)
	}
}

// tokenFile is a projected token file that Kubernetes rotates in place.
type tokenFile struct {
	mu   sync.Mutex
	data []byte
	err  error
}

func (f *tokenFile) set(token string, err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.data, f.err = []byte(token+"\n"), err
}

func (f *tokenFile) read(string) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.data, f.err
}

func newTestClient(t *testing.T, s *httpLeaseServer, f *tokenFile) *Client {
	t.Helper()
	c, err := NewClient(s.URL, relNS, FileToken(f.read, "/token"), s.Client(), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestClientTalksOnlyToTheFixedLeaseInTheReleaseNamespace(t *testing.T) {
	s := newHTTPLeaseServer(t, "tok")
	f := &tokenFile{}
	f.set("tok", nil)
	c := newTestClient(t, s, f)
	ctx := context.Background()
	if _, err := c.Get(ctx); !errors.Is(err, ErrNotFound) {
		t.Fatal(err)
	}
	l := newLease(poolA, "controller-0", DefaultLeaseDuration, epoch0) // a pool namespace must not be honoured
	created, err := c.Create(ctx, l)
	if err != nil {
		t.Fatal(err)
	}
	if created.UID != "uid-1" || created.Namespace != relNS || created.Name != LeaseName {
		t.Fatalf("created %+v", created)
	}
	if _, err := c.Create(ctx, l); !errors.Is(err, ErrAlreadyExists) {
		t.Fatalf("second create: %v", err)
	}
	created.HolderIdentity = "controller-1"
	updated, err := c.Update(ctx, created)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.Update(ctx, created); !errors.Is(err, ErrConflict) {
		t.Fatalf("stale update: %v", err)
	}
	if updated.HolderIdentity != "controller-1" || updated.Annotations[EpochAnnotation] != "1" {
		t.Fatalf("updated %+v", updated)
	}

	seen := s.seen()
	want := []struct{ method, path, contentType string }{
		{"GET", leaseURL + "/" + LeaseName, ""},
		{"POST", leaseURL, "application/json"},
		{"POST", leaseURL, "application/json"},
		{"PATCH", leaseURL + "/" + LeaseName, "application/merge-patch+json"},
		{"PATCH", leaseURL + "/" + LeaseName, "application/merge-patch+json"},
	}
	if len(seen) != len(want) {
		t.Fatalf("%d requests: %+v", len(seen), seen)
	}
	for i, w := range want {
		if seen[i].method != w.method || seen[i].path != w.path || seen[i].contentType != w.contentType {
			t.Errorf("request %d = %+v, want %+v", i, seen[i], w)
		}
		if strings.Contains(seen[i].path, poolA) {
			t.Errorf("request %d reached a pool namespace", i)
		}
	}
	var created1 map[string]any
	if err := json.Unmarshal([]byte(seen[1].body), &created1); err != nil {
		t.Fatal(err)
	}
	if meta := created1["metadata"].(map[string]any); meta["namespace"] != relNS || meta["name"] != LeaseName {
		t.Fatalf("create body names %v", meta)
	}
	// The merge patch carries the resourceVersion read and touches nothing outside the fence fields.
	var patch map[string]map[string]any
	if err := json.Unmarshal([]byte(seen[3].body), &patch); err != nil {
		t.Fatal(err)
	}
	if patch["metadata"]["resourceVersion"] != "1" || len(patch["metadata"]) != 2 {
		t.Fatalf("patch metadata %v", patch["metadata"])
	}
	if patch["metadata"]["annotations"].(map[string]any)[EpochAnnotation] != "1" {
		t.Fatalf("patch annotations %v", patch["metadata"]["annotations"])
	}
}

func TestNewClientRejectsPoolStyleAndInvalidNamespaces(t *testing.T) {
	for _, ns := range []string{"", "Pool_A", "a/b", "a.b", strings.Repeat("a", 64), "../x"} {
		if _, err := NewClient("https://10.0.0.1:443", ns, func() (string, error) { return "t", nil }, http.DefaultClient, 0); err == nil {
			t.Errorf("namespace %q accepted", ns)
		}
	}
	for _, base := range []string{"", "10.0.0.1", "ftp://x", "https://"} {
		if _, err := NewClient(base, relNS, func() (string, error) { return "t", nil }, http.DefaultClient, 0); err == nil {
			t.Errorf("API base %q accepted", base)
		}
	}
}

func TestClientReloadsTheTokenOnEveryCallAndFailsClosedWithoutOne(t *testing.T) {
	s := newHTTPLeaseServer(t, "tok-1")
	f := &tokenFile{}
	f.set("tok-1", nil)
	c := newTestClient(t, s, f)
	ctx := context.Background()
	if _, err := c.Get(ctx); !errors.Is(err, ErrNotFound) {
		t.Fatalf("first token: %v", err)
	}
	f.set("tok-2", nil) // Kubernetes rotates the projected token
	s.setToken("tok-2")
	if _, err := c.Get(ctx); !errors.Is(err, ErrNotFound) {
		t.Fatalf("rotated token not used: %v", err)
	}
	requests := len(s.seen())
	for name, set := range map[string]func(){
		"missing":    func() { f.set("", &fs.PathError{Op: "open", Path: "/token", Err: fs.ErrNotExist}) },
		"unreadable": func() { f.set("tok-2", &fs.PathError{Op: "open", Path: "/token", Err: fs.ErrPermission}) },
		"empty":      func() { f.set("", nil) },
		"blank":      func() { f.set("   ", nil) },
	} {
		set()
		for _, call := range []func() error{
			func() error { _, err := c.Get(ctx); return err },
			func() error { _, err := c.Create(ctx, newLease(relNS, "x", time.Second, epoch0)); return err },
			func() error {
				l := newLease(relNS, "x", time.Second, epoch0)
				l.ResourceVersion = "1"
				_, err := c.Update(ctx, l)
				return err
			},
		} {
			if err := call(); !errors.Is(err, ErrToken) {
				t.Fatalf("%s token: %v", name, err)
			}
		}
	}
	if len(s.seen()) != requests {
		t.Fatal("a call without a usable token still reached the API server")
	}
}

func TestClientErrorsNeverCarryTheTokenAndRedirectsAreNotFollowed(t *testing.T) {
	s := newHTTPLeaseServer(t, "tok-secret-value")
	f := &tokenFile{}
	f.set("tok-secret-value", nil)
	c := newTestClient(t, s, f)
	for name, respond := range map[string]func(w http.ResponseWriter, r *http.Request) bool{
		"500": func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(500)
			_, _ = io.WriteString(w, "tok-secret-value")
			return true
		},
		"redirect": func(w http.ResponseWriter, r *http.Request) bool {
			http.Redirect(w, r, "/followed", http.StatusFound)
			return true
		},
		"garbage": func(w http.ResponseWriter, r *http.Request) bool { _, _ = io.WriteString(w, "{"); return true },
	} {
		s.respond = respond
		_, err := c.Get(context.Background())
		if err == nil || strings.Contains(err.Error(), "tok-secret-value") {
			t.Fatalf("%s: %v", name, err)
		}
	}
	for _, r := range s.seen() {
		if r.path == "/followed" {
			t.Fatal("a redirect was followed")
		}
	}
	s.respond = func(w http.ResponseWriter, r *http.Request) bool { w.WriteHeader(http.StatusForbidden); return true }
	var api *APIError
	if _, err := c.Get(context.Background()); !errors.As(err, &api) || api.Status != 403 {
		t.Fatalf("want an APIError 403, got %v", err)
	}
}

// Two replicas elect over the real client against an HTTP Lease API while the token is
// rotated mid-run: renewals and a later takeover keep working, and a missing token fails the
// call closed until the gate closes.
func TestTokenRotationMidRunKeepsRenewalsAndLaterTakeoverWorking(t *testing.T) {
	s := newHTTPLeaseServer(t, "tok-1")
	f := &tokenFile{}
	f.set("tok-1", nil)
	clock, rec := newFakeClock(epoch0), &fakeReceiver{}
	newElector := func(id string) *Elector {
		e, err := New(Config{
			Identity: id, Namespace: relNS,
			LeaseDuration: DefaultLeaseDuration, RenewDeadline: DefaultRenewDeadline, RetryPeriod: DefaultRetryPeriod,
			Lease: newTestClient(t, s, f), Clock: clock,
		})
		if err != nil {
			t.Fatal(err)
		}
		return e
	}
	ctx := context.Background()
	a, b := newElector("controller-0"), newElector("controller-1")
	a.Tick(ctx)
	b.Tick(ctx)
	rotations := []string{"tok-2", "tok-3"}
	for step := 0; step < 8; step++ {
		clock.Advance(tick)
		if step%3 == 0 && len(rotations) > 0 {
			f.set(rotations[0], nil)
			s.setToken(rotations[0])
			rotations = rotations[1:]
		}
		a.Tick(ctx)
		b.Tick(ctx)
		if a.Term() == nil || b.Term() != nil {
			t.Fatalf("step %d: renewals broke after a rotation: a=%+v b=%+v", step, a.Status(), b.Status())
		}
	}
	// A stops. The token rotates again, and B still takes over.
	f.set("tok-4", nil)
	s.setToken("tok-4")
	for b.Term() == nil {
		clock.Advance(tick)
		b.Tick(ctx)
		if clock.Now().Sub(epoch0) > 10*time.Minute {
			t.Fatalf("takeover failed after rotation: %+v", b.Status())
		}
	}
	if err := b.Publish(ctx, rec, nil); err != nil || rec.holds().Epoch != 2 {
		t.Fatalf("publish %v, receiver %+v", err, rec.holds())
	}
	// The token disappears: renewals fail closed, no request is sent, and the gate closes.
	f.set("", &fs.PathError{Op: "open", Path: "/token", Err: fs.ErrNotExist})
	before := len(s.seen())
	for b.Term() != nil {
		clock.Advance(tick)
		b.Tick(ctx)
		if clock.Now().Sub(epoch0) > 10*time.Minute {
			t.Fatal("the gate never closed without a token")
		}
	}
	if got := len(s.seen()); got != before {
		t.Fatalf("%d requests were sent without a token", got-before)
	}
	if s := b.Status(); s.Reason != ReasonLeadershipLost {
		t.Fatalf("status %+v", s)
	}
}
