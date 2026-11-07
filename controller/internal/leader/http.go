package leader

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
)

const maxBody = 1 << 20

// HTTPClient uses only get/create/update on the one fixed leader Lease. The
// supplied HTTP client is constructed by preflight.NewHTTPClient from the CA.
// Token bytes are re-read per request and never retained on the client.
type HTTPClient struct {
	base    string
	http    *http.Client
	token   func() ([]byte, error)
	timeout time.Duration
}

func NewHTTPClient(base, namespace string, client *http.Client, token func() ([]byte, error), timeout time.Duration) (*HTTPClient, error) {
	u, err := url.Parse(base)
	if err != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || u.Path != "" || !preflight.ValidNamespace(namespace) || client == nil || token == nil || timeout <= 0 {
		return nil, errors.New("invalid lease client configuration")
	}
	h := *client
	h.Timeout = 0
	h.CheckRedirect = func(*http.Request, []*http.Request) error { return errors.New("lease redirect refused") }
	if tr, ok := h.Transport.(*http.Transport); ok {
		tr = tr.Clone()
		tr.Proxy = nil
		h.Transport = tr
	}
	return &HTTPClient{base: base + "/apis/coordination.k8s.io/v1/namespaces/" + namespace + "/leases", http: &h, token: token, timeout: timeout}, nil
}
func (c *HTTPClient) Get(ctx context.Context) (Lease, error) {
	return c.request(ctx, http.MethodGet, nil)
}
func (c *HTTPClient) Create(ctx context.Context, l Lease) (Lease, error) {
	if l.Metadata.ResourceVersion != "" {
		return Lease{}, errors.New("invalid lease create")
	}
	return c.request(ctx, http.MethodPost, &l)
}
func (c *HTTPClient) Update(ctx context.Context, l Lease) (Lease, error) {
	if l.Metadata.ResourceVersion == "" {
		return Lease{}, errors.New("lease resourceVersion required")
	}
	return c.request(ctx, http.MethodPut, &l)
}

// encodeLease retains unrecognized server fields while replacing the fields the
// elector controls, including an explicitly empty holder on graceful release.
func encodeLease(l Lease) ([]byte, error) {
	data, err := json.Marshal(l)
	if err != nil {
		return nil, errors.New("lease encoding failed")
	}
	if l.Raw == nil {
		return data, nil
	}
	var current map[string]json.RawMessage
	_ = json.Unmarshal(data, &current)
	merged := make(map[string]json.RawMessage, len(l.Raw))
	for k, v := range l.Raw {
		merged[k] = v
	}
	for k, v := range current {
		if k == "metadata" || k == "spec" {
			var fields map[string]json.RawMessage
			_ = json.Unmarshal(merged[k], &fields)
			if fields == nil {
				fields = map[string]json.RawMessage{}
			}
			var updates map[string]json.RawMessage
			_ = json.Unmarshal(v, &updates)
			for key, value := range updates {
				fields[key] = value
			}
			v, err = json.Marshal(fields)
			if err != nil {
				return nil, errors.New("lease encoding failed")
			}
		}
		merged[k] = v
	}
	return json.Marshal(merged)
}
func (c *HTTPClient) request(ctx context.Context, method string, l *Lease) (Lease, error) {
	fail := func() (Lease, error) { return Lease{}, errors.New("lease request failed") }
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var body []byte
	var err error
	if l != nil {
		body, err = encodeLease(*l)
		if err != nil || len(body) > maxBody {
			return fail()
		}
	}
	token, err := c.token()
	if err != nil || len(token) == 0 || len(token) > 16384 {
		return fail()
	}
	bearer := strings.TrimSpace(string(token))
	if bearer == "" || strings.ContainsAny(bearer, "\r\n\t ") {
		return fail()
	}
	path := c.base
	if method != http.MethodPost {
		path += "/" + preflight.LeaderLeaseName
	}
	req, err := http.NewRequestWithContext(ctx, method, path, bytes.NewReader(body))
	if err != nil {
		return fail()
	}
	req.Header.Set("Authorization", "Bearer "+bearer)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return fail()
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode == http.StatusNotFound && method == http.MethodGet {
		return Lease{}, ErrNotFound
	}
	if resp.StatusCode == http.StatusConflict {
		return Lease{}, ErrConflict
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return fail()
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxBody+1))
	if err != nil || len(data) > maxBody {
		return fail()
	}
	var result Lease
	if json.Unmarshal(data, &result) != nil {
		return fail()
	}
	if json.Unmarshal(data, &result.Raw) != nil || result.Raw == nil {
		return fail()
	}
	return result, nil
}
