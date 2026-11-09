// Package poolstatus defines ctr-pool-status-report. It does not read Kubernetes,
// prove descendant absence, delete objects, or emit reports to Falcone.
package poolstatus

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"math/big"
	"regexp"
	"slices"
	"strings"
	"time"
)

// ContractRevision is the revision the encoder emits. Revision 1.1 stays valid on
// the wire, and revision 1.2 adds the reporter lineage (the singleton Lease UID).
const (
	ContractRevision = "1.2"
	revision11       = "1.1"
)

// Match the published schema's syntax as well as parsing the calendar date:
// time.Parse alone accepts a single-digit hour, comma fractions and +24:00.
var readAtPattern = regexp.MustCompile(`^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](\.[0-9]+)?(Z|[+-]([01][0-9]|2[0-3]):[0-5][0-9])$`)

const (
	UIDMismatch             = "UIDMismatch"
	ResourceVersionMismatch = "ResourceVersionMismatch"
	OwnershipMismatch       = "OwnershipMismatch"
	MissingPreconditions    = "MissingPreconditions"
	TerminationPending      = "TerminationPending"
)

var ConditionReasons = map[string]string{
	UIDMismatch: "uid_mismatch", ResourceVersionMismatch: "resource_version_mismatch",
	OwnershipMismatch: "ownership_mismatch", MissingPreconditions: "missing_preconditions",
	TerminationPending: "termination_pending", "ImagePullFailed": "image_pull_failed",
	"ContainerStartFailed": "container_start_failed", "ContractProbeFailed": "contract_probe_failed",
	"Dematerialized": "dematerialized",
}
var ObjectKinds = []string{"Deployment", "Service", "Lease"}
var RequiredLineageFields = []string{"reporterLineage"}
var RequiredReportFields = []string{"poolRef", "observedGeneration", "reporterEpoch", "observationSeq", "desiredReplicas", "readyReplicas", "revisions", "readyCapacity", "draining", "conditions", "absenceVerified", "runtimeMode"}
var RequiredPoolRefFields = []string{"namespace", "name"}
var RequiredRevisionFields = []string{"revision", "ready"}
var RequiredConditionFields = []string{"type", "reason"}
var RequiredObjectRefFields = []string{"kind", "name"}
var RequiredDescendantFields = []string{"replicaSets", "pods"}
var RequiredOwnedObjectFields = []string{"kind", "namespace", "name", "uid", "resourceVersion"}
var RequiredAbsenceFields = []string{"generation", "readAt", "absent"}
var RequiredAbsentFields = []string{"deployments", "services", "leases", "replicaSets", "pods"}

// PoolRef is the authoritative, expected pool, never an observed object's name.
type PoolRef struct {
	Namespace string `json:"namespace"`
	Name      string `json:"name"`
}
type Revision struct {
	Revision string `json:"revision"`
	Ready    bool   `json:"ready"`
}
type ObjectRef struct {
	Kind string `json:"kind"`
	Name string `json:"name"`
}
type Descendants struct {
	ReplicaSets int64 `json:"replicaSets"`
	Pods        int64 `json:"pods"`
}
type Condition struct {
	Type                 string       `json:"type"`
	Reason               string       `json:"reason"`
	ObjectRef            *ObjectRef   `json:"objectRef,omitempty"`
	RemainingDescendants *Descendants `json:"remainingDescendants,omitempty"`
}
type OwnedObject struct {
	Kind            string `json:"kind"`
	Namespace       string `json:"namespace"`
	Name            string `json:"name"`
	UID             string `json:"uid"`
	ResourceVersion string `json:"resourceVersion"`
}
type Absent struct {
	Deployments bool `json:"deployments"`
	Services    bool `json:"services"`
	Leases      bool `json:"leases"`
	ReplicaSets bool `json:"replicaSets"`
	Pods        bool `json:"pods"`
}
type VerifiedAbsence struct {
	Revision   string `json:"revision,omitempty"`
	Generation int64  `json:"generation"`
	ReadAt     string `json:"readAt"`
	Absent     Absent `json:"absent"`
}
type Report struct {
	PoolRef            PoolRef           `json:"poolRef"`
	ObservedGeneration int64             `json:"observedGeneration"`
	ReporterEpoch      int64             `json:"reporterEpoch"`
	ReporterLineage    string            `json:"reporterLineage,omitempty"`
	ObservationSeq     int64             `json:"observationSeq"`
	DesiredReplicas    int64             `json:"desiredReplicas"`
	ReadyReplicas      int64             `json:"readyReplicas"`
	Revisions          []Revision        `json:"revisions"`
	ReadyCapacity      int64             `json:"readyCapacity"`
	Draining           bool              `json:"draining"`
	Conditions         []Condition       `json:"conditions"`
	AbsenceVerified    bool              `json:"absenceVerified"`
	RuntimeMode        string            `json:"runtimeMode"`
	Revision           string            `json:"revision,omitempty"`
	Materialized       *bool             `json:"materialized,omitempty"`
	ContractRevision   string            `json:"contractRevision,omitempty"`
	OwnedObjects       []OwnedObject     `json:"ownedObjects,omitempty"`
	VerifiedAbsence    []VerifiedAbsence `json:"verifiedAbsence,omitempty"`
}

// field describes a wire shape, separately from the Go structs so omitted keys
// and JSON null cannot become valid zero values during decoding.
type field struct {
	kind       string
	properties map[string]field
	required   []string
	item       *field
}

func object(properties map[string]field, required ...string) field {
	return field{kind: "object", properties: properties, required: required}
}
func array(item field) field { return field{kind: "array", item: &item} }

var text = field{kind: "string"}
var integer = field{kind: "integer"}
var boolean = field{kind: "boolean"}

func wireShape(extended, lineage bool) field {
	condition := object(map[string]field{"type": text, "reason": text}, RequiredConditionFields...)
	props := map[string]field{
		"poolRef":            object(map[string]field{"namespace": text, "name": text}, RequiredPoolRefFields...),
		"observedGeneration": integer, "reporterEpoch": integer, "observationSeq": integer,
		"desiredReplicas": integer, "readyReplicas": integer, "readyCapacity": integer, "draining": boolean,
		"revisions":       array(object(map[string]field{"revision": text, "ready": boolean}, RequiredRevisionFields...)),
		"absenceVerified": boolean, "runtimeMode": text, "revision": text, "materialized": boolean,
	}
	if extended {
		condition.properties["objectRef"] = object(map[string]field{"kind": text, "name": text}, RequiredObjectRefFields...)
		condition.properties["remainingDescendants"] = object(map[string]field{"replicaSets": integer, "pods": integer}, RequiredDescendantFields...)
		props["contractRevision"] = text
		props["ownedObjects"] = array(object(map[string]field{"kind": text, "namespace": text, "name": text, "uid": text, "resourceVersion": text}, RequiredOwnedObjectFields...))
		absent := map[string]field{}
		for _, key := range RequiredAbsentFields {
			absent[key] = boolean
		}
		props["verifiedAbsence"] = array(object(map[string]field{"revision": text, "generation": integer, "readAt": text, "absent": object(absent, RequiredAbsentFields...)}, RequiredAbsenceFields...))
	}
	props["conditions"] = array(condition)
	required := RequiredReportFields
	if lineage {
		props["reporterLineage"] = text
		required = append(slices.Clone(required), RequiredLineageFields...)
	}
	return object(props, required...)
}
func (f field) check(v any, strict bool) error {
	switch f.kind {
	case "object":
		obj, ok := v.(map[string]any)
		if !ok {
			return errors.New("expected object")
		}
		for _, key := range f.required {
			if _, ok := obj[key]; !ok {
				return fmt.Errorf("missing %s", key)
			}
		}
		for key, value := range obj {
			child, ok := f.properties[key]
			if !ok {
				if strict {
					return fmt.Errorf("unknown field %s", key)
				}
				continue
			}
			if err := child.check(value, strict); err != nil {
				return fmt.Errorf("%s: %w", key, err)
			}
		}
	case "array":
		list, ok := v.([]any)
		if !ok {
			return errors.New("expected array")
		}
		for _, item := range list {
			if err := f.item.check(item, strict); err != nil {
				return err
			}
		}
	case "string":
		if _, ok := v.(string); !ok {
			return errors.New("expected string")
		}
	case "integer":
		n, ok := v.(json.Number)
		if !ok {
			return errors.New("expected integer")
		}
		if _, err := wireInteger(n); err != nil {
			return err
		}
	case "boolean":
		if _, ok := v.(bool); !ok {
			return errors.New("expected boolean")
		}
	}
	return nil
}

// wireInteger accepts JSON Schema integer representations without float rounding.
// Keep the typed report's int64 range; fractional and overflowing inputs fail.
func wireInteger(n json.Number) (int64, error) {
	if value, err := n.Int64(); err == nil {
		return value, nil
	}
	mantissa, _, _ := strings.Cut(strings.ToLower(n.String()), "e")
	if strings.Trim(mantissa, "-+.0") == "" {
		return 0, nil
	}
	// Reject extreme exponents before allocating an exact rational. A nonzero
	// value that underflows to zero cannot be an integer.
	approx, err := n.Float64()
	if err == nil && approx != 0 && !math.IsInf(approx, 0) && math.Trunc(approx) == approx {
		if exact, ok := new(big.Rat).SetString(n.String()); ok && exact.IsInt() && exact.Num().IsInt64() {
			return exact.Num().Int64(), nil
		}
	}
	return 0, errors.New("expected int64 integer")
}

// canonicalValue runs after shape checking. Normalize integer representations
// for Go's struct decoder and remove unknown v1 keys at every depth: its
// case-insensitive tag matching must not override the exact contract names.
func (f field) canonicalValue(v any, discardUnknown bool) any {
	switch f.kind {
	case "object":
		obj := v.(map[string]any)
		for key, value := range obj {
			if child, ok := f.properties[key]; ok {
				obj[key] = child.canonicalValue(value, discardUnknown)
			} else if discardUnknown {
				delete(obj, key)
			}
		}
	case "array":
		for i, item := range v.([]any) {
			v.([]any)[i] = f.item.canonicalValue(item, discardUnknown)
		}
	case "integer":
		value, _ := wireInteger(v.(json.Number)) // Shape checking already succeeded.
		return value
	}
	return v
}

// ValidateJSON validates required keys, wire types, and revision-specific rules.
// Without contractRevision, v1 accepts unknown fields and condition values.
func ValidateJSON(data []byte) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	var doc map[string]any
	if err := dec.Decode(&doc); err != nil {
		return err
	}
	if _, err := dec.Token(); !errors.Is(err, io.EOF) {
		return errors.New("trailing JSON data")
	}
	rev, extended := doc["contractRevision"]
	if extended && rev != revision11 && rev != ContractRevision {
		return errors.New("unsupported contractRevision")
	}
	// Revision 1.1 keeps its strict field set, so a lineage there is an unknown field.
	shape := wireShape(extended, rev == ContractRevision)
	if err := shape.check(doc, extended); err != nil {
		return err
	}
	// OCI-only keys are forbidden by presence in Wasm reports, even when their
	// values would decode to an empty Go string.
	if doc["runtimeMode"] == "wasm" {
		for _, key := range []string{"revision", "materialized"} {
			if _, ok := doc[key]; ok {
				return errors.New("Wasm cannot carry OCI fields")
			}
		}
		if extended {
			if entries, ok := doc["verifiedAbsence"].([]any); ok {
				for _, entry := range entries {
					if _, ok := entry.(map[string]any)["revision"]; ok {
						return errors.New("Wasm absence cannot carry revision")
					}
				}
			}
		}
	}
	// Unknown extension fields in v1 are ignored, including their wire types.
	shape.canonicalValue(doc, !extended)
	canonical, err := json.Marshal(doc)
	if err != nil {
		return err
	}
	var r Report
	if err := json.Unmarshal(canonical, &r); err != nil {
		return err
	}
	return validate(r, extended)
}

// Validate checks a typed report by the same wire boundary as ValidateJSON.
func Validate(r Report) error {
	data, err := json.Marshal(r)
	if err != nil {
		return err
	}
	return ValidateJSON(data)
}
func blocksCompletion(typ string) bool {
	return slices.Contains([]string{UIDMismatch, ResourceVersionMismatch, OwnershipMismatch, MissingPreconditions, TerminationPending}, typ)
}
func validate(r Report, extended bool) error {
	switch r.RuntimeMode {
	case "oci":
		if r.Materialized == nil || len(r.Revisions) != 1 || r.Revision == "" || r.Revisions[0].Revision != r.Revision {
			return errors.New("OCI requires materialized and one matching revision")
		}
	case "wasm":
		if r.Revision != "" || r.Materialized != nil {
			return errors.New("Wasm cannot carry OCI fields")
		}
	default:
		return errors.New("unknown runtimeMode")
	}
	if !extended {
		return nil
	}
	if r.ContractRevision == ContractRevision && (r.ReporterLineage == "" || strings.ContainsAny(r.ReporterLineage, " \t\r\n")) {
		return errors.New("revision 1.2 requires a reporterLineage without whitespace")
	}
	absenceClaim := r.AbsenceVerified || len(r.VerifiedAbsence) != 0
	if absenceClaim {
		if len(r.OwnedObjects) != 0 || (r.Materialized != nil && *r.Materialized) || r.ReadyReplicas != 0 || r.ReadyCapacity != 0 {
			return errors.New("verified absence conflicts with live pool state")
		}
		for _, revision := range r.Revisions {
			if revision.Ready {
				return errors.New("verified absence conflicts with ready revision")
			}
		}
	}
	for _, c := range r.Conditions {
		reason, ok := ConditionReasons[c.Type]
		if !ok || reason != c.Reason {
			return errors.New("unknown condition type or reason")
		}
		if absenceClaim && blocksCompletion(c.Type) {
			return errors.New("condition blocks verified absence")
		}
		if c.ObjectRef != nil && (!slices.Contains(ObjectKinds, c.ObjectRef.Kind) || c.ObjectRef.Name == "" || c.ObjectRef.Name != r.PoolRef.Name || (c.ObjectRef.Kind == "Lease" && singleton(c.ObjectRef.Name))) {
			return errors.New("invalid objectRef")
		}
		if c.Type == TerminationPending {
			d := c.RemainingDescendants
			if d == nil || d.ReplicaSets < 0 || d.Pods < 0 || (d.ReplicaSets == 0 && d.Pods == 0) {
				return errors.New("TerminationPending requires remaining descendants")
			}
		} else if c.RemainingDescendants != nil {
			return errors.New("remainingDescendants requires TerminationPending")
		}
	}
	for _, o := range r.OwnedObjects {
		if !slices.Contains(ObjectKinds, o.Kind) || o.Namespace == "" || o.Name == "" || o.UID == "" || o.ResourceVersion == "" || o.Namespace != r.PoolRef.Namespace || o.Name != r.PoolRef.Name || (o.Kind == "Lease" && singleton(o.Name)) {
			return errors.New("incomplete owned object")
		}
	}
	if r.AbsenceVerified && len(r.VerifiedAbsence) == 0 {
		return errors.New("absenceVerified requires verifiedAbsence")
	}
	for _, a := range r.VerifiedAbsence {
		if a.Generation < 0 {
			return errors.New("negative absence generation")
		}
		if !readAtPattern.MatchString(a.ReadAt) {
			return errors.New("invalid readAt")
		}
		if _, err := time.Parse(time.RFC3339Nano, a.ReadAt); err != nil {
			return errors.New("invalid readAt")
		}
		if (r.RuntimeMode == "oci" && a.Revision != r.Revision) || (r.RuntimeMode == "wasm" && a.Revision != "") {
			return errors.New("absence revision differs from pool")
		}
		if !a.Absent.Deployments || !a.Absent.Services || !a.Absent.Leases || !a.Absent.ReplicaSets || !a.Absent.Pods {
			return errors.New("every scoped resource must be absent")
		}
	}
	return nil
}
