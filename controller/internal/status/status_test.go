package status_test

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status"
)

const (
	contractDir   = "../../../docs/contracts/pool-preflight/v1"
	prereqsDoc    = "../../../docs/operations/install-prerequisites.md"
	staleContrDir = "../../../docs/contracts/pool-status"
)

type conformanceReport struct {
	Name   string         `json:"name"`
	Report map[string]any `json:"report"`
}

type conformance struct {
	Valid   []conformanceReport `json:"valid"`
	Invalid []conformanceReport `json:"invalid"`
}

func readFile(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(name)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func loadConformance(t *testing.T) conformance {
	t.Helper()
	var c conformance
	if err := json.Unmarshal(readFile(t, filepath.Join(contractDir, "conformance.json")), &c); err != nil {
		t.Fatal(err)
	}
	if len(c.Valid) == 0 || len(c.Invalid) == 0 {
		t.Fatal("conformance.json must hold valid and invalid reports")
	}
	return c
}

func marshal(t *testing.T, v any) []byte {
	t.Helper()
	data, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// clone returns a deep copy of a decoded JSON document.
func clone(t *testing.T, v map[string]any) map[string]any {
	t.Helper()
	var out map[string]any
	if err := json.Unmarshal(marshal(t, v), &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func passedReport(t *testing.T) map[string]any {
	t.Helper()
	for _, r := range loadConformance(t).Valid {
		if r.Name == "passed" {
			return r.Report
		}
	}
	t.Fatal("conformance.json has no valid report named passed")
	return nil
}

func TestConformanceReportsAreClassifiedByValidate(t *testing.T) {
	c := loadConformance(t)
	for _, r := range c.Valid {
		t.Run("valid/"+r.Name, func(t *testing.T) {
			if err := status.ValidateJSON(marshal(t, r.Report)); err != nil {
				t.Fatalf("valid report rejected: %v", err)
			}
		})
	}
	for _, r := range c.Invalid {
		t.Run("invalid/"+r.Name, func(t *testing.T) {
			if err := status.ValidateJSON(marshal(t, r.Report)); err == nil {
				t.Fatal("invalid report accepted")
			}
		})
	}
}

func TestValidateRejectsEachRemovedRequiredField(t *testing.T) {
	cases := []struct {
		level  string
		fields []string
		locate func(r map[string]any) map[string]any
	}{
		{"report", status.RequiredReportFields, func(r map[string]any) map[string]any { return r }},
		{"preflight", status.RequiredPreflightFields, func(r map[string]any) map[string]any { return r["preflight"].(map[string]any) }},
		{"check", status.RequiredCheckFields, func(r map[string]any) map[string]any {
			return r["preflight"].(map[string]any)["checks"].([]any)[0].(map[string]any)
		}},
		{"prerequisite", status.RequiredPrerequisiteFields, func(r map[string]any) map[string]any { return r["prerequisites"].([]any)[0].(map[string]any) }},
	}
	for _, c := range cases {
		if len(c.fields) == 0 {
			t.Fatalf("%s has no required fields", c.level)
		}
		for _, field := range c.fields {
			t.Run(c.level+"/"+field, func(t *testing.T) {
				r := clone(t, passedReport(t))
				target := c.locate(r)
				if _, ok := target[field]; !ok {
					t.Fatalf("fixture lacks %s.%s", c.level, field)
				}
				delete(target, field)
				if err := status.ValidateJSON(marshal(t, r)); err == nil {
					t.Fatalf("report without %s.%s accepted", c.level, field)
				}
			})
		}
	}
}

func TestValidateRejectsInvalidEnumValues(t *testing.T) {
	for name, edit := range map[string]func(r map[string]any){
		"preflight result": func(r map[string]any) { r["preflight"].(map[string]any)["result"] = "ok" },
		"check result": func(r map[string]any) {
			r["preflight"].(map[string]any)["checks"].([]any)[0].(map[string]any)["result"] = "Allowed"
		},
		"prerequisite kind":  func(r map[string]any) { r["prerequisites"].([]any)[0].(map[string]any)["kind"] = "pool-namespace" },
		"prerequisite state": func(r map[string]any) { r["prerequisites"].([]any)[0].(map[string]any)["state"] = "verified" },
	} {
		t.Run(name, func(t *testing.T) {
			r := clone(t, passedReport(t))
			edit(r)
			if err := status.ValidateJSON(marshal(t, r)); err == nil {
				t.Fatal("report with an out-of-enum value accepted")
			}
		})
	}
}

func TestValidateRejectsUnknownFieldsAndTrailingData(t *testing.T) {
	r := clone(t, passedReport(t))
	r["verified"] = true
	if err := status.ValidateJSON(marshal(t, r)); err == nil {
		t.Fatal("unknown field accepted")
	}
	if err := status.ValidateJSON(append(marshal(t, passedReport(t)), []byte(" {}")...)); err == nil {
		t.Fatal("trailing data accepted")
	}
}

func TestValidateStructRejectsInconsistentResults(t *testing.T) {
	good := status.Report{
		SchemaVersion: status.SchemaVersion,
		Preflight: status.Preflight{
			Result: status.ResultPassed,
			Checks: []status.Check{{Namespace: "ns", Group: "apps", Resource: "deployments", Verb: "get", Result: status.CheckAllowed}},
		},
	}
	for _, k := range status.Kinds {
		good.Prerequisites = append(good.Prerequisites, status.Prerequisite{Kind: k, Name: "x", State: status.StateDeclared})
	}
	if err := status.Validate(good); err != nil {
		t.Fatalf("consistent report rejected: %v", err)
	}
	bad := good
	bad.Prerequisites = good.Prerequisites[1:]
	if err := status.Validate(bad); err == nil {
		t.Fatal("passed without the namespace kind accepted")
	}
}

func TestNewReportDerivesTheResult(t *testing.T) {
	allowed := status.Check{Namespace: "ns", Group: "apps", Resource: "deployments", Verb: "get", Result: status.CheckAllowed}
	var declared []status.Prerequisite
	for _, k := range status.Kinds {
		declared = append(declared, status.Prerequisite{Kind: k, Name: "x", State: status.StateDeclared})
	}
	missing := slices.Clone(declared)
	missing[2].State, missing[2].Name = status.StateMissing, ""
	denied := allowed
	denied.Result = status.CheckDenied
	for name, tc := range map[string]struct {
		checks      []status.Check
		prereqs     []status.Prerequisite
		configError string
		want        string
	}{
		"all good":       {[]status.Check{allowed}, declared, "", status.ResultPassed},
		"denied check":   {[]status.Check{allowed, denied}, declared, "", status.ResultFailed},
		"missing prereq": {[]status.Check{allowed}, missing, "", status.ResultFailed},
		"config error":   {nil, declared, "bad", status.ResultFailed},
		"no checks":      {nil, declared, "", status.ResultFailed},
	} {
		t.Run(name, func(t *testing.T) {
			r := status.NewReport(tc.checks, tc.prereqs, tc.configError)
			if r.Preflight.Result != tc.want {
				t.Fatalf("result = %s, want %s", r.Preflight.Result, tc.want)
			}
			if r.Preflight.Checks == nil || r.Prerequisites == nil {
				t.Fatal("slices must be non-nil so they encode as arrays")
			}
			if err := status.ValidateJSON(marshal(t, r)); err != nil {
				t.Fatalf("derived report invalid: %v", err)
			}
		})
	}
}

// The schema is an independent description of the contract; its required fields
// and enums must equal the constants the Go code validates against.
func TestSchemaMatchesGoConstants(t *testing.T) {
	var schema map[string]any
	if err := json.Unmarshal(readFile(t, filepath.Join(contractDir, "pool-preflight.schema.json")), &schema); err != nil {
		t.Fatal(err)
	}
	strings := func(v any) []string {
		var out []string
		for _, e := range v.([]any) {
			out = append(out, e.(string))
		}
		return out
	}
	props := func(m map[string]any) map[string]any { return m["properties"].(map[string]any) }
	sameSet := func(what string, got, want []string) {
		t.Helper()
		g, w := slices.Clone(got), slices.Clone(want)
		slices.Sort(g)
		slices.Sort(w)
		if !slices.Equal(g, w) {
			t.Errorf("%s: schema %v, Go %v", what, got, want)
		}
	}
	if props(schema)["schemaVersion"].(map[string]any)["const"] != status.SchemaVersion {
		t.Errorf("schemaVersion const differs from %s", status.SchemaVersion)
	}
	preflightSchema := props(schema)["preflight"].(map[string]any)
	checkSchema := props(preflightSchema)["checks"].(map[string]any)["items"].(map[string]any)
	prereqSchema := props(schema)["prerequisites"].(map[string]any)["items"].(map[string]any)

	sameSet("report required", strings(schema["required"]), status.RequiredReportFields)
	sameSet("preflight required", strings(preflightSchema["required"]), status.RequiredPreflightFields)
	sameSet("check required", strings(checkSchema["required"]), status.RequiredCheckFields)
	sameSet("prerequisite required", strings(prereqSchema["required"]), status.RequiredPrerequisiteFields)
	sameSet("preflight results", strings(props(preflightSchema)["result"].(map[string]any)["enum"]), []string{status.ResultPassed, status.ResultFailed})
	sameSet("check results", strings(props(checkSchema)["result"].(map[string]any)["enum"]), []string{status.CheckAllowed, status.CheckDenied, status.CheckError})
	sameSet("prerequisite kinds", strings(props(prereqSchema)["kind"].(map[string]any)["enum"]), status.Kinds)
	sameSet("prerequisite states", strings(props(prereqSchema)["state"].(map[string]any)["enum"]), []string{status.StateDeclared, status.StateMissing})
	for _, level := range []struct {
		name   string
		schema map[string]any
		fields []string
	}{
		{"report", schema, status.RequiredReportFields},
		{"preflight", preflightSchema, status.RequiredPreflightFields},
		{"check", checkSchema, status.RequiredCheckFields},
		{"prerequisite", prereqSchema, status.RequiredPrerequisiteFields},
	} {
		for _, f := range level.fields {
			if _, ok := props(level.schema)[f]; !ok {
				t.Errorf("%s: required field %s has no schema property", level.name, f)
			}
		}
	}
}

func TestNoPreflightDocumentUnderPoolStatus(t *testing.T) {
	err := filepath.WalkDir(staleContrDir, func(path string, entry os.DirEntry, err error) error {
		if os.IsNotExist(err) {
			return nil
		}
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		if strings.Contains(string(readFile(t, path)), "pool-preflight") {
			t.Errorf("preflight document lives under pool-status: %s", path)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

// --- docs/operations/install-prerequisites.md -------------------------------

type docTable struct {
	header []string
	rows   [][]string
}

func parseTables(doc string) []docTable {
	var tables []docTable
	var cur *docTable
	for _, line := range strings.Split(doc, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "|") {
			cur = nil
			continue
		}
		cells := strings.Split(strings.Trim(line, "|"), "|")
		for i := range cells {
			cells[i] = strings.TrimSpace(cells[i])
		}
		switch {
		case cur == nil:
			tables = append(tables, docTable{header: cells})
			cur = &tables[len(tables)-1]
		case strings.Trim(cells[0], "-: ") == "":
		default:
			cur.rows = append(cur.rows, cells)
		}
	}
	return tables
}

func findTable(t *testing.T, doc, firstHeader string) docTable {
	t.Helper()
	for _, tb := range parseTables(doc) {
		if tb.header[0] == firstHeader {
			return tb
		}
	}
	t.Fatalf("install-prerequisites.md has no table starting with column %q", firstHeader)
	return docTable{}
}

func unquote(s string) string { return strings.Trim(s, "` ") }

func TestInstallPrerequisitesListsEverySevenKindsWithProviderAndVerifier(t *testing.T) {
	tb := findTable(t, string(readFile(t, prereqsDoc)), "Kind")
	col := map[string]int{}
	for i, h := range tb.header {
		col[h] = i
	}
	for _, h := range []string{"Kind", "Provider", "Verifier"} {
		if _, ok := col[h]; !ok {
			t.Fatalf("prerequisite table lacks the %s column: %v", h, tb.header)
		}
	}
	var kinds []string
	for _, row := range tb.rows {
		if len(row) != len(tb.header) {
			t.Fatalf("row %v has %d cells, want %d", row, len(row), len(tb.header))
		}
		kinds = append(kinds, unquote(row[col["Kind"]]))
		for _, h := range []string{"Provider", "Verifier"} {
			if strings.TrimSpace(row[col[h]]) == "" {
				t.Errorf("kind %s has an empty %s", row[col["Kind"]], h)
			}
		}
	}
	if !slices.Equal(kinds, status.Kinds) {
		t.Fatalf("documented kinds %v, want %v", kinds, status.Kinds)
	}
}

type tuple struct{ scope, group, resource, verb, name string }

func TestInstallPrerequisitesListsExactlyTheCheckTable(t *testing.T) {
	tb := findTable(t, string(readFile(t, prereqsDoc)), "Scope")
	pools := []string{"pool-a", "pool-b"}
	var want []tuple
	var got []tuple
	for _, r := range preflight.Table(pools, "ctl") {
		scope := "each pool namespace"
		if r.Namespace == "ctl" {
			scope = "controllerNamespace"
		}
		want = append(want, tuple{scope + "/" + r.Namespace, r.Group, r.Resource, r.Verb, r.Name})
	}
	for _, row := range tb.rows {
		if len(row) != 5 {
			t.Fatalf("check table row %v must have Scope, Group, Resource, Verbs, Names", row)
		}
		scope, group, resource := unquote(row[0]), unquote(row[1]), unquote(row[2])
		if group == "(core)" {
			group = ""
		}
		var namespaces []string
		switch scope {
		case "each pool namespace":
			namespaces = pools
		case "controllerNamespace":
			namespaces = []string{"ctl"}
		default:
			t.Fatalf("unknown scope %q", scope)
		}
		names := []string{""}
		if n := unquote(row[4]); n != "any" {
			names = nil
			for _, name := range strings.Split(n, ",") {
				names = append(names, unquote(name))
			}
		}
		for _, ns := range namespaces {
			for _, verb := range strings.Split(row[3], ",") {
				for _, name := range names {
					got = append(got, tuple{scope + "/" + ns, group, resource, unquote(verb), name})
				}
			}
		}
	}
	sortTuples := func(s []tuple) {
		slices.SortFunc(s, func(a, b tuple) int {
			return strings.Compare(a.scope+"|"+a.group+"|"+a.resource+"|"+a.verb+"|"+a.name, b.scope+"|"+b.group+"|"+b.resource+"|"+b.verb+"|"+b.name)
		})
	}
	sortTuples(got)
	sortTuples(want)
	if !slices.Equal(got, want) {
		t.Fatalf("documented checks differ from preflight.Table\n got %d: %v\nwant %d: %v", len(got), got, len(want), want)
	}
	if len(want) != 38 {
		t.Fatalf("two pool namespaces must give 38 checks, got %d", len(want))
	}
}

func TestInstallPrerequisitesStatesCoverageAndFollowUp(t *testing.T) {
	doc := string(readFile(t, prereqsDoc))
	for _, token := range []string{
		"the checks in this table passed",
		"OPS-01 preflight read checks",
		"ctr-serverless-chart-values",
		"DEP-03",
		"FALCONE_SERVERLESS_VALUES",
		"preprovisioned",
	} {
		if !strings.Contains(doc, token) {
			t.Errorf("install-prerequisites.md must mention %q", token)
		}
	}
}
