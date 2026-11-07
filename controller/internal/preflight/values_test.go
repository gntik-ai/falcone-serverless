package preflight_test

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

const validValues = `{
  "watchedPoolNamespaces": ["pool-a", "pool-b"],
  "controllerNamespace": "falcone-system",
  "prerequisites": {
    "gatewayRoute": {"name": "gw"},
    "storage": {"name": "st"},
    "broker": {"name": "br"},
    "rbac": {"controllerRoleBinding": "rb"},
    "networkPolicyEnforcement": {"name": "np"},
    "existingSCC": {"name": "scc"}
  },
  "unknownKey": {"nested": [1, 2]}
}`

func TestParseValuesReadsTheContractKeysAndIgnoresTheRest(t *testing.T) {
	v, err := preflight.ParseValues([]byte(validValues))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(v.WatchedPoolNamespaces, ",") != "pool-a,pool-b" || v.ControllerNamespace != "falcone-system" {
		t.Fatalf("got %+v", v)
	}
	var got []string
	for _, p := range v.Prerequisites() {
		if p.State != status.StateDeclared {
			t.Errorf("%+v must be declared", p)
		}
		got = append(got, p.Kind+"="+p.Name)
	}
	want := "namespace=pool-a,namespace=pool-b,namespace=falcone-system,gateway=gw,storage=st,broker=br,rbac=rb,network-policy-enforcement=np,existing-scc=scc"
	if strings.Join(got, ",") != want {
		t.Fatalf("prerequisites\n got %s\nwant %s", strings.Join(got, ","), want)
	}
}

func TestParseValuesNamespaceMode(t *testing.T) {
	withMode := func(raw string) string {
		var m map[string]any
		_ = json.Unmarshal([]byte(validValues), &m)
		if raw != "<absent>" {
			m["namespaceMode"] = json.RawMessage(raw)
		}
		out, _ := json.Marshal(m)
		return string(out)
	}
	for _, ok := range []string{"<absent>", `"preprovisioned"`} {
		if _, err := preflight.ParseValues([]byte(withMode(ok))); err != nil {
			t.Errorf("namespaceMode %s rejected: %v", ok, err)
		}
	}
	for _, bad := range []string{`null`, `"create"`, `"shared"`, `""`, `"  "`, `"Preprovisioned"`, `"bogus"`, `5`} {
		_, err := preflight.ParseValues([]byte(withMode(bad)))
		if err == nil || !strings.Contains(err.Error(), "namespaceMode") {
			t.Errorf("namespaceMode %s: error %v must name namespaceMode", bad, err)
		}
	}
}

func TestParseValuesRejectsEachInvalidInput(t *testing.T) {
	edit := func(key string, value any) string {
		var m map[string]any
		_ = json.Unmarshal([]byte(validValues), &m)
		if value == nil {
			delete(m, key)
		} else {
			m[key] = value
		}
		out, _ := json.Marshal(m)
		return string(out)
	}
	for name, input := range map[string]string{
		"empty file":               "",
		"invalid JSON":             "{",
		"trailing JSON":            validValues + "{}",
		"not an object":            "[]",
		"null":                     "null",
		"no watchedPoolNamespaces": edit("watchedPoolNamespaces", nil),
		"empty watched list":       edit("watchedPoolNamespaces", []string{}),
		"blank namespace":          edit("watchedPoolNamespaces", []string{"pool-a", "  "}),
		"empty namespace":          edit("watchedPoolNamespaces", []string{""}),
		"duplicate namespace":      edit("watchedPoolNamespaces", []string{"pool-a", "pool-a"}),
		"wildcard namespace":       edit("watchedPoolNamespaces", []string{"*"}),
		"uppercase namespace":      edit("watchedPoolNamespaces", []string{"Pool-A"}),
		"underscore namespace":     edit("watchedPoolNamespaces", []string{"pool_a"}),
		"leading dash namespace":   edit("watchedPoolNamespaces", []string{"-pool"}),
		"dotted namespace":         edit("watchedPoolNamespaces", []string{"pool.a"}),
		"slash namespace":          edit("watchedPoolNamespaces", []string{"a/b"}),
		"namespace over 63":        edit("watchedPoolNamespaces", []string{strings.Repeat("a", 64)}),
		"namespaces not a list":    edit("watchedPoolNamespaces", "pool-a"),
		"absent controllerNs":      edit("controllerNamespace", nil),
		"blank controllerNs":       edit("controllerNamespace", "  "),
		"wildcard controllerNs":    edit("controllerNamespace", "*"),
		"controllerNs not string":  edit("controllerNamespace", 7),
	} {
		t.Run(name, func(t *testing.T) {
			if v, err := preflight.ParseValues([]byte(input)); err == nil {
				t.Fatalf("accepted: %+v", v)
			}
		})
	}
}

func TestParseValuesErrorsNeverEchoFileContent(t *testing.T) {
	for _, input := range []string{
		`{"watchedPoolNamespaces":["SECRET-LEAK"],"controllerNamespace":"ok"}`,
		`{"watchedPoolNamespaces":["pool-a"],"controllerNamespace":"SECRET_LEAK"}`,
		`{"watchedPoolNamespaces":"SECRET-LEAK"}`,
		`SECRET-LEAK`,
	} {
		_, err := preflight.ParseValues([]byte(input))
		if err == nil || strings.Contains(err.Error(), "LEAK") {
			t.Errorf("input %q: error %v", input, err)
		}
	}
}

func TestPrerequisitesAreMissingWhenBlankOrAbsent(t *testing.T) {
	cases := map[string]struct {
		path []string
		kind string
	}{
		"gateway":                    {[]string{"gatewayRoute", "name"}, status.KindGateway},
		"storage":                    {[]string{"storage", "name"}, status.KindStorage},
		"broker":                     {[]string{"broker", "name"}, status.KindBroker},
		"rbac":                       {[]string{"rbac", "controllerRoleBinding"}, status.KindRBAC},
		"network-policy-enforcement": {[]string{"networkPolicyEnforcement", "name"}, status.KindNetworkPolicyEnforcement},
		"existing-scc":               {[]string{"existingSCC", "name"}, status.KindExistingSCC},
	}
	for name, tc := range cases {
		for _, mode := range []string{"deleted field", "blank field", "deleted section", "no prerequisites"} {
			t.Run(name+"/"+mode, func(t *testing.T) {
				var m map[string]any
				_ = json.Unmarshal([]byte(validValues), &m)
				prereqs := m["prerequisites"].(map[string]any)
				section := prereqs[tc.path[0]]
				switch mode {
				case "deleted field":
					if s, ok := section.(map[string]any); ok {
						delete(s, tc.path[1])
					}
				case "blank field":
					if s, ok := section.(map[string]any); ok {
						s[tc.path[1]] = " \t"
					}
				case "deleted section":
					delete(prereqs, tc.path[0])
				case "no prerequisites":
					delete(m, "prerequisites")
				}
				data, _ := json.Marshal(m)
				v, err := preflight.ParseValues(data)
				if err != nil {
					t.Fatalf("a missing prerequisite is reported, not a parse error: %v", err)
				}
				missing := 0
				for _, p := range v.Prerequisites() {
					if p.Kind == tc.kind {
						if p.State != status.StateMissing || p.Name != "" {
							t.Errorf("%+v must be missing with an empty name", p)
						}
						missing++
					} else if mode != "no prerequisites" && p.State != status.StateDeclared {
						t.Errorf("%+v must stay declared", p)
					}
				}
				if missing != 1 {
					t.Fatalf("kind %s listed %d times", tc.kind, missing)
				}
			})
		}
	}
}

func TestMissingPrerequisitesCoversEveryKindWithEmptyNames(t *testing.T) {
	got := preflight.MissingPrerequisites()
	if len(got) != len(status.Kinds) {
		t.Fatalf("got %d entries", len(got))
	}
	for i, p := range got {
		if p.Kind != status.Kinds[i] || p.Name != "" || p.State != status.StateMissing {
			t.Errorf("entry %d = %+v", i, p)
		}
	}
}

func TestElectionTimingsFailClosed(t *testing.T) {
	for _, field := range []string{"leaseDurationSeconds", "renewDeadlineSeconds", "retryPeriodSeconds", "leaderElectionLeaseName"} {
		for _, value := range []any{0, -1, 301, 1.5, "wrong", nil} {
			var m map[string]any
			_ = json.Unmarshal([]byte(validValues), &m)
			m[field] = value
			data, _ := json.Marshal(m)
			if _, err := preflight.ParseValues(data); err == nil {
				t.Fatalf("accepted invalid %s=%v", field, value)
			}
		}
	}
}
