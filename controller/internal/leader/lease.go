// Package leader elects one controller replica to run the singleton duties. It
// holds the Lease falcone-pool-controller-leader in the controller's own release
// namespace through the coordination.k8s.io/v1 API, with the standard library only.
//
// Safety rests on local monotonic readings, never on timestamps in the Lease. A
// replica that cannot prove how long ago the previous holder last renewed waits a
// full lease duration before it writes (the recovery barrier in leader.go).
package leader

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"strconv"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
)

// LeaseName is the only Lease this package reads or writes (ctr-resource-ownership).
// No configuration selects another name or namespace.
const LeaseName = preflight.LeaderLeaseName

// Ownership metadata every Lease this controller creates carries, and an existing
// Lease must carry before it is taken over (docs/contracts/resource-ownership.md).
const (
	OwnerKey          = "falcone.ai/owner"
	OwnerValue        = "pool-controller"
	VersionKey        = "falcone.ai/ownership-version"
	VersionValue      = "1"
	maxResponseSize   = 1 << 20
	microTimeLayout   = "2006-01-02T15:04:05.000000Z07:00"
	leasePathTemplate = "/apis/coordination.k8s.io/v1/namespaces/%s/leases"
)

// Errors a Client returns for definite answers from the API. Any other error is an
// API failure (unreachable, timeout, 401, 403, 5xx, redirect, malformed body) and
// opens nothing.
var (
	ErrNotFound = errors.New("lease not found")
	ErrConflict = errors.New("lease conflict")
)

// Lease is the part of coordination.k8s.io/v1 Lease this package uses. AcquireTime
// and RenewTime are informational: no decision reads them.
type Lease struct {
	Name                 string
	UID                  string
	ResourceVersion      string
	Labels               map[string]string
	Annotations          map[string]string
	Holder               string
	LeaseDurationSeconds int64
	AcquireTime          string
	RenewTime            string
	LeaseTransitions     int64
}

// Client reads and writes the one election Lease. Each call must return within a
// bounded time or when ctx ends. The namespace is fixed when the client is built.
type Client interface {
	Get(ctx context.Context) (Lease, error)
	Create(ctx context.Context, l Lease) (Lease, error)
	Update(ctx context.Context, l Lease) (Lease, error)
}

var dnsLabel = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

func validNamespace(ns string) bool { return len(ns) <= 63 && dnsLabel.MatchString(ns) }

// HTTPClient is a Client for the Kubernetes REST API. Build it with NewHTTPClient.
type HTTPClient struct {
	apiBase   string
	namespace string
	token     func() (string, error)
	http      *http.Client
	timeout   time.Duration
}

// NewHTTPClient returns a client for the Lease in namespace. token is called for
// every request because the projected service account token rotates; its value is
// never logged. httpClient should come from preflight.NewHTTPClient (no proxy, no
// redirects). timeout bounds each request and must be positive.
func NewHTTPClient(apiBase, namespace string, token func() (string, error), httpClient *http.Client, timeout time.Duration) (*HTTPClient, error) {
	switch {
	case !validNamespace(namespace):
		return nil, errors.New("lease namespace is not a DNS-1123 label")
	case apiBase == "" || token == nil || httpClient == nil:
		return nil, errors.New("lease client needs an API base, a token source and an HTTP client")
	case timeout <= 0:
		return nil, errors.New("lease request timeout must be positive")
	}
	c := *httpClient
	c.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	c.Timeout = 0
	return &HTTPClient{apiBase: apiBase, namespace: namespace, token: token, http: &c, timeout: timeout}, nil
}

func (c *HTTPClient) path(withName bool) string {
	p := fmt.Sprintf(leasePathTemplate, url.PathEscape(c.namespace))
	if withName {
		p += "/" + url.PathEscape(LeaseName)
	}
	return p
}

// Get implements Client.
func (c *HTTPClient) Get(ctx context.Context) (Lease, error) {
	return c.do(ctx, http.MethodGet, c.path(true), nil, http.StatusOK)
}

// Create implements Client. A 409 is ErrConflict.
func (c *HTTPClient) Create(ctx context.Context, l Lease) (Lease, error) {
	body, err := c.encode(l, false)
	if err != nil {
		return Lease{}, err
	}
	return c.do(ctx, http.MethodPost, c.path(false), body, http.StatusCreated, http.StatusOK)
}

// Update implements Client. It sends metadata.resourceVersion (and uid when known)
// so a concurrent writer makes it fail with ErrConflict; 404 is ErrNotFound.
func (c *HTTPClient) Update(ctx context.Context, l Lease) (Lease, error) {
	if l.ResourceVersion == "" {
		return Lease{}, errors.New("update needs a resourceVersion")
	}
	body, err := c.encode(l, true)
	if err != nil {
		return Lease{}, err
	}
	return c.do(ctx, http.MethodPut, c.path(true), body, http.StatusOK)
}

type wireMeta struct {
	Name            string            `json:"name"`
	Namespace       string            `json:"namespace"`
	UID             string            `json:"uid,omitempty"`
	ResourceVersion string            `json:"resourceVersion,omitempty"`
	Labels          map[string]string `json:"labels,omitempty"`
	Annotations     map[string]string `json:"annotations,omitempty"`
}

type wireSpec struct {
	HolderIdentity       *string `json:"holderIdentity"`
	LeaseDurationSeconds *int64  `json:"leaseDurationSeconds,omitempty"`
	AcquireTime          *string `json:"acquireTime"`
	RenewTime            *string `json:"renewTime"`
	LeaseTransitions     *int64  `json:"leaseTransitions"`
}

type wireLease struct {
	APIVersion string   `json:"apiVersion"`
	Kind       string   `json:"kind"`
	Metadata   wireMeta `json:"metadata"`
	Spec       wireSpec `json:"spec"`
}

func optional(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}

func (c *HTTPClient) encode(l Lease, update bool) ([]byte, error) {
	w := wireLease{
		APIVersion: "coordination.k8s.io/v1",
		Kind:       "Lease",
		Metadata:   wireMeta{Name: LeaseName, Namespace: c.namespace, Labels: l.Labels, Annotations: l.Annotations},
	}
	holder, transitions := l.Holder, l.LeaseTransitions
	w.Spec = wireSpec{HolderIdentity: &holder, AcquireTime: optional(l.AcquireTime), RenewTime: optional(l.RenewTime), LeaseTransitions: &transitions}
	if l.LeaseDurationSeconds > 0 {
		w.Spec.LeaseDurationSeconds = &l.LeaseDurationSeconds
	}
	if update {
		w.Metadata.UID, w.Metadata.ResourceVersion = l.UID, l.ResourceVersion
	}
	return json.Marshal(w)
}

func (c *HTTPClient) do(ctx context.Context, method, path string, body []byte, okStatus ...int) (Lease, error) {
	token, err := c.token()
	if err != nil || token == "" {
		return Lease{}, errors.New("service account token is unavailable")
	}
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.apiBase+path, reader)
	if err != nil {
		return Lease{}, errors.New("lease request could not be built")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "application/json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return Lease{}, errors.New("lease request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseSize+1))
	switch {
	case resp.StatusCode == http.StatusNotFound:
		return Lease{}, ErrNotFound
	case resp.StatusCode == http.StatusConflict:
		return Lease{}, ErrConflict
	}
	okay := false
	for _, s := range okStatus {
		okay = okay || resp.StatusCode == s
	}
	if !okay {
		return Lease{}, errors.New("lease request returned HTTP " + strconv.Itoa(resp.StatusCode))
	}
	if err != nil || len(data) > maxResponseSize {
		return Lease{}, errors.New("lease response could not be read or is too large")
	}
	return c.decode(data)
}

func (c *HTTPClient) decode(data []byte) (Lease, error) {
	var w wireLease
	if err := json.Unmarshal(data, &w); err != nil {
		return Lease{}, errors.New("lease response is not valid JSON")
	}
	m := w.Metadata
	if w.Kind != "Lease" || m.Name != LeaseName || (m.Namespace != "" && m.Namespace != c.namespace) || m.ResourceVersion == "" || m.UID == "" {
		return Lease{}, errors.New("lease response is not the expected Lease")
	}
	l := Lease{Name: m.Name, UID: m.UID, ResourceVersion: m.ResourceVersion, Labels: m.Labels, Annotations: m.Annotations}
	s := w.Spec
	if s.HolderIdentity != nil {
		l.Holder = *s.HolderIdentity
	}
	if s.LeaseDurationSeconds != nil && *s.LeaseDurationSeconds > 0 {
		l.LeaseDurationSeconds = *s.LeaseDurationSeconds
	}
	if s.AcquireTime != nil {
		l.AcquireTime = *s.AcquireTime
	}
	if s.RenewTime != nil {
		l.RenewTime = *s.RenewTime
	}
	if s.LeaseTransitions != nil && *s.LeaseTransitions > 0 {
		l.LeaseTransitions = *s.LeaseTransitions
	}
	return l, nil
}
