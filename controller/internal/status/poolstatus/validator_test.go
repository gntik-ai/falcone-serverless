package poolstatus_test

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

func fixtureReport(t *testing.T, name string) map[string]any {
	t.Helper()
	for _, raw := range readJSON(t, "v1.1/conformance.json")["valid"].([]any) {
		f := raw.(map[string]any)
		if f["name"] == name {
			return f["report"].(map[string]any)
		}
	}
	t.Fatalf("missing fixture %s", name)
	return nil
}

func decodeFixtureReport(t *testing.T, name string) poolstatus.Report {
	t.Helper()
	var report poolstatus.Report
	if err := json.Unmarshal(encoded(t, fixtureReport(t, name)), &report); err != nil {
		t.Fatal(err)
	}
	return report
}

func TestOCIMaterializationFixtures(t *testing.T) {
	for _, name := range []string{"ImagePullFailed", "ContainerStartFailed", "ContractProbeFailed", "Dematerialized"} {
		t.Run(name, func(t *testing.T) {
			r := fixtureReport(t, name)
			objects, _ := r["ownedObjects"].([]any)
			if name == "Dematerialized" {
				if r["materialized"] != false || len(objects) != 0 {
					t.Fatal("dematerialized example must have no live owned objects")
				}
				return
			}
			if r["materialized"] != true {
				t.Fatal("startup failure example must show a materialized pool")
			}
			kinds := map[string]bool{}
			for _, o := range objects {
				kinds[o.(map[string]any)["kind"].(string)] = true
			}
			if !kinds["Deployment"] || !kinds["Service"] {
				t.Fatal("materialized example must show both Deployment and Service")
			}
		})
	}
}

func TestAbsenceRejectsContradictoryState(t *testing.T) {
	schema := readJSON(t, "v1.1/pool-status.schema.json")
	for _, flag := range []bool{false, true} {
		for _, state := range []string{"owned-object", "materialized", "ready-replicas", "ready-capacity", "ready-revision", "UIDMismatch", "ResourceVersionMismatch", "OwnershipMismatch", "MissingPreconditions", "TerminationPending"} {
			t.Run(fmt.Sprintf("%v/%s", flag, state), func(t *testing.T) {
				r := fixtureReport(t, "verified-absence-oci")
				r["absenceVerified"] = flag
				switch state {
				case "owned-object":
					r["ownedObjects"] = fixtureReport(t, "ImagePullFailed")["ownedObjects"]
				case "materialized":
					r["materialized"] = true
				case "ready-replicas":
					r["readyReplicas"] = 3
				case "ready-capacity":
					r["readyCapacity"] = 1
				case "ready-revision":
					r["revisions"].([]any)[0].(map[string]any)["ready"] = true
				default:
					r["conditions"] = fixtureReport(t, state)["conditions"]
				}
				r = clone(t, r)
				t.Run("schema", func(t *testing.T) {
					if err := schemaError(schema, schema, r); err == nil {
						t.Fatal("schema accepted contradictory absence")
					}
				})
				t.Run("validator", func(t *testing.T) {
					if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
						t.Fatal("validator accepted contradictory absence")
					}
				})
				t.Run("encoder", func(t *testing.T) {
					var report poolstatus.Report
					if err := json.Unmarshal(encoded(t, r), &report); err != nil {
						t.Fatal(err)
					}
					scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: "own-owner"}
					var observed []poolstatus.ObservedObject
					for _, o := range report.OwnedObjects {
						observed = append(observed, poolstatus.ObservedObject{OwnedObject: o, OwnerAnnotation: scope.OwnerAnnotation})
					}
					if data, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{}); err == nil || len(data) != 0 {
						t.Fatal("encoder accepted contradictory absence")
					}
				})
			})
		}
	}
}

func TestWireIdentitiesStayWithinPool(t *testing.T) {
	for _, edit := range []string{"namespace", "name", "objectRef", "falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
		t.Run(edit, func(t *testing.T) {
			r := fixtureReport(t, "ImagePullFailed")
			o := r["ownedObjects"].([]any)[0].(map[string]any)
			switch edit {
			case "namespace", "name":
				o[edit] = "other-tenant"
			case "objectRef":
				r["conditions"].([]any)[0].(map[string]any)["objectRef"].(map[string]any)["name"] = "other-tenant"
			default:
				// Even an internally consistent reference must exclude singleton Leases.
				r["poolRef"].(map[string]any)["name"] = edit
				o["kind"], o["name"] = "Lease", edit
				r["conditions"] = []any{}
				schema := readJSON(t, "v1.1/pool-status.schema.json")
				if err := schemaError(schema, schema, r); err == nil {
					t.Error("schema accepted singleton Lease")
				}
			}
			if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
				t.Error("validator accepted out-of-scope identity")
			}
		})
	}
}

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

func TestSchemaAndValidatorRejectInvalidIdentitiesAndShapes(t *testing.T) {
	schema := readJSON(t, "v1.1/pool-status.schema.json")
	for _, edit := range []string{"owned-kind", "owned-empty-uid", "owned-empty-resourceVersion", "objectRef-kind", "objectRef-singleton-leader", "objectRef-singleton-scaler", "descendants-on-repair", "negative-generation"} {
		t.Run(edit, func(t *testing.T) {
			r := fixtureReport(t, "ImagePullFailed")
			switch edit {
			case "owned-kind":
				r["ownedObjects"].([]any)[0].(map[string]any)["kind"] = "Pod"
			case "owned-empty-uid":
				r["ownedObjects"].([]any)[0].(map[string]any)["uid"] = ""
			case "owned-empty-resourceVersion":
				r["ownedObjects"].([]any)[0].(map[string]any)["resourceVersion"] = ""
			case "objectRef-kind":
				r = fixtureReport(t, "UIDMismatch")
				r["conditions"].([]any)[0].(map[string]any)["objectRef"].(map[string]any)["kind"] = "Pod"
			case "objectRef-singleton-leader", "objectRef-singleton-scaler":
				r = fixtureReport(t, "UIDMismatch")
				name := "falcone-pool-controller-leader"
				if edit == "objectRef-singleton-scaler" {
					name = "falcone-pool-controller-scaler"
				}
				r["poolRef"].(map[string]any)["name"] = name
				r["conditions"].([]any)[0].(map[string]any)["objectRef"] = map[string]any{"kind": "Lease", "name": name}
			case "descendants-on-repair":
				r = fixtureReport(t, "UIDMismatch")
				r["conditions"].([]any)[0].(map[string]any)["remainingDescendants"] = map[string]any{"replicaSets": 1, "pods": 2}
			case "negative-generation":
				r = fixtureReport(t, "verified-absence-wasm")
				r["verifiedAbsence"].([]any)[0].(map[string]any)["generation"] = -1
			}
			r = clone(t, r)
			t.Run("schema", func(t *testing.T) {
				if err := schemaError(schema, schema, r); err == nil {
					t.Fatal("schema accepted invalid identity or evidence shape")
				}
			})
			t.Run("validator", func(t *testing.T) {
				if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
					t.Fatal("validator accepted invalid identity or evidence shape")
				}
			})
		})
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
}

func TestValidatorRejectsUnknownRevisionAlone(t *testing.T) {
	r := fixtureReport(t, "UIDMismatch")
	if err := poolstatus.ValidateJSON(encoded(t, r)); err != nil {
		t.Fatal("control report must be valid", err)
	}
	r["contractRevision"] = "1.2"
	if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
		t.Fatal("unknown revision accepted on an otherwise valid report")
	}
}

func TestValidatorAcceptsJSONSchemaIntegerForms(t *testing.T) {
	for _, version := range []string{"v1", "v1.1"} {
		schema := readJSON(t, version+"/pool-status.schema.json")
		for _, number := range []string{"1.0", "1e2", "100e-2", "9.223372036854775807e18", "-9.223372036854775808e18", "0e-1000000"} {
			t.Run(version+"/"+number, func(t *testing.T) {
				r := readJSON(t, version+"/conformance.json")["valid"].([]any)[0].(map[string]any)["report"].(map[string]any)
				r["observationSeq"] = json.Number(number)
				if err := schemaError(schema, schema, clone(t, r)); err != nil {
					t.Fatalf("schema rejected integer representation: %v", err)
				}
				if err := poolstatus.ValidateJSON(encoded(t, r)); err != nil {
					t.Fatalf("valid integer representation rejected: %v", err)
				}
			})
		}
	}
}

func TestValidatorAcceptsNestedIntegerForms(t *testing.T) {
	for _, name := range []string{"TerminationPending", "verified-absence-wasm"} {
		t.Run(name, func(t *testing.T) {
			r := fixtureReport(t, name)
			if name == "TerminationPending" {
				counts := r["conditions"].([]any)[0].(map[string]any)["remainingDescendants"].(map[string]any)
				counts["replicaSets"], counts["pods"] = json.Number("1.0"), json.Number("2e0")
			} else {
				r["verifiedAbsence"].([]any)[0].(map[string]any)["generation"] = json.Number("5.0")
			}
			if err := poolstatus.ValidateJSON(encoded(t, r)); err != nil {
				t.Fatalf("valid nested integer representation rejected: %v", err)
			}
		})
	}
}

func TestValidatorRejectsInexactOrOverflowingIntegers(t *testing.T) {
	for _, number := range []string{"1.00000000000000001", "1e-1000000", "9223372036854775808", "-9223372036854775809", "1e309"} {
		t.Run(number, func(t *testing.T) {
			r := fixtureReport(t, "UIDMismatch")
			r["observationSeq"] = json.Number(number)
			if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
				t.Fatal("accepted fractional or overflowing sequence")
			}
		})
	}
}

func TestLegacyIgnoresCaseVariantsOfKnownFields(t *testing.T) {
	schema := readJSON(t, "v1/pool-status.schema.json")
	for _, valid := range []bool{false, true} {
		t.Run(fmt.Sprint(valid), func(t *testing.T) {
			r := readJSON(t, "v1/conformance.json")["valid"].([]any)[0].(map[string]any)["report"].(map[string]any)
			r["runtimeMode"] = "wasm"
			r["runtimemode"] = "oci"
			r["poolRef"].(map[string]any)["NAME"] = 42
			r["conditions"] = []any{map[string]any{"type": "FutureCondition", "reason": "future_reason", "TYPE": 42}}
			r["revisions"] = []any{map[string]any{"revision": "rev-1", "ready": false, "READY": 42}}
			if !valid {
				r["runtimeMode"] = "bogus"
				r["runtimemode"] = "wasm"
				delete(r["poolRef"].(map[string]any), "NAME")
				r["conditions"], r["revisions"] = []any{}, []any{}
			}
			r = clone(t, r)
			if err := schemaError(schema, schema, r); (err == nil) != valid {
				t.Fatal("unexpected v1 schema verdict", err)
			}
			if err := poolstatus.ValidateJSON(encoded(t, r)); (err == nil) != valid {
				t.Fatal("v1 validator differs from schema", err)
			}
		})
	}
}

func TestSchemaAndValidatorRejectMalformedEvidence(t *testing.T) {
	schema := readJSON(t, "v1.1/pool-status.schema.json")
	for _, edit := range []string{"empty-proof", "null-generation", "null-absent", "null-count", "unknown-absent", "wrong-reason", "empty-wasm-revision", "empty-wasm-evidence-revision"} {
		t.Run(edit, func(t *testing.T) {
			r := fixtureReport(t, "verified-absence-wasm")
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
				delete(r, "verifiedAbsence")
				r["conditions"] = []any{map[string]any{"type": "TerminationPending", "reason": "termination_pending", "remainingDescendants": map[string]any{"replicaSets": nil, "pods": 1}}}
			case "wrong-reason":
				r["absenceVerified"] = false
				delete(r, "verifiedAbsence")
				r["conditions"] = []any{map[string]any{"type": "UIDMismatch", "reason": "ownership_mismatch"}}
			case "empty-wasm-revision":
				r["revision"] = ""
			case "empty-wasm-evidence-revision":
				a["revision"] = ""
			}
			r = clone(t, r)
			if err := schemaError(schema, schema, r); err == nil {
				t.Fatal("schema accepted malformed evidence")
			}
			if err := poolstatus.ValidateJSON(encoded(t, r)); err == nil {
				t.Fatal("Go validator accepted malformed evidence")
			}
		})
	}
}
