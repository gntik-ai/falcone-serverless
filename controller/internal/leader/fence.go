package leader

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"
)

// Clock is the time source of the election. Decisions use only Now differences, so a
// monotonic reading is enough; no wall-clock value ever becomes an epoch.
type Clock interface {
	Now() time.Time
	After(d time.Duration) <-chan time.Time
}

type realClock struct{}

func (realClock) Now() time.Time                         { return time.Now() }
func (realClock) After(d time.Duration) <-chan time.Time { return time.After(d) }

// RealClock is the process clock.
func RealClock() Clock { return realClock{} }

// Stamp is the fence every singleton write carries: the Lease lineage (metadata.uid),
// the epoch of the term and a sequence that starts at 1 in each epoch and never repeats
// within it. The receiver records all three and refuses a different lineage whatever
// its epoch, and a superseded epoch.
type Stamp struct {
	Lineage string
	Epoch   int64
	Seq     int64
}

// Sink is the receiver of fenced writes, such as the Falcone pool-status ingestion.
type Sink interface {
	Write(ctx context.Context, s Stamp, payload []byte) error
}

// FencedError is a refusal by the receiver's fence. It deliberately carries nothing:
// the controller never learns, adopts or persists an epoch or lineage the receiver
// might disclose, and it never retries with a higher epoch.
type FencedError struct{}

func (*FencedError) Error() string { return "write refused by the receiver's fence" }

var (
	// ErrNotLeader means no valid term exists, so nothing was written.
	ErrNotLeader = errors.New("not the leader")
	// ErrFenceRejected means the receiver refused a write of this process; all duty
	// writes have stopped for good until the operator resets the receiver and the
	// controller restarts.
	ErrFenceRejected = errors.New("fence rejected")
)

// Term is one term of leadership. Its context is cancelled when the gate closes: on
// lost renewal, lost leadership, the renew deadline passing, a refused write and
// shutdown.
type Term struct {
	Lineage string
	Epoch   int64

	clock  Clock
	ctx    context.Context
	cancel context.CancelFunc

	mu        sync.Mutex
	seq       int64
	dl        time.Time
	rejected  bool
	stopTimer chan struct{} // closed when the expiry timer is re-armed
}

// newTerm opens a term whose gate closes at deadline unless it is extended. A term whose
// deadline has already passed is born cancelled. The expiry timer is registered before
// newTerm returns, so the gate and the context close together under any clock.
func newTerm(parent context.Context, clock Clock, lineage string, epoch int64, deadline time.Time) *Term {
	ctx, cancel := context.WithCancel(parent)
	t := &Term{Lineage: lineage, Epoch: epoch, clock: clock, ctx: ctx, cancel: cancel, dl: deadline}
	t.mu.Lock()
	defer t.mu.Unlock()
	if !clock.Now().Before(deadline) {
		cancel()
		return t
	}
	t.armLocked()
	return t
}

// armLocked starts the timer that cancels the term's context at its deadline, so that
// duty work stops when the gate closes even if no Tick runs.
func (t *Term) armLocked() {
	if t.stopTimer != nil {
		close(t.stopTimer)
	}
	stop := make(chan struct{})
	t.stopTimer = stop
	expired := t.clock.After(t.dl.Sub(t.clock.Now()))
	go func() {
		select {
		case <-stop:
		case <-t.ctx.Done():
		case <-expired:
			t.expire(stop)
		}
	}()
}

func (t *Term) expire(stop chan struct{}) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.stopTimer == stop && !t.clock.Now().Before(t.dl) {
		t.cancel()
	}
}

// Context is cancelled when the term ends; duty work must stop on it.
func (t *Term) Context() context.Context { return t.ctx }

// Valid reports whether the gate is open: the term has not ended, no write was refused
// and the last successful renewal is younger than the renew deadline. The gate is best
// effort; the fence is what keeps a stale term from being accepted.
func (t *Term) Valid() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.validLocked()
}

func (t *Term) validLocked() bool {
	return t.ctx.Err() == nil && !t.rejected && t.clock.Now().Before(t.dl)
}

// Stamp returns the next fence for a write, or ErrNotLeader when the gate is closed.
func (t *Term) Stamp() (Stamp, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.validLocked() {
		return Stamp{}, ErrNotLeader
	}
	t.seq++
	return Stamp{Lineage: t.Lineage, Epoch: t.Epoch, Seq: t.seq}, nil
}

// extend moves the deadline after a successful renewal. A term whose gate has closed,
// whatever the reason, is never revived: a late success changes nothing.
func (t *Term) extend(deadline time.Time) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.validLocked() {
		return false
	}
	t.dl = deadline
	t.armLocked()
	return true
}

func (t *Term) deadline() time.Time {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.dl
}

func (t *Term) reject() {
	t.mu.Lock()
	t.rejected = true
	t.mu.Unlock()
	t.cancel()
}

// Publish stamps payload with the fence of term, the term that produced it, and sends it
// to sink. It never substitutes another term: a duty that outlives its term, even one
// the process has since re-acquired under a higher epoch, writes nothing. With no valid
// term it writes nothing. A refusal by the receiver (FencedError) stops the whole
// instance, whatever term is current: all duty writes stop and the FenceRejected status
// is set and reported on every later call. It is never retried.
func (e *Elector) Publish(ctx context.Context, term *Term, sink Sink, payload []byte) error {
	if _, stopped := e.current(); stopped {
		return ErrFenceRejected
	}
	if term == nil {
		return ErrNotLeader
	}
	stamp, err := term.Stamp()
	if err != nil {
		return err
	}
	if err := sink.Write(ctx, stamp, payload); err != nil {
		var fenced *FencedError
		if errors.As(err, &fenced) {
			e.reject(term)
			return fmt.Errorf("%w: %w", ErrFenceRejected, err)
		}
		return err
	}
	return nil
}
