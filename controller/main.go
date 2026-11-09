// Command controller is the falcone-serverless namespaced pool controller.
//
// Placeholder until DEP-03 (#23): it manages no Kubernetes objects yet. The real
// controller writes only standard Deployments, Services and Leases in preprovisioned
// namespaces and is the sole writer of pool replica counts.
//
// At start-up it runs a read-only preflight (OPS-01): SelfSubjectAccessReviews prove
// its namespaced authority and the values file must declare every prerequisite. It
// prints one pool-preflight/v1 report and campaigns for leadership only when the
// result is passed (OPS-01.t3). The leader Lease lives in the release namespace from
// the downward API, which must equal controllerNamespace. Every replica stops its duties
// and, on SIGTERM, releases the Lease before it exits. The report is the only thing
// written to stdout; status and errors go to stderr and never carry the token.
package main

import (
	"context"
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

	podNameEnv      = "POD_NAME"      // downward API: metadata.name
	podNamespaceEnv = "POD_NAMESPACE" // downward API: metadata.namespace
)

// deps are the process boundaries run uses, injected so tests can replace them.
type deps struct {
	getenv   func(string) string
	readFile func(string) ([]byte, error)
	client   *http.Client  // built from the service account CA when nil
	timeout  time.Duration // per access review; preflight.DefaultTimeout when zero
	// campaign runs once, only after a passed preflight, until ctx ends. It is elect when nil.
	campaign func(ctx context.Context, c campaignConfig, d deps) error
	// duty is the singleton work of one leadership term; it must stop when ctx ends.
	duty func(ctx context.Context, term *leader.Term)
	// signals returns the context SIGTERM cancels; the process signals when nil.
	signals func() (context.Context, context.CancelFunc)
	stdout  io.Writer
	stderr  io.Writer
}

// campaignConfig is everything the election needs, all of it proven by the preflight.
type campaignConfig struct {
	identity  string // POD_NAME
	namespace string // POD_NAMESPACE, equal to controllerNamespace
	apiBase   string
	http      *http.Client
}

func main() {
	os.Exit(run(deps{
		getenv:   os.Getenv,
		readFile: os.ReadFile,
		stdout:   os.Stdout,
		stderr:   os.Stderr,
	}))
}

// run executes the preflight, prints the report once and returns the exit code. With a
// failed preflight it returns 1 without campaigning. Otherwise it campaigns until
// SIGTERM, then returns 0 after the duties stopped and the Lease was released.
func run(d deps) int {
	report, campaign := runPreflight(d)
	enc := json.NewEncoder(d.stdout)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(report)
	if report.Preflight.Result != status.ResultPassed {
		return 1
	}
	signals := d.signals
	if signals == nil {
		signals = func() (context.Context, context.CancelFunc) {
			return signal.NotifyContext(context.Background(), syscall.SIGTERM, os.Interrupt)
		}
	}
	ctx, stop := signals()
	defer stop()
	campaignFn := d.campaign
	if campaignFn == nil {
		campaignFn = elect
	}
	if d.stderr == nil {
		d.stderr = io.Discard
	}
	if err := campaignFn(ctx, campaign, d); err != nil && !errors.Is(err, context.Canceled) {
		_, _ = fmt.Fprintf(d.stderr, "falcone-serverless-controller: %v\n", err)
		return 1
	}
	return 0
}

// elect campaigns for the leader Lease until ctx ends. Every Lease call reloads the
// projected service-account token. The duties placeholder manages nothing yet, so it
// never writes; DEP-03 reconciles and publishes through Elector.Publish, so that every
// singleton write carries the term's fence.
func elect(ctx context.Context, c campaignConfig, d deps) error {
	token := leader.FileToken(d.readFile, serviceAccountDir+"token")
	client, err := leader.NewClient(c.apiBase, c.namespace, token, c.http, leader.DefaultRequestTimeout)
	if err != nil {
		return err
	}
	logf := func(format string, args ...any) {
		_, _ = fmt.Fprintf(d.stderr, "falcone-serverless-controller: "+format+"\n", args...)
	}
	elector, err := leader.New(leader.Config{
		Identity:      c.identity,
		Namespace:     c.namespace,
		LeaseDuration: leader.DefaultLeaseDuration,
		RenewDeadline: leader.DefaultRenewDeadline,
		RetryPeriod:   leader.DefaultRetryPeriod,
		Lease:         client,
		Logf:          logf,
	})
	if err != nil {
		return err
	}
	logf("%s campaigning as %s in %s", Version, c.identity, c.namespace)
	duty := d.duty
	if duty == nil {
		duty = func(ctx context.Context, _ *leader.Term) { <-ctx.Done() }
	}
	return elector.Run(ctx, duty)
}

func runPreflight(d deps) (status.Report, campaignConfig) {
	path := d.getenv(valuesEnv)
	if path == "" {
		path = defaultValuesPath
	}
	data, err := d.readFile(path)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), "values file is unreadable"), campaignConfig{}
	}
	values, err := preflight.ParseValues(data)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), strings.ReplaceAll(err.Error(), "\n", "; ")), campaignConfig{}
	}
	prerequisites := values.Prerequisites()

	// The Lease lives only in the release namespace, which the downward API names. It
	// must be controllerNamespace, the one namespace the Role grants Lease verbs in, so
	// no setting can move leader election to a pool namespace.
	identity, namespace := d.getenv(podNameEnv), d.getenv(podNamespaceEnv)
	if err := leader.ValidateIdentity(identity); err != nil {
		return status.NewReport(nil, prerequisites, podNameEnv+" is unset or is not a DNS-1123 subdomain"), campaignConfig{}
	}
	if namespace != values.ControllerNamespace {
		return status.NewReport(nil, prerequisites, podNamespaceEnv+" is unset or does not equal controllerNamespace"), campaignConfig{}
	}

	base, err := preflight.APIBase(d.getenv("KUBERNETES_SERVICE_HOST"), d.getenv("KUBERNETES_SERVICE_PORT"))
	if err != nil {
		return status.NewReport(nil, prerequisites, err.Error()), campaignConfig{}
	}
	tokenBytes, err := d.readFile(serviceAccountDir + "token")
	token := strings.TrimSpace(string(tokenBytes))
	if err != nil || token == "" {
		return status.NewReport(nil, prerequisites, "service account token is unreadable or empty"), campaignConfig{}
	}
	client := d.client
	if client == nil {
		ca, err := d.readFile(serviceAccountDir + "ca.crt")
		if err == nil {
			client, err = preflight.NewHTTPClient(ca)
		}
		if err != nil {
			return status.NewReport(nil, prerequisites, "service account CA is unreadable or invalid"), campaignConfig{}
		}
	}
	api := preflight.Client{APIBase: base, Token: token, HTTP: client, Timeout: d.timeout}
	checks := api.Run(context.Background(), preflight.Table(values.WatchedPoolNamespaces, values.ControllerNamespace))
	return status.NewReport(checks, prerequisites, ""), campaignConfig{identity: identity, namespace: namespace, apiBase: base, http: client}
}
