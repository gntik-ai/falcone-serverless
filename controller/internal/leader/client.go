package leader

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

// HTTPClient uses the service-account transport constructed by preflight. It
// never follows redirects or exposes response bodies, URLs or credentials in errors.
type HTTPClient struct {
	base, token string
	http        *http.Client
}

func NewHTTPClient(base, token string, client *http.Client) (*HTTPClient, error) {
	u, err := url.Parse(base)
	if err != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || u.Path != "" || u.RawQuery != "" || u.Fragment != "" || u.Opaque != "" || client == nil || token == "" {
		return nil, errors.New("invalid Lease API configuration")
	}
	copyClient := *client
	copyClient.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	copyClient.Timeout = 0 // Each request has its own bounded context below.
	return &HTTPClient{base: base, token: token, http: &copyClient}, nil
}
func (c *HTTPClient) Get(ctx context.Context, namespace, name string) (Lease, error) {
	if name != LeaseName {
		return Lease{}, ErrOwnership
	}
	return c.request(ctx, http.MethodGet, namespace, true, nil)
}
func (c *HTTPClient) Create(ctx context.Context, namespace string, l Lease) (Lease, error) {
	if l.Metadata.Namespace != namespace || l.Metadata.Name != LeaseName {
		return Lease{}, ErrOwnership
	}
	return c.request(ctx, http.MethodPost, namespace, false, &l)
}
func (c *HTTPClient) Update(ctx context.Context, namespace string, l Lease) (Lease, error) {
	if l.Metadata.Namespace != namespace || l.Metadata.Name != LeaseName || l.Metadata.UID == "" || l.Metadata.ResourceVersion == "" {
		return Lease{}, ErrOwnership
	}
	return c.request(ctx, http.MethodPut, namespace, true, &l)
}
func (c *HTTPClient) request(ctx context.Context, method, namespace string, item bool, l *Lease) (Lease, error) {
	if len(namespace) == 0 || len(namespace) > 63 || !dnsLabel.MatchString(namespace) {
		return Lease{}, ErrOwnership
	}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	path := "/apis/coordination.k8s.io/v1/namespaces/" + namespace + "/leases"
	if item {
		path += "/" + LeaseName
	}
	var body []byte
	if l != nil {
		var err error
		body, err = json.Marshal(l)
		if err != nil {
			return Lease{}, errors.New("Lease request encoding failed")
		}
	}
	req, err := http.NewRequestWithContext(ctx, method, c.base+path, bytes.NewReader(body))
	if err != nil {
		return Lease{}, errors.New("Lease request construction failed")
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return Lease{}, errors.New("Lease API request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode == http.StatusNotFound {
		return Lease{}, ErrNotFound
	}
	if resp.StatusCode == http.StatusConflict {
		return Lease{}, ErrConflict
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return Lease{}, errors.New("Lease API HTTP " + strconv.Itoa(resp.StatusCode))
	}
	const limit = 1 << 20
	data, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil || len(data) > limit {
		return Lease{}, errors.New("Lease API response unreadable or too large")
	}
	var out Lease
	if json.Unmarshal(data, &out) != nil {
		return Lease{}, errors.New("Lease API response invalid")
	}
	return out, nil
}
