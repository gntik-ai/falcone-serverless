package supplychain_test

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/supplychain"
)

func TestPolicyCases(t *testing.T) {
	table := loadTable(t)
	var cases []struct {
		Name     string             `json:"name"`
		Values   any                `json:"values"`
		Policy   supplychain.Policy `json:"policy"`
		Expected supplychain.Report `json:"expected"`
	}
	readJSON(t, "testdata/cases.json", &cases)
	if len(cases) == 0 {
		t.Fatal("empty case set")
	}
	seen := map[string]bool{}
	for _, tc := range cases {
		if seen[tc.Name] {
			t.Fatalf("duplicate case %s", tc.Name)
		}
		seen[tc.Name] = true
		t.Run(tc.Name, func(t *testing.T) {
			got, err := json.Marshal(supplychain.Evaluate(tc.Values, table, tc.Policy))
			if err != nil {
				t.Fatal(err)
			}
			want, err := json.Marshal(tc.Expected)
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(got, want) {
				t.Fatalf("got %s; want %s", got, want)
			}
			again, err := json.Marshal(supplychain.Evaluate(tc.Values, table, tc.Policy))
			if err != nil || !bytes.Equal(got, again) {
				t.Fatal("repeated evaluation changed report")
			}
		})
	}
	for _, name := range []string{"pass", "pass-oci-enabled", "tag-only", "tag-and-digest", "bad-digest", "missing-required", "untrusted-origin", "prefix-boundary", "oci-disabled-unpinned", "oci-enabled-missing", "empty-prefixes"} {
		if !seen[name] {
			t.Errorf("missing case %s", name)
		}
	}
}

func readJSON(t *testing.T, path string, out any) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if strings.HasPrefix(path, "testdata/") && (bytes.Contains(data, []byte("\r")) || !bytes.HasSuffix(data, []byte("\n")) || bytes.HasSuffix(data, []byte("\n\n"))) {
		t.Fatal("testdata must use LF and exactly one final newline")
	}
	if err := json.Unmarshal(data, out); err != nil {
		t.Fatal(err)
	}
	return data
}

func loadTable(t *testing.T) supplychain.Table {
	t.Helper()
	var table supplychain.Table
	readJSON(t, "testdata/chart-values-v1-images.json", &table)
	if err := table.Validate(); err != nil {
		t.Fatal(err)
	}
	return table
}

func TestTableValidation(t *testing.T) {
	loadTable(t)
	for name, mutate := range map[string]func(*supplychain.Table){
		"empty version":      func(v *supplychain.Table) { v.ContractVersion = "" },
		"unknown version":    func(v *supplychain.Table) { v.ContractVersion = "v2" },
		"empty enable path":  func(v *supplychain.Table) { v.OCIEnablePath = "" },
		"no entries":         func(v *supplychain.Table) { v.Entries = nil },
		"duplicate":          func(v *supplychain.Table) { v.Entries = append(v.Entries, v.Entries[0]) },
		"unknown kind":       func(v *supplychain.Table) { v.Entries[0].Kind = "unknown" },
		"empty path":         func(v *supplychain.Table) { v.Entries[0].Path = "" },
		"empty path segment": func(v *supplychain.Table) { v.Entries[0].Path = "controller..image" },
	} {
		t.Run(name, func(t *testing.T) {
			table := loadTable(t)
			mutate(&table)
			if err := table.Validate(); err == nil {
				t.Fatal("invalid table accepted")
			}
		})
	}
	for _, component := range []string{"controller", "worker", "compiler"} {
		t.Run("no image for "+component, func(t *testing.T) {
			table := loadTable(t)
			for i := range table.Entries {
				if table.Entries[i].Component == component {
					table.Entries[i].Kind = "repository"
				}
			}
			if err := table.Validate(); err == nil {
				t.Fatal("missing component image accepted")
			}
		})
	}
}

const testDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

func goodValues() map[string]any {
	return map[string]any{
		"controller": map[string]any{"image": map[string]any{"repository": "docker.io/fs-test/controller", "digest": testDigest}},
		"worker":     map[string]any{"image": map[string]any{"repository": "docker.io/fs-test/worker", "digest": testDigest}},
		"compiler":   map[string]any{"image": map[string]any{"repository": "docker.io/fs-test/compiler", "digest": testDigest}},
	}
}

func hubPolicy() supplychain.Policy {
	return supplychain.Policy{TrustedRepositoryPrefixes: []string{"docker.io/"}}
}

func TestDigestInputShapes(t *testing.T) {
	for name, image := range map[string]any{
		"tag in repository":     map[string]any{"repository": "docker.io/fs-test/worker:latest", "digest": testDigest},
		"digest in repository":  map[string]any{"repository": "docker.io/fs-test/worker@" + testDigest, "digest": testDigest},
		"unknown key":           map[string]any{"repository": "docker.io/fs-test/worker", "digest": testDigest, "extra": true},
		"missing repository":    map[string]any{"digest": testDigest},
		"empty repository":      map[string]any{"repository": "", "digest": testDigest},
		"missing digest":        map[string]any{"repository": "docker.io/fs-test/worker"},
		"non-string repository": map[string]any{"repository": 12, "digest": testDigest},
		"non-string digest":     map[string]any{"repository": "docker.io/fs-test/worker", "digest": []any{}},
		"uppercase digest":      map[string]any{"repository": "docker.io/fs-test/worker", "digest": "sha256:" + strings.Repeat("A", 64)},
		"digest newline":        map[string]any{"repository": "docker.io/fs-test/worker", "digest": testDigest + "\n"},
		"short digest":          map[string]any{"repository": "docker.io/fs-test/worker", "digest": "sha256:" + strings.Repeat("a", 63)},
		"long digest":           map[string]any{"repository": "docker.io/fs-test/worker", "digest": testDigest + "a"},
		"null":                  nil,
		"string":                "fs-test-not-an-object",
		"array":                 []any{},
	} {
		t.Run(name, func(t *testing.T) {
			values := goodValues()
			values["worker"].(map[string]any)["image"] = image
			report := supplychain.Evaluate(values, loadTable(t), hubPolicy())
			if report.Result != "fail" || !reflect.DeepEqual(report.Rules[0].Violations, []string{"worker.image: expected repository and sha256 digest without a tag"}) {
				t.Fatalf("invalid image accepted: %+v", report)
			}
		})
	}
}

func TestRepositoryNormalization(t *testing.T) {
	for _, tc := range []struct {
		repository, prefix string
		pass               bool
	}{
		{"fs-test-worker", "docker.io/library/", true},
		{"library/fs-test-worker", "docker.io/library/", true},
		{"fs-test/worker", "docker.io/fs-test/", true},
		{"docker.io/fs-test-worker", "docker.io/library/", true},
		{"docker.io/fs-test/worker", "docker.io/fs-test/", true},
		{"docker.io/fs-test/a/worker", "docker.io/fs-test/", true},
		{"docker.io/fs-test/worker", "docker.io/fs-test", false},
		{"docker.io/fs-test/worker", "", false},
		{"docker.io/fs-test-x/worker", "docker.io/fs-test/", false},
		{"quay.io/fs-test/worker", "docker.io/", false},
		{"localhost/fs-test/worker", "docker.io/", false},
		{"docker.io:5000/fs-test/worker", "docker.io/", false},
		{"https://docker.io/fs-test/worker", "docker.io/", false},
		{"docker.io/fs-test/../worker", "docker.io/", false},
		{"docker.io//worker", "docker.io/", false},
		{"docker.io/FS-TEST/worker", "docker.io/", false},
	} {
		t.Run(tc.repository+" prefix "+tc.prefix, func(t *testing.T) {
			values := goodValues()
			values["worker"].(map[string]any)["image"].(map[string]any)["repository"] = tc.repository
			report := supplychain.Evaluate(values, loadTable(t), supplychain.Policy{TrustedRepositoryPrefixes: []string{tc.prefix}})
			violations := report.Rules[1].Violations
			found := false
			for _, violation := range violations {
				found = found || violation == "worker.image: untrusted repository"
			}
			if found == tc.pass {
				t.Fatalf("origin verdict incorrect: %+v", report)
			}
		})
	}
}

func TestOCIRequiredOnlyOnBooleanTrue(t *testing.T) {
	for _, enabled := range []any{nil, false, "true", 1, map[string]any{}, []any{true}, true} {
		values := goodValues()
		values["oci"] = map[string]any{"enabled": enabled}
		report := supplychain.Evaluate(values, loadTable(t), hubPolicy())
		if enabledBool, _ := enabled.(bool); enabledBool {
			if !reflect.DeepEqual(report.Rules[1].Violations, []string{"oci.intakeMirrorRepository: missing"}) {
				t.Fatalf("required mirror not reported: %+v", report)
			}
		} else if report.Result != "pass" {
			t.Fatalf("non-true enable value required OCI: %+v", report)
		}
	}
}

func TestEmptyPrefixesRejectAllPresentPaths(t *testing.T) {
	values := goodValues()
	values["oci"] = map[string]any{"intakeMirrorRepository": "docker.io/fs-test/mirror"}
	report := supplychain.Evaluate(values, loadTable(t), supplychain.Policy{})
	want := []string{"compiler.image: untrusted repository", "controller.image: untrusted repository", "oci.intakeMirrorRepository: untrusted repository", "worker.image: untrusted repository"}
	if report.Rules[0].Result != "pass" || !reflect.DeepEqual(report.Rules[1].Violations, want) {
		t.Fatalf("not every present path rejected: %+v", report)
	}
}

func TestPathsComeFromTableIncludingOCIImages(t *testing.T) {
	table := loadTable(t)
	table.OCIEnablePath = "fsTest.active"
	table.Entries[0].Path = "fsTest.control"
	table.Entries = append(table.Entries, supplychain.Entry{Path: "fsTest.shim", Component: "fs-test-shim", Kind: "image", OCI: true})
	values := goodValues()
	values["fsTest"] = map[string]any{"control": values["controller"].(map[string]any)["image"]}
	delete(values, "controller")
	if report := supplychain.Evaluate(values, table, hubPolicy()); report.Result != "pass" {
		t.Fatalf("table paths not respected: %+v", report)
	}
	values["fsTest"].(map[string]any)["shim"] = map[string]any{"repository": "quay.io/fs-test/shim", "tag": "latest"}
	if report := supplychain.Evaluate(values, table, hubPolicy()); len(report.Rules[0].Violations) != 1 || len(report.Rules[1].Violations) != 1 {
		t.Fatalf("present disabled OCI image not checked by both rules: %+v", report)
	}
	values["fsTest"].(map[string]any)["active"] = true
	delete(values["fsTest"].(map[string]any), "shim")
	if report := supplychain.Evaluate(values, table, hubPolicy()); !reflect.DeepEqual(report.Rules[0].Violations, []string{"fsTest.shim: missing"}) {
		t.Fatalf("enabled OCI image not required: %+v", report)
	}
}

func TestMalformedValuesAndDeterminism(t *testing.T) {
	table := loadTable(t)
	for _, values := range []any{nil, true, 1, "fs-test", []any{}, map[string]any{"worker": false}, map[string]any{"controller": map[string]any{"image": false}, "oci": map[string]any{"intakeMirrorRepository": 42}}} {
		before, _ := json.Marshal(values)
		first := supplychain.Evaluate(values, table, hubPolicy())
		second := supplychain.Evaluate(values, table, hubPolicy())
		one, _ := json.Marshal(first)
		two, _ := json.Marshal(second)
		after, _ := json.Marshal(values)
		if first.Result != "fail" || !bytes.Equal(one, two) || !bytes.Equal(before, after) {
			t.Fatalf("malformed input accepted, changed or nondeterministic: %s", one)
		}
		if first.Rules[0].ID != "image-digest-pinned" || first.Rules[1].ID != "image-trusted-origin" {
			t.Fatal("rule order changed")
		}
		for _, rule := range first.Rules {
			if !sort.StringsAreSorted(rule.Violations) {
				t.Fatal("violations not sorted")
			}
		}
	}
	table.ContractVersion = "v2"
	if report := supplychain.Evaluate(goodValues(), table, hubPolicy()); report.Result != "fail" {
		t.Fatal("invalid table did not fail closed")
	}
}

func TestViolationsDoNotEchoValues(t *testing.T) {
	values := goodValues()
	marker := strings.Repeat("fs-test-sensitive-", 20)
	values["worker"] = map[string]any{"image": map[string]any{"repository": marker, "digest": marker}}
	values["unrelated"] = marker
	data, _ := json.Marshal(supplychain.Evaluate(values, loadTable(t), hubPolicy()))
	if bytes.Contains(data, []byte("fs-test-sensitive-")) || len(data) > 1024 {
		t.Fatal("report echoed input values or was unbounded")
	}
}

func TestTableMatchesPinnedSchema(t *testing.T) {
	table := loadTable(t)
	var schema map[string]any
	data := readJSON(t, "../../../docs/contracts/serverless-chart-values/chart-values.v1.schema.json", &schema)
	inventory, err := os.ReadFile("../../../docs/contracts/falcone-contract-inventory.md")
	if err != nil {
		t.Fatal(err)
	}
	pin := ""
	for _, line := range strings.Split(string(inventory), "\n") {
		cells := strings.Split(line, "|")
		if len(cells) == 6 && strings.TrimSpace(cells[1]) == "ctr-serverless-chart-values" && strings.TrimSpace(cells[2]) == "v1" {
			if pin != "" || strings.TrimSpace(cells[3]) != "docs/contracts/serverless-chart-values/chart-values.v1.schema.json" {
				t.Fatal("ambiguous inventory row or changed schema path")
			}
			pin = strings.TrimSpace(cells[4])
		}
	}
	if pin != fmt.Sprintf("%x", sha256.Sum256(data)) {
		t.Fatal("schema bytes do not match inventory pin")
	}
	if schema["$id"] != "ctr-serverless-chart-values/v1" || table.OCIEnablePath != "oci.enabled" {
		t.Fatal("wrong contract or enable path")
	}
	paths := map[string][]string{"image": {}, "repository": {}}
	var walk func(map[string]any, string, map[string]bool)
	walk = func(node map[string]any, path string, active map[string]bool) {
		if ref, ok := node["$ref"].(string); ok {
			switch ref {
			case "#/$defs/image", "#/$defs/repository":
				paths[strings.TrimPrefix(ref, "#/$defs/")] = append(paths[strings.TrimPrefix(ref, "#/$defs/")], path)
				return // Image objects are leaves; their repository member is not a separate table entry.
			}
			if !strings.HasPrefix(ref, "#/$defs/") || active[ref] {
				t.Fatal("non-local or cyclic schema reference")
			}
			defs := schema["$defs"].(map[string]any)
			target, ok := defs[strings.TrimPrefix(ref, "#/$defs/")].(map[string]any)
			if !ok {
				t.Fatal("unresolved schema reference")
			}
			active[ref] = true
			walk(target, path, active)
			delete(active, ref)
		}
		properties, _ := node["properties"].(map[string]any)
		for key, value := range properties {
			child := key
			if path != "" {
				child = path + "." + key
			}
			walk(value.(map[string]any), child, active)
		}
	}
	walk(schema, "", map[string]bool{})
	for _, kind := range []string{"image", "repository"} {
		got := []string{}
		for _, entry := range table.Entries {
			if entry.Kind == kind {
				got = append(got, entry.Path)
			}
			if entry.OCI != strings.HasPrefix(entry.Path, "oci.") || (entry.Kind == "image" && entry.Component != strings.Split(entry.Path, ".")[0]) {
				t.Fatal("table role does not match contract v1")
			}
		}
		sort.Strings(got)
		sort.Strings(paths[kind])
		if !reflect.DeepEqual(got, paths[kind]) {
			t.Fatalf("%s paths: table %v; schema %v", kind, got, paths[kind])
		}
	}
}

func checkDeployment(values, source, prefixes []byte, table supplychain.Table) error {
	var v any
	if err := json.Unmarshal(values, &v); err != nil {
		return fmt.Errorf("deployment values: invalid JSON")
	}
	var trusted []string
	if err := json.Unmarshal(prefixes, &trusted); err != nil || !reflect.DeepEqual(trusted, []string{"docker.io/"}) {
		return fmt.Errorf("policy.trustedRepositoryPrefixes must be exactly [docker.io/]")
	}
	report := supplychain.Evaluate(v, table, supplychain.Policy{TrustedRepositoryPrefixes: trusted})
	if report.Result != "pass" {
		data, _ := json.Marshal(report)
		return fmt.Errorf("deployment values: %s", data)
	}
	var provenance map[string]string
	if err := json.Unmarshal(source, &provenance); err != nil {
		return fmt.Errorf("deployment source: invalid object")
	}
	switch provenance["source"] {
	case "synthetic":
		if len(provenance) != 3 || provenance["contractVersion"] != "v1" || provenance["sha256"] != fmt.Sprintf("%x", sha256.Sum256(values)) {
			return fmt.Errorf("deployment source: synthetic version or values SHA256 mismatch")
		}
	case "falcone-charts":
		if len(provenance) != 5 || strings.TrimSpace(provenance["repo"]) == "" || strings.TrimSpace(provenance["path"]) == "" ||
			!regexp.MustCompile(`^[0-9a-fA-F]{40}$`).MatchString(provenance["commit"]) ||
			!regexp.MustCompile(`^[0-9a-fA-F]{64}$`).MatchString(provenance["sha256"]) {
			return fmt.Errorf("deployment source: chart repo, commit, path and SHA256 required")
		}
	default:
		return fmt.Errorf("deployment source: unknown source")
	}
	return nil
}

func TestDeploymentValues(t *testing.T) {
	values, source, prefixes := deploymentFiles(t)
	if err := checkDeployment(values, source, prefixes, loadTable(t)); err != nil {
		t.Fatal(err)
	}
}

func deploymentFiles(t *testing.T) ([]byte, []byte, []byte) {
	t.Helper()
	var v, s, p any
	values := readJSON(t, "testdata/deployment/values.json", &v)
	source := readJSON(t, "testdata/deployment/source.json", &s)
	prefixes := readJSON(t, "testdata/deployment/trusted-prefixes.json", &p)
	return values, source, prefixes
}

func TestDeploymentRejectsValueMutations(t *testing.T) {
	values, source, prefixes := deploymentFiles(t)
	for _, tc := range []struct {
		name string
		edit func(map[string]any)
	}{
		{"tag-only", func(v map[string]any) {
			v["worker"].(map[string]any)["image"] = map[string]any{"repository": "docker.io/fs-test/worker", "tag": "latest"}
		}},
		{"untrusted", func(v map[string]any) {
			v["worker"].(map[string]any)["image"].(map[string]any)["repository"] = "quay.io/fs-test/worker"
		}},
		{"deleted", func(v map[string]any) { delete(v["worker"].(map[string]any), "image") }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var v map[string]any
			if err := json.Unmarshal(values, &v); err != nil {
				t.Fatal(err)
			}
			tc.edit(v)
			mutated, _ := json.Marshal(v)
			err := checkDeployment(mutated, source, prefixes, loadTable(t))
			if err == nil || !strings.Contains(err.Error(), "worker.image:") {
				t.Fatalf("mutation did not fail on worker.image: %v", err)
			}
		})
	}
}

func TestDeploymentRejectsPrefixAndSourceMutations(t *testing.T) {
	values, source, prefixes := deploymentFiles(t)
	for _, bad := range []string{`[]`, `null`, `["quay.io/"]`, `["docker.io/","quay.io/"]`, `["docker.io/","docker.io/"]`, `["docker.io"]`, `{}`} {
		if err := checkDeployment(values, source, []byte(bad), loadTable(t)); err == nil {
			t.Fatalf("invalid prefix policy accepted: %s", bad)
		}
	}
	chartsSource := map[string]any{"source": "falcone-charts", "repo": "gntik-ai/falcone-charts", "commit": strings.Repeat("b", 40), "path": "fs-test/values.yaml", "sha256": strings.Repeat("c", 64)}
	charts, _ := json.Marshal(chartsSource)
	if err := checkDeployment(values, charts, prefixes, loadTable(t)); err != nil {
		t.Fatalf("valid chart source rejected: %v", err)
	}
	for _, valid := range [][]byte{source, charts} {
		var original map[string]any
		if err := json.Unmarshal(valid, &original); err != nil {
			t.Fatal(err)
		}
		for field := range original {
			for _, replacement := range []any{nil, "", 42, "fs-test-invalid"} {
				// repo and path are opaque non-empty source locations.
				if replacement == "fs-test-invalid" && (field == "repo" || field == "path") {
					continue
				}
				t.Run(original["source"].(string)+" "+field+fmt.Sprint(replacement), func(t *testing.T) {
					mutated := make(map[string]any)
					for k, v := range original {
						mutated[k] = v
					}
					if replacement == nil {
						delete(mutated, field)
					} else {
						mutated[field] = replacement
					}
					data, _ := json.Marshal(mutated)
					if err := checkDeployment(values, data, prefixes, loadTable(t)); err == nil {
						t.Fatal("invalid source accepted")
					}
				})
			}
		}
	}
	for _, bad := range []string{`null`, `[]`, `{`, `{"source":"synthetic"}`} {
		if err := checkDeployment(values, []byte(bad), prefixes, loadTable(t)); err == nil {
			t.Fatal("malformed source accepted")
		}
	}
	var synthetic map[string]any
	json.Unmarshal(source, &synthetic)
	synthetic["sha256"] = strings.Repeat("0", 64)
	stale, _ := json.Marshal(synthetic)
	if err := checkDeployment(values, stale, prefixes, loadTable(t)); err == nil {
		t.Fatal("stale synthetic hash accepted")
	}
}

func TestPolicyDocumentation(t *testing.T) {
	data, err := os.ReadFile("../../../docs/operations/supply-chain-policy.md")
	if err != nil {
		t.Fatal(err)
	}
	for _, term := range append([]string{"image-digest-pinned", "image-trusted-origin", "ctr-serverless-chart-values", "v1", "policy.trustedRepositoryPrefixes"}, tablePaths(loadTable(t))...) {
		if !strings.Contains(string(data), term) {
			t.Errorf("policy documentation missing %s", term)
		}
	}
}

func tablePaths(table supplychain.Table) []string {
	paths := make([]string, 0, len(table.Entries))
	for _, entry := range table.Entries {
		paths = append(paths, entry.Path)
	}
	return paths
}
