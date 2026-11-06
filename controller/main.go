// Command controller is the falcone-serverless namespaced pool controller.
//
// Placeholder until DEP-03 (#23): it manages no Kubernetes objects yet. The real
// controller writes only standard Deployments, Services and Leases in preprovisioned
// namespaces and is the sole writer of pool replica counts.
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
	"strings"
	"time"

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
	start    func()        // runs once, only after a passed preflight
	stdout   io.Writer
}

func main() {
	os.Exit(run(deps{
		getenv:   os.Getenv,
		readFile: os.ReadFile,
		start:    func() { fmt.Printf("falcone-serverless-controller %s\n", Version) },
		stdout:   os.Stdout,
	}))
}

// run executes the preflight, prints the report once and returns the exit code: 0
// after calling start on a passed result, 1 without calling it otherwise.
func run(d deps) int {
	report := runPreflight(d)
	enc := json.NewEncoder(d.stdout)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(report)
	if report.Preflight.Result != status.ResultPassed {
		return 1
	}
	d.start()
	return 0
}

func runPreflight(d deps) status.Report {
	path := d.getenv(valuesEnv)
	if path == "" {
		path = defaultValuesPath
	}
	data, err := d.readFile(path)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), "values file is unreadable")
	}
	values, err := preflight.ParseValues(data)
	if err != nil {
		return status.NewReport(nil, preflight.MissingPrerequisites(), strings.ReplaceAll(err.Error(), "\n", "; "))
	}
	prerequisites := values.Prerequisites()

	base, err := preflight.APIBase(d.getenv("KUBERNETES_SERVICE_HOST"), d.getenv("KUBERNETES_SERVICE_PORT"))
	if err != nil {
		return status.NewReport(nil, prerequisites, err.Error())
	}
	tokenBytes, err := d.readFile(serviceAccountDir + "token")
	token := strings.TrimSpace(string(tokenBytes))
	if err != nil || token == "" {
		return status.NewReport(nil, prerequisites, "service account token is unreadable or empty")
	}
	client := d.client
	if client == nil {
		ca, err := d.readFile(serviceAccountDir + "ca.crt")
		if err == nil {
			client, err = preflight.NewHTTPClient(ca)
		}
		if err != nil {
			return status.NewReport(nil, prerequisites, "service account CA is unreadable or invalid")
		}
	}
	api := preflight.Client{APIBase: base, Token: token, HTTP: client, Timeout: d.timeout}
	checks := api.Run(context.Background(), preflight.Table(values.WatchedPoolNamespaces, values.ControllerNamespace))
	return status.NewReport(checks, prerequisites, "")
}
