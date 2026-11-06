package poolstatus_test

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

func TestEncoderFixturesAndIsolation(t *testing.T) {
	schema := readJSON(t, "v1.1/pool-status.schema.json")
	old := readJSON(t, "v1/pool-status.schema.json")
	for _, raw := range readJSON(t, "v1.1/conformance.json")["valid"].([]any) {
		fixture := raw.(map[string]any)
		t.Run(fixture["name"].(string), func(t *testing.T) {
			var r poolstatus.Report
			if err := json.Unmarshal(encoded(t, fixture["report"]), &r); err != nil {
				t.Fatal(err)
			}
			scope := poolstatus.Scope{PoolRef: r.PoolRef, OwnerAnnotation: `["own-tenant","own-profile"]`}
			// Caller-supplied wire identities are never evidence of ownership.
			r.OwnedObjects = []poolstatus.OwnedObject{{Kind: "Deployment", Name: "other-tenant-name", UID: "other-tenant-uid"}}
			objects := []poolstatus.ObservedObject{
				{OwnedObject: poolstatus.OwnedObject{Kind: "Deployment", Namespace: scope.Namespace, Name: scope.Name, UID: "own-uid", ResourceVersion: "123"}, OwnerAnnotation: scope.OwnerAnnotation},
				{OwnedObject: poolstatus.OwnedObject{Kind: "Service", Namespace: scope.Namespace, Name: "other-tenant-name", UID: "other-tenant-uid", ResourceVersion: "999"}, OwnerAnnotation: `["other-tenant-owner"]`},
				// A hash collision in the shared namespace: expected name, foreign owner.
				{OwnedObject: poolstatus.OwnedObject{Kind: "Deployment", Namespace: scope.Namespace, Name: scope.Name, UID: "other-tenant-uid", ResourceVersion: "999"}, OwnerAnnotation: `["other-tenant-owner"]`},
			}
			for _, name := range []string{"falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
				objects = append(objects, poolstatus.ObservedObject{OwnedObject: poolstatus.OwnedObject{Kind: "Lease", Namespace: scope.Namespace, Name: name, UID: "singleton-uid", ResourceVersion: "5"}, OwnerAnnotation: scope.OwnerAnnotation})
			}
			for i := range r.Conditions {
				if r.Conditions[i].ObjectRef != nil {
					r.Conditions[i].ObjectRef.Name = "other-tenant-name"
				}
			}
			if r.AbsenceVerified {
				objects = objects[1:]
			} // No owned object remains in absence reports.
			data, err := poolstatus.Encode(r, scope, objects, poolstatus.Options{})
			if err != nil {
				t.Fatal(err)
			}
			for _, foreign := range []string{"other-tenant-name", "other-tenant-uid", "other-tenant-owner", "falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
				if bytes.Contains(data, []byte(foreign)) {
					t.Fatalf("leaked %s", foreign)
				}
			}
			if err := poolstatus.ValidateJSON(data); err != nil {
				t.Fatal(err)
			}
			var doc map[string]any
			if err := json.Unmarshal(data, &doc); err != nil {
				t.Fatal(err)
			}
			if err := schemaError(schema, schema, doc); err != nil {
				t.Fatal(err)
			}
			if err := schemaError(old, old, doc); err != nil {
				t.Fatalf("old consumer: %v", err)
			}
			// Compare the public payload to the published example, including omission of
			// all unverified identities on a repair report.
			if !bytes.Equal(encoded(t, doc), encoded(t, fixture["report"])) {
				t.Fatalf("encoded payload differs from fixture: %s", data)
			}
			again, err := poolstatus.Encode(r, scope, objects, poolstatus.Options{})
			if err != nil || !bytes.Equal(data, again) {
				t.Fatal("encoding not deterministic", err)
			}
			legacy, err := poolstatus.Encode(r, scope, objects, poolstatus.Options{DisableExtensions: true})
			if err != nil {
				t.Fatal(err)
			}
			var legacyDoc map[string]any
			if err := json.Unmarshal(legacy, &legacyDoc); err != nil {
				t.Fatal(err)
			}
			if err := schemaError(old, old, legacyDoc); err != nil {
				t.Fatal(err)
			}
			for _, key := range []string{"contractRevision", "ownedObjects", "verifiedAbsence"} {
				if _, ok := legacyDoc[key]; ok {
					t.Fatalf("kill switch emitted %s", key)
				}
			}
			if legacyDoc["absenceVerified"] != false {
				t.Fatal("kill switch asserted absence")
			}
			for _, raw := range legacyDoc["conditions"].([]any) {
				c := raw.(map[string]any)
				if len(c) != 2 {
					t.Fatal("kill switch emitted condition extensions")
				}
			}
			var roundTrip poolstatus.Report
			if err := json.Unmarshal(data, &roundTrip); err != nil {
				t.Fatal(err)
			}
			if err := poolstatus.Validate(roundTrip); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestEncoderRejectsIncompleteScope(t *testing.T) {
	_, err := poolstatus.Encode(poolstatus.Report{}, poolstatus.Scope{}, nil, poolstatus.Options{})
	if err == nil || strings.Contains(err.Error(), "uid") {
		t.Fatal("expected safe scope error")
	}
}

func TestEncoderRejectsVerifiedAbsenceWhileOwnedObjectsRemain(t *testing.T) {
	var report poolstatus.Report
	for _, raw := range readJSON(t, "v1.1/conformance.json")["valid"].([]any) {
		fixture := raw.(map[string]any)
		if fixture["name"] == "verified-absence-wasm" {
			if err := json.Unmarshal(encoded(t, fixture["report"]), &report); err != nil {
				t.Fatal(err)
			}
		}
	}
	if !report.AbsenceVerified {
		t.Fatal("missing verified absence fixture")
	}
	scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: `["own-tenant","own-profile"]`}
	for _, kind := range []string{"Deployment", "Service", "Lease"} {
		t.Run(kind, func(t *testing.T) {
			observed := []poolstatus.ObservedObject{{
				OwnedObject: poolstatus.OwnedObject{
					Kind: kind, Namespace: scope.Namespace, Name: scope.Name,
					UID: "own-uid", ResourceVersion: "123",
				},
				OwnerAnnotation: scope.OwnerAnnotation,
			}}
			data, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{})
			if err == nil || len(data) != 0 {
				t.Fatal("encoder asserted verified absence while an owned object remains")
			}
			legacy, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{DisableExtensions: true})
			if err != nil {
				t.Fatal(err)
			}
			var decoded poolstatus.Report
			if err := json.Unmarshal(legacy, &decoded); err != nil {
				t.Fatal(err)
			}
			if decoded.AbsenceVerified || decoded.ContractRevision != "" || len(decoded.OwnedObjects) != 0 {
				t.Fatal("kill switch must emit v1 without asserting absence")
			}
		})
	}
}

func TestEncoderRejectsForeignConditionTextWithKillSwitch(t *testing.T) {
	for _, disable := range []bool{false, true} {
		for _, field := range []string{"type", "reason"} {
			r := poolstatus.Report{RuntimeMode: "wasm", Conditions: []poolstatus.Condition{{Type: poolstatus.UIDMismatch, Reason: "uid_mismatch"}}}
			if field == "type" {
				r.Conditions[0].Type = "other-tenant-name"
			} else {
				r.Conditions[0].Reason = "other-tenant-owner"
			}
			data, err := poolstatus.Encode(r, poolstatus.Scope{PoolRef: poolstatus.PoolRef{Namespace: "shared", Name: "own-pool"}, OwnerAnnotation: `["own-tenant","own-profile"]`}, nil, poolstatus.Options{DisableExtensions: disable})
			if err == nil || len(data) != 0 {
				t.Fatalf("disable=%v: encoder accepted untrusted condition %s", disable, field)
			}
		}
	}
}
