package poolstatus_test

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"reflect"
	"regexp"
	"strings"
	"testing"
)

const contractDir = "../../../../docs/contracts/pool-status/"

func readJSON(t *testing.T, path string) map[string]any {
	t.Helper()
	data, err := os.ReadFile(contractDir + path)
	if err != nil {
		t.Fatal(err)
	}
	var out map[string]any
	if err := json.Unmarshal(data, &out); err != nil {
		t.Fatal(err)
	}
	return out
}
func encoded(t *testing.T, value any) []byte {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return data
}
func clone(t *testing.T, value map[string]any) map[string]any {
	t.Helper()
	var out map[string]any
	if err := json.Unmarshal(encoded(t, value), &out); err != nil {
		t.Fatal(err)
	}
	return out
}

// Interpret the JSON Schema keywords used by these contracts independently of
// the production Go validator. Unsupported keywords fail the test rather than
// silently weakening conformance coverage. Only local refs are permitted.
func schemaError(root, s map[string]any, v any) error {
	fail := func() error { return fmt.Errorf("schema rejected %v", v) }
	for key := range s {
		switch key {
		case "$schema", "$id", "$defs", "title", "description", "$ref", "type", "properties", "required", "additionalProperties", "items", "enum", "const", "minimum", "minLength", "minItems", "maxItems", "format", "pattern", "allOf", "anyOf", "not", "if", "then", "else", "contains":
		default:
			return fmt.Errorf("unsupported schema keyword %s", key)
		}
	}
	if ref, ok := s["$ref"].(string); ok {
		if !strings.HasPrefix(ref, "#/") {
			return fmt.Errorf("nonlocal ref %s", ref)
		}
		var target any = root
		for _, part := range strings.Split(strings.TrimPrefix(ref, "#/"), "/") {
			target = target.(map[string]any)[part]
		}
		if err := schemaError(root, target.(map[string]any), v); err != nil {
			return err
		}
	}
	if typ, ok := s["type"].(string); ok {
		valid := false
		switch typ {
		case "object":
			_, valid = v.(map[string]any)
		case "array":
			_, valid = v.([]any)
		case "string":
			_, valid = v.(string)
		case "boolean":
			_, valid = v.(bool)
		case "integer":
			n, ok := v.(float64)
			valid = ok && math.Trunc(n) == n
		}
		if !valid {
			return fail()
		}
	}
	if c, ok := s["const"]; ok && !reflect.DeepEqual(c, v) {
		return fail()
	}
	if choices, ok := s["enum"].([]any); ok {
		found := false
		for _, c := range choices {
			found = found || reflect.DeepEqual(c, v)
		}
		if !found {
			return fail()
		}
	}
	if obj, ok := v.(map[string]any); ok {
		if req, ok := s["required"].([]any); ok {
			for _, name := range req {
				if _, found := obj[name.(string)]; !found {
					return fail()
				}
			}
		}
		props, _ := s["properties"].(map[string]any)
		for name, value := range obj {
			if child, found := props[name]; found {
				if err := schemaError(root, child.(map[string]any), value); err != nil {
					return fmt.Errorf("%s: %w", name, err)
				}
			} else if s["additionalProperties"] == false {
				return fail()
			}
		}
	}
	if arr, ok := v.([]any); ok {
		if min, ok := s["minItems"].(float64); ok && float64(len(arr)) < min {
			return fail()
		}
		if max, ok := s["maxItems"].(float64); ok && float64(len(arr)) > max {
			return fail()
		}
		if item, ok := s["items"].(map[string]any); ok {
			for _, e := range arr {
				if err := schemaError(root, item, e); err != nil {
					return err
				}
			}
		}
		if item, ok := s["contains"].(map[string]any); ok {
			found := false
			for _, e := range arr {
				found = found || schemaError(root, item, e) == nil
			}
			if !found {
				return fail()
			}
		}
	}
	if str, ok := v.(string); ok {
		if min, ok := s["minLength"].(float64); ok && float64(len([]rune(str))) < min {
			return fail()
		}
		// Draft 2020-12 format is an annotation by default. Only assertion
		// keywords such as pattern can reject a malformed value here.
		if pattern, ok := s["pattern"].(string); ok {
			re, err := regexp.Compile(pattern)
			if err != nil {
				return err
			}
			if !re.MatchString(str) {
				return fail()
			}
		}
	}
	if n, ok := v.(float64); ok {
		if min, ok := s["minimum"].(float64); ok && n < min {
			return fail()
		}
	}
	if branches, ok := s["allOf"].([]any); ok {
		for _, b := range branches {
			if err := schemaError(root, b.(map[string]any), v); err != nil {
				return err
			}
		}
	}
	if branches, ok := s["anyOf"].([]any); ok {
		found := false
		for _, b := range branches {
			found = found || schemaError(root, b.(map[string]any), v) == nil
		}
		if !found {
			return fail()
		}
	}
	if b, ok := s["not"].(map[string]any); ok && schemaError(root, b, v) == nil {
		return fail()
	}
	if b, ok := s["if"].(map[string]any); ok {
		branch := "else"
		if schemaError(root, b, v) == nil {
			branch = "then"
		}
		if next, ok := s[branch].(map[string]any); ok {
			return schemaError(root, next, v)
		}
	}
	return nil
}

func TestSchemaConformance(t *testing.T) {
	for _, version := range []string{"v1", "v1.1"} {
		schema := readJSON(t, version+"/pool-status.schema.json")
		fixtures := readJSON(t, version+"/conformance.json")
		for _, group := range []string{"valid", "invalid"} {
			for _, raw := range fixtures[group].([]any) {
				fixture := raw.(map[string]any)
				t.Run(version+"/"+group+"/"+fixture["name"].(string), func(t *testing.T) {
					err := schemaError(schema, schema, fixture["report"])
					if (err == nil) != (group == "valid") {
						t.Fatalf("classification: %v", err)
					}
				})
			}
		}
	}
}

func TestCompatibilityCheck(t *testing.T) {
	baseline := readJSON(t, "v1/pool-status.schema.json")
	extended := readJSON(t, "v1.1/pool-status.schema.json")
	if err := compatibleSchemas(t, baseline, extended); err != nil {
		t.Fatal(err)
	}
	for _, raw := range readJSON(t, "v1/conformance.json")["valid"].([]any) {
		if err := schemaError(extended, extended, raw.(map[string]any)["report"]); err != nil {
			t.Fatal(err)
		}
	}
	for _, edit := range []string{"removed", "retyped", "legacy-optional-removed", "legacy-required-removed", "legacy-retyped", "legacy-enum", "legacy-required-added"} {
		schema := clone(t, extended)
		current := schema["$defs"].(map[string]any)["current"].(map[string]any)
		props := current["properties"].(map[string]any)
		switch edit {
		case "removed":
			delete(props, "readyCapacity")
		case "retyped":
			props["readyCapacity"] = map[string]any{"type": "string"}
		default:
			legacy := schema["$defs"].(map[string]any)["legacy"].(map[string]any)
			p := legacy["properties"].(map[string]any)
			switch edit {
			case "legacy-optional-removed":
				delete(p, "materialized")
			case "legacy-required-removed":
				delete(p, "draining")
				var required []any
				for _, key := range legacy["required"].([]any) {
					if key != "draining" {
						required = append(required, key)
					}
				}
				legacy["required"] = required
			case "legacy-retyped":
				p["materialized"] = map[string]any{"type": "string"}
			case "legacy-enum":
				p["runtimeMode"].(map[string]any)["enum"] = []any{"wasm"}
			case "legacy-required-added":
				legacy["required"] = append(legacy["required"].([]any), "futureField")
			}
		}
		if err := compatibleSchemas(t, baseline, schema); err == nil {
			t.Errorf("%s field accepted", edit)
		}
	}
}

func compatibleSchemas(t *testing.T, baseline, extended map[string]any) error {
	t.Helper()
	defs := extended["$defs"].(map[string]any)
	legacy := clone(t, baseline)
	delete(legacy, "$schema")
	delete(legacy, "$id")
	legacy["not"] = map[string]any{"required": []any{"contractRevision"}}
	// This branch handles every unrevisioned v1 report, including optional fields
	// and values absent from the finite fixtures. Keep the entire baseline,
	// including required keys, enums and conditional constraints, unchanged.
	if !reflect.DeepEqual(legacy, defs["legacy"]) {
		return fmt.Errorf("legacy branch differs from v1 baseline")
	}
	return compatible(baseline, defs["current"].(map[string]any))
}

// Compare every v1 property recursively, even optional OCI and nested fields that
// a finite fixture set could miss. Semantic conformance is checked separately.
func compatible(old, next map[string]any) error {
	if old["type"] != next["type"] {
		return fmt.Errorf("retyped %v to %v", old["type"], next["type"])
	}
	if props, ok := old["properties"].(map[string]any); ok {
		newer, ok := next["properties"].(map[string]any)
		if !ok {
			return fmt.Errorf("removed properties")
		}
		for name, child := range props {
			n, ok := newer[name].(map[string]any)
			if !ok {
				return fmt.Errorf("removed field %s", name)
			}
			if err := compatible(child.(map[string]any), n); err != nil {
				return fmt.Errorf("%s: %w", name, err)
			}
		}
	}
	if item, ok := old["items"].(map[string]any); ok {
		n, ok := next["items"].(map[string]any)
		if !ok {
			return fmt.Errorf("removed items")
		}
		return compatible(item, n)
	}
	return nil
}
