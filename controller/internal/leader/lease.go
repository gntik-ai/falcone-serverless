// Package leader elects one controller replica to run the singleton duties and
// fences every write that duty makes.
//
// Correctness rests on fencing, not on gate timing. Every term of leadership has an
// epoch, strictly greater than every earlier one of the same Lease, and every
// fenced write or report carries (lineage, epoch, seq): the Lease's metadata.uid,
// the epoch and a per-epoch sequence. The receiver (ctr-pool-status-report) keeps
// the highest it has seen, and refuses a superseded epoch and any other lineage. The
// lease, renew and retry timings only bound how long two replicas can both believe
// they lead; they never guarantee that they do not.
package leader

import (
	"context"
	"encoding/json"
	"errors"
	"maps"
	"math"
	"regexp"
	"strconv"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
)

const (
	// LeaseName is the fixed leader-election Lease (ctr-resource-ownership).
	LeaseName = preflight.LeaderLeaseName

	// DutyName is the duty segment of the owner annotation of the leader Lease.
	DutyName = "leader"

	OwnerLabel        = "falcone.ai/owner"
	OwnerValue        = "pool-controller"
	OwnerAnnotation   = "falcone.ai/owner"
	VersionAnnotation = "falcone.ai/ownership-version"
	OwnershipVersion  = "1"

	// EpochAnnotation records the epoch of the latest term. A new term writes it
	// plus one with optimistic concurrency, so it survives handover and restart.
	EpochAnnotation = "falcone.ai/leader-epoch"

	// FirstEpoch is the epoch a Lease created by the controller starts with.
	FirstEpoch int64 = 1
)

// Defaults are the bounded timings of ctr-controller-config.
const (
	DefaultLeaseDuration = 15 * time.Second
	DefaultRenewDeadline = 10 * time.Second
	DefaultRetryPeriod   = 2 * time.Second
)

var (
	// ErrNotFound is a 404 from the API server.
	ErrNotFound = errors.New("lease not found")
	// ErrAlreadyExists is a 409 on create.
	ErrAlreadyExists = errors.New("lease already exists")
	// ErrConflict is a 409 on update: the resourceVersion is stale.
	ErrConflict = errors.New("lease changed since it was read")
)

var (
	dns1123Label     = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]{0,61}[a-z0-9])?$`)
	dns1123Subdomain = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$`)
	epochPattern     = regexp.MustCompile(`^[1-9][0-9]{0,18}$`)
)

// ValidateNamespace accepts a DNS-1123 label.
func ValidateNamespace(namespace string) error {
	if !dns1123Label.MatchString(namespace) {
		return errors.New("namespace is not a DNS-1123 label")
	}
	return nil
}

// ValidateIdentity accepts a pod name: a DNS-1123 subdomain of at most 253 characters.
func ValidateIdentity(identity string) error {
	if len(identity) > 253 || !dns1123Subdomain.MatchString(identity) {
		return errors.New("identity is not a DNS-1123 subdomain")
	}
	return nil
}

// Lease is the part of a coordination.k8s.io/v1 Lease the election reads and writes.
type Lease struct {
	Name            string
	Namespace       string
	UID             string
	ResourceVersion string
	Labels          map[string]string
	Annotations     map[string]string

	HolderIdentity       string
	LeaseDurationSeconds int32
	AcquireTime          string // informational only; never used to decide anything
	RenewTime            string // informational only; never used to decide anything
	LeaseTransitions     int32
}

// Clone returns a copy that shares no map with l.
func (l *Lease) Clone() *Lease {
	c := *l
	c.Labels = maps.Clone(l.Labels)
	c.Annotations = maps.Clone(l.Annotations)
	return &c
}

// LeaseClient reads and writes the one leader Lease in the release namespace. The
// namespace and name are fixed when the client is built; no call can name another.
type LeaseClient interface {
	// Get returns ErrNotFound when the Lease is absent.
	Get(ctx context.Context) (*Lease, error)
	// Create stores l and returns the stored Lease, including its UID. It returns
	// ErrAlreadyExists when the Lease exists.
	Create(ctx context.Context, l *Lease) (*Lease, error)
	// Update writes the holder, durations, times, transitions and epoch annotation
	// of l conditioned on l.ResourceVersion, and returns the stored Lease. It
	// returns ErrConflict on a stale resourceVersion and ErrNotFound when the
	// Lease is gone.
	Update(ctx context.Context, l *Lease) (*Lease, error)
}

// OwnershipError says which part of the singleton ownership identity failed. It never
// echoes a value from the observed object.
type OwnershipError struct{ Reason string }

func (e *OwnershipError) Error() string { return "lease ownership check failed: " + e.Reason }

// ExpectedOwner is the owner annotation of the leader Lease in namespace.
func ExpectedOwner(namespace string) string {
	data, _ := json.Marshal([]string{namespace, DutyName})
	return string(data)
}

// VerifyOwnership checks the full singleton ownership identity of l: the name and
// namespace, the owner label, the ownership version and the owner annotation, a JSON
// array naming the release namespace and the leader duty. A Lease that fails is never
// taken over or updated.
func VerifyOwnership(l *Lease, namespace string) error {
	fail := func(reason string) error { return &OwnershipError{Reason: reason} }
	if l.Name != LeaseName {
		return fail("name is not the fixed leader Lease name")
	}
	if l.Namespace != "" && l.Namespace != namespace {
		return fail("namespace is not the release namespace")
	}
	if v, ok := l.Labels[OwnerLabel]; !ok || v != OwnerValue {
		return fail("label " + OwnerLabel + " is missing or not " + OwnerValue)
	}
	if v, ok := l.Annotations[VersionAnnotation]; !ok || v != OwnershipVersion {
		return fail("annotation " + VersionAnnotation + " is missing or not " + OwnershipVersion)
	}
	raw, ok := l.Annotations[OwnerAnnotation]
	if !ok {
		return fail("annotation " + OwnerAnnotation + " is missing")
	}
	var owner []string
	if err := json.Unmarshal([]byte(raw), &owner); err != nil {
		return fail("annotation " + OwnerAnnotation + " is not a JSON array of strings")
	}
	if len(owner) != 2 || owner[0] != namespace || owner[1] != DutyName {
		return fail("annotation " + OwnerAnnotation + " does not name this namespace and the leader duty")
	}
	return nil
}

// EpochError is a missing or malformed epoch annotation, or an exhausted epoch.
type EpochError struct{ Reason string }

func (e *EpochError) Error() string { return "lease epoch is unusable: " + e.Reason }

// EpochOf returns the epoch recorded on l. The annotation must be a positive base-10
// integer without sign, leading zeros or spaces; anything else is an error and the
// Lease must not be modified.
func EpochOf(l *Lease) (int64, error) {
	raw, ok := l.Annotations[EpochAnnotation]
	if !ok {
		return 0, &EpochError{Reason: "annotation " + EpochAnnotation + " is missing"}
	}
	if !epochPattern.MatchString(raw) {
		return 0, &EpochError{Reason: "annotation " + EpochAnnotation + " is not a positive integer"}
	}
	epoch, err := strconv.ParseInt(raw, 10, 64)
	if err != nil {
		return 0, &EpochError{Reason: "annotation " + EpochAnnotation + " is out of range"}
	}
	return epoch, nil
}

// nextEpoch is the epoch of the term that follows epoch.
func nextEpoch(epoch int64) (int64, error) {
	if epoch == math.MaxInt64 {
		return 0, &EpochError{Reason: "epoch is exhausted"}
	}
	return epoch + 1, nil
}

// newLease is the Lease the controller creates when none exists: epoch 1, held by
// identity, with the full ownership metadata.
func newLease(namespace, identity string, leaseDuration time.Duration, now time.Time) *Lease {
	stamp := formatMicroTime(now)
	return &Lease{
		Name:      LeaseName,
		Namespace: namespace,
		Labels:    map[string]string{OwnerLabel: OwnerValue},
		Annotations: map[string]string{
			VersionAnnotation: OwnershipVersion,
			OwnerAnnotation:   ExpectedOwner(namespace),
			EpochAnnotation:   strconv.FormatInt(FirstEpoch, 10),
		},
		HolderIdentity:       identity,
		LeaseDurationSeconds: durationSeconds(leaseDuration),
		AcquireTime:          stamp,
		RenewTime:            stamp,
	}
}

func durationSeconds(d time.Duration) int32 {
	seconds := int32((d + time.Second - 1) / time.Second)
	if seconds < 1 {
		seconds = 1
	}
	return seconds
}

// formatMicroTime is the metav1.MicroTime wire format.
func formatMicroTime(t time.Time) string {
	return t.UTC().Format("2006-01-02T15:04:05.000000Z07:00")
}
