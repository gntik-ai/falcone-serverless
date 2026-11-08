package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }
func response(code int, body string) *http.Response {
	return &http.Response{StatusCode: code, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header)}
}

func TestHTTPClientNamespacedLeaseWireContract(t *testing.T) {
	const base = "https://kubernetes.example:443"
	const item = "/apis/coordination.k8s.io/v1/namespaces/falcone-system/leases/" + leader.LeaseName
	var calls []string
	client, err := leader.NewHTTPClient(base, "test-token", &http.Client{Transport: roundTrip(func(r *http.Request) (*http.Response, error) {
		calls = append(calls, r.Method+" "+r.URL.Path)
		if r.Header.Get("Authorization") != "Bearer test-token" {
			t.Fatal("missing authentication")
		}
		if _, ok := r.Context().Deadline(); !ok {
			t.Fatal("unbounded API call")
		}
		if r.Method != "GET" {
			var l leader.Lease
			if err := json.NewDecoder(r.Body).Decode(&l); err != nil {
				t.Fatal(err)
			}
			if l.Metadata.Namespace != "falcone-system" || l.Metadata.Name != leader.LeaseName {
				t.Fatal("wrong Lease identity")
			}
			if r.Method == "PUT" && (l.Metadata.ResourceVersion != "17" || l.Metadata.UID != "uid-1") {
				t.Fatal("missing optimistic concurrency")
			}
		}
		return response(200, `{"apiVersion":"coordination.k8s.io/v1","kind":"Lease","metadata":{"namespace":"falcone-system","name":"falcone-pool-controller-leader","uid":"uid-1","resourceVersion":"17"}}`), nil
	})})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	l, err := client.Get(ctx, "falcone-system", leader.LeaseName)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.Update(ctx, "falcone-system", l); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Create(ctx, "falcone-system", l); err != nil {
		t.Fatal(err)
	}
	want := []string{"GET " + item, "PUT " + item, "POST " + strings.TrimSuffix(item, "/"+leader.LeaseName)}
	if strings.Join(calls, "\n") != strings.Join(want, "\n") {
		t.Fatalf("calls: %v", calls)
	}
}

func TestHTTPFailuresAreBoundedAndRedacted(t *testing.T) {
	for _, code := range []int{404, 409, 403, 500, 302} {
		t.Run(http.StatusText(code), func(t *testing.T) {
			client, err := leader.NewHTTPClient("https://api.example", "sensitive-token", &http.Client{Transport: roundTrip(func(*http.Request) (*http.Response, error) { return response(code, "sensitive-token"), nil })})
			if err != nil {
				t.Fatal(err)
			}
			_, err = client.Get(context.Background(), "falcone-system", leader.LeaseName)
			if err == nil || strings.Contains(err.Error(), "sensitive-token") {
				t.Fatalf("unredacted or absent error: %v", err)
			}
			if code == 404 && !errors.Is(err, leader.ErrNotFound) {
				t.Fatal(err)
			}
			if code == 409 && !errors.Is(err, leader.ErrConflict) {
				t.Fatal(err)
			}
		})
	}
	for _, base := range []string{"http://api.example", "https://user@api.example", "https://api.example/prefix", "https://api.example?x=y"} {
		if _, err := leader.NewHTTPClient(base, "token", http.DefaultClient); err == nil {
			t.Fatalf("accepted unsafe API base %q", base)
		}
	}
}
