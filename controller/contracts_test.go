package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strings"
	"testing"
)

// These are artifact conformance tests, not runtime permission checks. Only the
// two versioned OPS-01 contracts use this format; other contracts are independent.
var contractsFiles = []string{"resource-ownership", "controller-config"}

var contractsForbiddenToken = regexp.MustCompile(`(?i)\b(customresourcedefinitions?|crds?|clusterroles?|clusterrolebindings?|runtimeclass(?:es)?)\b`)
var contractsSCCToken = regexp.MustCompile(`(?i)\b(sccs?|securitycontextconstraints?)\b`)

func contractsRead(t *testing.T, relative string) string {
	t.Helper()
	_, source, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate contracts_test.go")
	}
	// Preserve ../ in this absolute path. Cleaning it with filepath.Join moves
	// the docs lexically outside the controller module, so Go ignores them as
	// test-cache inputs and can reuse a pass after a contract changes/disappears.
	path := filepath.Dir(source) + string(filepath.Separator) + filepath.FromSlash(relative)
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read required contract %s: %v", path, err)
	}
	return string(body)
}

func checkContracts(name, doc string) []string {
	var reasons []string
	for _, line := range []string{"Version: 1", "Contract: ctr-" + name} {
		if !slices.Contains(strings.Split(doc, "\n"), line) {
			reasons = append(reasons, fmt.Sprintf("header: missing %s", line))
		}
	}
	sections := splitSections(doc)
	required := []string{"Configuration", "Prerequisites", "RBAC", "OCI", "Forbidden"}
	if name == "resource-ownership" {
		required = []string{"Naming", "Ownership", "Repair conditions", "OCI", "Forbidden"}
	}
	counts := map[string]int{}
	for _, line := range strings.Split(doc, "\n") {
		if strings.HasPrefix(line, "## ") {
			counts[strings.TrimSpace(strings.TrimPrefix(line, "## "))]++
		}
	}
	for _, section := range required {
		switch counts[section] {
		case 0:
			reasons = append(reasons, "section: missing "+section)
		case 1:
		default:
			reasons = append(reasons, "section: duplicate "+section)
		}
	}
	if name == "controller-config" {
		reasons = append(reasons, contractsCheckRows(sections["Prerequisites"], "Prerequisites", "Prerequisite", "Requirement", map[string][]string{
			"namespaces": {"Preprovisioned namespaces", "namespaceMode"},
			"gateway":    {"ingress/TLS"}, "storage": {"PostgreSQL", "object storage"}, "broker": {"Kafka"},
			"RBAC": {"namespaced"}, "network-policy enforcement": {"enforcement"}, "existing SCC": {"Existing", "reference only"},
		})...)
		reasons = append(reasons, contractsCheckRows(sections["Configuration"], "Configuration", "Key", "Default or requirement", map[string][]string{
			"namespaceMode":         {"`preprovisioned` (default)"},
			"watchedPoolNamespaces": {"namespace"}, "watchedOCINamespaces": {"namespace"},
			"namespaceBindingsPath":   {"/etc/falcone/namespace-bindings.json", "TEN-01"},
			"controllerNamespace":     {"namespace"},
			"leaderElectionLeaseName": {"falcone-pool-controller-leader"}, "scalerLeaseName": {"falcone-pool-controller-scaler"},
			"leaseDurationSeconds": {"`15`"}, "renewDeadlineSeconds": {"`10`"}, "retryPeriodSeconds": {"`2`"},
			"falconeDesiredStateEndpoint": {"HTTPS"}, "falconeStatusEndpoint": {"HTTPS"},
			"serviceIdentitySecretName":      {"falcone-controller-service-identity"},
			"platformRegistryPullSecretName": {"falcone-platform-registry-pull"},
			"metricsPort":                    {"`9090`"}, "probePort": {"`8081`"},
		})...)
	} else {
		for section, phrases := range map[string][]string{
			"Naming": {"falcone-pool-controller-leader", "falcone-pool-controller-scaler"},
			"Ownership": {"`falcone-pool-controller`", "Labels are not a security boundary or API authorization.",
				"`falcone.ai/owner`", "`falcone.ai/desired-state-generation`", "`falcone.ai/ownership-version`", "UID", "resourceVersion"},
			"OCI": {"one Deployment", "one headless Service", "`falcone.ai/runtime-mode`", "`falcone.ai/revision`"},
		} {
			for _, phrase := range phrases {
				if !strings.Contains(strings.Join(strings.Fields(sections[section]), " "), phrase) {
					reasons = append(reasons, section+": missing "+phrase)
				}
			}
		}
		reasons = append(reasons, contractsCheckRows(sections["Repair conditions"], "Repair conditions", "Condition", "Trigger and effect", map[string][]string{
			"UIDMismatch":             {"keep the resource", "block completion"},
			"ResourceVersionMismatch": {"keep the resource", "block completion"},
			"OwnershipMismatch":       {"keep the resource", "block completion"},
		})...)
	}
	// splitSections drops the preamble. Retain it explicitly for permission tables.
	for i, line := range strings.Split(doc, "\n") {
		if strings.HasPrefix(line, "## ") {
			sections[""] = strings.Join(strings.Split(doc, "\n")[:i], "\n")
			break
		}
	}
	for section, body := range sections {
		// Markdown permits tables without outer pipes. Normalize those lines for
		// detection only; the canonical RBAC table still has the fixed format.
		lines := strings.Split(body, "\n")
		for i, line := range lines {
			if strings.Contains(line, "|") && !strings.HasPrefix(strings.TrimSpace(line), "|") {
				lines[i] = "| " + strings.TrimSpace(line)
			}
		}
		for _, table := range parseTables(strings.Join(lines, "\n")) {
			// Detection is case-insensitive even though the canonical table is fixed.
			header := mdTable{header: make([]string, len(table.header))}
			for i, value := range table.header {
				header.header[i] = strings.ToLower(cellText(value))
			}
			if _, shaped := columns(header, "kind", "verbs"); shaped && (section != "RBAC" || name != "controller-config") {
				reasons = append(reasons, "RBAC: table outside RBAC in "+section)
			}
		}
	}
	if name == "controller-config" {
		reasons = append(reasons, contractsCheckRBAC(sections["RBAC"])...)
	}
	section := ""
	for _, line := range strings.Split(doc, "\n") {
		if strings.HasPrefix(line, "## ") {
			section = strings.TrimSpace(strings.TrimPrefix(line, "## "))
		}
		if section != "Forbidden" {
			for _, token := range contractsForbiddenToken.FindAllString(line, -1) {
				reasons = append(reasons, "forbidden token: "+token+" outside Forbidden")
			}
			if section != "Prerequisites" {
				for _, token := range contractsSCCToken.FindAllString(line, -1) {
					reasons = append(reasons, "SCC: token "+token+" outside Prerequisites and Forbidden")
				}
			}
		}
	}
	for _, token := range []string{"CustomResourceDefinition", "CRD", "ClusterRole", "ClusterRoleBinding", "RuntimeClass"} {
		if !regexp.MustCompile(`\b` + token + `\b`).MatchString(sections["Forbidden"]) {
			reasons = append(reasons, "Forbidden: missing "+token)
		}
	}
	if !strings.Contains(strings.Join(strings.Fields(sections["Forbidden"]), " "), "Creating or modifying any SCC") {
		reasons = append(reasons, "Forbidden: missing Creating or modifying any SCC")
	}
	return reasons
}

func contractsCheckRBAC(body string) []string {
	tb, found := findTable(body, "Access")
	cols, ok := columns(tb, "Access", "Kind", "Verbs", "Scope")
	if !found || !ok || len(parseTables(body)) != 1 || len(tb.header) != 4 {
		return []string{"RBAC: invalid fixed table"}
	}
	var reasons []string
	seen := map[string]int{}
	writeVerbs := []string{"create", "delete", "get", "list", "patch", "update", "watch"}
	readVerbs := []string{"get", "list", "watch"}
	for _, row := range tb.rows {
		kind := cell(row, cols[1])
		if len(row) != len(tb.header) || cell(row, cols[3]) == "" {
			reasons = append(reasons, "RBAC: unparseable row "+kind)
			continue
		}
		seen[kind]++
		var wantAccess string
		var wantVerbs []string
		switch kind {
		case "Deployment", "Service", "Lease":
			wantAccess, wantVerbs = "write", writeVerbs
		case "ConfigMap", "NetworkPolicy":
			wantAccess, wantVerbs = "read", readVerbs
		case "Secret":
			wantAccess, wantVerbs = "read", []string{"get"}
			if !strings.Contains(cell(row, cols[3]), "Named Secrets only via resourceNames") {
				reasons = append(reasons, "RBAC: invalid Secret scope")
			}
		default:
			reasons = append(reasons, "RBAC: forbidden kind "+kind)
			continue
		}
		if cell(row, cols[0]) != wantAccess {
			reasons = append(reasons, "RBAC: forbidden access for "+kind)
		}
		verbs := strings.Split(cell(row, cols[2]), ",")
		for i := range verbs {
			verbs[i] = strings.TrimSpace(verbs[i])
		}
		slices.Sort(verbs)
		if !slices.Equal(verbs, wantVerbs) {
			reasons = append(reasons, "RBAC: invalid verbs for "+kind)
		}
	}
	for _, kind := range []string{"Deployment", "Service", "Lease", "ConfigMap", "Secret", "NetworkPolicy"} {
		if seen[kind] != 1 {
			reasons = append(reasons, "RBAC: expected exactly one "+kind)
		}
	}
	for _, line := range strings.Split(body, "\n") {
		if strings.Contains(line, "|") && !strings.HasPrefix(strings.TrimSpace(line), "|") {
			reasons = append(reasons, "RBAC: unparseable row")
		}
	}
	return reasons
}

// The shared Markdown helpers intentionally tolerate loose tables. Contract tables
// are stricter: exactly one, with fixed columns, one complete row per required key.
func contractsCheckRows(body, section, keyColumn, valueColumn string, requirements map[string][]string) []string {
	tables := parseTables(body)
	tb, found := findTable(body, keyColumn)
	cols, ok := columns(tb, keyColumn, valueColumn)
	if !found || !ok || len(tables) != 1 || len(tb.header) != 2 {
		return []string{section + ": invalid fixed table"}
	}
	var reasons []string
	seen := map[string]int{}
	for _, row := range tb.rows {
		key := cell(row, cols[0])
		seen[key]++
		phrases, known := requirements[key]
		if !known || len(row) != len(tb.header) {
			reasons = append(reasons, section+": unparseable row "+key)
			continue
		}
		for _, phrase := range phrases {
			// Keep Markdown in values: literal names/defaults are part of v1.
			if !strings.Contains(row[cols[1]], phrase) {
				reasons = append(reasons, section+": "+key+" missing "+phrase)
			}
		}
	}
	for key := range requirements {
		if seen[key] != 1 {
			reasons = append(reasons, section+": expected exactly one "+key)
		}
	}
	return reasons
}

func TestContractsCanonical(t *testing.T) {
	for _, name := range contractsFiles {
		t.Run(name, func(t *testing.T) {
			doc := contractsRead(t, "../docs/contracts/"+name+".md")
			if reasons := checkContracts(name, doc); len(reasons) != 0 {
				t.Fatalf("invalid canonical contract:\n%s", strings.Join(reasons, "\n"))
			}
		})
	}
}

func TestContractsVersion(t *testing.T) {
	for _, name := range contractsFiles {
		t.Run(name, func(t *testing.T) {
			doc := contractsRead(t, "../docs/contracts/"+name+".md")
			wantError(t, checkContracts(name, replaceOnce(t, doc, "Version: 1\n", "")), "header: missing Version: 1")
		})
	}
}

func TestContractsStructure(t *testing.T) {
	for name, sections := range map[string][]string{
		"resource-ownership": {"Naming", "Ownership", "Repair conditions", "OCI", "Forbidden"},
		"controller-config":  {"Configuration", "Prerequisites", "RBAC", "OCI", "Forbidden"},
	} {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, section := range sections {
			t.Run(name+"/missing/"+section, func(t *testing.T) {
				wantError(t, checkContracts(name, dropSection(t, doc, section)), "section: missing "+section)
			})
			t.Run(name+"/duplicate/"+section, func(t *testing.T) {
				wantError(t, checkContracts(name, doc+"\n## "+section+"\nextra\n"), "section: duplicate "+section)
			})
		}
	}
}

func TestContractsPrerequisites(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	for _, key := range []string{"namespaces", "gateway", "storage", "broker", "RBAC", "network-policy enforcement", "existing SCC"} {
		t.Run(key, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", dropRow(t, doc, key)), "Prerequisites", key)
		})
	}
	for _, key := range []string{"namespaces", "broker"} {
		t.Run("empty/"+key, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", setCell(t, doc, key, 1, "")), "Prerequisites", key)
		})
	}
}

func TestContractsConfiguration(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	for _, key := range []string{
		"namespaceMode", "watchedPoolNamespaces", "watchedOCINamespaces", "namespaceBindingsPath", "controllerNamespace",
		"leaderElectionLeaseName", "scalerLeaseName", "leaseDurationSeconds", "renewDeadlineSeconds", "retryPeriodSeconds",
		"falconeDesiredStateEndpoint", "falconeStatusEndpoint", "serviceIdentitySecretName", "platformRegistryPullSecretName", "metricsPort", "probePort",
	} {
		t.Run(key, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", dropRow(t, doc, key)), "Configuration", key)
		})
	}
	for key, value := range map[string]string{
		"namespaceMode": "shared (default)", "serviceIdentitySecretName": "unnamed", "platformRegistryPullSecretName": "unnamed",
	} {
		t.Run("changed/"+key, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", setCell(t, doc, key, 1, value)), "Configuration", key)
		})
	}
}

func contractsEditSection(t *testing.T, doc, section string, edit func(string) string) string {
	t.Helper()
	marker := "\n## " + section + "\n"
	start := strings.Index(doc, marker)
	if start < 0 {
		t.Fatalf("missing section %s", section)
	}
	start += len(marker)
	end := len(doc)
	if next := strings.Index(doc[start:], "\n## "); next >= 0 {
		end = start + next
	}
	return doc[:start] + edit(doc[start:end]) + doc[end:]
}

func TestContractsOwnership(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/resource-ownership.md")
	for _, tc := range []struct{ section, phrase string }{
		{"Naming", "falcone-pool-controller-leader"},
		{"Naming", "falcone-pool-controller-scaler"},
		{"Ownership", "`falcone-pool-controller`"},
		{"Ownership", "Labels are not a security boundary or API authorization."},
		{"Ownership", "`falcone.ai/owner`"},
		{"Ownership", "`falcone.ai/desired-state-generation`"},
		{"Ownership", "`falcone.ai/ownership-version`"},
		{"Ownership", "UID"},
		{"Ownership", "resourceVersion"},
		{"OCI", "one Deployment"},
		{"OCI", "one headless Service"},
		{"OCI", "`falcone.ai/runtime-mode`"},
		{"OCI", "`falcone.ai/revision`"},
	} {
		t.Run(tc.section+"/"+tc.phrase, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, tc.section, func(body string) string {
				if !strings.Contains(body, tc.phrase) {
					t.Fatalf("mutation did not land: %s", tc.phrase)
				}
				return strings.ReplaceAll(body, tc.phrase, "removed")
			})
			wantError(t, checkContracts("resource-ownership", mutated), tc.section, tc.phrase)
		})
	}
	for _, reason := range []string{"UIDMismatch", "ResourceVersionMismatch", "OwnershipMismatch"} {
		t.Run(reason, func(t *testing.T) {
			wantError(t, checkContracts("resource-ownership", dropRow(t, doc, reason)), "Repair conditions", reason)
		})
	}
}

func TestContractsRBAC(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	row := "| write | Deployment | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces |"
	for _, kind := range []string{"Pod", "Job", "Secret", "ClusterRole", "CRD", "Deployment, Job", "*"} {
		t.Run("write/"+kind, func(t *testing.T) {
			mutated := replaceOnce(t, doc, row, row+"\n| write | "+kind+" | create | extra |")
			wantError(t, checkContracts("controller-config", mutated), "RBAC", kind)
		})
	}
	for _, tc := range []struct{ name, old, replacement, reason string }{
		{"wildcard", "get, list, watch, create, update, patch, delete", "*", "verbs"},
		{"unknown verb", "get, list, watch, create, update, patch, delete", "get, list, watch, create, update, patch, delete, bind", "verbs"},
		{"missing verb", "get, list, watch, create, update, patch, delete", "get, list, watch, create, update, patch", "verbs"},
		{"duplicate verb", "get, list, watch, create, update, patch, delete", "get, list, watch, create, update, patch, delete, get", "verbs"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			newRow := strings.Replace(row, tc.old, tc.replacement, 1)
			wantError(t, checkContracts("controller-config", replaceOnce(t, doc, row, newRow)), "RBAC", tc.reason)
		})
	}
	for _, verbs := range []string{"get, list", "get, watch", "get, create"} {
		t.Run("Secret/"+verbs, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", replaceOnce(t, doc, "| read | Secret | get |", "| read | Secret | "+verbs+" |")), "RBAC", "Secret", "verbs")
		})
	}
	for _, row := range []string{
		"| read | Namespace | get | extra |", "| write | Deployment | create |", "| unknown | Deployment | get | extra |",
		"| read | Pod | get | extra |", "| read | Job | get | extra |", "| read | ConfigMap, Secret | get | extra |",
	} {
		t.Run(row, func(t *testing.T) {
			wantError(t, checkContracts("controller-config", replaceOnce(t, doc, "\n## OCI\n", row+"\n\n## OCI\n")), "RBAC")
		})
	}
	for _, kind := range []string{"Deployment", "Service", "Lease", "ConfigMap", "Secret", "NetworkPolicy"} {
		t.Run("missing/"+kind, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "RBAC", func(body string) string {
				var lines []string
				for _, line := range strings.Split(body, "\n") {
					if strings.HasPrefix(line, "| ") && strings.Contains(line, "| "+kind+" |") {
						continue
					}
					lines = append(lines, line)
				}
				if strings.Join(lines, "\n") == body {
					t.Fatal("row removal did not land")
				}
				return strings.Join(lines, "\n")
			})
			wantError(t, checkContracts("controller-config", mutated), "RBAC", kind)
		})
	}
	for _, name := range contractsFiles {
		original := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, section := range []string{"preamble", "OCI", "Forbidden", "OCI extra"} {
			t.Run(name+"/extra table/"+section, func(t *testing.T) {
				extra := "\n| kind | verbs |\n| --- | --- |\n| Deployment | get |\n"
				mutated := original
				switch section {
				case "preamble":
					mutated = extra + original
				case "OCI extra":
					mutated += "\n## OCI extra\n" + extra
				default:
					mutated = contractsEditSection(t, original, section, func(body string) string { return body + extra })
				}
				wantError(t, checkContracts(name, mutated), "RBAC: table outside RBAC")
			})
		}
	}
}

func TestContractsTokens(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		bodySection := "Configuration"
		if name == "resource-ownership" {
			bodySection = "Naming"
		}
		for _, token := range []string{
			"CustomResourceDefinition", "customresourcedefinitions", "CRD", "crds", "ClusterRole", "clusterroles",
			"ClusterRoleBinding", "clusterrolebindings", "RuntimeClass", "runtimeclasses", "SCC", "sccs", "SecurityContextConstraints", "securitycontextconstraints",
		} {
			for _, section := range []string{"preamble", bodySection, "OCI"} {
				t.Run(name+"/"+section+"/"+token, func(t *testing.T) {
					mutated := token + "\n" + doc
					if section != "preamble" {
						mutated = contractsEditSection(t, doc, section, func(body string) string { return body + "\n" + token + "\n" })
					}
					reason := "forbidden token: " + token
					if strings.HasPrefix(strings.ToLower(token), "scc") || strings.HasPrefix(strings.ToLower(token), "securitycontext") {
						reason = "SCC: token " + token
					}
					wantError(t, checkContracts(name, mutated), reason)
				})
			}
		}
		t.Run(name+"/substrings", func(t *testing.T) {
			mutated := "sccache disccommand myclusterrolesuffix runtimeclassesExtra\n" + doc
			if reasons := checkContracts(name, mutated); len(reasons) != 0 {
				t.Fatalf("token substrings are not whole tokens: %v", reasons)
			}
		})
	}
}

func TestContractsForbidden(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, phrase := range []string{"CustomResourceDefinition", "CRD", "ClusterRole", "ClusterRoleBinding", "RuntimeClass", "Creating or modifying any SCC"} {
			t.Run(name+"/"+phrase, func(t *testing.T) {
				mutated := contractsEditSection(t, doc, "Forbidden", func(body string) string {
					old := phrase
					if phrase == "ClusterRole" {
						old += "," // Do not also edit ClusterRoleBinding.
					}
					return replaceOnce(t, body, old, "removed")
				})
				wantError(t, checkContracts(name, mutated), "Forbidden", phrase)
			})
		}
	}
}

func TestContractsFixtures(t *testing.T) {
	for _, tc := range []struct{ file, reason string }{
		{"positive.md", ""},
		{"no-version.md", "header: missing Version: 1"},
		{"missing-broker.md", "Prerequisites: expected exactly one broker"},
		{"clusterrole-write.md", "RBAC: forbidden kind ClusterRole"},
		{"crd-write.md", "RBAC: forbidden kind CRD"},
		{"scc-write.md", "SCC: token SecurityContextConstraints"},
		{"pod-write.md", "RBAC: forbidden kind Pod"},
		{"job-write.md", "RBAC: forbidden kind Job"},
		{"secret-write.md", "RBAC: forbidden access for Secret"},
	} {
		t.Run(tc.file, func(t *testing.T) {
			doc := contractsRead(t, "testdata/contracts/"+tc.file)
			reasons := checkContracts("controller-config", doc)
			if tc.reason == "" {
				if len(reasons) != 0 {
					t.Fatalf("positive existing-SCC fixture rejected: %v", reasons)
				}
			} else {
				wantError(t, reasons, tc.reason)
			}
		})
	}
}

func TestContractsLocationIndependent(t *testing.T) {
	t.Chdir(t.TempDir())
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		if reasons := checkContracts(name, doc); len(reasons) != 0 {
			t.Fatalf("contract %s from another working directory: %v", name, reasons)
		}
	}
}

func TestContractsRBACWithoutOuterPipes(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		extra := "\nKind | Verbs\n--- | ---\nPod | create\n"
		mutated := contractsEditSection(t, doc, "OCI", func(body string) string { return body + extra })
		wantError(t, checkContracts(name, mutated), "RBAC: table outside RBAC")
	}
}

func TestContractsSecretScope(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	mutated := replaceOnce(t, doc,
		"Named Secrets only via resourceNames, never credential values in reports or logs",
		"All Secrets")
	wantError(t, checkContracts("controller-config", mutated), "RBAC", "Secret", "scope")
}

// Exercise the documented command, including Go's result cache, in an isolated
// checkout layout. Mutating the real contracts here would race other tests.
func TestContractsCacheInvalidation(t *testing.T) {
	_, source, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate test source")
	}
	root := t.TempDir()
	controller := filepath.Join(root, "controller")
	docs := filepath.Join(root, "docs", "contracts")
	for _, dir := range []string{controller, docs} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	entries, err := os.ReadDir(filepath.Dir(source))
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), ".go") && entry.Name() != "go.mod" {
			continue
		}
		body := contractsRead(t, entry.Name())
		if err := os.WriteFile(filepath.Join(controller, entry.Name()), []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	for _, name := range contractsFiles {
		body := contractsRead(t, "../docs/contracts/"+name+".md")
		if err := os.WriteFile(filepath.Join(docs, name+".md"), []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	run := func() (string, error) {
		cmd := exec.Command("go", "test", "-run", "^TestContractsCanonical$", "./...")
		cmd.Dir = controller
		cmd.Env = append(os.Environ(), "GOWORK=off", "GOTOOLCHAIN=local")
		output, err := cmd.CombinedOutput()
		return string(output), err
	}
	for _, name := range contractsFiles {
		for _, mutation := range []string{"version", "missing"} {
			t.Run(name+"/"+mutation, func(t *testing.T) {
				if out, err := run(); err != nil {
					t.Fatalf("valid command failed: %v\n%s", err, out)
				}
				path := filepath.Join(docs, name+".md")
				body, err := os.ReadFile(path)
				if err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() {
					if err := os.WriteFile(path, body, 0o600); err != nil {
						t.Fatal(err)
					}
				})
				want := "header: missing Version: 1"
				if mutation == "missing" {
					if err := os.Remove(path); err != nil {
						t.Fatal(err)
					}
					want = "read required contract"
				} else if err := os.WriteFile(path, []byte(replaceOnce(t, string(body), "Version: 1\n", "")), 0o600); err != nil {
					t.Fatal(err)
				}
				out, err := run()
				if err == nil || !strings.Contains(out, want) {
					t.Fatalf("command must reject %s %s with %q; err=%v\n%s", name, mutation, want, err, out)
				}
			})
		}
	}
}
