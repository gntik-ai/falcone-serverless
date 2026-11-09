package poolstatus_test

import (
	"bytes"
	"encoding/json"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

func TestEncoderFixturesAndIsolation(t *testing.T) {
	schema := readJSON(t, "v1.2/pool-status.schema.json")
	old := readJSON(t, "v1/pool-status.schema.json")
	for _, raw := range readJSON(t, "v1.2/conformance.json")["valid"].([]any) {
		fixture := raw.(map[string]any)
		t.Run(fixture["name"].(string), func(t *testing.T) {
			var r poolstatus.Report
			if err := json.Unmarshal(encoded(t, fixture["report"]), &r); err != nil {
				t.Fatal(err)
			}
			scope := poolstatus.Scope{PoolRef: r.PoolRef, OwnerAnnotation: `["own-tenant","own-profile"]`}
			r.PoolRef = poolstatus.PoolRef{Namespace: "other-tenant-namespace", Name: "other-tenant-pool"}
			// Caller-supplied wire identities are never evidence of ownership.
			r.OwnedObjects = []poolstatus.OwnedObject{{Kind: "Deployment", Name: "other-tenant-name", UID: "other-tenant-uid"}}
			objects := []poolstatus.ObservedObject{
				{OwnedObject: poolstatus.OwnedObject{Kind: "Deployment", Namespace: scope.Namespace, Name: scope.Name, UID: "own-uid", ResourceVersion: "123"}, OwnerAnnotation: scope.OwnerAnnotation},
				{OwnedObject: poolstatus.OwnedObject{Kind: "Service", Namespace: scope.Namespace, Name: "other-tenant-name", UID: "other-tenant-uid", ResourceVersion: "999"}, OwnerAnnotation: `["other-tenant-owner"]`},
				// A hash collision in the shared namespace: expected name, foreign owner.
				{OwnedObject: poolstatus.OwnedObject{Kind: "Deployment", Namespace: scope.Namespace, Name: scope.Name, UID: "other-tenant-uid", ResourceVersion: "999"}, OwnerAnnotation: `["other-tenant-owner"]`},
			}
			if r.RuntimeMode == "oci" && r.Materialized != nil && *r.Materialized {
				objects = append(objects, poolstatus.ObservedObject{
					OwnedObject:     poolstatus.OwnedObject{Kind: "Service", Namespace: scope.Namespace, Name: scope.Name, UID: "own-service-uid", ResourceVersion: "124"},
					OwnerAnnotation: scope.OwnerAnnotation,
				})
			}
			for _, name := range []string{"falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
				objects = append(objects, poolstatus.ObservedObject{OwnedObject: poolstatus.OwnedObject{Kind: "Lease", Namespace: scope.Namespace, Name: name, UID: "singleton-uid", ResourceVersion: "5"}, OwnerAnnotation: scope.OwnerAnnotation})
			}
			for i := range r.Conditions {
				if r.Conditions[i].ObjectRef != nil {
					r.Conditions[i].ObjectRef.Name = "other-tenant-name"
				}
			}
			if r.AbsenceVerified || fixture["name"] == "Dematerialized" {
				objects = slices.DeleteFunc(objects, func(o poolstatus.ObservedObject) bool {
					return o.Namespace == scope.Namespace && o.Name == scope.Name
				})
			} // Neither owned nor mismatching objects remain at the pool name.
			data, err := poolstatus.Encode(r, scope, objects, poolstatus.Options{})
			if err != nil {
				t.Fatal(err)
			}
			for _, foreign := range []string{"other-tenant-namespace", "other-tenant-pool", "other-tenant-name", "other-tenant-uid", "other-tenant-owner", "falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
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
				if c["type"] == poolstatus.MissingPreconditions || c["type"] == poolstatus.TerminationPending {
					t.Fatal("kill switch emitted an extension condition")
				}
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

func TestEncoderCanonicalizesObservationLists(t *testing.T) {
	for _, name := range []string{"ImagePullFailed", "verified-absence-wasm"} {
		t.Run(name, func(t *testing.T) {
			r := decodeFixtureReport(t, name)
			r.Conditions = []poolstatus.Condition{
				{Type: "ImagePullFailed", Reason: "image_pull_failed", ObjectRef: &poolstatus.ObjectRef{Kind: "Service", Name: "untrusted"}},
				{Type: "ImagePullFailed", Reason: "image_pull_failed", ObjectRef: &poolstatus.ObjectRef{Kind: "Deployment", Name: "untrusted"}},
				{Type: "ContainerStartFailed", Reason: "container_start_failed"},
			}
			if r.RuntimeMode == "wasm" {
				r.Revisions = []poolstatus.Revision{{Revision: "rev-b"}, {Revision: "rev-a"}}
				a := r.VerifiedAbsence[0]
				a.Generation = 4
				r.VerifiedAbsence = append(r.VerifiedAbsence, a)
			}
			scope := poolstatus.Scope{PoolRef: r.PoolRef, OwnerAnnotation: "own-owner"}
			before := encoded(t, r)
			first, err := poolstatus.Encode(r, scope, nil, poolstatus.Options{})
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(before, encoded(t, r)) {
				t.Fatal("encoder mutated caller's observation")
			}
			slices.Reverse(r.Conditions)
			slices.Reverse(r.Revisions)
			slices.Reverse(r.VerifiedAbsence)
			second, err := poolstatus.Encode(r, scope, nil, poolstatus.Options{})
			if err != nil || !bytes.Equal(first, second) {
				t.Fatal("equivalent observations have different payloads", err)
			}
		})
	}
}

func TestEncoderRejectsIncompleteScope(t *testing.T) {
	for _, missing := range []string{"namespace", "name", "owner"} {
		for _, disable := range []bool{false, true} {
			t.Run(missing+"/disable="+strconv.FormatBool(disable), func(t *testing.T) {
				r := decodeFixtureReport(t, "ImagePullFailed")
				scope := poolstatus.Scope{PoolRef: r.PoolRef, OwnerAnnotation: "own-owner"}
				switch missing {
				case "namespace":
					scope.Namespace = ""
				case "name":
					scope.Name = ""
				case "owner":
					scope.OwnerAnnotation = ""
				}
				observed := []poolstatus.ObservedObject{{
					OwnedObject: r.OwnedObjects[0],
					// Missing ownership must never match an incomplete trusted scope.
					OwnerAnnotation: "",
				}}
				data, err := poolstatus.Encode(r, scope, observed, poolstatus.Options{DisableExtensions: disable})
				if err == nil || len(data) != 0 || err.Error() != "authoritative pool scope is required" {
					t.Fatal("expected safe scope error with no report", err)
				}
			})
		}
	}
}

func TestEncoderKillSwitchClearsDescendantsOnLegacyCondition(t *testing.T) {
	r := decodeFixtureReport(t, "UIDMismatch")
	r.Conditions[0].RemainingDescendants = &poolstatus.Descendants{ReplicaSets: 1, Pods: 2}
	r.AbsenceVerified = true
	scope := poolstatus.Scope{PoolRef: r.PoolRef, OwnerAnnotation: "own-owner"}
	data, err := poolstatus.Encode(r, scope, nil, poolstatus.Options{DisableExtensions: true})
	if err != nil {
		t.Fatal(err)
	}
	var doc map[string]any
	if err := json.Unmarshal(data, &doc); err != nil {
		t.Fatal(err)
	}
	schema := readJSON(t, "v1/pool-status.schema.json")
	if err := schemaError(schema, schema, doc); err != nil {
		t.Fatal(err)
	}
	if doc["absenceVerified"] != false {
		t.Fatal("kill switch asserted absence")
	}
	for _, key := range []string{"contractRevision", "ownedObjects", "verifiedAbsence"} {
		if _, present := doc[key]; present {
			t.Fatalf("kill switch emitted %s", key)
		}
	}
	conditions := doc["conditions"].([]any)
	if len(conditions) != 1 {
		t.Fatal("kill switch must preserve the legacy repair condition")
	}
	c := conditions[0].(map[string]any)
	if len(c) != 2 || c["type"] != "UIDMismatch" || c["reason"] != "uid_mismatch" {
		t.Fatal("kill switch must emit only the v1 condition fields", c)
	}
}

func TestEncoderRejectsSingletonScope(t *testing.T) {
	for _, name := range []string{"falcone-pool-controller-leader", "falcone-pool-controller-scaler"} {
		for _, disable := range []bool{false, true} {
			t.Run(name+"/disable="+strconv.FormatBool(disable), func(t *testing.T) {
				report := decodeFixtureReport(t, "ImagePullFailed")
				scope := poolstatus.Scope{PoolRef: poolstatus.PoolRef{Namespace: report.PoolRef.Namespace, Name: name}, OwnerAnnotation: "own-owner"}
				if data, err := poolstatus.Encode(report, scope, nil, poolstatus.Options{DisableExtensions: disable}); err == nil || len(data) != 0 {
					t.Fatal("encoder accepted a controller singleton as pool scope")
				}
			})
		}
	}
}

func TestEncoderCanonicalizesOwnedObjects(t *testing.T) {
	report := decodeFixtureReport(t, "ImagePullFailed")
	scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: "own-owner"}
	// UIDs deliberately sort differently from kinds, so UID sorting cannot mask
	// a missing kind comparison. Each object is verified in the same shared pool.
	want := []poolstatus.OwnedObject{
		{Kind: "Deployment", Namespace: scope.Namespace, Name: scope.Name, UID: "z-deployment", ResourceVersion: "30"},
		{Kind: "Lease", Namespace: scope.Namespace, Name: scope.Name, UID: "m-lease", ResourceVersion: "20"},
		{Kind: "Service", Namespace: scope.Namespace, Name: scope.Name, UID: "a-service", ResourceVersion: "10"},
	}
	schema := readJSON(t, "v1.2/pool-status.schema.json")
	var first []byte
	for _, order := range [][3]int{{0, 1, 2}, {0, 2, 1}, {1, 0, 2}, {1, 2, 0}, {2, 0, 1}, {2, 1, 0}} {
		var observed []poolstatus.ObservedObject
		for _, i := range order {
			observed = append(observed, poolstatus.ObservedObject{OwnedObject: want[i], OwnerAnnotation: scope.OwnerAnnotation})
		}
		before := slices.Clone(observed)
		data, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{})
		if err != nil {
			t.Fatal(err)
		}
		var got poolstatus.Report
		if err := json.Unmarshal(data, &got); err != nil {
			t.Fatal(err)
		}
		if !slices.Equal(got.OwnedObjects, want) {
			t.Fatalf("owned object identities/order differ: %+v", got.OwnedObjects)
		}
		var doc map[string]any
		if err := json.Unmarshal(data, &doc); err != nil {
			t.Fatal(err)
		}
		if err := schemaError(schema, schema, doc); err != nil {
			t.Fatal(err)
		}
		if first == nil {
			first = data
		}
		if !bytes.Equal(first, data) {
			t.Fatalf("owned object order %v changed the payload", order)
		}
		if !slices.Equal(before, observed) {
			t.Fatal("encoder mutated caller's observations")
		}
	}
}

func TestEncoderRejectsMalformedReadAt(t *testing.T) {
	for _, readAt := range []string{"2026-01-01T1:00:00Z", "2026-01-01T00:00:00,5Z", "2026-01-01T00:00:00+24:00", "2026-02-30T00:00:00Z"} {
		t.Run(readAt, func(t *testing.T) {
			report := decodeFixtureReport(t, "verified-absence-wasm")
			report.VerifiedAbsence[0].ReadAt = readAt
			scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: "own-owner"}
			if data, err := poolstatus.Encode(report, scope, nil, poolstatus.Options{}); err == nil || len(data) != 0 {
				t.Fatal("encoder emitted malformed fresh-read time")
			}
		})
	}
}

func TestEncoderPreservesValidReadAt(t *testing.T) {
	schema := readJSON(t, "v1.2/pool-status.schema.json")
	for _, readAt := range []string{"2026-01-01T00:00:00.5Z", "2026-01-01T00:00:00.123456789+05:30", "2026-01-01T23:59:59-23:59"} {
		t.Run(readAt, func(t *testing.T) {
			report := decodeFixtureReport(t, "verified-absence-wasm")
			report.VerifiedAbsence[0].ReadAt = readAt
			scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: "own-owner"}
			data, err := poolstatus.Encode(report, scope, nil, poolstatus.Options{})
			if err != nil {
				t.Fatal(err)
			}
			var doc map[string]any
			if err := json.Unmarshal(data, &doc); err != nil {
				t.Fatal(err)
			}
			if err := schemaError(schema, schema, doc); err != nil {
				t.Fatal(err)
			}
			if got := doc["verifiedAbsence"].([]any)[0].(map[string]any)["readAt"]; got != readAt {
				t.Fatalf("fresh-read time changed: %v", got)
			}
		})
	}
}

func TestEncoderRejectsVerifiedAbsenceWhileOwnedObjectsRemain(t *testing.T) {
	report := decodeFixtureReport(t, "verified-absence-wasm")
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

func TestEncoderRejectsAbsenceAtOccupiedPoolName(t *testing.T) {
	report := decodeFixtureReport(t, "verified-absence-wasm")
	scope := poolstatus.Scope{PoolRef: report.PoolRef, OwnerAnnotation: `["own-tenant","own-profile"]`}
	for _, owner := range []string{"", `["other-tenant-owner"]`, scope.OwnerAnnotation} {
		for _, kind := range []string{"Deployment", "Service", "Lease"} {
			t.Run(kind+"/"+owner, func(t *testing.T) {
				observed := []poolstatus.ObservedObject{{OwnedObject: poolstatus.OwnedObject{
					Kind: kind, Namespace: scope.Namespace, Name: scope.Name, UID: "other-tenant-uid", ResourceVersion: "999",
				}, OwnerAnnotation: owner}}
				data, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{})
				if err == nil || len(data) != 0 {
					t.Fatal("absence accepted while expected name is occupied")
				}
				if strings.Contains(err.Error(), "other-tenant") || strings.Contains(err.Error(), "999") {
					t.Fatal("error leaked observed identity")
				}
				if _, err := poolstatus.Encode(report, scope, observed, poolstatus.Options{DisableExtensions: true}); err != nil {
					t.Fatal("kill switch must still clear absence evidence", err)
				}
			})
		}
	}
}
