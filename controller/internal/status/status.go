// Package status defines the local pool-preflight/v1 report the controller prints
// at startup (docs/contracts/pool-preflight/v1). It is not ctr-pool-status-report,
// the per-pool report sent to Falcone.
package status

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"
)

// SchemaVersion is the schemaVersion every report carries.
const SchemaVersion = "pool-preflight/v1"

// Preflight results.
const (
	ResultPassed = "passed"
	ResultFailed = "failed"
)

// Access-check results. error means the check could not be answered.
const (
	CheckAllowed = "allowed"
	CheckDenied  = "denied"
	CheckError   = "error"
)

// Prerequisite states. declared means the values file names the prerequisite;
// it never means the controller verified that the object exists.
const (
	StateDeclared = "declared"
	StateMissing  = "missing"
)

// Prerequisite kinds, the seven facility prerequisites of ctr-controller-config.
const (
	KindNamespace                = "namespace"
	KindGateway                  = "gateway"
	KindStorage                  = "storage"
	KindBroker                   = "broker"
	KindRBAC                     = "rbac"
	KindNetworkPolicyEnforcement = "network-policy-enforcement"
	KindExistingSCC              = "existing-scc"
)

// Kinds lists every prerequisite kind in report order.
var Kinds = []string{
	KindNamespace, KindGateway, KindStorage, KindBroker, KindRBAC,
	KindNetworkPolicyEnforcement, KindExistingSCC,
}

// Required JSON keys at each level of the report.
var (
	RequiredReportFields       = []string{"schemaVersion", "preflight", "prerequisites"}
	RequiredPreflightFields    = []string{"result", "checks"}
	RequiredCheckFields        = []string{"namespace", "group", "resource", "verb", "result", "reason"}
	RequiredPrerequisiteFields = []string{"kind", "name", "state"}
)

// Check is one SelfSubjectAccessReview outcome. Group is empty for the core API
// group and Name is empty unless the review was restricted to one object name.
type Check struct {
	Namespace string `json:"namespace"`
	Group     string `json:"group"`
	Resource  string `json:"resource"`
	Verb      string `json:"verb"`
	Name      string `json:"name,omitempty"`
	Result    string `json:"result"`
	Reason    string `json:"reason"`
}

// Preflight is the access-check section of the report.
type Preflight struct {
	Result      string  `json:"result"`
	ConfigError string  `json:"configError,omitempty"`
	Checks      []Check `json:"checks"`
}

// Prerequisite reports whether the values file declares one prerequisite.
type Prerequisite struct {
	Kind  string `json:"kind"`
	Name  string `json:"name"`
	State string `json:"state"`
}

// Report is the pool-preflight/v1 document.
type Report struct {
	SchemaVersion string         `json:"schemaVersion"`
	Preflight     Preflight      `json:"preflight"`
	Prerequisites []Prerequisite `json:"prerequisites"`
}

// NewReport assembles a report and derives its result: passed only when there is
// no configError, at least one check ran, every check was allowed, and every kind
// is declared at least once and no entry is missing.
func NewReport(checks []Check, prerequisites []Prerequisite, configError string) Report {
	if checks == nil {
		checks = []Check{}
	}
	if prerequisites == nil {
		prerequisites = []Prerequisite{}
	}
	r := Report{
		SchemaVersion: SchemaVersion,
		Preflight:     Preflight{Result: ResultFailed, ConfigError: configError, Checks: checks},
		Prerequisites: prerequisites,
	}
	if len(failureCauses(r)) == 0 {
		r.Preflight.Result = ResultPassed
	}
	return r
}

// failureCauses lists why the report cannot be a passed one.
func failureCauses(r Report) []string {
	var causes []string
	if r.Preflight.ConfigError != "" {
		causes = append(causes, "configError is set")
	}
	if len(r.Preflight.Checks) == 0 {
		causes = append(causes, "no checks ran")
	}
	for i, c := range r.Preflight.Checks {
		if c.Result != CheckAllowed {
			causes = append(causes, fmt.Sprintf("check %d is %s", i, c.Result))
		}
	}
	for _, kind := range Kinds {
		if !slices.ContainsFunc(r.Prerequisites, func(p Prerequisite) bool { return p.Kind == kind && p.State == StateDeclared }) {
			causes = append(causes, "prerequisite kind "+kind+" is not declared")
		}
	}
	for _, p := range r.Prerequisites {
		if p.State != StateDeclared {
			causes = append(causes, "prerequisite "+p.Kind+" is "+p.State)
		}
	}
	return causes
}

// Validate rejects a report that breaks the pool-preflight/v1 contract: a wrong
// schemaVersion, a blank required value, an out-of-enum value, a declared
// prerequisite without a name, or a result its contents do not support.
func Validate(r Report) error {
	var errs []error
	fail := func(format string, args ...any) { errs = append(errs, fmt.Errorf(format, args...)) }
	if r.SchemaVersion != SchemaVersion {
		fail("schemaVersion must be %s", SchemaVersion)
	}
	if r.Preflight.Result != ResultPassed && r.Preflight.Result != ResultFailed {
		fail("preflight.result %q is not passed or failed", r.Preflight.Result)
	}
	if r.Preflight.Checks == nil {
		fail("preflight.checks must be an array")
	}
	for i, c := range r.Preflight.Checks {
		for field, value := range map[string]string{"namespace": c.Namespace, "resource": c.Resource, "verb": c.Verb} {
			if strings.TrimSpace(value) == "" {
				fail("checks[%d].%s is blank", i, field)
			}
		}
		if !slices.Contains([]string{CheckAllowed, CheckDenied, CheckError}, c.Result) {
			fail("checks[%d].result %q is not allowed, denied or error", i, c.Result)
		}
	}
	if r.Prerequisites == nil {
		fail("prerequisites must be an array")
	}
	for i, p := range r.Prerequisites {
		if !slices.Contains(Kinds, p.Kind) {
			fail("prerequisites[%d].kind %q is unknown", i, p.Kind)
		}
		switch p.State {
		case StateDeclared:
			if strings.TrimSpace(p.Name) == "" {
				fail("prerequisites[%d] is declared with a blank name", i)
			}
		case StateMissing:
		default:
			fail("prerequisites[%d].state %q is not declared or missing", i, p.State)
		}
	}
	if len(errs) == 0 {
		causes := failureCauses(r)
		switch {
		case r.Preflight.Result == ResultPassed && len(causes) > 0:
			fail("result passed but %s", causes[0])
		case r.Preflight.Result == ResultFailed && len(causes) == 0:
			fail("result failed but nothing failed")
		}
	}
	return errors.Join(errs...)
}

// ValidateJSON checks an encoded report: every required key must be present at
// every level (an empty group is allowed, an absent one is not), no unknown key is
// accepted, and the decoded report must pass Validate.
func ValidateJSON(data []byte) error {
	var doc map[string]any
	dec := json.NewDecoder(bytes.NewReader(data))
	if err := dec.Decode(&doc); err != nil {
		return fmt.Errorf("report is not a JSON object: %w", err)
	}
	if _, err := dec.Token(); !errors.Is(err, io.EOF) {
		return errors.New("report has data after the JSON object")
	}
	var errs []error
	requireKeys := func(where string, obj map[string]any, fields []string) {
		for _, f := range fields {
			if _, ok := obj[f]; !ok {
				errs = append(errs, fmt.Errorf("%s.%s is required", where, f))
			}
		}
	}
	requireKeys("report", doc, RequiredReportFields)
	if pf, ok := doc["preflight"].(map[string]any); ok {
		requireKeys("preflight", pf, RequiredPreflightFields)
		if checks, ok := pf["checks"].([]any); ok {
			for i, c := range checks {
				if obj, ok := c.(map[string]any); ok {
					requireKeys(fmt.Sprintf("checks[%d]", i), obj, RequiredCheckFields)
				}
			}
		}
	}
	if prereqs, ok := doc["prerequisites"].([]any); ok {
		for i, p := range prereqs {
			if obj, ok := p.(map[string]any); ok {
				requireKeys(fmt.Sprintf("prerequisites[%d]", i), obj, RequiredPrerequisiteFields)
			}
		}
	}
	if len(errs) > 0 {
		return errors.Join(errs...)
	}
	var r Report
	strict := json.NewDecoder(bytes.NewReader(data))
	strict.DisallowUnknownFields()
	if err := strict.Decode(&r); err != nil {
		return fmt.Errorf("report does not match the schema: %w", err)
	}
	return Validate(r)
}
