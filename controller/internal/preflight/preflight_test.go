package preflight_test

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"io"
	"log"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

const (
	fakeToken  = "fake-sa-token-0123456789"
	ssarPath   = "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews"
	controller = "falcone-system"
)

var pools = []string{"pool-a", "pool-b"}

// fakeAPI is a recording Kubernetes API server answering SelfSubjectAccessReviews.
type fakeAPI struct {
	*httptest.Server
	mu       sync.Mutex
	requests []recorded
	granted  map[preflight.Requirement]bool
	respond  func(w http.ResponseWriter, r *http.Request) bool // true when it handled the request
}

type recorded struct {
	method, path, auth string
	attrs              preflight.Requirement
	hasNamespace       bool
}

func grantAll(reqs []preflight.Requirement) map[preflight.Requirement]bool {
	g := map[preflight.Requirement]bool{}
	for _, r := range reqs {
		g[r] = true
	}
	return g
}

func newFakeAPI(t *testing.T, granted map[preflight.Requirement]bool) *fakeAPI {
	t.Helper()
	f := &fakeAPI{granted: granted}
	f.Server = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		rec := recorded{method: r.Method, path: r.URL.Path, auth: r.Header.Get("Authorization")}
		var review struct {
			Spec struct {
				ResourceAttributes map[string]string `json:"resourceAttributes"`
			} `json:"spec"`
		}
		if json.Unmarshal(body, &review) == nil {
			a := review.Spec.ResourceAttributes
			rec.attrs = preflight.Requirement{Namespace: a["namespace"], Group: a["group"], Resource: a["resource"], Verb: a["verb"], Name: a["name"]}
			_, rec.hasNamespace = a["namespace"]
		}
		f.mu.Lock()
		f.requests = append(f.requests, rec)
		f.mu.Unlock()
		if f.respond != nil && f.respond(w, r) {
			return
		}
		allowed := f.granted[rec.attrs]
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{"status": map[string]any{"allowed": allowed, "reason": "fake"}})
	}))
	f.Config.ErrorLog = log.New(io.Discard, "", 0)
	f.StartTLS()
	t.Cleanup(f.Close)
	return f
}

func (f *fakeAPI) client(timeout time.Duration) preflight.Client {
	return preflight.Client{APIBase: f.URL, Token: fakeToken, HTTP: f.Client(), Timeout: timeout}
}

func (f *fakeAPI) recordedRequests() []recorded {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.requests)
}

func requireOnlySSARPosts(t *testing.T, f *fakeAPI) {
	t.Helper()
	for _, r := range f.recordedRequests() {
		if r.method != http.MethodPost || r.path != ssarPath {
			t.Fatalf("unexpected request %s %s", r.method, r.path)
		}
	}
}

func TestTableHas14NPlus10ChecksWithLeaseScoping(t *testing.T) {
	for n, want := range map[int]int{1: 24, 2: 38, 3: 52} {
		ns := []string{"pool-a", "pool-b", "pool-c"}[:n]
		if got := len(preflight.Table(ns, controller)); got != want {
			t.Errorf("%d pool namespaces: %d checks, want %d", n, got, want)
		}
	}
	table := preflight.Table(pools, controller)
	counts := map[string]int{}
	for _, r := range table {
		key := r.Namespace + "/" + r.Group + "/" + r.Resource
		counts[key]++
		if r.Resource == "leases" && r.Namespace != controller {
			t.Errorf("Lease check outside controllerNamespace: %+v", r)
		}
		if r.Namespace == controller && r.Resource != "leases" {
			t.Errorf("%s check targets controllerNamespace although it is not a pool namespace", r.Resource)
		}
		if r.Namespace == "" {
			t.Errorf("empty namespace would mean all namespaces: %+v", r)
		}
		if r.Resource == "leases" {
			switch r.Verb {
			case "update", "patch", "delete":
				if r.Name != "falcone-pool-controller-leader" && r.Name != "falcone-pool-controller-scaler" {
					t.Errorf("%s on Leases must carry a fixed Lease name, got %q", r.Verb, r.Name)
				}
			default:
				if r.Name != "" {
					t.Errorf("%s on Leases must be unnamed, got %q", r.Verb, r.Name)
				}
			}
		} else if r.Name != "" {
			t.Errorf("unexpected name on %+v", r)
		}
	}
	want := map[string]int{
		"pool-a/apps/deployments": 7, "pool-a//services": 7,
		"pool-b/apps/deployments": 7, "pool-b//services": 7,
		controller + "/coordination.k8s.io/leases": 10,
	}
	for k, v := range want {
		if counts[k] != v {
			t.Errorf("%s: %d checks, want %d", k, counts[k], v)
		}
	}
}

func TestTableChecksDeploymentsAndServicesInControllerNamespaceWhenItIsAPool(t *testing.T) {
	table := preflight.Table([]string{"pool-a", controller}, controller)
	if len(table) != 38 {
		t.Fatalf("got %d checks, want 38", len(table))
	}
	got := 0
	for _, r := range table {
		if r.Namespace == controller && r.Resource != "leases" {
			got++
		}
	}
	if got != 14 {
		t.Fatalf("controllerNamespace that is also a pool must get 14 Deployment/Service checks, got %d", got)
	}
}

func TestRunReportsEveryAllowedCheckWithTheTableAttributes(t *testing.T) {
	table := preflight.Table(pools, controller)
	api := newFakeAPI(t, grantAll(table))
	checks := api.client(5*time.Second).Run(context.Background(), table)
	if len(checks) != 38 {
		t.Fatalf("got %d checks, want 38", len(checks))
	}
	for i, c := range checks {
		want := table[i]
		got := preflight.Requirement{Namespace: c.Namespace, Group: c.Group, Resource: c.Resource, Verb: c.Verb, Name: c.Name}
		if got != want || c.Result != status.CheckAllowed {
			t.Errorf("check %d = %+v, want %+v allowed", i, c, want)
		}
	}
	reqs := api.recordedRequests()
	if len(reqs) != 38 {
		t.Fatalf("API saw %d requests, want 38", len(reqs))
	}
	requireOnlySSARPosts(t, api)
	for _, r := range reqs {
		if r.auth != "Bearer "+fakeToken {
			t.Fatalf("unexpected Authorization header %q", r.auth)
		}
		if !r.hasNamespace || r.attrs.Namespace == "" {
			t.Fatalf("a review without a namespace asks about all namespaces: %+v", r)
		}
	}
}

func TestRunDeniesExactlyTheMissingGrant(t *testing.T) {
	table := preflight.Table(pools, controller)
	for i, removed := range table {
		granted := grantAll(table)
		delete(granted, removed)
		api := newFakeAPI(t, granted)
		checks := api.client(5*time.Second).Run(context.Background(), table)
		for j, c := range checks {
			want := status.CheckAllowed
			if j == i {
				want = status.CheckDenied
			}
			if c.Result != want {
				t.Fatalf("removing %+v: check %d (%+v) is %s, want %s", removed, j, c, c.Result, want)
			}
		}
		requireOnlySSARPosts(t, api)
		api.Close()
	}
}

func TestRunReportsAnErrorForEveryUnusableResponse(t *testing.T) {
	table := preflight.Table([]string{"pool-a"}, controller)
	redirected := false
	for name, tc := range map[string]struct {
		respond func(w http.ResponseWriter, r *http.Request) bool
		timeout time.Duration
	}{
		"HTTP 403": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			http.Error(w, `{"status":{"allowed":true,"reason":"BODY-MARKER"}}`, http.StatusForbidden)
			return true
		}},
		"HTTP 500": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			http.Error(w, `{"status":{"allowed":true,"reason":"BODY-MARKER"}}`, http.StatusInternalServerError)
			return true
		}},
		"redirect": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			if r.URL.Path != ssarPath {
				redirected = true
			}
			http.Redirect(w, r, "/elsewhere", http.StatusTemporaryRedirect)
			return true
		}},
		"malformed body": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"status": BODY-MARKER`)
			return true
		}},
		"no status.allowed": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"status":{"reason":"x"}}`)
			return true
		}},
		"no status": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{}`)
			return true
		}},
		"oversized body": {respond: func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"status":{"allowed":true,"reason":"`+strings.Repeat("a", 2<<20)+`"}}`)
			return true
		}},
		"delay beyond timeout": {timeout: 50 * time.Millisecond, respond: func(w http.ResponseWriter, r *http.Request) bool {
			select {
			case <-r.Context().Done():
			case <-time.After(3 * time.Second):
			}
			return true
		}},
	} {
		t.Run(name, func(t *testing.T) {
			api := newFakeAPI(t, grantAll(table))
			api.respond = tc.respond
			timeout := tc.timeout
			if timeout == 0 {
				timeout = 5 * time.Second
			}
			checks := api.client(timeout).Run(context.Background(), table)
			if len(checks) != len(table) {
				t.Fatalf("got %d checks, want all %d reported", len(checks), len(table))
			}
			for _, c := range checks {
				if c.Result != status.CheckError || c.Reason == "" {
					t.Fatalf("check %+v must be an error with a reason", c)
				}
				if strings.Contains(c.Reason, fakeToken) || strings.Contains(c.Reason, "BODY-MARKER") {
					t.Fatalf("reason leaks response or token content: %q", c.Reason)
				}
			}
			if redirected {
				t.Fatal("a redirect was followed")
			}
			requireOnlySSARPosts(t, api)
		})
	}
}

func TestRunReportsAnErrorWhenTheServerIsUnreachable(t *testing.T) {
	table := preflight.Table([]string{"pool-a"}, controller)
	api := newFakeAPI(t, grantAll(table))
	c := api.client(time.Second)
	api.Close()
	for _, check := range c.Run(context.Background(), table) {
		if check.Result != status.CheckError {
			t.Fatalf("unreachable API must give error, got %+v", check)
		}
		if strings.Contains(check.Reason, "127.0.0.1") {
			t.Fatalf("reason leaks the address: %q", check.Reason)
		}
	}
}

func TestRunKeepsOnlyAShortCleanAPIReason(t *testing.T) {
	table := preflight.Table([]string{"pool-a"}, controller)[:1]
	api := newFakeAPI(t, nil)
	api.respond = func(w http.ResponseWriter, r *http.Request) bool {
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{"status": map[string]any{"allowed": false, "reason": "no\nmatch\x00" + strings.Repeat("é", 500)}})
		return true
	}
	c := api.client(5*time.Second).Run(context.Background(), table)[0]
	if c.Result != status.CheckDenied {
		t.Fatalf("got %+v", c)
	}
	if len(c.Reason) > 256 || strings.ContainsAny(c.Reason, "\n\x00") || !strings.HasPrefix(c.Reason, "no match") {
		t.Fatalf("reason not clamped and cleaned: %q", c.Reason)
	}
	if json.Valid([]byte(`"`+c.Reason+`"`)) == false {
		t.Fatalf("reason is not valid text: %q", c.Reason)
	}
}

func TestNewHTTPClientTrustsOnlyTheGivenCAAndNeverUsesAProxy(t *testing.T) {
	table := preflight.Table([]string{"pool-a"}, controller)
	api := newFakeAPI(t, grantAll(table))
	caPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: api.Certificate().Raw})
	client, err := preflight.NewHTTPClient(caPEM)
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("HTTPS_PROXY", "http://127.0.0.1:1")
	c := preflight.Client{APIBase: api.URL, Token: fakeToken, HTTP: client, Timeout: 5 * time.Second}
	for _, check := range c.Run(context.Background(), table) {
		if check.Result != status.CheckAllowed {
			t.Fatalf("check through the CA-pinned client: %+v", check)
		}
	}
	// A server whose certificate the CA does not cover is refused, and never sees a request.
	other := newUntrustedTLSServer(t)
	c.APIBase = other.URL
	for _, check := range c.Run(context.Background(), table[:1]) {
		if check.Result != status.CheckError {
			t.Fatalf("untrusted certificate accepted: %+v", check)
		}
	}
	if n := other.hits.Load(); n != 0 {
		t.Fatalf("untrusted server received %d requests", n)
	}
	for _, bad := range [][]byte{nil, []byte("not pem")} {
		if _, err := preflight.NewHTTPClient(bad); err == nil {
			t.Fatalf("CA %q accepted", bad)
		}
	}
}

type untrustedServer struct {
	*httptest.Server
	hits atomic.Int64
}

// newUntrustedTLSServer answers every request with an allowed review, behind a
// certificate generated independently of the httptest default one.
func newUntrustedTLSServer(t *testing.T) *untrustedServer {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(42),
		Subject:      pkix.Name{CommonName: "untrusted"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		IPAddresses:  []net.IP{net.ParseIP("127.0.0.1")},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	s := &untrustedServer{}
	s.Server = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.hits.Add(1)
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{"status": map[string]any{"allowed": true}})
	}))
	s.Config.ErrorLog = log.New(io.Discard, "", 0)
	s.TLS = &tls.Config{Certificates: []tls.Certificate{{Certificate: [][]byte{der}, PrivateKey: key}}}
	s.StartTLS()
	t.Cleanup(s.Close)
	return s
}

func TestClientSendsNoRequestForAnEmptyTable(t *testing.T) {
	api := newFakeAPI(t, nil)
	if checks := api.client(time.Second).Run(context.Background(), nil); len(checks) != 0 {
		t.Fatalf("got %v", checks)
	}
	if n := len(api.recordedRequests()); n != 0 {
		t.Fatalf("%d requests", n)
	}
}

func TestAPIBaseIsHTTPSOnTheServiceHostAndPort(t *testing.T) {
	for _, tc := range []struct{ host, port, want string }{
		{"10.43.0.1", "443", "https://10.43.0.1:443"},
		{"fd00::1", "6443", "https://[fd00::1]:6443"},
	} {
		got, err := preflight.APIBase(tc.host, tc.port)
		if err != nil || got != tc.want {
			t.Errorf("APIBase(%q,%q) = %q, %v; want %q", tc.host, tc.port, got, err, tc.want)
		}
		if u, err := url.Parse(got); err != nil || u.Scheme != "https" {
			t.Errorf("%q is not an https URL", got)
		}
	}
	for _, tc := range [][2]string{{"", "443"}, {"  ", "443"}, {"10.0.0.1", ""}, {"10.0.0.1", "0"}, {"10.0.0.1", "70000"}, {"10.0.0.1", "https"}, {"a/b", "443"}} {
		if got, err := preflight.APIBase(tc[0], tc[1]); err == nil {
			t.Errorf("APIBase(%q,%q) = %q accepted", tc[0], tc[1], got)
		}
	}
}
