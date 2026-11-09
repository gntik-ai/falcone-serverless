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
	"strconv"
	"strings"
	"time"
)

const (
	// DefaultRequestTimeout bounds each Lease call.
	DefaultRequestTimeout = 5 * time.Second

	leasesPath      = "/apis/coordination.k8s.io/v1/namespaces/%s/leases"
	maxResponseSize = 1 << 20
)

// ErrToken means the service-account token could not be read; the call fails closed
// before any request is sent.
var ErrToken = errors.New("service account token is unreadable or empty")

// APIError is an unexpected HTTP status. It carries no response content, so neither
// the token nor object data can reach a log through it.
type APIError struct{ Status int }

func (e *APIError) Error() string { return "kubernetes API returned HTTP " + strconv.Itoa(e.Status) }

// TokenProvider returns the current bearer token. The long-running client calls it for
// every request, so a token Kubernetes rotates in place is picked up.
type TokenProvider func() (string, error)

// FileToken reads the projected token from path on every call. A missing, unreadable
// or blank file is ErrToken.
func FileToken(readFile func(string) ([]byte, error), path string) TokenProvider {
	return func() (string, error) {
		data, err := readFile(path)
		token := strings.TrimSpace(string(data))
		if err != nil || token == "" {
			return "", ErrToken
		}
		return token, nil
	}
}

// Client is the LeaseClient for the in-cluster API server. It talks only to the Lease
// named LeaseName in its own namespace, never follows a redirect and uses no proxy of
// its own.
type Client struct {
	apiBase   string
	namespace string
	token     TokenProvider
	http      http.Client
	timeout   time.Duration
}

var _ LeaseClient = (*Client)(nil)

// NewClient builds the client. namespace must be a DNS-1123 label: the release
// namespace from the downward API, never a pool namespace.
func NewClient(apiBase, namespace string, token TokenProvider, httpClient *http.Client, timeout time.Duration) (*Client, error) {
	if err := ValidateNamespace(namespace); err != nil {
		return nil, err
	}
	base, err := url.Parse(apiBase)
	if err != nil || base.Host == "" || (base.Scheme != "https" && base.Scheme != "http") {
		return nil, errors.New("API base URL is invalid")
	}
	if token == nil || httpClient == nil {
		return nil, errors.New("token provider and HTTP client are required")
	}
	if timeout <= 0 {
		timeout = DefaultRequestTimeout
	}
	c := &Client{apiBase: strings.TrimRight(apiBase, "/"), namespace: namespace, token: token, http: *httpClient, timeout: timeout}
	c.http.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	c.http.Timeout = 0
	return c, nil
}

// wireLease is the JSON shape of a Lease.
type wireLease struct {
	APIVersion string   `json:"apiVersion,omitempty"`
	Kind       string   `json:"kind,omitempty"`
	Metadata   wireMeta `json:"metadata"`
	Spec       wireSpec `json:"spec"`
}

type wireMeta struct {
	Name            string            `json:"name,omitempty"`
	Namespace       string            `json:"namespace,omitempty"`
	UID             string            `json:"uid,omitempty"`
	ResourceVersion string            `json:"resourceVersion,omitempty"`
	Labels          map[string]string `json:"labels,omitempty"`
	Annotations     map[string]string `json:"annotations,omitempty"`
}

type wireSpec struct {
	HolderIdentity       *string `json:"holderIdentity,omitempty"`
	LeaseDurationSeconds *int32  `json:"leaseDurationSeconds,omitempty"`
	AcquireTime          *string `json:"acquireTime,omitempty"`
	RenewTime            *string `json:"renewTime,omitempty"`
	LeaseTransitions     *int32  `json:"leaseTransitions,omitempty"`
}

func fromWire(w wireLease) *Lease {
	l := &Lease{
		Name: w.Metadata.Name, Namespace: w.Metadata.Namespace, UID: w.Metadata.UID,
		ResourceVersion: w.Metadata.ResourceVersion, Labels: w.Metadata.Labels, Annotations: w.Metadata.Annotations,
	}
	if w.Spec.HolderIdentity != nil {
		l.HolderIdentity = *w.Spec.HolderIdentity
	}
	if w.Spec.LeaseDurationSeconds != nil {
		l.LeaseDurationSeconds = *w.Spec.LeaseDurationSeconds
	}
	if w.Spec.AcquireTime != nil {
		l.AcquireTime = *w.Spec.AcquireTime
	}
	if w.Spec.RenewTime != nil {
		l.RenewTime = *w.Spec.RenewTime
	}
	if w.Spec.LeaseTransitions != nil {
		l.LeaseTransitions = *w.Spec.LeaseTransitions
	}
	return l
}

func specOf(l *Lease) wireSpec {
	s := wireSpec{HolderIdentity: &l.HolderIdentity, LeaseDurationSeconds: &l.LeaseDurationSeconds, LeaseTransitions: &l.LeaseTransitions}
	if l.AcquireTime != "" {
		s.AcquireTime = &l.AcquireTime
	}
	if l.RenewTime != "" {
		s.RenewTime = &l.RenewTime
	}
	return s
}

// Get implements LeaseClient.
func (c *Client) Get(ctx context.Context) (*Lease, error) {
	return c.do(ctx, http.MethodGet, "/"+LeaseName, "", nil, http.StatusOK)
}

// Create implements LeaseClient. The Lease is always created in the client's own
// namespace under the fixed name, whatever l says.
func (c *Client) Create(ctx context.Context, l *Lease) (*Lease, error) {
	body, err := json.Marshal(wireLease{
		APIVersion: "coordination.k8s.io/v1", Kind: "Lease",
		Metadata: wireMeta{Name: LeaseName, Namespace: c.namespace, Labels: l.Labels, Annotations: l.Annotations},
		Spec:     specOf(l),
	})
	if err != nil {
		return nil, err
	}
	return c.do(ctx, http.MethodPost, "", "application/json", body, http.StatusCreated, http.StatusOK)
}

// Update implements LeaseClient with a JSON merge patch on the fixed name. The patch
// carries the resourceVersion read with l, so the API server rejects it with 409 when
// anything changed since, and it leaves every other field of the Lease untouched.
func (c *Client) Update(ctx context.Context, l *Lease) (*Lease, error) {
	if l.ResourceVersion == "" {
		return nil, errors.New("update needs the resourceVersion that was read")
	}
	meta := wireMeta{ResourceVersion: l.ResourceVersion}
	if epoch, ok := l.Annotations[EpochAnnotation]; ok {
		meta.Annotations = map[string]string{EpochAnnotation: epoch}
	}
	body, err := json.Marshal(wireLease{Metadata: meta, Spec: specOf(l)})
	if err != nil {
		return nil, err
	}
	return c.do(ctx, http.MethodPatch, "/"+LeaseName, "application/merge-patch+json", body, http.StatusOK)
}

func (c *Client) do(ctx context.Context, method, suffix, contentType string, body []byte, ok ...int) (*Lease, error) {
	token, err := c.token() // reloaded on every call; failing here sends nothing
	if err != nil {
		return nil, ErrToken
	}
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, c.apiBase+fmt.Sprintf(leasesPath, c.namespace)+suffix, bytes.NewReader(body))
	if err != nil {
		return nil, errors.New("request could not be built")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "application/json")
	if contentType != "" {
		req.Header.Set("Content-Type", contentType)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return nil, errors.New("request timed out")
		}
		return nil, errors.New("request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseSize+1))
	if err != nil || len(data) > maxResponseSize {
		return nil, errors.New("response could not be read")
	}
	switch {
	case resp.StatusCode == http.StatusNotFound:
		return nil, ErrNotFound
	case resp.StatusCode == http.StatusConflict && method == http.MethodPost:
		return nil, ErrAlreadyExists
	case resp.StatusCode == http.StatusConflict:
		return nil, ErrConflict
	}
	accepted := false
	for _, code := range ok {
		accepted = accepted || resp.StatusCode == code
	}
	if !accepted {
		return nil, &APIError{Status: resp.StatusCode}
	}
	var w wireLease
	if err := json.Unmarshal(data, &w); err != nil {
		return nil, errors.New("response is not a valid Lease")
	}
	return fromWire(w), nil
}
