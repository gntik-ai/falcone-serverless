package poolstatus_test

import (
	"os"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

func TestContractReasonCodesMatchGoConstants(t *testing.T) {
	data, err := os.ReadFile(contractDir + "../pool-status-report.md")
	if err != nil {
		t.Fatal(err)
	}
	_, table, ok := strings.Cut(string(data), "| Reason | Type |\n| --- | --- |\n")
	if !ok {
		t.Fatal("missing condition reason table")
	}
	table, _, _ = strings.Cut(table, "\n\n")
	got := map[string]string{}
	for _, row := range strings.Split(strings.TrimSpace(table), "\n") {
		cells := strings.Split(row, "|")
		if len(cells) != 4 {
			t.Fatalf("malformed reason row: %s", row)
		}
		typ, reason := strings.TrimSpace(cells[2]), strings.TrimSpace(cells[1])
		if _, exists := got[typ]; exists {
			t.Fatalf("duplicate condition reason: %s", typ)
		}
		got[typ] = reason
	}
	if !reflect.DeepEqual(got, poolstatus.ConditionReasons) {
		t.Fatalf("contract reason codes %v differ from Go %v", got, poolstatus.ConditionReasons)
	}
}

func TestSchemaMatchesGoTypesAndConstants(t *testing.T) {
	schema := readJSON(t, "v1.2/pool-status.schema.json")["$defs"].(map[string]any)["current"].(map[string]any)
	var compare func(map[string]any, reflect.Type, string)
	compare = func(s map[string]any, typ reflect.Type, path string) {
		if typ.Kind() == reflect.Pointer {
			typ = typ.Elem()
		}
		switch typ.Kind() {
		case reflect.Struct:
			if s["type"] != "object" || s["additionalProperties"] != false {
				t.Fatalf("%s must be a strict object", path)
			}
			props := s["properties"].(map[string]any)
			if len(props) != typ.NumField() {
				t.Fatalf("%s property count differs from Go type", path)
			}
			for i := 0; i < typ.NumField(); i++ {
				f := typ.Field(i)
				key := strings.Split(f.Tag.Get("json"), ",")[0]
				child, ok := props[key].(map[string]any)
				if !ok {
					t.Fatalf("%s missing %s", path, key)
				}
				compare(child, f.Type, path+"."+key)
			}
		case reflect.Slice:
			if s["type"] != "array" {
				t.Fatalf("%s must be array", path)
			}
			compare(s["items"].(map[string]any), typ.Elem(), path+"[]")
		default:
			want := "string"
			if typ.Kind() == reflect.Bool {
				want = "boolean"
			}
			if typ.Kind() == reflect.Int64 {
				want = "integer"
			}
			if s["type"] != want {
				t.Fatalf("%s schema type %v differs from %s", path, s["type"], want)
			}
		}
	}
	compare(schema, reflect.TypeFor[poolstatus.Report](), "report")
	props := schema["properties"].(map[string]any)
	if props["contractRevision"].(map[string]any)["const"] != poolstatus.ContractRevision {
		t.Fatal("revision drift")
	}
	setEqual := func(got any, want []string) {
		t.Helper()
		var values []string
		for _, v := range got.([]any) {
			values = append(values, v.(string))
		}
		want = slices.Clone(want)
		slices.Sort(values)
		slices.Sort(want)
		if !slices.Equal(values, want) {
			t.Fatalf("schema %v != Go %v", values, want)
		}
	}
	setEqual(schema["required"], append(append(slices.Clone(poolstatus.RequiredReportFields), poolstatus.RequiredLineageFields...), "contractRevision"))
	abs := props["verifiedAbsence"].(map[string]any)["items"].(map[string]any)
	setEqual(abs["required"], poolstatus.RequiredAbsenceFields)
	setEqual(props["poolRef"].(map[string]any)["required"], poolstatus.RequiredPoolRefFields)
	setEqual(props["revisions"].(map[string]any)["items"].(map[string]any)["required"], poolstatus.RequiredRevisionFields)
	setEqual(props["ownedObjects"].(map[string]any)["items"].(map[string]any)["required"], poolstatus.RequiredOwnedObjectFields)

	setEqual(abs["properties"].(map[string]any)["absent"].(map[string]any)["required"], poolstatus.RequiredAbsentFields)
	cond := props["conditions"].(map[string]any)["items"].(map[string]any)
	cp := cond["properties"].(map[string]any)
	setEqual(cond["required"], poolstatus.RequiredConditionFields)
	setEqual(cp["objectRef"].(map[string]any)["required"], poolstatus.RequiredObjectRefFields)
	setEqual(cp["remainingDescendants"].(map[string]any)["required"], poolstatus.RequiredDescendantFields)

	var types, reasons []string
	for typ, reason := range poolstatus.ConditionReasons {
		types = append(types, typ)
		reasons = append(reasons, reason)
	}
	setEqual(cp["type"].(map[string]any)["enum"], types)
	setEqual(cp["reason"].(map[string]any)["enum"], reasons)
	setEqual(cp["objectRef"].(map[string]any)["properties"].(map[string]any)["kind"].(map[string]any)["enum"], poolstatus.ObjectKinds)
	setEqual(props["ownedObjects"].(map[string]any)["items"].(map[string]any)["properties"].(map[string]any)["kind"].(map[string]any)["enum"], poolstatus.ObjectKinds)
	for _, typ := range types {
		found := false
		for _, raw := range cond["allOf"].([]any) {
			b := raw.(map[string]any)
			c := b["if"].(map[string]any)["properties"].(map[string]any)["type"].(map[string]any)["const"]
			if c == typ {
				if p, ok := b["then"].(map[string]any)["properties"].(map[string]any); ok {
					if r, ok := p["reason"].(map[string]any); ok {
						found = r["const"] == poolstatus.ConditionReasons[typ]
					}
				}
			}
		}
		if !found {
			t.Fatalf("condition/reason pairing drift: %s", typ)
		}
	}
}
