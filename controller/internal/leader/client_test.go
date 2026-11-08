package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

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

func TestHTTPClientLeaseTimestampsUseKubernetesMicroTime(t *testing.T) {
	for _, tc := range []struct{ name, input, want string }{
		{"whole second", "2026-11-08T14:15:42Z", "2026-11-08T14:15:42.000000Z"},
		{"nanoseconds", "2026-11-08T14:15:42.235373062Z", "2026-11-08T14:15:42.235373Z"},
		{"trailing zeroes on release", "2026-11-08T14:15:42.309900Z", "2026-11-08T14:15:42.309900Z"},
		{"non-UTC input", "2026-11-08T15:15:42.123456789+01:00", "2026-11-08T14:15:42.123456Z"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			body := `{"apiVersion":"coordination.k8s.io/v1","kind":"Lease","metadata":{"namespace":"falcone-system","name":"falcone-pool-controller-leader","uid":"uid-1","resourceVersion":"17"},"spec":{"acquireTime":"` + tc.input + `","renewTime":"` + tc.input + `"}}`
			put := false
			client, err := leader.NewHTTPClient("https://api.example", "test-token", &http.Client{Transport: roundTrip(func(r *http.Request) (*http.Response, error) {
				if r.Method == http.MethodPut {
					put = true
					var wire struct {
						Spec map[string]string `json:"spec"`
					}
					if err := json.NewDecoder(r.Body).Decode(&wire); err != nil {
						t.Fatal(err)
					}
					for _, field := range []string{"acquireTime", "renewTime"} {
						value := wire.Spec[field]
						// Match metav1.MicroTime's fixed-width parser, independently
						// of the controller's own Lease decoder.
						if _, err := time.Parse("2006-01-02T15:04:05.000000Z07:00", value); err != nil {
							t.Errorf("%s is not Kubernetes MicroTime: %q: %v", field, value, err)
						}
						if value != tc.want {
							t.Errorf("%s = %q, want %q", field, value, tc.want)
						}
					}
				}
				return response(http.StatusOK, body), nil
			})})
			if err != nil {
				t.Fatal(err)
			}
			l, err := client.Get(context.Background(), "falcone-system", leader.LeaseName)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := client.Update(context.Background(), "falcone-system", l); err != nil {
				t.Fatal(err)
			}
			if !put {
				t.Fatal("Lease update was not sent")
			}
		})
	}
}

func TestHTTPClientUpdatePreservesOtherLeaseSpecFields(t *testing.T) {
	const body = `{"apiVersion":"coordination.k8s.io/v1","kind":"Lease","metadata":{"namespace":"falcone-system","name":"falcone-pool-controller-leader","uid":"uid-1","resourceVersion":"17"},"spec":{"leaseTransitions":7,"preferredHolder":"pod-b","strategy":"OldestEmulationVersion"}}`
	put := false
	client, err := leader.NewHTTPClient("https://api.example", "test-token", &http.Client{Transport: roundTrip(func(r *http.Request) (*http.Response, error) {
		if r.Method == http.MethodPut {
			put = true
			var wire struct {
				Spec map[string]json.RawMessage `json:"spec"`
			}
			if err := json.NewDecoder(r.Body).Decode(&wire); err != nil {
				t.Fatal(err)
			}
			for field, want := range map[string]string{"leaseTransitions": "7", "preferredHolder": `"pod-b"`, "strategy": `"OldestEmulationVersion"`} {
				if got := string(wire.Spec[field]); got != want {
					t.Errorf("%s = %s, want %s", field, got, want)
				}
			}
		}
		return response(http.StatusOK, body), nil
	})})
	if err != nil {
		t.Fatal(err)
	}
	l, err := client.Get(context.Background(), "falcone-system", leader.LeaseName)
	if err != nil {
		t.Fatal(err)
	}
	l.Spec.HolderIdentity = ""
	if _, err := client.Update(context.Background(), "falcone-system", l); err != nil {
		t.Fatal(err)
	}
	if !put {
		t.Fatal("Lease update was not sent")
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
