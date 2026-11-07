package leader

import (
	"context"
	"errors"
	"sync"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

// Reasons a write or a publication is refused. Each is a degraded state that
// docs/operations/degraded-behavior.md describes.
var (
	ErrNotLeader               = errors.New("not the leader: the leadership gate is closed")
	ErrDesiredStateUnavailable = errors.New("desired state is unavailable")
	ErrDesiredStateStale       = errors.New("desired state is stale")
)

// DefaultMaxDesiredStateAge is how old Falcone desired state may be before writes
// stop. The value is the contract of this package; a duty may require less.
const DefaultMaxDesiredStateAge = 60 * time.Second

// Leadership is the source of a fencing token: a *Gate for one term, or the
// *Elector for whichever term is current.
type Leadership interface {
	Token() (Token, bool)
}

// DesiredState is what a duty knows about the freshness of Falcone's desired state.
type DesiredState struct {
	Available  bool
	ObservedAt time.Time // local clock reading of the last successful read
}

// Admission admits a write only with an open gate and fresh desired state.
type Admission struct {
	Leadership         Leadership
	Clock              Clock
	MaxDesiredStateAge time.Duration // DefaultMaxDesiredStateAge when zero
}

// Admit returns the fencing token under which a write may proceed, or the reason
// it may not. It never admits on stale or unavailable data, so a degraded
// controller changes no replica count and deletes nothing.
func (a Admission) Admit(ds DesiredState) (Token, error) {
	if a.Leadership == nil || a.Clock == nil {
		return Token{}, ErrNotLeader
	}
	tok, ok := a.Leadership.Token()
	if !ok {
		return Token{}, ErrNotLeader
	}
	if !ds.Available || ds.ObservedAt.IsZero() {
		return Token{}, ErrDesiredStateUnavailable
	}
	limit := a.MaxDesiredStateAge
	if limit <= 0 {
		limit = DefaultMaxDesiredStateAge
	}
	if a.Clock.Now().Sub(ds.ObservedAt) > limit {
		return Token{}, ErrDesiredStateStale
	}
	return tok, nil
}

// Object names one Kubernetes object a Writer touches.
type Object struct {
	Kind      string
	Namespace string
	Name      string
}

// Writer is the Kubernetes write boundary of the duties (DEP-03 implements it).
type Writer interface {
	Apply(ctx context.Context, obj Object) error
	Delete(ctx context.Context, obj Object) error
}

// GuardedWriter forwards a write to Inner only when Admission admits it, checked
// again on every call.
type GuardedWriter struct {
	Admission Admission
	Desired   func() DesiredState
	Inner     Writer
}

func (w GuardedWriter) admit() error {
	if w.Desired == nil || w.Inner == nil {
		return ErrDesiredStateUnavailable
	}
	_, err := w.Admission.Admit(w.Desired())
	return err
}

// Apply implements Writer.
func (w GuardedWriter) Apply(ctx context.Context, obj Object) error {
	if err := w.admit(); err != nil {
		return err
	}
	return w.Inner.Apply(ctx, obj)
}

// Delete implements Writer.
func (w GuardedWriter) Delete(ctx context.Context, obj Object) error {
	if err := w.admit(); err != nil {
		return err
	}
	return w.Inner.Delete(ctx, obj)
}

// Sink receives an encoded ctr-pool-status-report. DEP-03 implements it with the
// Falcone status publisher.
type Sink interface {
	Publish(ctx context.Context, payload []byte) error
}

// Reporter publishes pool-status reports for the leader only. The reporterEpoch of
// every report is the leadership token's epoch, and observationSeq restarts when
// the epoch rises and never repeats within an epoch.
type Reporter struct {
	Leadership Leadership
	Sink       Sink

	mu    sync.Mutex
	epoch int64
	seq   int64
}

// Publish encodes and sends one observation. Without an open gate it sends
// nothing and returns ErrNotLeader. Anything the caller put in ReporterEpoch or
// ObservationSeq is replaced.
func (r *Reporter) Publish(ctx context.Context, rep poolstatus.Report, scope poolstatus.Scope, observed []poolstatus.ObservedObject, options poolstatus.Options) error {
	if r.Leadership == nil || r.Sink == nil {
		return ErrNotLeader
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	tok, ok := r.Leadership.Token()
	if !ok {
		return ErrNotLeader
	}
	seq := r.seq + 1
	if tok.Epoch != r.epoch || r.seq == 0 {
		seq = 1
	}
	rep.ReporterEpoch, rep.ObservationSeq = tok.Epoch, seq
	payload, err := poolstatus.Encode(rep, scope, observed, options)
	if err != nil {
		return err
	}
	if again, ok := r.Leadership.Token(); !ok || again != tok {
		return ErrNotLeader // leadership changed while encoding
	}
	if err := r.Sink.Publish(ctx, payload); err != nil {
		return err
	}
	r.epoch, r.seq = tok.Epoch, seq
	return nil
}
