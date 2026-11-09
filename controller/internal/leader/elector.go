package leader

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"sync"
	"time"
)

// Status reasons. Leading and Standby are healthy; every other reason is a failure the
// instance reports while it stays unready.
const (
	ReasonStarting          = "Starting"
	ReasonLeading           = "Leading"
	ReasonStandby           = "Standby"
	ReasonLeaseUnavailable  = "LeaseUnavailable"
	ReasonOwnershipMismatch = "OwnershipMismatch"
	ReasonEpochInvalid      = "EpochInvalid"
	ReasonLeadershipLost    = "LeadershipLost"
	ReasonFenceRejected     = "FenceRejected"
	ReasonStopped           = "Stopped"
)

// Status is what the instance reports about itself. Detail is built from fixed
// strings only: it never carries a token or a value observed on a foreign object.
type Status struct {
	Leading bool
	Ready   bool
	Reason  string
	Detail  string
	Epoch   int64
	Lineage string
}

// Config configures one instance. Identity is the pod name and Namespace the release
// namespace, both from the downward API.
type Config struct {
	Identity      string
	Namespace     string
	LeaseDuration time.Duration
	RenewDeadline time.Duration
	RetryPeriod   time.Duration
	Lease         LeaseClient
	Clock         Clock                            // RealClock when nil
	Logf          func(format string, args ...any) // discarded when nil
}

// Validate checks the identity, the namespace and the bounded timings: positive, with
// leaseDuration > renewDeadline > retryPeriod (ctr-controller-config).
func (c Config) Validate() error {
	if err := ValidateIdentity(c.Identity); err != nil {
		return err
	}
	if err := ValidateNamespace(c.Namespace); err != nil {
		return err
	}
	if c.RetryPeriod <= 0 || c.RenewDeadline <= c.RetryPeriod || c.LeaseDuration <= c.RenewDeadline {
		return errors.New("lease timings must satisfy leaseDuration > renewDeadline > retryPeriod > 0")
	}
	if c.Lease == nil {
		return errors.New("a Lease client is required")
	}
	return nil
}

// Duty is the singleton work of one term. ctx is cancelled when the gate closes, and
// every write must go through Elector.Publish with that term (or carry term.Stamp()).
type Duty func(ctx context.Context, term *Term)

// Elector campaigns for the leader Lease. Tick drives it one step at a time, which keeps
// it deterministic under a fake clock; Run is the loop around Tick for the process.
type Elector struct {
	cfg Config

	mu      sync.Mutex // guards term, stopped, status and onTerm
	term    *Term
	stopped bool
	status  Status
	onTerm  func(*Term)

	// Touched only by the goroutine that calls Tick and Run.
	lease          *Lease // the Lease as last written by this instance while it leads
	observed       bool
	observedRV     string
	observedHolder string
	observedAt     time.Time
	lastLogged     string

	beforeTerm func() // test seam: runs between the deadline calculation and the term's context
}

// New validates cfg and returns an elector that has not contacted the API server.
func New(cfg Config) (*Elector, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	if cfg.Clock == nil {
		cfg.Clock = RealClock()
	}
	if cfg.Logf == nil {
		cfg.Logf = func(string, ...any) {}
	}
	return &Elector{cfg: cfg, status: Status{Reason: ReasonStarting}}, nil
}

// Status returns the current status.
func (e *Elector) Status() Status {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.status
}

// Term returns the current term while its gate is open, and nil otherwise.
func (e *Elector) Term() *Term {
	term, _ := e.current()
	if term == nil || !term.Valid() {
		return nil
	}
	return term
}

func (e *Elector) current() (term *Term, stopped bool) {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.term, e.stopped
}

// setStatus records the status unless a refused write has stopped the instance: that
// reason is kept for the life of the process.
func (e *Elector) setStatus(s Status) {
	e.mu.Lock()
	if e.stopped {
		e.mu.Unlock()
		return
	}
	s.Ready = s.Reason == ReasonLeading || s.Reason == ReasonStandby
	s.Leading = s.Reason == ReasonLeading
	e.status = s
	e.mu.Unlock()
	if key := s.Reason + ": " + s.Detail; key != e.lastLogged {
		e.lastLogged = key
		e.cfg.Logf("leader election: %s", key)
	}
}

func (e *Elector) fail(reason string, err error) time.Duration {
	e.setStatus(Status{Reason: reason, Detail: err.Error()})
	return e.cfg.RetryPeriod
}

// reject stops the instance after the receiver refused a write: every term ends, all
// duty writes stop and the FenceRejected status stays. The refused term may not be the
// current one (a refusal can arrive after the process re-acquired the Lease), so the
// current term is cancelled too, and an acquisition still in flight opens no term. The
// epoch is neither retried nor raised, and nothing is learned from the refusal. It may
// run on any goroutine, so it leaves the Tick-owned state to Tick.
func (e *Elector) reject(term *Term) {
	e.mu.Lock()
	e.stopped = true
	current := e.term
	e.term = nil
	e.status = Status{Reason: ReasonFenceRejected, Detail: "the receiver refused a write of this process; reset the receiver, then restart the controller"}
	e.mu.Unlock()
	term.reject()
	if current != nil && current != term {
		current.reject()
	}
	e.cfg.Logf("leader election: %s: write refused by the receiver's fence, all duty writes stopped", ReasonFenceRejected)
}

// endTerm closes the gate and cancels the duty work. Tick goroutine only.
func (e *Elector) endTerm(term *Term, reason, detail string) {
	e.mu.Lock()
	if e.term == term {
		e.term = nil
	}
	e.mu.Unlock()
	term.cancel()
	e.lease = nil
	e.setStatus(Status{Reason: reason, Detail: detail})
}

// Tick performs one step: renew when leading, otherwise campaign. It returns how long
// to wait before the next step.
func (e *Elector) Tick(ctx context.Context) time.Duration {
	term, stopped := e.current()
	if stopped {
		e.releaseHeld()
		return e.cfg.RetryPeriod
	}
	if term != nil {
		return e.renew(ctx, term)
	}
	e.lease = nil
	return e.campaign(ctx)
}

func (e *Elector) renew(ctx context.Context, term *Term) time.Duration {
	clock := e.cfg.Clock
	start := clock.Now()
	if !term.Valid() {
		e.endTerm(term, ReasonLeadershipLost, "the renew deadline passed")
		return e.cfg.RetryPeriod
	}
	next := e.lease.Clone()
	next.RenewTime = formatMicroTime(start)
	updated, err := e.cfg.Lease.Update(ctx, next)
	switch {
	case err == nil:
		if e.ownsTerm(updated, term) != nil {
			e.endTerm(term, ReasonLeadershipLost, "the Lease no longer belongs to this term")
			return e.cfg.RetryPeriod
		}
		if !term.extend(start.Add(e.cfg.RenewDeadline)) {
			// The renewal landed after the gate closed (a slow call): the term is not
			// revived. This instance holds the Lease, so it hands it back.
			e.endTerm(term, ReasonLeadershipLost, "the renew deadline passed before the renewal returned")
			e.release(updated)
			return e.cfg.RetryPeriod
		}
		e.lease = updated
		return e.cfg.RetryPeriod
	case errors.Is(err, ErrConflict):
		e.endTerm(term, ReasonLeadershipLost, "the Lease was changed by another writer")
		return e.cfg.RetryPeriod
	case errors.Is(err, ErrNotFound):
		e.endTerm(term, ReasonLeadershipLost, "the Lease was deleted")
		return e.cfg.RetryPeriod
	}
	// The API is unreachable or slow: keep the gate until the renew deadline, then step down.
	now := clock.Now()
	if !now.Before(term.deadline()) {
		e.endTerm(term, ReasonLeadershipLost, "renewal failed until the renew deadline")
		return e.cfg.RetryPeriod
	}
	return min(e.cfg.RetryPeriod, term.deadline().Sub(now))
}

// ownsTerm checks that a Lease returned by a write still carries this term.
func (e *Elector) ownsTerm(l *Lease, term *Term) error {
	epoch, err := EpochOf(l)
	if err != nil {
		return err
	}
	if l.UID != term.Lineage || l.HolderIdentity != e.cfg.Identity || epoch != term.Epoch {
		return errors.New("lease does not carry this term")
	}
	return VerifyOwnership(l, e.cfg.Namespace)
}

func (e *Elector) campaign(ctx context.Context) time.Duration {
	clock := e.cfg.Clock
	l, err := e.cfg.Lease.Get(ctx)
	if errors.Is(err, ErrNotFound) {
		e.observed = false
		return e.bootstrap(ctx)
	}
	if err != nil {
		return e.fail(ReasonLeaseUnavailable, err)
	}
	if err := VerifyOwnership(l, e.cfg.Namespace); err != nil {
		return e.fail(ReasonOwnershipMismatch, err)
	}
	epoch, err := EpochOf(l)
	if err != nil {
		return e.fail(ReasonEpochInvalid, err)
	}
	now := clock.Now()
	if !e.observed || e.observedRV != l.ResourceVersion || e.observedHolder != l.HolderIdentity {
		e.observed, e.observedRV, e.observedHolder, e.observedAt = true, l.ResourceVersion, l.HolderIdentity, now
	}
	if l.HolderIdentity != "" && now.Sub(e.observedAt) < e.cfg.LeaseDuration {
		e.setStatus(Status{Reason: ReasonStandby, Detail: "another instance holds the Lease"})
		// Wake no later than the end of the observation window, so a takeover is never
		// a whole retry period late when the observation is off the retry grid.
		return min(e.cfg.RetryPeriod, e.observedAt.Add(e.cfg.LeaseDuration).Sub(now))
	}
	// The Lease is released or has not been renewed for a lease duration as seen by this
	// instance's own clock. Take over with the next epoch, conditioned on the
	// resourceVersion just read, so a concurrent writer wins instead of this instance.
	nextEpoch, err := nextEpoch(epoch)
	if err != nil {
		return e.fail(ReasonEpochInvalid, err)
	}
	start := clock.Now()
	taken := l.Clone()
	taken.HolderIdentity = e.cfg.Identity
	taken.LeaseDurationSeconds = durationSeconds(e.cfg.LeaseDuration)
	taken.AcquireTime, taken.RenewTime = formatMicroTime(start), formatMicroTime(start)
	taken.LeaseTransitions++
	taken.Annotations[EpochAnnotation] = strconv.FormatInt(nextEpoch, 10)
	updated, err := e.cfg.Lease.Update(ctx, taken)
	switch {
	case err == nil:
		return e.lead(ctx, updated, nextEpoch, start)
	case errors.Is(err, ErrConflict), errors.Is(err, ErrNotFound):
		e.setStatus(Status{Reason: ReasonStandby, Detail: "another writer changed the Lease first"})
		return e.cfg.RetryPeriod
	}
	// An ambiguous failure may or may not have landed. Either way this instance does not
	// lead: it only leads on a successful answer, and the next Get decides what to do.
	return e.fail(ReasonLeaseUnavailable, err)
}

// bootstrap creates the Lease at epoch 1 when none exists. Fencing, not this step, keeps
// a Lease an operator deleted by hand from publishing: the receiver holds the old
// lineage and refuses the new one until the operator resets it.
func (e *Elector) bootstrap(ctx context.Context) time.Duration {
	start := e.cfg.Clock.Now()
	created, err := e.cfg.Lease.Create(ctx, newLease(e.cfg.Namespace, e.cfg.Identity, e.cfg.LeaseDuration, start))
	switch {
	case err == nil:
		return e.lead(ctx, created, FirstEpoch, start)
	case errors.Is(err, ErrAlreadyExists):
		e.setStatus(Status{Reason: ReasonStandby, Detail: "another instance created the Lease first"})
		return e.cfg.RetryPeriod
	}
	return e.fail(ReasonLeaseUnavailable, err)
}

// lead opens the gate for a term whose Lease write succeeded. The Lease must carry the
// term, including a UID: without a lineage there is nothing to fence with. The deadline
// counts from start, taken before the write was sent, so a slow or delayed answer
// shortens the term instead of extending it; one that arrives after the deadline opens
// no term at all.
func (e *Elector) lead(ctx context.Context, l *Lease, epoch int64, start time.Time) time.Duration {
	if l.UID == "" {
		return e.fail(ReasonLeaseUnavailable, errors.New("the stored Lease has no UID"))
	}
	probe := &Term{Lineage: l.UID, Epoch: epoch}
	if err := e.ownsTerm(l, probe); err != nil {
		return e.fail(ReasonLeaseUnavailable, err)
	}
	deadline := start.Add(e.cfg.RenewDeadline)
	if e.beforeTerm != nil {
		e.beforeTerm()
	}
	term := newTerm(ctx, e.cfg.Clock, l.UID, epoch, deadline)
	e.lease = l
	e.mu.Lock()
	if e.stopped {
		// A refused write stopped the instance while this acquisition was in flight.
		e.mu.Unlock()
		term.cancel()
		return e.cfg.RetryPeriod
	}
	if !term.Valid() {
		e.mu.Unlock()
		term.cancel()
		e.releaseHeld()
		return e.fail(ReasonLeaseUnavailable, errors.New("the renew deadline passed before the term started"))
	}
	e.term = term
	onTerm := e.onTerm
	e.mu.Unlock()
	e.setStatus(Status{Reason: ReasonLeading, Detail: "holds the Lease", Epoch: epoch, Lineage: l.UID})
	if onTerm != nil {
		onTerm(term)
	}
	return e.cfg.RetryPeriod
}

// release hands the Lease back so a standby takes over without waiting for expiry. The
// epoch annotation stays, so the next term still counts up from it. The update is
// conditioned on the resourceVersion this instance last wrote, so it never releases a
// Lease someone else has since taken.
func (e *Elector) release(l *Lease) {
	rel := l.Clone()
	rel.HolderIdentity = ""
	rel.LeaseDurationSeconds = 1
	rel.RenewTime = formatMicroTime(e.cfg.Clock.Now())
	if _, err := e.cfg.Lease.Update(context.Background(), rel); err != nil && !errors.Is(err, ErrConflict) && !errors.Is(err, ErrNotFound) {
		e.cfg.Logf("leader election: the Lease could not be released: %v", err)
	}
}

func (e *Elector) releaseHeld() {
	if e.lease != nil {
		l := e.lease
		e.lease = nil
		e.release(l)
	}
}

// Run campaigns until ctx is cancelled, starting duty in its own goroutine for every
// term. On cancellation (SIGTERM) it stops the duties first, waits for them for at most
// the renew deadline, and then releases the Lease.
func (e *Elector) Run(ctx context.Context, duty Duty) error {
	var duties sync.WaitGroup
	e.mu.Lock()
	e.onTerm = func(t *Term) {
		duties.Add(1)
		go func() {
			defer duties.Done()
			duty(t.Context(), t)
		}()
	}
	e.mu.Unlock()
	for ctx.Err() == nil {
		wait := e.Tick(ctx)
		select {
		case <-ctx.Done():
		case <-e.cfg.Clock.After(wait):
		}
	}
	e.shutdown(&duties)
	return nil
}

func (e *Elector) shutdown(duties *sync.WaitGroup) {
	lease := e.lease
	if term, _ := e.current(); term != nil {
		e.endTerm(term, ReasonStopped, "shutting down")
	}
	done := make(chan struct{})
	go func() { duties.Wait(); close(done) }()
	select {
	case <-done:
	case <-e.cfg.Clock.After(e.cfg.RenewDeadline):
		e.cfg.Logf("leader election: duty work did not stop within %s", e.cfg.RenewDeadline)
	}
	if lease != nil {
		e.lease = nil
		e.release(lease)
	}
	e.setStatus(Status{Reason: ReasonStopped, Detail: fmt.Sprintf("%s released the Lease", e.cfg.Identity)})
}
