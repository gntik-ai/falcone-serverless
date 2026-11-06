package poolstatus

import (
	"cmp"
	"encoding/json"
	"errors"
	"slices"
)

// Scope must come from authoritative desired state and verified bindings, never
// an observed Kubernetes object. All pool-scoped kinds use the expected pool name.
// OwnerAnnotation is the full expected tenant/profile[/revision] JSON annotation.
// This encoder is defense in depth, not a substitute for fresh ownership reads.
type Scope struct {
	PoolRef
	OwnerAnnotation string
}

// ObservedObject is an already ownership-verified, fresh-read object. The encoder
// additionally checks its name, binding and full owner annotation against Scope.
// These values are report data, never delete or write preconditions.
type ObservedObject struct {
	OwnedObject
	OwnerAnnotation string
}

// Options controls only this encoder; controller configuration and emission are
// separate work. Disabling extensions always clears the completion signal.
type Options struct{ DisableExtensions bool }

// Encode builds a deterministic report, discarding caller-supplied OwnedObjects
// and replacing objectRef names with the authoritative pool name. Observations
// from foreign objects and the controller's singleton Leases are never encoded.
// The caller must supply reviewed descendant-proof evidence before setting
// AbsenceVerified; no such proof or Kubernetes access is implemented here.
func Encode(r Report, scope Scope, observed []ObservedObject, options Options) ([]byte, error) {
	if scope.Namespace == "" || scope.Name == "" || scope.OwnerAnnotation == "" || singleton(scope.Name) {
		return nil, errors.New("authoritative pool scope is required")
	}
	r.PoolRef = scope.PoolRef
	r.OwnedObjects = nil
	r.Conditions = slices.Clone(r.Conditions)
	if r.Revisions == nil {
		r.Revisions = []Revision{}
	}
	if r.Conditions == nil {
		r.Conditions = []Condition{}
	}
	r.ContractRevision = ContractRevision
	blocked := false
	for i, c := range r.Conditions {
		// Legacy wire validation accepts future fields, but the encoder must never
		// turn untrusted condition text into a report, even in rollback mode.
		if reason, ok := ConditionReasons[c.Type]; !ok || reason != c.Reason {
			return nil, errors.New("unknown condition type or reason")
		}
		if blocksCompletion(c.Type) {
			blocked = true
		}
		if c.ObjectRef != nil {
			r.Conditions[i].ObjectRef = &ObjectRef{Kind: c.ObjectRef.Kind, Name: scope.Name}
		}
	}
	if !blocked {
		for _, o := range observed {
			if o.Namespace == scope.Namespace && o.Name == scope.Name && !singleton(o.Name) && o.OwnerAnnotation == scope.OwnerAnnotation {
				r.OwnedObjects = append(r.OwnedObjects, o.OwnedObject)
			}
		}
	}
	slices.SortFunc(r.OwnedObjects, func(a, b OwnedObject) int {
		if c := cmp.Compare(a.Kind, b.Kind); c != 0 {
			return c
		}
		if c := cmp.Compare(a.UID, b.UID); c != 0 {
			return c
		}
		return cmp.Compare(a.ResourceVersion, b.ResourceVersion)
	})
	if options.DisableExtensions {
		r.ContractRevision = ""
		r.OwnedObjects = nil
		r.VerifiedAbsence = nil
		r.AbsenceVerified = false
		conditions := make([]Condition, 0, len(r.Conditions))
		for _, c := range r.Conditions {
			if c.Type == MissingPreconditions || c.Type == TerminationPending {
				continue
			}
			c.ObjectRef = nil
			c.RemainingDescendants = nil
			conditions = append(conditions, c)
		}
		r.Conditions = conditions
	}
	if r.AbsenceVerified && len(r.OwnedObjects) != 0 {
		return nil, errors.New("verified absence conflicts with observed owned objects")
	}
	data, err := json.Marshal(r)
	if err != nil {
		return nil, err
	}
	if err := ValidateJSON(data); err != nil {
		return nil, err
	}
	return data, nil
}
func singleton(name string) bool {
	return name == "falcone-pool-controller-leader" || name == "falcone-pool-controller-scaler"
}
