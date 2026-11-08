package leader_test

import (
	"bytes"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

// Black-box coverage of main lives here because main_test.go is outside this
// ChangeSet's allow-list. No service-account material or live API is accessed.
func TestControllerIdentityFailureEmitsExactlyOnePreflightReport(t *testing.T) {
	dir := t.TempDir()
	binary := filepath.Join(dir, "controller")
	values := filepath.Join(dir, "values.json")
	if err := os.WriteFile(values, []byte(`{"watchedPoolNamespaces":["pool-a","pool-b"],"controllerNamespace":"falcone-system"}`), 0600); err != nil {
		t.Fatal(err)
	}
	build := exec.Command("go", "build", "-o", binary, "../..")
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build controller: %v: %s", err, out)
	}
	for _, tc := range []struct{ name, pod, namespace, want string }{
		{"missing pod", "", "falcone-system", "POD_NAME"},
		{"invalid pod", "bad/pod", "falcone-system", "POD_NAME"},
		{"missing namespace", "pod-a", "", "POD_NAMESPACE"},
		{"wrong namespace", "pod-a", "pool-a", "POD_NAMESPACE"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cmd := exec.Command(binary)
			cmd.Env = []string{"FALCONE_SERVERLESS_VALUES=" + values, "POD_NAME=" + tc.pod, "POD_NAMESPACE=" + tc.namespace}
			var stdout, stderr bytes.Buffer
			cmd.Stdout = &stdout
			cmd.Stderr = &stderr
			err := cmd.Run()
			if exit, ok := err.(*exec.ExitError); !ok || exit.ExitCode() != 1 {
				t.Fatalf("exit: %v; stderr: %s", err, stderr.String())
			}
			dec := json.NewDecoder(&stdout)
			var raw json.RawMessage
			if err := dec.Decode(&raw); err != nil {
				t.Fatal(err)
			}
			var more any
			if err := dec.Decode(&more); err != io.EOF {
				t.Fatalf("extra stdout: %v", err)
			}
			if err := status.ValidateJSON(raw); err != nil {
				t.Fatal(err)
			}
			var report status.Report
			_ = json.Unmarshal(raw, &report)
			if report.Preflight.Result != status.ResultFailed || !strings.Contains(report.Preflight.ConfigError, tc.want) {
				t.Fatalf("wrong failure: %+v", report.Preflight)
			}
		})
	}
}
