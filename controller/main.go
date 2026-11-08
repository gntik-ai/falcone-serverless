// Command controller is the falcone-serverless namespaced pool controller.
//
// Until DEP-03 (#23), only the leader-election Lease is managed. Pool resource
// reconciliation and the authenticated status transport remain deferred.
//
// At start-up it runs a read-only preflight (OPS-01): SelfSubjectAccessReviews prove
// its namespaced authority and the values file must declare every prerequisite. It
// prints one pool-preflight/v1 report and starts only when the result is passed.
package main

import (
	"context"
	"encoding/json"
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
	client   *http.Client  // built from the service account CA when nil
	timeout  time.Duration // per access review; preflight.DefaultTimeout when zero
	start    func()        // preflight-only harness: skips pod identity and election
	stdout   io.Writer
	ctx      context.Context
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	code := run(deps{
		getenv:   os.Getenv,
		readFile: os.ReadFile,
		stdout:   os.Stdout,
		ctx:      ctx,
	})
	stop()
	os.Exit(code)
}

// run prints one preflight report. Failed preflight exits 1; passed preflight
// enters the election loop until shutdown (or calls the injected startup hook).
func run(d deps) int {
	if d.ctx == nil {
		d.ctx = context.Background()
	}
	report, election := runPreflight(d)
	enc := json.NewEncoder(d.stdout)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(report)
	if report.Preflight.Result != status.ResultPassed {
		return 1
	}
	if d.start != nil {
		d.start()
	} else {
		// No pool writes exist before DEP-03. That duty must use Gate.Write with
		// fresh authoritative desired state and receiver-enforced fencing.
		election.Run(d.ctx, nil, func(err error) { fmt.Fprintln(os.Stderr, "leader:", err) })
	}
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
	// The existing preflight-only harness supplies neither downward-API identity
	// nor an election loop. Production never supplies start; its identity failure
	// and single-report behavior are covered by internal/leader/process_test.go.
	if d.start == nil {
		if err := leader.ValidateIdentity(d.getenv("POD_NAME"), d.getenv("POD_NAMESPACE"), values.ControllerNamespace); err != nil {
			return status.NewReport(nil, prerequisites, err.Error()), nil
		}
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
	api := preflight.Client{APIBase: base, Token: token, HTTP: client, Timeout: d.timeout}
	checks := api.Run(d.ctx, preflight.Table(values.WatchedPoolNamespaces, values.ControllerNamespace))
	report := status.NewReport(checks, prerequisites, "")
	if report.Preflight.Result != status.ResultPassed || d.start != nil {
		return report, nil
	}
	leaseClient, err := leader.NewHTTPClient(base, token, client)
	if err != nil {
		return status.NewReport(checks, prerequisites, err.Error()), nil
	}
	cfg := leader.DefaultConfig(d.getenv("POD_NAMESPACE"), d.getenv("POD_NAME"))
	cfg.ControllerNamespace = values.ControllerNamespace
	cfg.PreflightPassed = true
	election, err := leader.New(cfg, leaseClient, leader.RealClock{})
	if err != nil {
		return status.NewReport(checks, prerequisites, err.Error()), nil
	}
	return report, election
}
