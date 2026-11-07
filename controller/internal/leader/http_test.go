package leader_test

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

type transport func(*http.Request) (*http.Response, error)

func (f transport) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }
func TestHTTPLeaseIsolationRotatingTokenAndResourceVersion(t *testing.T) {
	var requests int
	tokens := []string{"first", "second", "third"}
	var stored leader.Lease
	s := &store{}
	c := clock()
	e := elector(t, s, c, "a")
	e.Tick(context.Background())
	stored = copyLease(*s.lease)
	h, err := leader.NewHTTPClient("https://api:443", "release", &http.Client{Transport: transport(func(r *http.Request) (*http.Response, error) {
		if r.URL.Path != "/apis/coordination.k8s.io/v1/namespaces/release/leases/falcone-pool-controller-leader" {
			t.Fatalf("unexpected path %s", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer "+tokens[requests] {
			t.Fatal("token not refreshed")
		}
		requests++
		if _, ok := r.Context().Deadline(); !ok {
			t.Fatal("unbounded request")
		}
		if r.Method == http.MethodPut {
			var l leader.Lease
			if json.NewDecoder(r.Body).Decode(&l) != nil || l.Metadata.ResourceVersion != "1" {
				t.Fatal("missing concurrency precondition")
			}
		}
		b, _ := json.Marshal(stored)
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(string(b))), Header: make(http.Header)}, nil
	})}, func() ([]byte, error) { return []byte(tokens[requests]), nil }, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	l, err := h.Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = h.Update(ctx, l); err != nil {
		t.Fatal(err)
	}
	if _, err = h.Get(ctx); err != nil {
		t.Fatal(err)
	}
	if requests != 3 {
		t.Fatal(requests)
	}
}

func TestHTTPFailuresAreBoundedAndRedacted(t *testing.T) {
	for _, tc := range []struct {
		name     string
		code     int
		body     string
		location string
	}{
		{"missing", 404, "credential-body", ""}, {"conflict", 409, "credential-body", ""}, {"error", 500, "credential-body", ""}, {"invalid", 200, "credential-body", ""}, {"too-large", 200, strings.Repeat("x", (1<<20)+1), ""}, {"redirect", 302, "credential-body", "https://foreign.example/steal"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			h, err := leader.NewHTTPClient("https://api:443", "release", &http.Client{Transport: transport(func(*http.Request) (*http.Response, error) {
				calls++
				return &http.Response{StatusCode: tc.code, Body: io.NopCloser(strings.NewReader(tc.body)), Header: http.Header{"Location": []string{tc.location}}}, nil
			})}, func() ([]byte, error) { return []byte("credential-token"), nil }, time.Second)
			if err != nil {
				t.Fatal(err)
			}
			_, err = h.Get(context.Background())
			if err == nil || strings.Contains(err.Error(), "credential") || calls != 1 {
				t.Fatalf("unbounded/leaking error %v, calls %d", err, calls)
			}
		})
	}
	h, err := leader.NewHTTPClient("https://api:443", "release", &http.Client{Transport: transport(func(r *http.Request) (*http.Response, error) { <-r.Context().Done(); return nil, r.Context().Err() })}, func() ([]byte, error) { return []byte("token"), nil }, time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = h.Get(context.Background()); err == nil {
		t.Fatal("request exceeded deadline")
	}
}

func TestHTTPUpdatePreservesUnknownMetadataAndClearsHolder(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	body, _ := json.Marshal(s.lease)
	var doc map[string]any
	_ = json.Unmarshal(body, &doc)
	doc["metadata"].(map[string]any)["finalizers"] = []string{"operator.example/keep"}
	doc["spec"].(map[string]any)["futureField"] = "keep"
	body, _ = json.Marshal(doc)
	h, err := leader.NewHTTPClient("https://api:443", "release", &http.Client{Transport: transport(func(r *http.Request) (*http.Response, error) {
		if r.Method == http.MethodPut {
			var updated map[string]any
			if json.NewDecoder(r.Body).Decode(&updated) != nil {
				t.Fatal("bad update")
			}
			m := updated["metadata"].(map[string]any)
			spec := updated["spec"].(map[string]any)
			if m["finalizers"] == nil || spec["futureField"] != "keep" || spec["holderIdentity"] != "" {
				t.Fatal("update lost metadata or did not clear holder")
			}
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(string(body))), Header: make(http.Header)}, nil
	})}, func() ([]byte, error) { return []byte("token"), nil }, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	l, err := h.Get(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	l.Spec.HolderIdentity = ""
	if _, err = h.Update(context.Background(), l); err != nil {
		t.Fatal(err)
	}
}
