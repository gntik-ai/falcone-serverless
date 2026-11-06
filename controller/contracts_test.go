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

var contractsForbiddenToken = regexp.MustCompile(`(?i)^(customresourcedefinitions?|crds?|clusterroles?|clusterrolebindings?|runtimeclass(?:es)?)$`)
var contractsWords = regexp.MustCompile(`[\pL\pN]+`)
var contractsSCCToken = regexp.MustCompile(`(?i)^(sccs?|securitycontextconstraints?)$`)

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
	versions := 0
	inPreamble := true
	// These fixed-format contracts support ATX headings only. Reject other
	// heading forms rather than let rendered sections inherit token exemptions.
	for _, line := range strings.Split(doc, "\n") {
		if heading := contractsHeading.FindStringSubmatch(line); heading != nil && heading[1] == "##" {
			inPreamble = false
		}
		if strings.HasPrefix(line, "Version:") {
			versions++
			if !inPreamble {
				reasons = append(reasons, "header: version outside preamble")
			}
		}
		unquoted := strings.TrimLeft(line, " >\t")
		quotedHeading := strings.HasPrefix(strings.TrimSpace(line), ">") &&
			(contractsHeading.MatchString(unquoted) || contractsSetextUnderline.MatchString(unquoted))
		if contractsSetextUnderline.MatchString(line) || quotedHeading || contractsHTMLHeading.MatchString(line) {
			reasons = append(reasons, "structure: unsupported heading syntax")
		}
		if strings.Contains(line, "<!--") || contractsHTMLTable.MatchString(line) {
			reasons = append(reasons, "structure: raw HTML unsupported")
		}
	}
	if versions != 1 {
		reasons = append(reasons, "header: expected exactly one version")
	}
	for _, line := range []string{"Version: 1", "Contract: ctr-" + name} {
		if !slices.Contains(strings.Split(doc, "\n"), line) {
			reasons = append(reasons, fmt.Sprintf("header: missing %s", line))
		}
	}
	blocks := contractsBlocks(doc)
	counts := map[string]int{}
	var normalized []string
	for _, block := range blocks {
		if block.section != "" {
			counts[block.section]++
		}
		normalized = append(normalized, block.lines...)
	}
	sections := splitSections(strings.Join(normalized, "\n"))
	required := []string{"Configuration", "Prerequisites", "RBAC", "OCI", "Forbidden"}
	if name == "resource-ownership" {
		required = []string{"Naming", "Ownership", "Repair conditions", "OCI", "Forbidden"}
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
		for _, phrase := range []string{"No config key widens the Role.", "OCI adds no verb or kind."} {
			if !strings.Contains(sections["RBAC"], phrase) {
				reasons = append(reasons, "permission ceiling: missing "+phrase)
			}
		}
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
				"`falcone.ai/owner`", "`falcone.ai/desired-state-generation`", "`falcone.ai/ownership-version`", "UID", "resourceVersion",
				"No durable per-object UID/resourceVersion registry", "After restart or failover", "re-establish ownership",
				"fresh read that passed ownership verification in the same attempt",
				"observed before that verified read are not mismatches", "current attempt only",
				"retry after a fresh read and full ownership verification"},
			"OCI": {"one Deployment", "one headless Service", "`falcone.ai/runtime-mode`", "`falcone.ai/revision`"},
		} {
			for _, phrase := range phrases {
				if !strings.Contains(strings.Join(strings.Fields(sections[section]), " "), phrase) {
					reasons = append(reasons, section+": missing "+phrase)
				}
			}
		}
		reasons = append(reasons, contractsCheckRows(sections["Repair conditions"], "Repair conditions", "Condition", "Trigger and effect", map[string][]string{
			"UIDMismatch": {"keep the resource", "block completion", "UID changed since the ownership-verified read in this attempt"},
			"ResourceVersionMismatch": {"keep the resource", "block completion", "resourceVersion precondition conflict in this attempt",
				"clear after a fresh read verifies the same UID and owner", "retry using that read's resourceVersion"},
			"OwnershipMismatch": {"keep the resource", "block completion"},
		})...)
	}
	reasons = append(reasons, contractsCheckExtraRBAC(name, blocks)...)
	if name == "controller-config" {
		reasons = append(reasons, contractsCheckRBAC(sections["RBAC"])...)
	}
	for _, block := range blocks {
		// Underscores delimit Markdown emphasis as well as token boundaries.
		// Scan words independently so adjacent restricted tokens are all seen.
		for _, line := range block.lines {
			for _, token := range contractsWords.FindAllString(line, -1) {
				if block.section != "Forbidden" && contractsForbiddenToken.MatchString(token) {
					reasons = append(reasons, "forbidden token: "+token+" outside Forbidden")
				}
				if block.section != "Forbidden" && block.section != "Prerequisites" && contractsSCCToken.MatchString(token) {
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

// Every ATX heading starts a new permission-scan block, even an empty title or
// a heading at a different level. Never inherit an exception across headings.
var contractsHeading = regexp.MustCompile(`^ {0,3}(#{1,6})(?:[ \t]+(.*)|[ \t]*)$`)
var contractsClosingHashes = regexp.MustCompile(`(?:^|[ \t]+)#+[ \t]*$`)
var contractsSetextUnderline = regexp.MustCompile(`^ {0,3}(?:=+|-+)[ \t]*$`)
var contractsHTMLHeading = regexp.MustCompile(`(?i)</?h[1-6](?:[ \t/>]|$)`)
var contractsHTMLTable = regexp.MustCompile(`(?i)</?(?:table|thead|tbody|tfoot|tr|th|td)(?:[ \t/>]|$)`)

type contractsBlock struct {
	section string
	lines   []string
}

// Use one heading walker for section counts, bodies and permission exemptions.
// Normalize level-two headings for the shared splitSections helper while keeping
// preamble and other heading levels as separate permission-scan blocks.
func contractsBlocks(doc string) []contractsBlock {
	blocks := []contractsBlock{{}}
	for _, line := range strings.Split(doc, "\n") {
		if heading := contractsHeading.FindStringSubmatch(line); heading != nil {
			block := contractsBlock{}
			if heading[1] == "##" {
				block.section = strings.TrimSpace(contractsClosingHashes.ReplaceAllString(heading[2], ""))
				line = "## " + block.section
			}
			blocks = append(blocks, block)
		}
		last := len(blocks) - 1
		blocks[last].lines = append(blocks[last].lines, line)
	}
	return blocks
}

func contractsCheckExtraRBAC(name string, blocks []contractsBlock) []string {
	var reasons []string
	for _, block := range blocks {
		var lines []string
		for _, line := range block.lines {
			// Normalize blockquoted and pipeless Markdown tables for detection only.
			line = strings.TrimLeft(line, "> \t")
			if strings.Contains(line, "|") && !strings.HasPrefix(line, "|") {
				line = "| " + line
			}
			lines = append(lines, line)
		}
		for _, table := range parseTables(strings.Join(lines, "\n")) {
			// A pipe-containing paragraph may precede the real Markdown header
			// without a blank line. Inspect every row in the shared parser's run.
			for _, row := range append([][]string{table.header}, table.rows...) {
				// Include emphasized, plural and resource/verb aliases so they
				// cannot add permissions outside the fixed table.
				header := mdTable{header: make([]string, len(row))}
				for i, value := range row {
					value = strings.ToLower(strings.TrimSpace(strings.Trim(cellText(value), "*_")))
					switch value {
					case "kinds", "resource", "resources", "api kind":
						value = "kind"
					case "verb", "allowed verbs", "permissions":
						value = "verbs"
					}
					header.header[i] = value
				}
				if _, shaped := columns(header, "kind", "verbs"); shaped && (block.section != "RBAC" || name != "controller-config") {
					reasons = append(reasons, "RBAC: table outside RBAC in "+block.section)
				}
			}
		}
	}
	return reasons
}

func contractsCheckRBAC(body string) []string {
	var reasons []string
	for _, line := range strings.Split(body, "\n") {
		if strings.Contains(line, "|") && !strings.HasPrefix(strings.TrimSpace(line), "|") {
			reasons = append(reasons, "RBAC: unparseable row")
		}
	}
	tb, found := findTable(body, "Access")
	cols, ok := columns(tb, "Access", "Kind", "Verbs", "Scope")
	if !found || !ok || len(parseTables(body)) != 1 || len(tb.header) != 4 {
		return append(reasons, "RBAC: invalid fixed table")
	}
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
		default:
			reasons = append(reasons, "RBAC: forbidden kind "+kind)
			continue
		}
		scope := cell(row, cols[3])
		wantScope := map[string]string{
			"Deployment":    "Bound pool and OCI namespaces",
			"Service":       "Bound pool and OCI namespaces",
			"Lease":         "controllerNamespace, create is unrestricted by name in controllerNamespace; resourceNames restricts update, patch and delete to the two fixed Lease names",
			"ConfigMap":     "Authorized configuration in explicitly watched namespaces",
			"Secret":        "Named Secrets only via resourceNames, never credential values in reports or logs",
			"NetworkPolicy": "Bound namespaces, prerequisite inspection only",
		}[kind]
		if wantScope != "" && scope != wantScope {
			reasons = append(reasons, "RBAC: invalid "+kind+" scope")
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
	return reasons
}

// The shared Markdown helpers intentionally tolerate loose tables. Contract tables
// are stricter: exactly one, with fixed columns, one complete row per required key.
func contractsCheckRows(body, section, keyColumn, valueColumn string, requirements map[string][]string) []string {
	var reasons []string
	for _, line := range strings.Split(body, "\n") {
		if strings.Contains(line, "|") && !strings.HasPrefix(strings.TrimSpace(line), "|") {
			reasons = append(reasons, section+": unparseable row")
		}
	}
	tables := parseTables(body)
	tb, found := findTable(body, keyColumn)
	cols, ok := columns(tb, keyColumn, valueColumn)
	if !found || !ok || len(tables) != 1 || len(tb.header) != 2 {
		return append(reasons, section+": invalid fixed table")
	}
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

func TestContractsVersionPlacement(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		t.Run(name+"/conflicting version", func(t *testing.T) {
			mutated := replaceOnce(t, doc, "Version: 1\n", "Version: 1\nVersion: 2\n")
			wantError(t, checkContracts(name, mutated), "header: expected exactly one version")
		})
		t.Run(name+"/version in Forbidden", func(t *testing.T) {
			mutated := replaceOnce(t, doc, "Version: 1\n", "")
			mutated = contractsEditSection(t, mutated, "Forbidden", func(body string) string { return body + "\nVersion: 1\n" })
			wantError(t, checkContracts(name, mutated), "header: version outside preamble")
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
			for _, prefix := range []string{"## ", " ## ", "  ## ", "   ## ", "##\t", "   ##\t"} {
				t.Run(name+"/duplicate/"+section+"/"+prefix, func(t *testing.T) {
					wantError(t, checkContracts(name, doc+"\n"+prefix+section+"\nextra\n"), "section: duplicate "+section)
				})
			}
		}
	}
}

func TestContractsPermissionHeadingAliases(t *testing.T) {
	for _, tc := range []struct{ name, heading, body, reason string }{
		{"controller-config", "   ## RBAC", "| Kind | Verbs |\n| --- | --- |\n| Pod | create |", "RBAC"},
		{"controller-config", "##\tRBAC", "| Kind | Verbs |\n| --- | --- |\n| Pod | create |", "RBAC"},
		{"controller-config", "  ## Forbidden", "ClusterRole write granted", "Forbidden"},
		{"controller-config", "  ## Forbidden ###", "ClusterRole write granted", "Forbidden"},
		{"resource-ownership", "  ## Forbidden", "CRD managed here", "Forbidden"},
		{"resource-ownership", "  ## Forbidden ###", "CRD managed here", "Forbidden"},
		{"controller-config", " ## Prerequisites", "The controller creates an SCC", "Prerequisites"},
	} {
		t.Run(tc.name+"/"+tc.heading, func(t *testing.T) {
			doc := contractsRead(t, "../docs/contracts/"+tc.name+".md")
			wantError(t, checkContracts(tc.name, doc+"\n"+tc.heading+"\n"+tc.body+"\n"), "section: duplicate "+tc.reason)
		})
	}
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, prefix := range []string{" ## ", "  ## ", "   ## ", "##\t", "   ##\t"} {
			t.Run(name+"/valid/"+prefix, func(t *testing.T) {
				mutated := strings.ReplaceAll(doc, "\n## ", "\n"+prefix)
				if reasons := checkContracts(name, mutated); len(reasons) != 0 {
					t.Fatalf("valid level-two headings rejected: %v", reasons)
				}
			})
		}
	}
}

func TestContractsClosingHeadings(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		lines := strings.Split(doc, "\n")
		for i, line := range lines {
			if strings.HasPrefix(line, "## ") {
				lines[i] = line + " ###\t"
			}
		}
		t.Run(name, func(t *testing.T) {
			if reasons := checkContracts(name, strings.Join(lines, "\n")); len(reasons) != 0 {
				t.Fatalf("valid closing heading hashes rejected: %v", reasons)
			}
		})
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

func TestContractsSecretPrerequisites(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	body := strings.Join(strings.Fields(splitSections(doc)["Prerequisites"]), " ")
	for _, clause := range []string{
		"`serviceIdentitySecretName` must exist in `controllerNamespace`.",
		"When OCI is enabled, `platformRegistryPullSecretName` must exist in every resolved OCI namespace.",
		"A missing named Secret is reported and makes the entire controller not ready",
		"nothing is created or adopted in any watched namespace, including Wasm pools.",
		"A pull Secret is not required when OCI is disabled.",
	} {
		if !strings.Contains(body, clause) {
			t.Errorf("Prerequisites: missing Secret readiness rule: %s", clause)
		}
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
	for _, tc := range []struct{ row, reason string }{
		{"| read | Namespace | get | extra |", "RBAC: forbidden kind Namespace"},
		{"| write | Deployment | create |", "RBAC: unparseable row Deployment"},
		{"| write | Deployment | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces | extra |", "RBAC: unparseable row Deployment"},
		{"| unknown | Deployment | get | extra |", "RBAC: forbidden access for Deployment"},
		{"| read | Pod | get | extra |", "RBAC: forbidden kind Pod"},
		{"| read | Job | get | extra |", "RBAC: forbidden kind Job"},
		{"| read | ConfigMap, Secret | get | extra |", "RBAC: forbidden kind ConfigMap, Secret"},
		{"write | Deployment | create | extra", "RBAC: unparseable row"},
	} {
		t.Run(tc.row, func(t *testing.T) {
			mutated := replaceOnce(t, doc, row, row+"\n"+tc.row)
			wantError(t, checkContracts("controller-config", mutated), tc.reason)
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
			sections := []string{"preamble", bodySection, "OCI", "RBAC"}
			reason := "forbidden token: " + token
			if strings.HasPrefix(strings.ToLower(token), "scc") || strings.HasPrefix(strings.ToLower(token), "securitycontext") {
				reason = "SCC: token " + token
			} else {
				sections = append(sections, "Prerequisites")
			}
			for _, section := range sections {
				t.Run(name+"/"+section+"/"+token, func(t *testing.T) {
					mutated := token + "\n" + doc
					if section != "preamble" {
						if strings.Contains(doc, "\n## "+section+"\n") {
							mutated = contractsEditSection(t, doc, section, func(body string) string { return body + "\n" + token + "\n" })
						} else {
							mutated = doc + "\n## " + section + "\n" + token + "\n"
						}
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

func TestContractsRBACAfterPipeParagraph(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, paragraph := range []string{"see a | b", "| note |", "text | before | table"} {
			t.Run(name+"/"+paragraph, func(t *testing.T) {
				mutated := contractsEditSection(t, doc, "OCI", func(body string) string {
					return body + "\n" + paragraph + "\n| Kind | Verbs |\n| --- | --- |\n| Pod | create |\n"
				})
				wantError(t, checkContracts(name, mutated), "RBAC: table outside RBAC in OCI")
			})
		}
	}
}

func TestContractsSecretScope(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	for _, scope := range []string{
		"All Secrets",
		"Named Secrets only via resourceNames in all namespaces cluster wide, never credential values in reports or logs",
	} {
		t.Run(scope, func(t *testing.T) {
			mutated := replaceOnce(t, doc,
				"Named Secrets only via resourceNames, never credential values in reports or logs", scope)
			wantError(t, checkContracts("controller-config", mutated), "RBAC: invalid Secret scope")
		})
	}
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

// Permission scans must inspect every Markdown block, including content that the
// shared level-two section helper intentionally discards.
func TestContractsRBACMarkdownBoundaries(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, heading := range []string{"## OCI extra", "## ", "# Appendix", "### Allowed"} {
			for _, header := range []string{
				"**Kind** | **Verbs**", "__Kind__ | _Verbs_", "Kinds | Verbs", "Kind | Verb", "Resource | Verbs", "Resources | Verb",
				"API kind | Allowed verbs", "Kind | Permissions",
			} {
				t.Run(name+"/"+heading+"/"+header, func(t *testing.T) {
					mutated := doc + "\n" + heading + "\n| " + header + " |\n| --- | --- |\n| Pod | create |\n"
					wantError(t, checkContracts(name, mutated), "RBAC: table outside RBAC")
				})
			}
		}
	}
}

func TestContractsTokenMarkdownBoundaries(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		for _, token := range []string{"__CRD__", "_ClusterRole_", "__clusterrolebindings__", "_runtimeclasses_", "_SCC_", "__securitycontextconstraints__", "CRD SCC ClusterRole SCC"} {
			for _, heading := range []string{"preamble", "# Appendix", "### Allowed", "## ", "  ## Appendix"} {
				t.Run(name+"/"+heading+"/"+token, func(t *testing.T) {
					mutated := doc + "\n" + heading + "\n" + token + "\n"
					if heading == "preamble" {
						mutated = token + "\n" + doc
					}
					if strings.Contains(strings.ToLower(token), "scc") || strings.Contains(token, "securitycontext") {
						wantError(t, checkContracts(name, mutated), "SCC: token")
					}
					if strings.Contains(strings.ToLower(token), "crd") || strings.Contains(strings.ToLower(token), "clusterrole") || strings.Contains(token, "runtimeclasses") {
						wantError(t, checkContracts(name, mutated), "forbidden token:")
					}
				})
			}
		}
	}
}

func TestContractsUnsupportedHeadings(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		sections := []string{"Forbidden"}
		if name == "controller-config" {
			sections = append(sections, "Prerequisites")
		}
		for _, section := range sections {
			prose := "The controller creates a ClusterRole and a CRD."
			if section == "Prerequisites" {
				prose = "The installer writes a new SCC."
			}
			for _, heading := range []string{
				"Allowed\n-------", "Allowed\n=======", "Allowed\n   ---  ",
				"> ## Allowed", "> > ### Allowed", "> Allowed\n> -------",
				"<h2>Allowed</h2>", "<H2 id=allowed>Allowed</H2>",
				"<h2\n>Allowed",
			} {
				t.Run(name+"/"+section+"/"+heading, func(t *testing.T) {
					mutated := contractsEditSection(t, doc, section, func(body string) string {
						return body + "\n" + heading + "\n" + prose + "\n"
					})
					wantError(t, checkContracts(name, mutated), "structure: unsupported heading syntax")
				})
			}
		}
	}
}

func TestContractsRawHTML(t *testing.T) {
	for _, name := range contractsFiles {
		doc := contractsRead(t, "../docs/contracts/"+name+".md")
		t.Run(name+"/commented Forbidden", func(t *testing.T) {
			mutated := replaceOnce(t, doc, "## Forbidden", "<!--\n## Forbidden") + "\n-->\n"
			wantError(t, checkContracts(name, mutated), "structure: raw HTML unsupported")
		})
		t.Run(name+"/HTML permissions table", func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "OCI", func(body string) string {
				return body + "\n<table><tr><th>Kind</th><th>Verbs</th></tr><tr><td>Pod</td><td>create</td></tr></table>\n"
			})
			wantError(t, checkContracts(name, mutated), "structure: raw HTML unsupported")
		})
		t.Run(name+"/multiline HTML table", func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "OCI", func(body string) string {
				return body + "\n<table\n>\nKind | Verbs\n--- | ---\nPod | create\n"
			})
			wantError(t, checkContracts(name, mutated), "structure: raw HTML unsupported")
		})
	}
}

func TestContractsFixedTableWidths(t *testing.T) {
	for _, tc := range []struct{ name, key, section string }{
		{"controller-config", "broker", "Prerequisites"},
		{"controller-config", "metricsPort", "Configuration"},
		{"resource-ownership", "UIDMismatch", "Repair conditions"},
	} {
		doc := contractsRead(t, "../docs/contracts/"+tc.name+".md")
		for _, extra := range []string{"", " | unexpected"} {
			t.Run(tc.section+"/"+extra, func(t *testing.T) {
				var original string
				for _, line := range strings.Split(doc, "\n") {
					if strings.HasPrefix(line, "| "+tc.key+" |") || strings.HasPrefix(line, "| `"+tc.key+"` |") {
						original = line
						break
					}
				}
				if original == "" {
					t.Fatal("required fixture row not found")
				}
				row := "| " + tc.key + " |"
				if extra != "" {
					row = strings.TrimSuffix(original, "|") + extra + " |"
				}
				mutated := replaceOnce(t, doc, original, row)
				wantError(t, checkContracts(tc.name, mutated), tc.section+": unparseable row "+tc.key)
			})
		}
	}
}

func TestContractsFixedTablePipelessRows(t *testing.T) {
	for _, tc := range []struct{ name, section, row string }{
		{"controller-config", "Configuration", "widenRole | true"},
		{"controller-config", "Prerequisites", "new SCC | create one"},
		{"resource-ownership", "Repair conditions", "IgnoreMismatch | adopt anyway"},
	} {
		t.Run(tc.section, func(t *testing.T) {
			doc := contractsRead(t, "../docs/contracts/"+tc.name+".md")
			mutated := contractsEditSection(t, doc, tc.section, func(body string) string {
				lines := strings.Split(body, "\n")
				for i := len(lines) - 1; i >= 0; i-- {
					if strings.HasPrefix(lines[i], "|") {
						lines[i] += "\n" + tc.row
						return strings.Join(lines, "\n")
					}
				}
				t.Fatal("fixed table not found")
				return ""
			})
			wantError(t, checkContracts(tc.name, mutated), tc.section+": unparseable row")
		})
	}
}

func TestContractsRBACScopesAndCeiling(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	for _, tc := range []struct{ kind, scope string }{
		{"Deployment", "Bound pool and OCI namespaces"},
		{"Service", "Bound pool and OCI namespaces"},
		{"Lease", "controllerNamespace"},
		{"ConfigMap", "Authorized configuration in explicitly watched namespaces"},
		{"NetworkPolicy", "Bound namespaces, prerequisite inspection only"},
	} {
		t.Run(tc.kind, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "RBAC", func(body string) string {
				lines := strings.Split(body, "\n")
				for i, line := range lines {
					if strings.Contains(line, "| "+tc.kind+" |") {
						if !strings.Contains(line, tc.scope) {
							t.Fatal("expected namespaced scope not found")
						}
						lines[i] = replaceOnce(t, line, splitRow(line)[3], "All namespaces cluster-wide")
						return strings.Join(lines, "\n")
					}
				}
				t.Fatal("required RBAC row not found")
				return ""
			})
			wantError(t, checkContracts("controller-config", mutated), "RBAC: invalid "+tc.kind+" scope")
		})
	}
	for _, phrase := range []string{"No config key widens the Role.", "OCI adds no verb or kind."} {
		t.Run(phrase, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "RBAC", func(body string) string {
				return replaceOnce(t, body, phrase, "removed")
			})
			wantError(t, checkContracts("controller-config", mutated), "permission ceiling: missing "+phrase)
		})
	}
}

// Validate removal of the normative concurrency clauses through the same seam
// as every other contract rule. Surrounding prose can be reworded independently.
func TestContractsOwnershipConcurrency(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/resource-ownership.md")
	for _, tc := range []struct{ section, phrase string }{
		{"Ownership", "No durable per-object UID/resourceVersion registry"},
		{"Ownership", "After restart or failover"},
		{"Ownership", "re-establish ownership"},
		{"Ownership", "fresh read that passed ownership verification in the same attempt"},
		{"Ownership", "observed before that verified read are not mismatches"},
		{"Ownership", "current attempt only"},
		{"Ownership", "retry after a fresh read and full ownership verification"},
		{"Repair conditions", "UID changed since the ownership-verified read in this attempt"},
		{"Repair conditions", "resourceVersion precondition conflict in this attempt"},
		{"Repair conditions", "clear after a fresh read verifies the same UID and owner"},
		{"Repair conditions", "retry using that read's resourceVersion"},
	} {
		t.Run(tc.section+"/"+tc.phrase, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, tc.section, func(body string) string {
				// Match across Markdown line wrapping without changing table layout.
				words := strings.Fields(tc.phrase)
				for i := range words {
					words[i] = regexp.QuoteMeta(words[i])
				}
				pattern := regexp.MustCompile(strings.Join(words, `\s+`))
				if len(pattern.FindAllStringIndex(body, -1)) != 1 {
					t.Fatal("expected exactly one concurrency clause to remove")
				}
				return pattern.ReplaceAllString(body, "removed")
			})
			wantError(t, checkContracts("resource-ownership", mutated), tc.section, "missing "+tc.phrase)
		})
	}
	t.Run("reworded prose", func(t *testing.T) {
		mutated := replaceOnce(t, doc, "are taken from the fresh read", "come from the fresh read")
		mutated = replaceOnce(t, mutated, "re-establish ownership from verified bindings", "re-establish ownership using verified bindings")
		if reasons := checkContracts("resource-ownership", mutated); len(reasons) != 0 {
			t.Fatalf("meaning-preserving prose changes rejected: %v", reasons)
		}
	})
}

func TestContractsLeaseCreateScope(t *testing.T) {
	doc := contractsRead(t, "../docs/contracts/controller-config.md")
	for _, phrase := range []string{"create is unrestricted by name in controllerNamespace", "resourceNames restricts update, patch and delete to the two fixed Lease names"} {
		t.Run(phrase, func(t *testing.T) {
			mutated := contractsEditSection(t, doc, "RBAC", func(body string) string {
				return replaceOnce(t, body, phrase, "removed")
			})
			wantError(t, checkContracts("controller-config", mutated), "RBAC: invalid Lease scope")
		})
	}
}
