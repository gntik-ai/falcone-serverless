package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/signal"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

const (
	pfToken      = "fake-sa-token-0123456789"
	pfTokenPath  = "/var/run/secrets/kubernetes.io/serviceaccount/token"
	pfValuesPath = "/etc/falcone-serverless/values.json"
	pfController = "falcone-system"
	pfSSARPath   = "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews"
)

var pfPools = []string{"pool-a", "pool-b"}

func TestVersionIsSet(t *testing.T) {
	if Version == "" {
		t.Fatal("Version must not be empty")
	}
}

// pfAPI is a recording fake Kubernetes API answering SelfSubjectAccessReviews.
type pfAPI struct {
	*httptest.Server
	mu       sync.Mutex
	requests []pfRequest
	granted  map[preflight.Requirement]bool
	respond  func(w http.ResponseWriter, r *http.Request) bool
}

type pfRequest struct {
	method, path string
	attrs        preflight.Requirement
	hasNamespace bool
}

func newPFAPI(t *testing.T, table []preflight.Requirement) *pfAPI {
	t.Helper()
	f := &pfAPI{granted: map[preflight.Requirement]bool{}}
	for _, r := range table {
		f.granted[r] = true
	}
	f.Server = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		rec := pfRequest{method: r.Method, path: r.URL.Path}
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
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{"status": map[string]any{"allowed": f.granted[rec.attrs]}})
	}))
	f.Config.ErrorLog = log.New(io.Discard, "", 0)
	f.StartTLS()
	t.Cleanup(f.Close)
	return f
}

func (f *pfAPI) seen() []pfRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.requests)
}

// pfEnv is the process environment and filesystem of one run() call.
type pfEnv struct {
	env    map[string]string
	files  map[string][]byte
	calls  int
	client *http.Client
	ctx    context.Context
	out    bytes.Buffer
	api    *pfAPI
}

func pfValues(edit func(m map[string]any)) []byte {
	m := map[string]any{
		"watchedPoolNamespaces": pfPools,
		"controllerNamespace":   pfController,
		"prerequisites": map[string]any{
			"gatewayRoute":             map[string]any{"name": "gw"},
			"storage":                  map[string]any{"name": "st"},
			"broker":                   map[string]any{"name": "br"},
			"rbac":                     map[string]any{"controllerRoleBinding": "rb"},
			"networkPolicyEnforcement": map[string]any{"name": "np"},
			"existingSCC":              map[string]any{"name": "scc"},
		},
		"unrelated": map[string]any{"chart": "value"},
	}
	if edit != nil {
		edit(m)
	}
	out, err := json.Marshal(m)
	if err != nil {
		panic(err)
	}
	return out
}

func pfNewEnv(t *testing.T, values []byte) *pfEnv {
	t.Helper()
	table := preflight.Table(pfPools, pfController)
	api := newPFAPI(t, table)
	host, port, err := net.SplitHostPort(api.Listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	return &pfEnv{
		api: api,
		env: map[string]string{"POD_NAMESPACE": pfController, "KUBERNETES_SERVICE_HOST": host, "KUBERNETES_SERVICE_PORT": port},
		files: map[string][]byte{
			pfValuesPath: values,
			pfTokenPath:  []byte(pfToken + "\n"),
		},
	}
}

func (e *pfEnv) run() int {
	client := e.client
	if client == nil {
		client = e.api.Client()
	}
	ctx := e.ctx
	if ctx == nil {
		var cancel context.CancelFunc
		ctx, cancel = context.WithCancel(context.Background())
		cancel()
	}
	return run(deps{ctx: ctx,
		getenv: func(k string) string { return e.env[k] },
		readFile: func(name string) ([]byte, error) {
			if data, ok := e.files[name]; ok {
				return data, nil
			}
			return nil, &fs.PathError{Op: "open", Path: name, Err: fs.ErrNotExist}
		},
		client:  client,
		timeout: 2 * time.Second,
		start:   func() { e.calls++ },
		stdout:  &e.out,
	})
}

// report decodes the single JSON report on stdout and checks the invariants that
// hold for every run: one report, valid against the contract, no token.
func (e *pfEnv) report(t *testing.T) status.Report {
	t.Helper()
	if strings.Contains(e.out.String(), pfToken) {
		t.Fatal("the token reached stdout")
	}
	dec := json.NewDecoder(bytes.NewReader(e.out.Bytes()))
	var raw json.RawMessage
	if err := dec.Decode(&raw); err != nil {
		t.Fatalf("stdout holds no JSON report: %v\n%s", err, e.out.String())
	}
	if dec.More() {
		t.Fatalf("stdout holds more than one JSON value:\n%s", e.out.String())
	}
	if err := status.ValidateJSON(raw); err != nil {
		t.Fatalf("report violates pool-preflight/v1: %v", err)
	}
	var r status.Report
	if err := json.Unmarshal(raw, &r); err != nil {
		t.Fatal(err)
	}
	return r
}

func (e *pfEnv) requireOnlySSARPosts(t *testing.T) {
	t.Helper()
	for _, r := range e.api.seen() {
		if r.method != http.MethodPost || r.path != pfSSARPath {
			t.Fatalf("unexpected API request %s %s", r.method, r.path)
		}
		if !r.hasNamespace || r.attrs.Namespace == "" {
			t.Fatalf("a review without a namespace asks about all namespaces: %+v", r)
		}
	}
}

func (e *pfEnv) requireFailedBeforeAnyRequest(t *testing.T, code int) status.Report {
	t.Helper()
	r := e.report(t)
	if code != 1 || r.Preflight.Result != status.ResultFailed || e.calls != 0 || len(e.api.seen()) != 0 {
		t.Fatalf("exit %d, result %s, hook calls %d, requests %d; want exit 1, failed, 0, 0", code, r.Preflight.Result, e.calls, len(e.api.seen()))
	}
	if r.Preflight.ConfigError == "" || len(r.Preflight.Checks) != 0 {
		t.Fatalf("a configuration failure reports configError and no checks: %+v", r.Preflight)
	}
	return r
}

func pfKinds(r status.Report) map[string][]status.Prerequisite {
	out := map[string][]status.Prerequisite{}
	for _, p := range r.Prerequisites {
		out[p.Kind] = append(out[p.Kind], p)
	}
	return out
}

func TestRunPassesWithFullGrants(t *testing.T) {
	e := pfNewEnv(t, pfValues(nil))
	code := e.run()
	r := e.report(t)
	if code != 0 || r.Preflight.Result != status.ResultPassed || e.calls != 1 {
		t.Fatalf("exit %d, result %s, hook calls %d; want 0, passed, 1", code, r.Preflight.Result, e.calls)
	}
	table := preflight.Table(pfPools, pfController)
	if len(table) != 38 || len(r.Preflight.Checks) != 38 {
		t.Fatalf("table %d, reported %d checks, want 38", len(table), len(r.Preflight.Checks))
	}
	for i, c := range r.Preflight.Checks {
		got := preflight.Requirement{Namespace: c.Namespace, Group: c.Group, Resource: c.Resource, Verb: c.Verb, Name: c.Name}
		if got != table[i] || c.Result != status.CheckAllowed {
			t.Errorf("check %d = %+v, want %+v allowed", i, c, table[i])
		}
	}
	if len(e.api.seen()) != 38 {
		t.Fatalf("API saw %d requests, want 38", len(e.api.seen()))
	}
	e.requireOnlySSARPosts(t)
	kinds := pfKinds(r)
	if len(kinds) != 7 || len(kinds[status.KindNamespace]) != 3 {
		t.Fatalf("want 7 kinds and 3 namespace entries, got %v", kinds)
	}
	for _, p := range r.Prerequisites {
		if p.State != status.StateDeclared {
			t.Errorf("%+v must be declared", p)
		}
	}
}

func TestRunClaimsNoVerification(t *testing.T) {
	e := pfNewEnv(t, pfValues(nil))
	e.run()
	var doc any
	if err := json.Unmarshal(e.out.Bytes(), &doc); err != nil {
		t.Fatal(err)
	}
	var walk func(v any)
	walk = func(v any) {
		switch v := v.(type) {
		case map[string]any:
			for k, child := range v {
				if strings.Contains(strings.ToLower(k), "verif") {
					t.Errorf("report key %q claims verification", k)
				}
				walk(child)
			}
		case []any:
			for _, child := range v {
				walk(child)
			}
		case string:
			if v == "verified" {
				t.Error("report value claims verification")
			}
		}
	}
	walk(doc)
}

func TestRunFailsWhenAnySingleGrantIsRemoved(t *testing.T) {
	table := preflight.Table(pfPools, pfController)
	for i, removed := range table {
		e := pfNewEnv(t, pfValues(nil))
		delete(e.api.granted, removed)
		code := e.run()
		r := e.report(t)
		if code != 1 || r.Preflight.Result != status.ResultFailed || e.calls != 0 {
			t.Fatalf("removing %+v: exit %d, result %s, hook calls %d", removed, code, r.Preflight.Result, e.calls)
		}
		if len(r.Preflight.Checks) != 38 {
			t.Fatalf("removing %+v: %d checks reported, want all 38", removed, len(r.Preflight.Checks))
		}
		for j, c := range r.Preflight.Checks {
			want := status.CheckAllowed
			if j == i {
				want = status.CheckDenied
			}
			if c.Result != want {
				t.Fatalf("removing %+v: check %d (%+v) is %s, want %s", removed, j, c, c.Result, want)
			}
		}
		e.requireOnlySSARPosts(t)
		e.api.Close()
	}
}

func TestRunChecksLeasesOnlyInTheControllerNamespaceWithFixedNames(t *testing.T) {
	e := pfNewEnv(t, pfValues(nil))
	e.run()
	leases, namedWrites := 0, 0
	for _, r := range e.api.seen() {
		a := r.attrs
		if a.Resource == "leases" {
			leases++
			if a.Namespace != pfController {
				t.Errorf("Lease check outside controllerNamespace: %+v", a)
			}
			if slices.Contains([]string{"update", "patch", "delete"}, a.Verb) {
				if a.Name != "falcone-pool-controller-leader" && a.Name != "falcone-pool-controller-scaler" {
					t.Errorf("Lease %s without a fixed name: %+v", a.Verb, a)
				}
				namedWrites++
			} else if a.Name != "" {
				t.Errorf("Lease %s must be unnamed: %+v", a.Verb, a)
			}
		} else if a.Namespace == pfController {
			t.Errorf("%s check targets controllerNamespace: %+v", a.Resource, a)
		}
	}
	if leases != 10 || namedWrites != 6 {
		t.Fatalf("%d Lease checks and %d named writes, want 10 and 6", leases, namedWrites)
	}
}

func TestRunFailsClosedOnEveryUnusableAPIResponse(t *testing.T) {
	for name, respond := range map[string]func(w http.ResponseWriter, r *http.Request) bool{
		"HTTP 403": func(w http.ResponseWriter, r *http.Request) bool { w.WriteHeader(http.StatusForbidden); return true },
		"HTTP 500": func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusInternalServerError)
			return true
		},
		"3xx": func(w http.ResponseWriter, r *http.Request) bool {
			http.Redirect(w, r, "/followed", http.StatusFound)
			return true
		},
		"malformed body": func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, "{")
			return true
		},
		"no status.allowed": func(w http.ResponseWriter, r *http.Request) bool {
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"status":{}}`)
			return true
		},
		"delay beyond timeout": func(w http.ResponseWriter, r *http.Request) bool {
			select {
			case <-r.Context().Done():
			case <-time.After(10 * time.Second):
			}
			return true
		},
	} {
		t.Run(name, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(nil))
			e.api.respond = respond
			code := e.run()
			r := e.report(t)
			if code != 1 || r.Preflight.Result != status.ResultFailed || e.calls != 0 {
				t.Fatalf("exit %d, result %s, hook calls %d", code, r.Preflight.Result, e.calls)
			}
			if len(r.Preflight.Checks) != 38 {
				t.Fatalf("%d checks reported, want 38", len(r.Preflight.Checks))
			}
			for _, c := range r.Preflight.Checks {
				if c.Result != status.CheckError {
					t.Fatalf("check %+v must be error", c)
				}
			}
			e.requireOnlySSARPosts(t)
			for _, rec := range e.api.seen() {
				if rec.path == "/followed" {
					t.Fatal("redirect followed")
				}
			}
		})
	}
}

func TestRunRejectsBadValuesFilesBeforeAnyRequest(t *testing.T) {
	good := string(pfValues(nil))
	for name, file := range map[string][]byte{
		"invalid JSON": []byte("{"),
		"empty file":   {},
		"empty watched namespaces": pfValues(func(m map[string]any) {
			m["watchedPoolNamespaces"] = []string{}
		}),
		"absent watched namespaces": pfValues(func(m map[string]any) { delete(m, "watchedPoolNamespaces") }),
		"blank namespace":           pfValues(func(m map[string]any) { m["watchedPoolNamespaces"] = []string{"pool-a", " "} }),
		"duplicate namespace":       pfValues(func(m map[string]any) { m["watchedPoolNamespaces"] = []string{"pool-a", "pool-a"} }),
		"wildcard namespace":        pfValues(func(m map[string]any) { m["watchedPoolNamespaces"] = []string{"*"} }),
		"non DNS-1123 namespace":    pfValues(func(m map[string]any) { m["watchedPoolNamespaces"] = []string{"Pool_A"} }),
		"blank controllerNamespace": pfValues(func(m map[string]any) { m["controllerNamespace"] = " " }),
		"absent controllerNamespace": pfValues(func(m map[string]any) {
			delete(m, "controllerNamespace")
		}),
	} {
		t.Run(name, func(t *testing.T) {
			e := pfNewEnv(t, file)
			r := e.requireFailedBeforeAnyRequest(t, e.run())
			if len(r.Prerequisites) != 7 {
				t.Fatalf("want all seven kinds, got %+v", r.Prerequisites)
			}
			for _, p := range r.Prerequisites {
				if p.State != status.StateMissing || p.Name != "" {
					t.Errorf("%+v must be missing with an empty name", p)
				}
			}
		})
	}
	t.Run("missing file", func(t *testing.T) {
		e := pfNewEnv(t, nil)
		delete(e.files, pfValuesPath)
		e.requireFailedBeforeAnyRequest(t, e.run())
	})
	t.Run("custom path from env", func(t *testing.T) {
		e := pfNewEnv(t, pfValues(nil))
		e.files["/custom/values.json"] = e.files[pfValuesPath]
		delete(e.files, pfValuesPath)
		e.env["FALCONE_SERVERLESS_VALUES"] = "/custom/values.json"
		if code := e.run(); code != 0 {
			t.Fatalf("exit %d with the values file at the env path", code)
		}
	})
	t.Run("error does not echo file content", func(t *testing.T) {
		e := pfNewEnv(t, []byte(strings.Replace(good, "pool-a", "SECRET-LEAK", 1)))
		e.run()
		if strings.Contains(e.out.String(), "LEAK") {
			t.Fatalf("file content reached stdout: %s", e.out.String())
		}
	})
}

func TestRunRejectsBadEnvironmentBeforeAnyRequest(t *testing.T) {
	for name, edit := range map[string]func(e *pfEnv){
		"token file missing": func(e *pfEnv) { delete(e.files, pfTokenPath) },
		"token empty":        func(e *pfEnv) { e.files[pfTokenPath] = nil },
		"token blank":        func(e *pfEnv) { e.files[pfTokenPath] = []byte(" \n") },
		"host unset":         func(e *pfEnv) { delete(e.env, "KUBERNETES_SERVICE_HOST") },
		"host blank":         func(e *pfEnv) { e.env["KUBERNETES_SERVICE_HOST"] = " " },
		"port unset":         func(e *pfEnv) { delete(e.env, "KUBERNETES_SERVICE_PORT") },
	} {
		t.Run(name, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(nil))
			edit(e)
			r := e.requireFailedBeforeAnyRequest(t, e.run())
			for _, p := range r.Prerequisites {
				if p.State != status.StateDeclared {
					t.Errorf("values were readable, so %+v must be declared", p)
				}
			}
		})
	}
}

func TestRunNamespaceMode(t *testing.T) {
	for _, mode := range []any{nil, "preprovisioned"} {
		e := pfNewEnv(t, pfValues(func(m map[string]any) {
			if mode != nil {
				m["namespaceMode"] = mode
			}
		}))
		if code := e.run(); code != 0 || e.calls != 1 {
			t.Errorf("namespaceMode %v: exit %d, hook calls %d; want 0, 1", mode, code, e.calls)
		}
		e.report(t)
	}
	for _, mode := range []string{"create", "shared", "", "  ", "Preprovisioned", "unknown"} {
		t.Run("mode "+mode, func(t *testing.T) {
			e := pfNewEnv(t, pfValues(func(m map[string]any) { m["namespaceMode"] = mode }))
			r := e.requireFailedBeforeAnyRequest(t, e.run())
			if !strings.Contains(r.Preflight.ConfigError, "namespaceMode") {
				t.Fatalf("configError %q must name namespaceMode", r.Preflight.ConfigError)
			}
		})
	}
}

func TestRunRejectsANullNamespaceMode(t *testing.T) {
	e := pfNewEnv(t, pfValues(func(m map[string]any) { m["namespaceMode"] = nil }))
	r := e.requireFailedBeforeAnyRequest(t, e.run())
	if !strings.Contains(r.Preflight.ConfigError, "namespaceMode") {
		t.Fatalf("configError %q must name namespaceMode", r.Preflight.ConfigError)
	}
}

func TestRunReportsEachMissingPrerequisiteAndStartsNothing(t *testing.T) {
	fields := map[string]struct {
		section, key, kind string
	}{
		"gateway":                    {"gatewayRoute", "name", status.KindGateway},
		"storage":                    {"storage", "name", status.KindStorage},
		"broker":                     {"broker", "name", status.KindBroker},
		"rbac":                       {"rbac", "controllerRoleBinding", status.KindRBAC},
		"network-policy-enforcement": {"networkPolicyEnforcement", "name", status.KindNetworkPolicyEnforcement},
		"existing-scc":               {"existingSCC", "name", status.KindExistingSCC},
	}
	for name, f := range fields {
		for _, mode := range []string{"deleted", "blank"} {
			t.Run(name+"/"+mode, func(t *testing.T) {
				e := pfNewEnv(t, pfValues(func(m map[string]any) {
					section := m["prerequisites"].(map[string]any)[f.section].(map[string]any)
					if mode == "deleted" {
						delete(section, f.key)
					} else {
						section[f.key] = "  "
					}
				}))
				code := e.run()
				r := e.report(t)
				if code != 1 || r.Preflight.Result != status.ResultFailed || e.calls != 0 {
					t.Fatalf("exit %d, result %s, hook calls %d", code, r.Preflight.Result, e.calls)
				}
				if len(r.Preflight.Checks) != 38 {
					t.Fatalf("grants are still checked: %d checks", len(r.Preflight.Checks))
				}
				for _, p := range r.Prerequisites {
					want := status.StateDeclared
					if p.Kind == f.kind {
						want = status.StateMissing
					}
					if p.State != want {
						t.Errorf("%+v, want state %s", p, want)
					}
				}
				e.requireOnlySSARPosts(t)
			})
		}
	}
}

// --- image inputs ------------------------------------------------------------

// Every non-test Go file under controller/internal is compiled into the binary,
// so the Containerfile must copy each one to the same relative path.
func TestContainerfileCopiesEveryInternalSourceFile(t *testing.T) {
	copied := map[string]bool{}
	for _, in := range instructions(readFile(t, "Containerfile")) {
		if in.op != "COPY" {
			continue
		}
		var words []string
		for _, arg := range in.args {
			if !strings.HasPrefix(arg, "--") {
				words = append(words, arg)
			}
		}
		if len(words) < 2 {
			continue
		}
		dest := words[len(words)-1]
		for _, src := range words[:len(words)-1] {
			rel, ok := strings.CutPrefix(src, "controller/")
			if !ok || !strings.HasPrefix(rel, "internal/") {
				continue
			}
			copied[path.Join(dest, path.Base(rel))] = true
			if path.Join(dest, path.Base(rel)) != "internal/"+strings.TrimPrefix(rel, "internal/") {
				t.Errorf("COPY %s lands at %s, not at %s", src, path.Join(dest, path.Base(rel)), rel)
			}
		}
	}
	var sources []string
	err := filepath.WalkDir("internal", func(p string, d fs.DirEntry, err error) error {
		if err == nil && !d.IsDir() && strings.HasSuffix(p, ".go") && !strings.HasSuffix(p, "_test.go") {
			sources = append(sources, filepath.ToSlash(p))
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(sources) < 3 {
		t.Fatalf("expected the preflight and status sources, found %v", sources)
	}
	for _, src := range sources {
		if !copied[src] {
			t.Errorf("%s is imported by main.go but the Containerfile does not copy it", src)
		}
	}
	for dest := range copied {
		if !slices.Contains(sources, dest) {
			t.Errorf("Containerfile copies %s, which is not a non-test source file", dest)
		}
	}
}

func TestNoGoSumAndNoDependencies(t *testing.T) {
	if _, err := os.Stat("go.sum"); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("go.sum must not exist (err=%v): the controller is stdlib only", err)
	}
	if strings.Contains(readFile(t, "go.mod"), "require") {
		t.Fatal("go.mod must declare no dependencies")
	}
}

func TestRunRequiresDownwardAPINamespace(t *testing.T) {
	for _, ns := range []string{"", "Pool_A", "different-release"} {
		t.Run(ns, func(t *testing.T) {
			e := pfOfflineEnv(pfValues(nil))
			e.env["POD_NAMESPACE"] = ns
			e.requireFailedBeforeAnyRequest(t, e.run())
		})
	}
}

func TestRunRejectsElectionConfiguration(t *testing.T) {
	for _, edit := range []func(map[string]any){
		func(m map[string]any) { m["leaseDurationSeconds"] = 301 },
		func(m map[string]any) { m["renewDeadlineSeconds"] = 15 },
		func(m map[string]any) { m["retryPeriodSeconds"] = 0 },
		func(m map[string]any) { m["retryPeriodSeconds"] = 1.5 },
		func(m map[string]any) { m["leaseDurationSeconds"] = "15" },
		func(m map[string]any) { m["renewDeadlineSeconds"] = nil },
		func(m map[string]any) { m["leaderElectionLeaseName"] = "other" },
	} {
		e := pfOfflineEnv(pfValues(edit))
		e.env["POD_NAMESPACE"] = pfController
		e.requireFailedBeforeAnyRequest(t, e.run())
	}
}

type mainTransport func(*http.Request) (*http.Response, error)

func (f mainTransport) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }
func pfOfflineEnv(values []byte) *pfEnv {
	e := &pfEnv{api: &pfAPI{}, env: map[string]string{"POD_NAMESPACE": pfController, "KUBERNETES_SERVICE_HOST": "api", "KUBERNETES_SERVICE_PORT": "443"}, files: map[string][]byte{pfValuesPath: values, pfTokenPath: []byte(pfToken)}}
	e.client = &http.Client{Transport: mainTransport(func(r *http.Request) (*http.Response, error) {
		e.api.mu.Lock()
		e.api.requests = append(e.api.requests, pfRequest{method: r.Method, path: r.URL.Path})
		e.api.mu.Unlock()
		return &http.Response{StatusCode: 201, Body: io.NopCloser(strings.NewReader(`{"status":{"allowed":true}}`)), Header: make(http.Header)}, nil
	})}
	return e
}

func TestLeasePreflightDenialAndErrorNeverCampaign(t *testing.T) {
	for _, response := range []string{`{"status":{"allowed":false}}`, `{invalid`} {
		e := pfOfflineEnv(pfValues(nil))
		base := e.client.Transport
		e.client.Transport = mainTransport(func(r *http.Request) (*http.Response, error) {
			data, _ := io.ReadAll(r.Body)
			r.Body = io.NopCloser(bytes.NewReader(data))
			resp, err := base.RoundTrip(r)
			if bytes.Contains(data, []byte(`"resource":"leases"`)) {
				resp.Body = io.NopCloser(strings.NewReader(response))
			}
			return resp, err
		})
		if code := e.run(); code != 1 || e.calls != 0 {
			t.Fatalf("exit %d start %d", code, e.calls)
		}
		for _, r := range e.api.seen() {
			if r.path != pfSSARPath {
				t.Fatal("failed preflight campaigned")
			}
		}
	}
}

// pfLeaseAPI records forbidden traffic without aborting before the final
// assertions. beforeGet injects external API events at the HTTP boundary.
type pfLeaseAPI struct {
	t                                                   *testing.T
	base                                                http.RoundTripper
	lease                                               map[string]any
	gets, renewals, poolWrites, replicaChanges, deletes int
	beforeGet                                           func()
	unavailable                                         bool
}

func (f *pfLeaseAPI) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Path == pfSSARPath {
		return f.base.RoundTrip(r)
	}
	collection := "/apis/coordination.k8s.io/v1/namespaces/" + pfController + "/leases"
	if r.Method == http.MethodDelete {
		f.deletes++
	}
	if r.Method == http.MethodPatch {
		f.replicaChanges++
	}
	expectedPath := collection + "/falcone-pool-controller-leader"
	if r.Method == http.MethodPost {
		expectedPath = collection
	}
	if r.URL.Path != expectedPath {
		f.poolWrites++
		return nil, errors.New("unexpected pool traffic")
	}
	code := 200
	switch r.Method {
	case http.MethodGet:
		f.gets++
		if f.beforeGet != nil {
			f.beforeGet()
		}
		if f.unavailable {
			return nil, errors.New("API unavailable")
		}
		if f.lease == nil {
			code = 404
		}
	case http.MethodPost, http.MethodPut:
		if f.unavailable {
			return nil, errors.New("API unavailable")
		}
		if r.Context().Err() != nil {
			return nil, r.Context().Err()
		}
		var next map[string]any
		if json.NewDecoder(r.Body).Decode(&next) != nil {
			f.t.Fatal("invalid Lease")
		}
		metadata := next["metadata"].(map[string]any)
		if r.Method == http.MethodPut && metadata["resourceVersion"] != "1" {
			f.t.Fatal("missing resourceVersion")
		}
		metadata["resourceVersion"] = "1"
		f.lease = next
		if r.Method == http.MethodPut && next["spec"].(map[string]any)["holderIdentity"] != "" {
			f.renewals++
		}
	default:
		return nil, errors.New("unexpected Lease verb")
	}
	b, _ := json.Marshal(f.lease)
	return &http.Response{StatusCode: code, Body: io.NopCloser(bytes.NewReader(b)), Header: make(http.Header)}, nil
}
func (f *pfLeaseAPI) requireNoPoolMutations(t *testing.T) {
	t.Helper()
	if f.poolWrites != 0 || f.replicaChanges != 0 || f.deletes != 0 {
		t.Fatalf("pool writes=%d replica changes=%d deletes=%d", f.poolWrites, f.replicaChanges, f.deletes)
	}
}
func requireReportAndVersion(t *testing.T, out *bytes.Buffer) {
	t.Helper()
	dec := json.NewDecoder(out)
	var report status.Report
	if dec.Decode(&report) != nil || report.Preflight.Result != status.ResultPassed {
		t.Fatal("missing report")
	}
	rest, _ := io.ReadAll(io.MultiReader(dec.Buffered(), out))
	if strings.TrimSpace(string(rest)) != "falcone-serverless-controller "+Version {
		t.Fatalf("unexpected stdout %q", rest)
	}
}

func TestSignalledRunReleasesLeaseAndKeepsStdoutBounded(t *testing.T) {
	e := pfOfflineEnv(pfValues(func(m map[string]any) {
		m["leaseDurationSeconds"] = 3
		m["renewDeadlineSeconds"] = 2
		m["retryPeriodSeconds"] = 1
	}))
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer cancel()
	fake := &pfLeaseAPI{t: t, base: e.client.Transport}
	fake.beforeGet = func() {
		if fake.gets == 2 {
			if err := syscall.Kill(os.Getpid(), syscall.SIGTERM); err != nil {
				t.Fatal(err)
			}
			<-ctx.Done()
		}
	}
	e.client.Transport = fake
	code := run(deps{ctx: ctx, getenv: func(k string) string { return e.env[k] }, readFile: func(p string) ([]byte, error) { return e.files[p], nil }, client: e.client, stdout: &e.out, start: func() { e.out.WriteString("falcone-serverless-controller " + Version + "\n") }})
	if code != 0 || fake.lease == nil {
		t.Fatalf("exit %d, missing Lease", code)
	}
	spec := fake.lease["spec"].(map[string]any)
	if spec["holderIdentity"] != "" || spec["leaseTransitions"] != float64(0) {
		t.Fatal("release did not preserve fencing history")
	}
	fake.requireNoPoolMutations(t)
	requireReportAndVersion(t, &e.out)
}

func TestDegradedBehaviorDocumentsFourScenarios(t *testing.T) {
	data, err := os.ReadFile("../docs/operations/degraded-behavior.md")
	if err != nil {
		t.Fatal(err)
	}
	for _, text := range []string{"Leader loss", "API server unavailable", "Stale desired state", "Failed preflight", "300", "POD_NAMESPACE", "leaseTransitions", "renewDeadline"} {
		if !strings.Contains(string(data), text) {
			t.Errorf("missing %s", text)
		}
	}
}

// DEP-03 has not supplied a desired-state adapter. These tests exercise the
// production no-op duties and record every HTTP request, including while the
// API is healthy and authoritative desired state is unavailable/stale.
func TestProductionDutiesMakeNoPoolMutationsWhileDegraded(t *testing.T) {
	for _, scenario := range []string{"Leader loss", "API server unavailable", "Stale desired state", "Failed preflight"} {
		t.Run(scenario, func(t *testing.T) {
			e := pfOfflineEnv(pfValues(func(m map[string]any) {
				m["leaseDurationSeconds"] = 3
				m["renewDeadlineSeconds"] = 2
				m["retryPeriodSeconds"] = 1
			}))
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			e.ctx = ctx
			base := e.client.Transport
			if scenario == "Failed preflight" {
				e.client.Transport = mainTransport(func(r *http.Request) (*http.Response, error) {
					resp, err := base.RoundTrip(r)
					resp.Body = io.NopCloser(strings.NewReader(`{"status":{"allowed":false}}`))
					return resp, err
				})
			}
			fake := &pfLeaseAPI{t: t, base: e.client.Transport}
			fake.beforeGet = func() {
				if fake.gets == 3 {
					cancel()
				}
				if fake.gets >= 2 && scenario == "API server unavailable" {
					fake.unavailable = true
				}
				if fake.lease != nil && fake.gets >= 2 && scenario == "Leader loss" {
					fake.lease["spec"].(map[string]any)["holderIdentity"] = "another-process"
				}
			}
			e.client.Transport = fake
			code := e.run()
			if scenario == "Failed preflight" {
				if code != 1 || e.calls != 0 || fake.gets != 0 {
					t.Fatal("failed preflight started election")
				}
			} else if code != 0 {
				t.Fatal(code)
			}
			fake.requireNoPoolMutations(t)
			if scenario == "Stale desired state" && fake.renewals == 0 {
				t.Fatal("stale desired state stopped Lease renewal")
			}
		})
	}
}

func TestRunRotatesLeaseTokenAndLogsOnlyToStderr(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		e := pfOfflineEnv(pfValues(nil))
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		fake := &pfLeaseAPI{t: t, base: e.client.Transport}
		fake.beforeGet = func() {
			if fake.gets == 2 {
				delete(fake.lease["metadata"].(map[string]any), "labels")
			}
			if fake.gets == 3 {
				cancel()
			}
		}
		currentToken := pfToken
		leaseRequests, rotatedRequests := 0, 0
		client := &http.Client{Transport: mainTransport(func(r *http.Request) (*http.Response, error) {
			if r.Header.Get("Authorization") != "Bearer "+currentToken {
				t.Fatal("main retained a token across rotation")
			}
			if r.URL.Path != pfSSARPath {
				leaseRequests++
				if currentToken != pfToken {
					rotatedRequests++
				}
			}
			resp, err := fake.RoundTrip(r)
			if r.URL.Path != pfSSARPath {
				currentToken += "-rotated"
			}
			return resp, err
		})}
		var stderr bytes.Buffer
		code := run(deps{ctx: ctx, stderr: &stderr, getenv: func(k string) string { return e.env[k] },
			readFile: func(p string) ([]byte, error) {
				if p == pfTokenPath {
					return []byte(currentToken), nil
				}
				return e.files[p], nil
			},
			client: client, stdout: &e.out, start: func() { e.out.WriteString("falcone-serverless-controller " + Version + "\n") },
		})
		if code != 0 || leaseRequests < 3 || rotatedRequests < 2 {
			t.Fatal("did not exercise rotated Lease requests")
		}
		if stderr.String() != "leader lease ownership unverified\n" {
			t.Fatalf("unexpected stderr %q", stderr.String())
		}
		fake.requireNoPoolMutations(t)
		requireReportAndVersion(t, &e.out)
	})
}
