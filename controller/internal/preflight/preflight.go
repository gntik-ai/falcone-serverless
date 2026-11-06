// Package preflight proves the controller's namespaced authority before it acts.
// It asks the Kubernetes API, with SelfSubjectAccessReviews only, whether the
// controller's own identity holds the verbs ctr-controller-config grants it. It
// never writes, creates or adopts anything.
package preflight

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

// Fixed Lease names from ctr-resource-ownership and ctr-controller-config.
const (
	LeaderLeaseName = "falcone-pool-controller-leader"
	ScalerLeaseName = "falcone-pool-controller-scaler"
)

const (
	// DefaultTimeout bounds each access review.
	DefaultTimeout = 10 * time.Second

	reviewPath      = "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews"
	maxResponseSize = 1 << 20
	maxReasonBytes  = 200
	concurrency     = 8
)

// Requirement is one access the controller must hold. Group is empty for the core
// API group; Name restricts the review to one object name when set.
type Requirement struct {
	Namespace string
	Group     string
	Resource  string
	Verb      string
	Name      string
}

var (
	poolVerbs        = []string{"get", "list", "watch", "create", "update", "patch", "delete"}
	leaseUnnamedVerb = []string{"get", "list", "watch", "create"}
	leaseNamedVerbs  = []string{"update", "patch", "delete"}
	leaseNames       = []string{LeaderLeaseName, ScalerLeaseName}
)

// Table is the one list of checks the preflight runs, 14N+10 for N pool namespaces.
// Each pool namespace needs the Deployment and Service write set (14). The
// controllerNamespace alone needs Leases: get, list, watch and create unnamed, and
// update, patch and delete for each fixed Lease name (10). ConfigMap, Secret and
// NetworkPolicy reads are deferred to the "OPS-01 preflight read checks" follow-up.
func Table(poolNamespaces []string, controllerNamespace string) []Requirement {
	var table []Requirement
	for _, ns := range poolNamespaces {
		for _, kind := range []struct{ group, resource string }{{"apps", "deployments"}, {"", "services"}} {
			for _, verb := range poolVerbs {
				table = append(table, Requirement{Namespace: ns, Group: kind.group, Resource: kind.resource, Verb: verb})
			}
		}
	}
	const leases = "coordination.k8s.io"
	for _, verb := range leaseUnnamedVerb {
		table = append(table, Requirement{Namespace: controllerNamespace, Group: leases, Resource: "leases", Verb: verb})
	}
	for _, name := range leaseNames {
		for _, verb := range leaseNamedVerbs {
			table = append(table, Requirement{Namespace: controllerNamespace, Group: leases, Resource: "leases", Verb: verb, Name: name})
		}
	}
	return table
}

// APIBase builds the in-cluster API server URL from KUBERNETES_SERVICE_HOST and
// KUBERNETES_SERVICE_PORT.
func APIBase(host, port string) (string, error) {
	host = strings.TrimSpace(host)
	if host == "" || strings.ContainsAny(host, "/?#@ ") {
		return "", errors.New("KUBERNETES_SERVICE_HOST is unset or invalid")
	}
	if n, err := strconv.Atoi(strings.TrimSpace(port)); err != nil || n < 1 || n > 65535 {
		return "", errors.New("KUBERNETES_SERVICE_PORT is unset or invalid")
	}
	return "https://" + net.JoinHostPort(host, strings.TrimSpace(port)), nil
}

// NewHTTPClient returns a client that trusts only caPEM, never uses an environment
// proxy and never follows a redirect.
func NewHTTPClient(caPEM []byte) (*http.Client, error) {
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(caPEM) {
		return nil, errors.New("service account CA holds no certificate")
	}
	return &http.Client{
		Transport: &http.Transport{
			Proxy:           nil,
			TLSClientConfig: &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12},
		},
		CheckRedirect: noRedirect,
	}, nil
}

func noRedirect(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }

// Client sends access reviews to the API server at APIBase with the bearer Token.
type Client struct {
	APIBase string
	Token   string
	HTTP    *http.Client
	Timeout time.Duration // per request; DefaultTimeout when zero
}

// Run reviews every requirement and returns one check per requirement, in order.
// It does not stop at the first failure.
func (c Client) Run(ctx context.Context, reqs []Requirement) []status.Check {
	checks := make([]status.Check, len(reqs))
	httpClient := *c.HTTP
	httpClient.CheckRedirect = noRedirect
	httpClient.Timeout = 0
	var wg sync.WaitGroup
	slots := make(chan struct{}, concurrency)
	for i, req := range reqs {
		wg.Add(1)
		slots <- struct{}{}
		go func() {
			defer wg.Done()
			defer func() { <-slots }()
			checks[i] = c.review(ctx, &httpClient, req)
		}()
	}
	wg.Wait()
	return checks
}

// review asks one SelfSubjectAccessReview. Failure reasons are fixed strings so
// that neither the token nor response content reaches the report.
func (c Client) review(ctx context.Context, httpClient *http.Client, req Requirement) status.Check {
	check := status.Check{Namespace: req.Namespace, Group: req.Group, Resource: req.Resource, Verb: req.Verb, Name: req.Name}
	fail := func(reason string) status.Check {
		check.Result, check.Reason = status.CheckError, reason
		return check
	}
	attrs := map[string]string{"namespace": req.Namespace, "verb": req.Verb, "group": req.Group, "resource": req.Resource}
	if req.Name != "" {
		attrs["name"] = req.Name
	}
	body, err := json.Marshal(map[string]any{
		"apiVersion": "authorization.k8s.io/v1",
		"kind":       "SelfSubjectAccessReview",
		"spec":       map[string]any{"resourceAttributes": attrs},
	})
	if err != nil {
		return fail("request could not be built")
	}
	timeout := c.Timeout
	if timeout <= 0 {
		timeout = DefaultTimeout
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, c.APIBase+reviewPath, bytes.NewReader(body))
	if err != nil {
		return fail("request could not be built")
	}
	httpReq.Header.Set("Authorization", "Bearer "+c.Token)
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "application/json")
	resp, err := httpClient.Do(httpReq)
	if err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return fail("request timed out")
		}
		return fail("request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, maxResponseSize))
		return fail("HTTP " + strconv.Itoa(resp.StatusCode))
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseSize+1))
	if err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return fail("request timed out")
		}
		return fail("response could not be read")
	}
	if len(data) > maxResponseSize {
		return fail("response is too large")
	}
	var review struct {
		Status *struct {
			Allowed *bool  `json:"allowed"`
			Reason  string `json:"reason"`
		} `json:"status"`
	}
	if err := json.Unmarshal(data, &review); err != nil {
		return fail("response is not valid JSON")
	}
	if review.Status == nil || review.Status.Allowed == nil {
		return fail("response has no status.allowed")
	}
	check.Reason = shortReason(review.Status.Reason)
	check.Result = status.CheckDenied
	if *review.Status.Allowed {
		check.Result = status.CheckAllowed
	}
	return check
}

// shortReason keeps the API's own status.reason, cleaned of control characters
// and cut to maxReasonBytes on a rune boundary.
func shortReason(reason string) string {
	reason = strings.Map(func(r rune) rune {
		if r < ' ' || r == 0x7f || r == utf8.RuneError {
			return ' '
		}
		return r
	}, reason)
	reason = strings.TrimSpace(reason)
	if len(reason) > maxReasonBytes {
		cut := maxReasonBytes
		for cut > 0 && !utf8.RuneStart(reason[cut]) {
			cut--
		}
		reason = reason[:cut]
	}
	return reason
}
