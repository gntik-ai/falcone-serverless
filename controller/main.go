// Command controller is the falcone-serverless namespaced pool controller.
//
// Placeholder until DEP-03 (#23): it manages no Kubernetes objects yet. The real
// controller writes only standard Deployments, Services and Leases in preprovisioned
// namespaces and is the sole writer of pool replica counts.
//
// At start-up it runs a read-only preflight (OPS-01): SelfSubjectAccessReviews prove
// its namespaced authority and the values file must declare every prerequisite. It
// prints one pool-preflight/v1 report and starts only when the result is passed.
//
// After a passed preflight it campaigns for the leader Lease in its own release
// namespace (POD_NAMESPACE, which must equal controllerNamespace) and runs the
// singleton duties only while it leads. SIGTERM and SIGINT close the leadership
// gate, drain the duties and release the Lease before the process exits 0. A failed
// preflight or an unusable POD_NAMESPACE or POD_NAME exits 1 before any Lease request.
package main

import (
	"context"
	crand "crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

// Version is the controller's build identity.
const Version = "0.1.0"

const (
	valuesEnv         = "FALCONE_SERVERLESS_VALUES"
	defaultValuesPath = "/etc/falcone-serverless/values.json"
	serviceAccountDir = "/var/run/secrets/kubernetes.io/serviceaccount/"
)

// deps are the process boundaries run uses, injected so tests can replace them.
type deps struct {
	getenv   func(string) string
	readFile func(string) ([]byte, error)
	client   *http.Client   // built from the service account CA when nil
	timeout  time.Duration  // per access review; preflight.DefaultTimeout when zero
	start    func()         // runs once, after a passed preflight and before the campaign
	clock    leader.Clock   // the system clock when nil
	random   io.Reader      // crypto/rand when nil
	timings  leader.Timings // leader.DefaultTimings when zero
	duties   []leader.Duty  // singleton duties, run only behind the leadership gate
	stdout   io.Writer
	stderr   io.Writer // diagnostics only; never the token. io.Discard when nil
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, os.Interrupt)
	code := run(ctx, deps{
		getenv:   os.Getenv,
		readFile: os.ReadFile,
		start:    func() { fmt.Printf("falcone-serverless-controller %s\n", Version) },
		stdout:   os.Stdout,
		stderr:   os.Stderr,
	})
	stop()
	os.Exit(code)
}

// run executes the preflight, prints the report once and returns the exit code: 1
// without campaigning when the preflight failed or the pod identity is unusable,
// otherwise it calls start, leads or stands by until ctx ends, shuts down and returns 0.
func run(ctx context.Context, d deps) int {
	if d.stderr == nil {
		d.stderr = io.Discard
	}
	pf := runPreflight(ctx, d)
	enc := json.NewEncoder(d.stdout)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(pf.report)
	if pf.report.Preflight.Result != status.ResultPassed {
		return 1
	}
	elector, err := newElector(d, pf)
	if err != nil {
		_, _ = fmt.Fprintf(d.stderr, "leader election not started: %v\n", err)
		return 1
	}
	d.start()
	elector.Run(ctx)
	return 0
}

// newElector checks the pod identity and builds the election for the one Lease in
// the release namespace. It sends no request.
func newElector(d deps, pf preflightResult) (*leader.Elector, error) {
	namespace := d.getenv("POD_NAMESPACE")
	switch {
	case namespace == "":
		return nil, errors.New("POD_NAMESPACE is absent")
	case namespace != pf.controllerNamespace:
		return nil, errors.New("POD_NAMESPACE differs from controllerNamespace")
	}
	random := d.random
	if random == nil {
		random = crand.Reader
	}
	identity, err := leader.NewIdentity(d.getenv("POD_NAME"), random)
	if err != nil {
		return nil, err
	}
	timings := d.timings
	if timings == (leader.Timings{}) {
		timings = leader.DefaultTimings
	}
	if err := timings.Validate(); err != nil {
		return nil, err
	}
	// The projected token rotates, so it is read again for every request.
	token := func() (string, error) {
		data, err := d.readFile(serviceAccountDir + "token")
		return strings.TrimSpace(string(data)), err
	}
	lease, err := leader.NewHTTPClient(pf.apiBase, namespace, token, pf.client, timings.RequestTimeout())
	if err != nil {
		return nil, err
	}
	clock := d.clock
	if clock == nil {
		clock = leader.RealClock{}
	}
	return leader.New(leader.Config{
		Namespace: namespace,
		Identity:  identity,
		Timings:   timings,
		Client:    lease,
		Clock:     clock,
		Duties:    d.duties,
		Logf:      func(format string, args ...any) { _, _ = fmt.Fprintf(d.stderr, format+"\n", args...) },
	})
}

// preflightResult is the report plus what the election needs from the same checks.
type preflightResult struct {
	report              status.Report
	controllerNamespace string
	apiBase             string
	client              *http.Client
}

func runPreflight(ctx context.Context, d deps) preflightResult {
	path := d.getenv(valuesEnv)
	if path == "" {
		path = defaultValuesPath
	}
	data, err := d.readFile(path)
	if err != nil {
		return preflightResult{report: status.NewReport(nil, preflight.MissingPrerequisites(), "values file is unreadable")}
	}
	values, err := preflight.ParseValues(data)
	if err != nil {
		return preflightResult{report: status.NewReport(nil, preflight.MissingPrerequisites(), strings.ReplaceAll(err.Error(), "\n", "; "))}
	}
	prerequisites := values.Prerequisites()
	fail := func(reason string) preflightResult {
		return preflightResult{report: status.NewReport(nil, prerequisites, reason)}
	}

	base, err := preflight.APIBase(d.getenv("KUBERNETES_SERVICE_HOST"), d.getenv("KUBERNETES_SERVICE_PORT"))
	if err != nil {
		return fail(err.Error())
	}
	tokenBytes, err := d.readFile(serviceAccountDir + "token")
	token := strings.TrimSpace(string(tokenBytes))
	if err != nil || token == "" {
		return fail("service account token is unreadable or empty")
	}
	client := d.client
	if client == nil {
		ca, err := d.readFile(serviceAccountDir + "ca.crt")
		if err == nil {
			client, err = preflight.NewHTTPClient(ca)
		}
		if err != nil {
			return fail("service account CA is unreadable or invalid")
		}
	}
	api := preflight.Client{APIBase: base, Token: token, HTTP: client, Timeout: d.timeout}
	checks := api.Run(ctx, preflight.Table(values.WatchedPoolNamespaces, values.ControllerNamespace))
	return preflightResult{
		report:              status.NewReport(checks, prerequisites, ""),
		controllerNamespace: values.ControllerNamespace,
		apiBase:             base,
		client:              client,
	}
}
