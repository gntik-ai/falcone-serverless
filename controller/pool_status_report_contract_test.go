package main

import (
	"fmt"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"testing"
)

// Conformance gate for docs/contracts/pool-status-report.md (ctr-pool-status-report).
// It parses the Markdown contract and checks fields, sections, refusal rules, the
// ordering table against a reference comparator, the status and OCI mappings and
// field visibility. It reads repository files only and cannot judge whether the
// prose is wise; reviewers still read the semantics. A change to the contract bumps
// contractVersion and updates this checker and the comparator in the same change.

const (
	poolStatusContractPath = "../docs/contracts/pool-status-report.md"
	poolStatusFixturePath  = "testdata/pool-status-report-incomplete.md"
)

var (
	poolStatusSections = []string{"Fields", "Transport", "Authentication", "Authorization", "Generation semantics", "Ordering cases", "Repair conditions", "Status mapping"}

	// Phrases a section must contain, compared after collapsing whitespace.
	poolStatusPhrases = map[string][]string{
		"Fields":               {"no free-text message field", "exactly the entry named by `revision`"},
		"Transport":            {"HTTP", "gRPC"},
		"Authentication":       {"ctr-platform-service-identity"},
		"Authorization":        {"ctr-namespace-bindings", "never from report fields", "namespaceMode", "ADR 0011", "alone does not establish tenant ownership", "before ordering"},
		"Generation semantics": {"observedGeneration", "reporterEpoch", "observationSeq", "Lease", "fencing", "contractVersion"},
		"Ordering cases":       {"lexicographic"},
		"Repair conditions":    {"absenceVerified"},
		"Status mapping":       {"readiness only"},
	}

	poolStatusFields = []struct{ name, typ, required string }{
		{"poolRef", "object", "yes"},
		{"poolRef.namespace", "string", "yes"},
		{"poolRef.name", "string", "yes"},
		{"observedGeneration", "integer", "yes"},
		{"reporterEpoch", "integer", "yes"},
		{"observationSeq", "integer", "yes"},
		{"desiredReplicas", "integer", "yes"},
		{"readyReplicas", "integer", "yes"},
		{"revisions", "array", "yes"},
		{"revisions[].revision", "string", "yes"},
		{"revisions[].ready", "boolean", "yes"},
		{"readyCapacity", "integer", "yes"},
		{"draining", "boolean", "yes"},
		{"conditions", "array", "yes"},
		{"conditions[].type", "string", "yes"},
		{"conditions[].reason", "string", "yes"},
		{"absenceVerified", "boolean", "yes"},
		{"runtimeMode", "wasm or oci", "yes"},
		{"revision", "string", "oci"},
		{"materialized", "boolean", "oci"},
	}

	poolStatusAuthRules = []string{"unverified_identity", "unbound_namespace"}
	poolStatusRepair    = []string{"UIDMismatch", "ResourceVersionMismatch", "OwnershipMismatch"}
	poolStatusStatuses  = []string{"disabled", "unavailable", "deploying", "active", "deletion_pending"}
	poolStatusOCIConds  = []string{"ImagePullFailed", "ContainerStartFailed", "ContractProbeFailed", "Dematerialized"}
	poolStatusOutcomes  = []string{"applied", "replay_noop", "refused", "conflict_refused"}
	poolStatusCases     = []string{"G1", "G2", "G3", "G4", "G5", "G6", "G7", "G8"}

	tenantReasonFormat = regexp.MustCompile(`^[a-z][a-z0-9_]*$`)
	tenantReasonBanned = []string{"registry", "pod", "node"}
	reportKeyFormat    = regexp.MustCompile(`^\((\d+),(\d+),(\d+)\)$`)
	k8sIdentityFields  = []string{"namespace", "uid", "resourceversion", "name"}
)

type mdTable struct {
	header []string
	rows   [][]string
}

func splitRow(line string) []string {
	line = strings.TrimSpace(line)
	line = strings.TrimPrefix(line, "|")
	line = strings.TrimSuffix(line, "|")
	cells := strings.Split(line, "|")
	for i := range cells {
		cells[i] = strings.TrimSpace(cells[i])
	}
	return cells
}

func cellText(cell string) string { return strings.TrimSpace(strings.ReplaceAll(cell, "`", "")) }

func isSeparatorRow(cells []string) bool {
	for _, c := range cells {
		if strings.Trim(c, "-: ") != "" {
			return false
		}
	}
	return true
}

func parseTables(body string) []mdTable {
	var tables []mdTable
	var cur *mdTable
	for _, line := range strings.Split(body, "\n") {
		if !strings.HasPrefix(strings.TrimSpace(line), "|") {
			cur = nil
			continue
		}
		cells := splitRow(line)
		switch {
		case cur == nil:
			tables = append(tables, mdTable{header: cells})
			cur = &tables[len(tables)-1]
		case isSeparatorRow(cells):
		default:
			cur.rows = append(cur.rows, cells)
		}
	}
	return tables
}

// splitSections returns the body of every level-two section by title.
func splitSections(doc string) map[string]string {
	sections := map[string]string{}
	title := ""
	for _, line := range strings.Split(doc, "\n") {
		if strings.HasPrefix(line, "## ") {
			title = strings.TrimSpace(strings.TrimPrefix(line, "## "))
			sections[title] += ""
			continue
		}
		if title != "" {
			sections[title] += line + "\n"
		}
	}
	return sections
}

func findTable(body string, firstHeader string) (mdTable, bool) {
	for _, tb := range parseTables(body) {
		if len(tb.header) > 0 && cellText(tb.header[0]) == firstHeader {
			return tb, true
		}
	}
	return mdTable{}, false
}

func columns(tb mdTable, names ...string) ([]int, bool) {
	idx := make([]int, len(names))
	for i, n := range names {
		idx[i] = slices.IndexFunc(tb.header, func(h string) bool { return cellText(h) == n })
		if idx[i] < 0 {
			return nil, false
		}
	}
	return idx, true
}

// rowsByKey groups table rows by the text of their first cell.
func rowsByKey(tb mdTable) map[string][][]string {
	out := map[string][][]string{}
	for _, r := range tb.rows {
		k := cellText(r[0])
		out[k] = append(out[k], r)
	}
	return out
}

func cell(row []string, i int) string {
	if i >= len(row) {
		return ""
	}
	return cellText(row[i])
}

// requireRows checks that each wanted key has exactly one row and no other key exists.
func requireRows(label, noun string, tb mdTable, want []string) (map[string][]string, []string) {
	var errs []string
	byKey := rowsByKey(tb)
	rows := map[string][]string{}
	for _, k := range want {
		switch n := len(byKey[k]); {
		case n == 0:
			errs = append(errs, fmt.Sprintf("%s: missing %s %q", label, noun, k))
		case n > 1:
			errs = append(errs, fmt.Sprintf("%s: %s %q has %d rows, want exactly one", label, noun, k, n))
		default:
			rows[k] = byKey[k][0]
		}
	}
	for k := range byKey {
		if !slices.Contains(want, k) {
			errs = append(errs, fmt.Sprintf("%s: unexpected %s %q", label, noun, k))
		}
	}
	slices.Sort(errs)
	return rows, errs
}

// tenantReasonError returns why a tenant reason is unacceptable, or "".
func tenantReasonError(reason string) string {
	if !tenantReasonFormat.MatchString(reason) {
		return fmt.Sprintf("tenant reason %q does not match %s", reason, tenantReasonFormat)
	}
	for _, b := range tenantReasonBanned {
		if strings.Contains(strings.ToLower(reason), b) {
			return fmt.Sprintf("tenant reason %q contains %q", reason, b)
		}
	}
	return ""
}

// reportKey is (observedGeneration, reporterEpoch, observationSeq).
type reportKey struct{ generation, epoch, seq int }

func (k reportKey) compare(o reportKey) int {
	for _, p := range [][2]int{{k.generation, o.generation}, {k.epoch, o.epoch}, {k.seq, o.seq}} {
		if p[0] != p[1] {
			if p[0] < p[1] {
				return -1
			}
			return 1
		}
	}
	return 0
}

func parseReportKey(s string) (reportKey, bool) {
	m := reportKeyFormat.FindStringSubmatch(s)
	if m == nil {
		return reportKey{}, false
	}
	var n [3]int
	for i := range n {
		n[i], _ = strconv.Atoi(m[i+1])
	}
	return reportKey{n[0], n[1], n[2]}, true
}

// referenceOutcome is the test-only reference comparator for the ordering rule.
// A generation above Falcone's desired generation is refused before keys compare.
func referenceOutcome(stored, report reportKey, payloadSame bool, desiredGeneration int) string {
	if report.generation > desiredGeneration {
		return "refused"
	}
	switch c := report.compare(stored); {
	case c < 0:
		return "refused"
	case c == 0 && payloadSame:
		return "replay_noop"
	case c == 0:
		return "conflict_refused"
	default:
		return "applied"
	}
}

// orderingScenario pins what each required G case must exercise, independently
// of the row's own cells, so a row cannot be rewritten into a different case.
type orderingScenario struct {
	shape   string                                           // human description for errors
	payload string                                           // "same", "different" or "" when unconstrained
	outcome string                                           // required outcome
	holds   func(stored, report reportKey, desired int) bool // required key relationship
}

var orderingScenarios = map[string]orderingScenario{
	"G1": {"report generation below stored generation", "", "refused",
		func(s, r reportKey, d int) bool { return r.generation < s.generation && r.generation <= d }},
	"G2": {"report key equal to stored key", "same", "replay_noop",
		func(s, r reportKey, d int) bool { return r == s && r.generation <= d }},
	"G3": {"report key equal to stored key", "different", "conflict_refused",
		func(s, r reportKey, d int) bool { return r == s && r.generation <= d }},
	"G4": {"same generation and epoch, higher seq", "different", "applied",
		func(s, r reportKey, d int) bool {
			return r.generation == s.generation && r.epoch == s.epoch && r.seq > s.seq && r.generation <= d
		}},
	"G5": {"same generation and epoch, higher seq", "different", "applied",
		func(s, r reportKey, d int) bool {
			return r.generation == s.generation && r.epoch == s.epoch && r.seq > s.seq && r.generation <= d
		}},
	"G6": {"same generation and epoch, lower seq", "", "refused",
		func(s, r reportKey, d int) bool {
			return r.generation == s.generation && r.epoch == s.epoch && r.seq < s.seq && r.generation <= d
		}},
	"G7": {"same generation, higher epoch, lower seq", "", "applied",
		func(s, r reportKey, d int) bool {
			return r.generation == s.generation && r.epoch > s.epoch && r.seq < s.seq && r.generation <= d
		}},
	"G8": {"report generation above desired generation", "", "refused",
		func(s, r reportKey, d int) bool { return r.generation > d }},
}

func checkPoolStatusReport(doc string) []string {
	var errs []string
	lines := strings.Split(doc, "\n")
	for _, want := range []string{"Contract: ctr-pool-status-report", "contractVersion: 1"} {
		if !slices.Contains(lines, want) {
			errs = append(errs, fmt.Sprintf("header: missing line %q", want))
		}
	}

	sections := splitSections(doc)
	for _, name := range poolStatusSections {
		body, ok := sections[name]
		if !ok {
			errs = append(errs, fmt.Sprintf("missing section %q", name))
			continue
		}
		flat := strings.Join(strings.Fields(body), " ")
		for _, p := range poolStatusPhrases[name] {
			if !strings.Contains(flat, p) {
				errs = append(errs, fmt.Sprintf("section %q: missing phrase %q", name, p))
			}
		}
	}

	if body, ok := sections["Fields"]; ok {
		errs = append(errs, checkPoolStatusFields(body)...)
	}
	if body, ok := sections["Authorization"]; ok {
		errs = append(errs, checkPoolStatusAuthorization(body)...)
	}
	if body, ok := sections["Ordering cases"]; ok {
		errs = append(errs, checkPoolStatusOrdering(body)...)
	}
	if body, ok := sections["Repair conditions"]; ok {
		errs = append(errs, checkPoolStatusRepair(body)...)
	}
	if body, ok := sections["Status mapping"]; ok {
		errs = append(errs, checkPoolStatusMapping(body)...)
	}
	return errs
}

func checkPoolStatusFields(body string) []string {
	tb, ok := findTable(body, "Name")
	cols, hasCols := columns(tb, "Name", "Type", "Required", "Visibility")
	if !ok || !hasCols {
		return []string{"Fields: missing table with columns Name, Type, Required, Visibility"}
	}
	byName := rowsByKey(tb)
	var errs []string
	for _, f := range poolStatusFields {
		rows := byName[f.name]
		switch {
		case len(rows) == 0:
			errs = append(errs, fmt.Sprintf("missing required field %q", f.name))
		case len(rows) > 1:
			errs = append(errs, fmt.Sprintf("field %q has %d rows, want exactly one", f.name, len(rows)))
		default:
			if got := cell(rows[0], cols[1]); got != f.typ {
				errs = append(errs, fmt.Sprintf("field %q: type %q, want %q", f.name, got, f.typ))
			}
			if got := cell(rows[0], cols[2]); got != f.required {
				errs = append(errs, fmt.Sprintf("field %q: required %q, want %q", f.name, got, f.required))
			}
		}
	}
	for _, row := range tb.rows {
		name := cellText(row[0])
		known := slices.ContainsFunc(poolStatusFields, func(f struct{ name, typ, required string }) bool { return f.name == name })
		if !known {
			errs = append(errs, fmt.Sprintf("unexpected field %q", name))
		}
		vis := cell(row, cols[3])
		identity := slices.ContainsFunc(k8sIdentityFields, func(s string) bool { return strings.Contains(strings.ToLower(name), s) })
		switch {
		case vis != "tenant" && vis != "operator":
			errs = append(errs, fmt.Sprintf("field %q: visibility %q, want tenant or operator", name, vis))
		case name == "runtimeMode" && vis != "tenant":
			errs = append(errs, fmt.Sprintf("field %q: visibility %q, runtimeMode must be tenant", name, vis))
		case name != "runtimeMode" && vis == "tenant" && identity:
			errs = append(errs, fmt.Sprintf("field %q: visibility tenant, but it carries a Kubernetes name, UID or resourceVersion", name))
		case name != "runtimeMode" && vis == "tenant":
			errs = append(errs, fmt.Sprintf("field %q: visibility tenant, only runtimeMode is tenant-visible", name))
		}
	}
	return errs
}

func checkPoolStatusAuthorization(body string) []string {
	tb, ok := findTable(body, "Rule")
	cols, hasCols := columns(tb, "Rule", "Outcome", "State")
	if !ok || !hasCols {
		return []string{"Authorization: missing table with columns Rule, Condition, Outcome, State"}
	}
	rows := rowsByKey(tb)
	var errs []string
	for _, id := range poolStatusAuthRules {
		switch n := len(rows[id]); {
		case n == 0:
			errs = append(errs, fmt.Sprintf("Authorization: missing refusal rule %q", id))
		case n > 1:
			errs = append(errs, fmt.Sprintf("Authorization: refusal rule %q has %d rows, want exactly one", id, n))
		default:
			if got := cell(rows[id][0], cols[1]); got != "refused" {
				errs = append(errs, fmt.Sprintf("Authorization: rule %q outcome %q, want refused", id, got))
			}
			if got := cell(rows[id][0], cols[2]); got != "unchanged" {
				errs = append(errs, fmt.Sprintf("Authorization: rule %q state %q, want unchanged", id, got))
			}
		}
	}
	return errs
}

func checkPoolStatusOrdering(body string) []string {
	tb, ok := findTable(body, "Case")
	cols, hasCols := columns(tb, "Case", "Stored key", "Report key", "Payload", "Desired generation", "Outcome", "Notes")
	if !ok || !hasCols {
		return []string{"Ordering cases: missing table with columns Case, Stored key, Report key, Payload, Desired generation, Outcome, Notes"}
	}
	rows, errs := requireRows("Ordering cases", "case", tb, poolStatusCases)
	for _, id := range poolStatusCases {
		row, present := rows[id]
		if !present {
			continue
		}
		stored, okStored := parseReportKey(cell(row, cols[1]))
		report, okReport := parseReportKey(cell(row, cols[2]))
		desired, errDesired := strconv.Atoi(cell(row, cols[4]))
		payload := cell(row, cols[3])
		outcome := cell(row, cols[5])
		switch {
		case !okStored || !okReport:
			errs = append(errs, fmt.Sprintf("ordering case %s: stored and report keys must look like (generation,epoch,seq)", id))
		case errDesired != nil:
			errs = append(errs, fmt.Sprintf("ordering case %s: desired generation %q is not an integer", id, cell(row, cols[4])))
		case payload != "same" && payload != "different":
			errs = append(errs, fmt.Sprintf("ordering case %s: payload %q, want same or different", id, payload))
		case !slices.Contains(poolStatusOutcomes, outcome):
			errs = append(errs, fmt.Sprintf("ordering case %s: outcome %q is not one of %v", id, outcome, poolStatusOutcomes))
		default:
			want := referenceOutcome(stored, report, payload == "same", desired)
			if outcome != want {
				errs = append(errs, fmt.Sprintf("ordering case %s: outcome %q disagrees with reference comparator %q", id, outcome, want))
			}
			sc := orderingScenarios[id]
			if !sc.holds(stored, report, desired) {
				errs = append(errs, fmt.Sprintf("ordering case %s: keys must exercise %s", id, sc.shape))
			}
			if sc.payload != "" && payload != sc.payload {
				errs = append(errs, fmt.Sprintf("ordering case %s: payload must be %s", id, sc.payload))
			}
			if outcome == want && outcome != sc.outcome {
				errs = append(errs, fmt.Sprintf("ordering case %s: outcome must be %s", id, sc.outcome))
			}
		}
		notes := map[string]string{"G4": "readyReplicas 0 to 1", "G5": "readyReplicas 1 to 0"}
		if want, ok := notes[id]; ok && !strings.Contains(cell(row, cols[6]), want) {
			errs = append(errs, fmt.Sprintf("ordering case %s: notes must record %q", id, want))
		}
	}
	return errs
}

func checkPoolStatusRepair(body string) []string {
	tb, ok := findTable(body, "Condition")
	cols, hasCols := columns(tb, "Condition", "Effect")
	if !ok || !hasCols {
		return []string{"Repair conditions: missing table with columns Condition, Effect"}
	}
	rows, errs := requireRows("Repair conditions", "condition", tb, poolStatusRepair)
	for _, c := range poolStatusRepair {
		if row, present := rows[c]; present {
			effect := strings.ToLower(cell(row, cols[1]))
			if !strings.Contains(effect, "keep the resource") || !strings.Contains(effect, "block completion") {
				errs = append(errs, fmt.Sprintf("Repair conditions: %q must keep the resource and block completion", c))
			}
		}
	}
	return errs
}

func checkPoolStatusMapping(body string) []string {
	var errs []string
	if tb, ok := findTable(body, "Status"); !ok {
		errs = append(errs, "Status mapping: missing table with columns Status, Derivation")
	} else if cols, hasCols := columns(tb, "Status", "Derivation"); !hasCols {
		errs = append(errs, "Status mapping: missing table with columns Status, Derivation")
	} else {
		rows, rowErrs := requireRows("Status mapping", "status", tb, poolStatusStatuses)
		errs = append(errs, rowErrs...)
		for s, row := range rows {
			if cell(row, cols[1]) == "" {
				errs = append(errs, fmt.Sprintf("Status mapping: status %q has no derivation", s))
			}
		}
	}

	tb, ok := findTable(body, "Condition")
	cols, hasCols := columns(tb, "Condition", "Visibility", "Status", "Tenant reason")
	if !ok || !hasCols {
		return append(errs, "Status mapping: missing OCI table with columns Condition, Visibility, Status, Tenant reason")
	}
	rows, rowErrs := requireRows("Status mapping", "OCI condition", tb, poolStatusOCIConds)
	errs = append(errs, rowErrs...)
	for _, c := range poolStatusOCIConds {
		row, present := rows[c]
		if !present {
			continue
		}
		status := cell(row, cols[2])
		switch {
		case cell(row, cols[1]) != "operator":
			errs = append(errs, fmt.Sprintf("OCI condition %s: visibility %q, want operator", c, cell(row, cols[1])))
		case !slices.Contains(poolStatusStatuses, status):
			errs = append(errs, fmt.Sprintf("OCI condition %s: status %q is not a runtime status", c, status))
		case status == "active" && c != "Dematerialized":
			errs = append(errs, fmt.Sprintf("OCI condition %s: a failure condition must not map to active", c))
		}
		if msg := tenantReasonError(cell(row, cols[3])); msg != "" {
			errs = append(errs, fmt.Sprintf("OCI condition %s: %s", c, msg))
		}
	}
	return errs
}

// In-memory mutation helpers. Each fails the test when the edit does not land, so a
// mutation that silently changes nothing cannot make a negative test pass.

func isTableRowFor(line, key string) bool {
	return strings.HasPrefix(strings.TrimSpace(line), "|") && cellText(splitRow(line)[0]) == key
}

func dropRow(t *testing.T, doc, key string) string {
	t.Helper()
	var out []string
	dropped := 0
	for _, line := range strings.Split(doc, "\n") {
		if isTableRowFor(line, key) {
			dropped++
			continue
		}
		out = append(out, line)
	}
	if dropped != 1 {
		t.Fatalf("dropRow(%q) removed %d rows, want 1", key, dropped)
	}
	return strings.Join(out, "\n")
}

func editRow(t *testing.T, doc, key string, edit func(cells []string) []string) string {
	t.Helper()
	lines := strings.Split(doc, "\n")
	edited := 0
	for i, line := range lines {
		if isTableRowFor(line, key) {
			lines[i] = "| " + strings.Join(edit(splitRow(line)), " | ") + " |"
			edited++
		}
	}
	if edited != 1 {
		t.Fatalf("editRow(%q) matched %d rows, want 1", key, edited)
	}
	return strings.Join(lines, "\n")
}

func setCell(t *testing.T, doc, key string, col int, value string) string {
	t.Helper()
	out := editRow(t, doc, key, func(c []string) []string {
		if c[col] == value {
			t.Fatalf("setCell(%q, %d): cell already %q", key, col, value)
		}
		c[col] = value
		return c
	})
	return out
}

// duplicateRow repeats a row under a new first cell (or the same one when newKey is "").
func duplicateRow(t *testing.T, doc, key, newKey string) string {
	t.Helper()
	lines := strings.Split(doc, "\n")
	for i, line := range lines {
		if isTableRowFor(line, key) {
			cells := splitRow(line)
			if newKey != "" {
				cells[0] = newKey
			}
			dup := "| " + strings.Join(cells, " | ") + " |"
			return strings.Join(slices.Insert(lines, i+1, dup), "\n")
		}
	}
	t.Fatalf("duplicateRow(%q): no such row", key)
	return ""
}

func dropSection(t *testing.T, doc, name string) string {
	t.Helper()
	var out []string
	skipping, found := false, false
	for _, line := range strings.Split(doc, "\n") {
		if strings.HasPrefix(line, "## ") {
			skipping = strings.TrimSpace(strings.TrimPrefix(line, "## ")) == name
			found = found || skipping
		}
		if !skipping {
			out = append(out, line)
		}
	}
	if !found {
		t.Fatalf("dropSection(%q): no such section", name)
	}
	return strings.Join(out, "\n")
}

func replaceOnce(t *testing.T, doc, old, repl string) string {
	t.Helper()
	if strings.Count(doc, old) != 1 {
		t.Fatalf("replaceOnce(%q) matched %d times, want 1", old, strings.Count(doc, old))
	}
	return strings.Replace(doc, old, repl, 1)
}

func wantError(t *testing.T, errs []string, parts ...string) {
	t.Helper()
	for _, e := range errs {
		if containsAll(e, parts) {
			return
		}
	}
	t.Fatalf("no error contains all of %q; got %q", parts, errs)
}

func containsAll(s string, parts []string) bool {
	for _, p := range parts {
		if !strings.Contains(s, p) {
			return false
		}
	}
	return true
}

func poolStatusDoc(t *testing.T) string {
	t.Helper()
	return readFile(t, poolStatusContractPath)
}

// Column positions in the tables of the contract.
const (
	colFieldType, colFieldRequired, colFieldVisibility = 1, 2, 3
	colOrderingStored, colOrderingReport               = 1, 2
	colOrderingPayload, colOrderingDesired             = 3, 4
	colOrderingOutcome                                 = 5
	colAuthOutcome, colAuthState                       = 2, 3
	colOCIVisibility, colOCIStatus, colOCIReason       = 1, 2, 3
)

func TestPoolStatusReportContract(t *testing.T) {
	if errs := checkPoolStatusReport(poolStatusDoc(t)); len(errs) != 0 {
		t.Fatalf("contract does not conform:\n%s", strings.Join(errs, "\n"))
	}
}

func TestPoolStatusReportRejectsMissingHeader(t *testing.T) {
	doc := poolStatusDoc(t)
	wantError(t, checkPoolStatusReport(replaceOnce(t, doc, "contractVersion: 1\n", "contractVersion: 2\n")), "contractVersion: 1")
	wantError(t, checkPoolStatusReport(replaceOnce(t, doc, "Contract: ctr-pool-status-report\n", "")), "Contract: ctr-pool-status-report")
}

func TestPoolStatusReportRejectsMissingField(t *testing.T) {
	doc := poolStatusDoc(t)
	alwaysRequired := []string{
		"poolRef", "observedGeneration", "reporterEpoch", "observationSeq", "desiredReplicas", "readyReplicas",
		"revisions", "readyCapacity", "draining", "conditions", "absenceVerified", "runtimeMode",
	}
	ociRequired := []string{"revision", "materialized"}
	for _, name := range append(slices.Clone(alwaysRequired), ociRequired...) {
		t.Run(name, func(t *testing.T) {
			errs := checkPoolStatusReport(dropRow(t, doc, name))
			wantError(t, errs, fmt.Sprintf("missing required field %q", name))
		})
	}
	for _, name := range alwaysRequired {
		t.Run("not required/"+name, func(t *testing.T) {
			errs := checkPoolStatusReport(setCell(t, doc, name, colFieldRequired, "no"))
			wantError(t, errs, fmt.Sprintf("field %q: required", name))
		})
	}
	for _, name := range ociRequired {
		t.Run("not oci/"+name, func(t *testing.T) {
			errs := checkPoolStatusReport(setCell(t, doc, name, colFieldRequired, "yes"))
			wantError(t, errs, fmt.Sprintf("field %q: required", name))
		})
	}
	t.Run("nested", func(t *testing.T) {
		for _, name := range []string{"poolRef.namespace", "poolRef.name", "revisions[].revision", "revisions[].ready", "conditions[].type", "conditions[].reason"} {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, name)), fmt.Sprintf("missing required field %q", name))
		}
	})
	t.Run("wrong type", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(setCell(t, doc, "observationSeq", colFieldType, "string")), `field "observationSeq": type`)
	})
	t.Run("free-text message field", func(t *testing.T) {
		mutated := duplicateRow(t, doc, "conditions[].reason", "conditions[].message")
		wantError(t, checkPoolStatusReport(mutated), `unexpected field "conditions[].message"`)
	})
}

func TestPoolStatusReportRejectsMissingSection(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, name := range []string{"Transport", "Authentication", "Authorization", "Generation semantics", "Ordering cases", "Repair conditions", "Status mapping"} {
		t.Run(name, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropSection(t, doc, name)), fmt.Sprintf("missing section %q", name))
		})
	}
}

func TestPoolStatusReportRejectsWeakAuthorization(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, id := range []string{"unverified_identity", "unbound_namespace"} {
		t.Run("missing "+id, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, id)), fmt.Sprintf("missing refusal rule %q", id))
		})
		t.Run("accepted "+id, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(setCell(t, doc, id, colAuthOutcome, "applied")), id, "want refused")
		})
		t.Run("changed state "+id, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(setCell(t, doc, id, colAuthState, "updated")), id, "want unchanged")
		})
	}
	for _, phrase := range poolStatusPhrases["Authorization"] {
		t.Run("phrase "+phrase, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(strings.ReplaceAll(doc, phrase, "elided")), "Authorization", phrase)
		})
	}
}

func TestPoolStatusReportRejectsMissingOrderingRow(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, id := range poolStatusCases {
		t.Run(id, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, id)), fmt.Sprintf("missing case %q", id))
		})
	}
	t.Run("extra case", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(duplicateRow(t, doc, "G8", "G9")), `unexpected case "G9"`)
	})
}

func TestPoolStatusReportRejectsDisagreeingOrderingOutcome(t *testing.T) {
	doc := poolStatusDoc(t)
	byID := map[string]string{}
	tb, _ := findTable(splitSections(doc)["Ordering cases"], "Case")
	for _, r := range tb.rows {
		byID[cellText(r[0])] = cellText(r[colOrderingOutcome])
	}
	for _, id := range poolStatusCases {
		for _, outcome := range poolStatusOutcomes {
			if outcome == byID[id] {
				continue
			}
			t.Run(id+" as "+outcome, func(t *testing.T) {
				errs := checkPoolStatusReport(setCell(t, doc, id, colOrderingOutcome, outcome))
				wantError(t, errs, "ordering case "+id+":", fmt.Sprintf("outcome %q disagrees", outcome))
			})
		}
	}
	t.Run("unknown outcome", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(setCell(t, doc, "G4", colOrderingOutcome, "merged")), "ordering case G4", "merged")
	})
}

// A row rewritten into a different scenario is consistent with the comparator
// but no longer exercises the required case.
func TestPoolStatusReportRejectsReplacedOrderingScenario(t *testing.T) {
	doc := poolStatusDoc(t)
	replace := func(t *testing.T, id, stored, report, payload, desired, outcome string) string {
		return editRow(t, doc, id, func(c []string) []string {
			c[colOrderingStored], c[colOrderingReport], c[colOrderingPayload] = stored, report, payload
			c[colOrderingDesired], c[colOrderingOutcome] = desired, outcome
			return c
		})
	}
	cases := []struct {
		name, id, stored, report, payload, desired, outcome, want string
	}{
		{"G4 as equal-key replay", "G4", "(5,2,10)", "(5,2,10)", "same", "5", "replay_noop", "ordering case G4: keys must exercise"},
		{"G5 as equal-key replay", "G5", "(5,2,11)", "(5,2,11)", "same", "5", "replay_noop", "ordering case G5: keys must exercise"},
		{"G4 with same payload", "G4", "(5,2,10)", "(5,2,11)", "same", "5", "replay_noop", "ordering case G4: payload must be different"},
		{"G1 as higher generation", "G1", "(5,2,10)", "(6,2,11)", "different", "6", "applied", "ordering case G1: keys must exercise"},
		{"G2 as different payload", "G2", "(5,2,10)", "(5,2,10)", "different", "5", "conflict_refused", "ordering case G2: payload must be same"},
		{"G3 as same payload", "G3", "(5,2,10)", "(5,2,10)", "same", "5", "replay_noop", "ordering case G3: payload must be different"},
		{"G6 as higher seq", "G6", "(5,2,10)", "(5,2,11)", "different", "5", "applied", "ordering case G6: keys must exercise"},
		{"G7 as same epoch", "G7", "(5,2,10)", "(5,2,9)", "different", "5", "refused", "ordering case G7: keys must exercise"},
		{"G8 within desired", "G8", "(5,2,10)", "(6,2,1)", "different", "6", "applied", "ordering case G8: keys must exercise"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(replace(t, c.id, c.stored, c.report, c.payload, c.desired, c.outcome)), c.want)
		})
	}
}

func TestPoolStatusReportRejectsIncompleteMappings(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, s := range poolStatusStatuses {
		t.Run("missing status "+s, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, s)), fmt.Sprintf("missing status %q", s))
		})
		t.Run("duplicate status "+s, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(duplicateRow(t, doc, s, "")), fmt.Sprintf("status %q has 2 rows", s))
		})
	}
	t.Run("extra status", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(duplicateRow(t, doc, "active", "degraded")), `unexpected status "degraded"`)
	})
	for _, c := range poolStatusRepair {
		t.Run("missing repair "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, c)), fmt.Sprintf("missing condition %q", c))
		})
	}
}

func TestPoolStatusReportRejectsBadOCIConditions(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, c := range poolStatusOCIConds {
		t.Run("missing "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(dropRow(t, doc, c)), fmt.Sprintf("missing OCI condition %q", c))
		})
		t.Run("duplicate "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(duplicateRow(t, doc, c, "")), fmt.Sprintf("OCI condition %q has 2 rows", c))
		})
		t.Run("tenant visible "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(setCell(t, doc, c, colOCIVisibility, "tenant")), "OCI condition "+c, "want operator")
		})
		t.Run("invalid status "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(setCell(t, doc, c, colOCIStatus, "broken")), "OCI condition "+c, "not a runtime status")
		})
		leaks := map[string]string{
			"registry path": "registry.example.com/team/app",
			"digest":        "sha256:" + strings.Repeat("a", 64),
			"pod name":      "pod-web-7d9f6c",
			"node name":     "node-worker-3",
			"banned word":   "node_unreachable",
			"empty":         "",
			"uppercase":     "ImageUnavailable",
		}
		for what, reason := range leaks {
			t.Run(c+" reason "+what, func(t *testing.T) {
				wantError(t, checkPoolStatusReport(setCell(t, doc, c, colOCIReason, reason)), "OCI condition "+c, "tenant reason")
			})
		}
	}
	for _, c := range []string{"ImagePullFailed", "ContainerStartFailed", "ContractProbeFailed"} {
		t.Run("active "+c, func(t *testing.T) {
			wantError(t, checkPoolStatusReport(setCell(t, doc, c, colOCIStatus, "active")), "OCI condition "+c, "must not map to active")
		})
	}
}

func TestPoolStatusReportRejectsTenantVisibleFields(t *testing.T) {
	doc := poolStatusDoc(t)
	for _, name := range []string{"poolRef.namespace", "poolRef.name", "conditions", "conditions[].type", "conditions[].reason", "revision", "materialized", "poolRef"} {
		t.Run(name, func(t *testing.T) {
			errs := checkPoolStatusReport(setCell(t, doc, name, colFieldVisibility, "tenant"))
			wantError(t, errs, fmt.Sprintf("field %q: visibility tenant", name))
		})
	}
	t.Run("runtimeMode must be tenant", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(setCell(t, doc, "runtimeMode", colFieldVisibility, "operator")), `field "runtimeMode"`, "must be tenant")
	})
	t.Run("unknown visibility", func(t *testing.T) {
		wantError(t, checkPoolStatusReport(setCell(t, doc, "draining", colFieldVisibility, "public")), `field "draining"`, "want tenant or operator")
	})
	for _, injected := range []string{"ownerUID", "resourceVersion", "podName", "nodeName", "serviceName", "targetNamespace"} {
		t.Run("injected "+injected, func(t *testing.T) {
			mutated := duplicateRow(t, doc, "draining", injected)
			mutated = setCell(t, mutated, injected, colFieldVisibility, "tenant")
			wantError(t, checkPoolStatusReport(mutated), fmt.Sprintf("field %q: visibility tenant", injected), "Kubernetes name, UID or resourceVersion")
		})
	}
}

// The reference comparator is checked against literal outcomes from the contract's
// rules, independent of the Ordering cases table it later judges.
func TestPoolStatusReportReferenceComparator(t *testing.T) {
	tests := []struct {
		name        string
		stored      reportKey
		report      reportKey
		payloadSame bool
		desired     int
		want        string
	}{
		{"lower generation", reportKey{5, 2, 10}, reportKey{4, 2, 11}, false, 5, "refused"},
		{"replay", reportKey{5, 2, 10}, reportKey{5, 2, 10}, true, 5, "replay_noop"},
		{"conflict", reportKey{5, 2, 10}, reportKey{5, 2, 10}, false, 5, "conflict_refused"},
		{"higher seq", reportKey{5, 2, 10}, reportKey{5, 2, 11}, false, 5, "applied"},
		{"same epoch lower seq", reportKey{5, 2, 10}, reportKey{5, 2, 9}, false, 5, "refused"},
		{"higher epoch lower seq", reportKey{5, 2, 10}, reportKey{5, 3, 1}, false, 5, "applied"},
		{"lower epoch same generation", reportKey{5, 3, 1}, reportKey{5, 2, 99}, false, 5, "refused"},
		{"generation above desired", reportKey{5, 2, 10}, reportKey{7, 2, 1}, false, 6, "refused"},
		{"above desired beats equal key", reportKey{7, 2, 1}, reportKey{7, 2, 1}, true, 6, "refused"},
		{"generation outranks epoch", reportKey{5, 2, 10}, reportKey{6, 1, 1}, false, 6, "applied"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := referenceOutcome(tc.stored, tc.report, tc.payloadSame, tc.desired); got != tc.want {
				t.Fatalf("referenceOutcome = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestPoolStatusReportContractRejectsIncompleteFixture(t *testing.T) {
	errs := checkPoolStatusReport(readFile(t, poolStatusFixturePath))
	want := [][]string{
		{`missing required field "observationSeq"`},
		{"ordering case G4:", `outcome "replay_noop" disagrees`},
		{`missing refusal rule "unbound_namespace"`},
		{`field "poolRef.namespace": visibility tenant`},
		{"OCI condition ImagePullFailed", "tenant reason"},
	}
	if len(errs) != len(want) {
		t.Fatalf("got %d errors, want %d:\n%s", len(errs), len(want), strings.Join(errs, "\n"))
	}
	for _, parts := range want {
		wantError(t, errs, parts...)
	}
}
