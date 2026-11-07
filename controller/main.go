// Command controller is the falcone-serverless namespaced pool controller.
//
// Until DEP-03 (#23), pool reconcile and publishing duties remain no-ops. The real
// controller writes only standard Deployments, Services and Leases in preprovisioned
// namespaces and is the sole writer of pool replica counts.
//
// At start-up it runs a read-only preflight (OPS-01): SelfSubjectAccessReviews prove
// its namespaced authority and the values file must declare every prerequisite. It
// prints one pool-preflight/v1 report and campaigns only when the result is passed.
// It runs until SIGTERM/SIGINT, fences lost leadership, and releases after duties stop.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
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
	ctx      context.Context
	stderr   io.Writer
	getenv   func(string) string
	readFile func(string) ([]byte, error)
	client   *http.Client  // built from the service account CA when nil
	timeout  time.Duration // per access review; preflight.DefaultTimeout when zero
	start    func()        // runs once, only after a passed preflight
	stdout   io.Writer
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer stop()
	os.Exit(run(deps{ctx: ctx, stderr: os.Stderr,
		getenv:   os.Getenv,
		readFile: os.ReadFile,
		start:    func() { fmt.Printf("falcone-serverless-controller %s\n", Version) },
		stdout:   os.Stdout,
	}))
}

// run executes the preflight, prints the report once and returns the exit code: 0
// after a graceful shutdown, 1 without starting on a failed result.
func run(d deps) int {
	report, elector := runPreflight(d)
	enc := json.NewEncoder(d.stdout)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(report)
	if report.Preflight.Result != status.ResultPassed {
		return 1
	}
	d.start()
	ctx := d.ctx
	if ctx == nil {
		ctx = context.Background()
	}
	elector.Run(ctx, func(ctx context.Context, _ *leader.Gate) { <-ctx.Done() })
	return 0
}

func runPreflight(d deps) (status.Report, *leader.Elector) {
	path := d.getenv(valuesEnv)
	if path == "" {
		path = defaultValuesPath
	}
	data, err := d.readFile(path)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), "values file is unreadable"), nil
	}
	values, err := preflight.ParseValues(data)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), strings.ReplaceAll(err.Error(), "\n", "; ")), nil
	}
	prerequisites := values.Prerequisites()
	namespace := d.getenv("POD_NAMESPACE")
	if !preflight.ValidNamespace(namespace) || namespace != values.ControllerNamespace {
		return status.NewReport(nil, prerequisites, "POD_NAMESPACE must be a DNS-1123 label equal to controllerNamespace"), nil
	}

	base, err := preflight.APIBase(d.getenv("KUBERNETES_SERVICE_HOST"), d.getenv("KUBERNETES_SERVICE_PORT"))
	if err != nil {
		return status.NewReport(nil, prerequisites, err.Error()), nil
	}
	tokenBytes, err := d.readFile(serviceAccountDir + "token")
	token := strings.TrimSpace(string(tokenBytes))
	if err != nil || token == "" {
		return status.NewReport(nil, prerequisites, "service account token is unreadable or empty"), nil
	}
	client := d.client
	if client == nil {
		ca, err := d.readFile(serviceAccountDir + "ca.crt")
		if err == nil {
			client, err = preflight.NewHTTPClient(ca)
		}
		if err != nil {
			return status.NewReport(nil, prerequisites, "service account CA is unreadable or invalid"), nil
		}
	}
	identity, err := leader.NewIdentity()
	if err != nil {
		return status.NewReport(nil, prerequisites, "leader identity unavailable"), nil
	}
	leaseClient, err := leader.NewHTTPClient(base, namespace, client, func() ([]byte, error) { return d.readFile(serviceAccountDir + "token") }, values.RetryPeriod)
	if err != nil {
		return status.NewReport(nil, prerequisites, "leader client configuration invalid"), nil
	}
	stderr := d.stderr
	if stderr == nil {
		stderr = io.Discard
	}
	logger := log.New(stderr, "", 0)
	elector, err := leader.New(leader.Config{Namespace: namespace, Identity: identity, LeaseDuration: values.LeaseDuration, RenewDeadline: values.RenewDeadline, RetryPeriod: values.RetryPeriod}, leaseClient, leader.RealClock{}, func(reason string) { logger.Print(reason) })
	if err != nil {
		return status.NewReport(nil, prerequisites, "leader configuration invalid"), nil
	}
	api := preflight.Client{APIBase: base, Token: token, HTTP: client, Timeout: d.timeout}
	checks := api.Run(context.Background(), preflight.Table(values.WatchedPoolNamespaces, values.ControllerNamespace))
	return status.NewReport(checks, prerequisites, ""), elector
}
