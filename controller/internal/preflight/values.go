package preflight

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"regexp"
	"strings"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

// ModePreprovisioned is the only namespaceMode this controller implements.
const ModePreprovisioned = "preprovisioned"

var dnsLabel = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// Values holds the fields the preflight reads from the chart values file
// (ctr-controller-config keys plus the prerequisites section of
// ctr-serverless-chart-values). Every other key is ignored. Prerequisite fields
// hold trimmed names, empty when absent or blank.
type Values struct {
	LeaseDuration            time.Duration
	RenewDeadline            time.Duration
	RetryPeriod              time.Duration
	WatchedPoolNamespaces    []string
	ControllerNamespace      string
	Gateway                  string
	Storage                  string
	Broker                   string
	RBAC                     string
	NetworkPolicyEnforcement string
	ExistingSCC              string
}

type namedRef struct {
	Name string `json:"name"`
}

type rawValues struct {
	LeaseDuration         json.RawMessage `json:"leaseDurationSeconds"`
	RenewDeadline         json.RawMessage `json:"renewDeadlineSeconds"`
	RetryPeriod           json.RawMessage `json:"retryPeriodSeconds"`
	LeaderLeaseName       json.RawMessage `json:"leaderElectionLeaseName"`
	WatchedPoolNamespaces []string        `json:"watchedPoolNamespaces"`
	ControllerNamespace   string          `json:"controllerNamespace"`
	NamespaceMode         json.RawMessage `json:"namespaceMode"`
	Prerequisites         struct {
		GatewayRoute namedRef `json:"gatewayRoute"`
		Storage      namedRef `json:"storage"`
		Broker       namedRef `json:"broker"`
		RBAC         struct {
			ControllerRoleBinding string `json:"controllerRoleBinding"`
		} `json:"rbac"`
		NetworkPolicyEnforcement namedRef `json:"networkPolicyEnforcement"`
		ExistingSCC              namedRef `json:"existingSCC"`
	} `json:"prerequisites"`
}

// ParseValues validates the values file. Its errors name fields and indexes, never
// file content, except the namespaceMode value, which is clipped.
func ParseValues(data []byte) (Values, error) {
	var raw rawValues
	dec := json.NewDecoder(bytes.NewReader(data))
	if err := dec.Decode(&raw); err != nil {
		return Values{}, errors.New("values file is not a valid JSON object with the expected field types")
	}
	if _, err := dec.Token(); !errors.Is(err, io.EOF) {
		return Values{}, errors.New("values file has data after the JSON object")
	}
	var errs []error
	timings := []int64{15, 10, 2}
	for i, value := range []json.RawMessage{raw.LeaseDuration, raw.RenewDeadline, raw.RetryPeriod} {
		if len(value) > 0 {
			var n int64
			if err := json.Unmarshal(value, &n); err != nil || n <= 0 || n > 300 {
				errs = append(errs, errors.New("leader election timings must be positive integer seconds at most 300"))
			}
			timings[i] = n
		}
	}
	if timings[0] <= timings[1] || timings[1] <= timings[2] {
		errs = append(errs, errors.New("leader election requires leaseDurationSeconds > renewDeadlineSeconds > retryPeriodSeconds > 0"))
	}
	if len(raw.LeaderLeaseName) > 0 {
		var name string
		if json.Unmarshal(raw.LeaderLeaseName, &name) != nil || name != LeaderLeaseName {
			errs = append(errs, errors.New("leaderElectionLeaseName must be falcone-pool-controller-leader"))
		}
	}

	if err := checkNamespaceMode(raw.NamespaceMode); err != nil {
		errs = append(errs, err)
	}
	if len(raw.WatchedPoolNamespaces) == 0 {
		errs = append(errs, errors.New("watchedPoolNamespaces must list at least one namespace"))
	}
	seen := map[string]bool{}
	for i, ns := range raw.WatchedPoolNamespaces {
		switch {
		case !validNamespace(ns):
			errs = append(errs, fmt.Errorf("watchedPoolNamespaces[%d] is not a DNS-1123 label", i))
		case seen[ns]:
			errs = append(errs, fmt.Errorf("watchedPoolNamespaces[%d] repeats an earlier namespace", i))
		}
		seen[ns] = true
	}
	if !validNamespace(raw.ControllerNamespace) {
		errs = append(errs, errors.New("controllerNamespace is absent or not a DNS-1123 label"))
	}
	if len(errs) > 0 {
		return Values{}, errors.Join(errs...)
	}
	p := raw.Prerequisites
	return Values{
		LeaseDuration: time.Duration(timings[0]) * time.Second, RenewDeadline: time.Duration(timings[1]) * time.Second, RetryPeriod: time.Duration(timings[2]) * time.Second,
		WatchedPoolNamespaces:    raw.WatchedPoolNamespaces,
		ControllerNamespace:      raw.ControllerNamespace,
		Gateway:                  strings.TrimSpace(p.GatewayRoute.Name),
		Storage:                  strings.TrimSpace(p.Storage.Name),
		Broker:                   strings.TrimSpace(p.Broker.Name),
		RBAC:                     strings.TrimSpace(p.RBAC.ControllerRoleBinding),
		NetworkPolicyEnforcement: strings.TrimSpace(p.NetworkPolicyEnforcement.Name),
		ExistingSCC:              strings.TrimSpace(p.ExistingSCC.Name),
	}, nil
}

// checkNamespaceMode accepts an absent mode (the default) and preprovisioned. The
// create, shared, blank and unknown modes fail closed: none is implemented.
func checkNamespaceMode(raw json.RawMessage) error {
	if len(raw) == 0 {
		return nil
	}
	var mode string
	if err := json.Unmarshal(raw, &mode); err != nil {
		return errors.New("namespaceMode must be a string")
	}
	if mode != ModePreprovisioned {
		return fmt.Errorf("namespaceMode %q is not supported; only %s is implemented", clip(mode), ModePreprovisioned)
	}
	return nil
}

// ValidNamespace reports whether a release namespace is a DNS-1123 label.
func ValidNamespace(ns string) bool { return validNamespace(ns) }

func validNamespace(ns string) bool { return len(ns) <= 63 && dnsLabel.MatchString(ns) }

func clip(s string) string {
	if r := []rune(s); len(r) > 40 {
		return string(r[:40]) + "..."
	}
	return s
}

// Prerequisites reports the seven kinds. A kind is declared when the values file
// names it; the controller verifies none of them. The namespace kind has one entry
// per pool namespace plus the controllerNamespace.
func (v Values) Prerequisites() []status.Prerequisite {
	var out []status.Prerequisite
	for _, ns := range append(append([]string{}, v.WatchedPoolNamespaces...), v.ControllerNamespace) {
		out = append(out, declaration(status.KindNamespace, ns))
	}
	for _, p := range []struct{ kind, name string }{
		{status.KindGateway, v.Gateway},
		{status.KindStorage, v.Storage},
		{status.KindBroker, v.Broker},
		{status.KindRBAC, v.RBAC},
		{status.KindNetworkPolicyEnforcement, v.NetworkPolicyEnforcement},
		{status.KindExistingSCC, v.ExistingSCC},
	} {
		out = append(out, declaration(p.kind, p.name))
	}
	return out
}

func declaration(kind, name string) status.Prerequisite {
	if name == "" {
		return status.Prerequisite{Kind: kind, Name: "", State: status.StateMissing}
	}
	return status.Prerequisite{Kind: kind, Name: name, State: status.StateDeclared}
}

// MissingPrerequisites is the report when no values could be read: every kind is
// missing, with an empty name.
func MissingPrerequisites() []status.Prerequisite {
	out := make([]status.Prerequisite, len(status.Kinds))
	for i, kind := range status.Kinds {
		out[i] = status.Prerequisite{Kind: kind, State: status.StateMissing}
	}
	return out
}
