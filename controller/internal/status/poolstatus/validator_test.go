package poolstatus_test

import (
	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
	"testing"
)

func TestValidatorConformance(t *testing.T) {
	for _, version := range []string{"v1", "v1.1"} {
		fixtures := readJSON(t, version+"/conformance.json")
		for _, group := range []string{"valid", "invalid"} {
			for _, raw := range fixtures[group].([]any) {
				f := raw.(map[string]any)
				t.Run(version+"/"+group+"/"+f["name"].(string), func(t *testing.T) {
					err := poolstatus.ValidateJSON(encoded(t, f["report"]))
					if (err == nil) != (group == "valid") {
						t.Fatalf("classification: %v", err)
					}
				})
			}
		}
	}
}
func TestLegacyUnknownFieldsAndTrailingData(t *testing.T) {
	r := readJSON(t, "v1/conformance.json")["valid"].([]any)[0].(map[string]any)["report"].(map[string]any)
	r["futureField"] = true
	r["conditions"] = []any{map[string]any{"type": "FutureCondition", "reason": "future_reason", "future": true}}
	if err := poolstatus.ValidateJSON(encoded(t, r)); err != nil {
		t.Fatal(err)
	}
	if err := poolstatus.ValidateJSON(append(encoded(t, r), []byte(" {}")...)); err == nil {
		t.Fatal("trailing data accepted")
	}
	r["contractRevision"] = "1.2"
	if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
		t.Fatal("unknown revision accepted")
	}
}

func TestSchemaAndValidatorRejectMalformedEvidence(t *testing.T) {
	fixtures := readJSON(t, "v1.1/conformance.json")["valid"].([]any)
	schema := readJSON(t, "v1.1/pool-status.schema.json")
	for _, edit := range []string{"empty-proof", "null-generation", "null-absent", "null-count", "unknown-absent", "wrong-reason", "empty-wasm-revision", "empty-wasm-evidence-revision"} {
		t.Run(edit, func(t *testing.T) {
			var r map[string]any
			for _, raw := range fixtures {
				f := raw.(map[string]any)
				if f["name"] == "verified-absence-wasm" {
					r = clone(t, f["report"].(map[string]any))
				}
			}
			if r == nil {
				t.Fatal("missing fixture")
			}
			a := r["verifiedAbsence"].([]any)[0].(map[string]any)
			switch edit {
			case "empty-proof":
				r["verifiedAbsence"] = []any{}
			case "null-generation":
				a["generation"] = nil
			case "null-absent":
				a["absent"] = nil
			case "unknown-absent":
				a["absent"].(map[string]any)["jobs"] = true
			case "null-count":
				r["absenceVerified"] = false
				r["conditions"] = []any{map[string]any{"type": "TerminationPending", "reason": "termination_pending", "remainingDescendants": map[string]any{"replicaSets": nil, "pods": 1}}}
			case "wrong-reason":
				r["absenceVerified"] = false
				r["conditions"] = []any{map[string]any{"type": "UIDMismatch", "reason": "ownership_mismatch"}}
			case "empty-wasm-revision":
				r["revision"] = ""
			case "empty-wasm-evidence-revision":
				a["revision"] = ""
			}
			if err := schemaError(schema, schema, r); err == nil {
				t.Fatal("schema accepted malformed evidence")
			}
			if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
				t.Fatal("Go validator accepted malformed evidence")
			}
		})
	}
}
