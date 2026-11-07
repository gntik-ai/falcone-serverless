package leader

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"regexp"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// MaxLeaseDuration is the finite ceiling for a lease duration. A longer value would
// stall every takeover and every recovery barrier for longer than an operator can
// reasonably wait, so it fails closed.
const MaxLeaseDuration = 5 * time.Minute

// Timings are the lease, renew and retry durations (ctr-controller-config:
// leaseDurationSeconds 15, renewDeadlineSeconds 10, retryPeriodSeconds 2).
type Timings struct {
	LeaseDuration time.Duration
	RenewDeadline time.Duration
	RetryPeriod   time.Duration
}

// DefaultTimings are the contract defaults.
var DefaultTimings = Timings{LeaseDuration: 15 * time.Second, RenewDeadline: 10 * time.Second, RetryPeriod: 2 * time.Second}

// Validate fails closed: every duration is positive, lease > renew > retry, the
// lease duration is a whole number of seconds (the Lease records seconds) and does
// not exceed MaxLeaseDuration.
func (t Timings) Validate() error {
	switch {
	case t.LeaseDuration <= 0 || t.RenewDeadline <= 0 || t.RetryPeriod <= 0:
		return errors.New("lease, renew and retry durations must all be positive")
	case t.LeaseDuration <= t.RenewDeadline || t.RenewDeadline <= t.RetryPeriod:
		return errors.New("lease duration must exceed the renew deadline, which must exceed the retry period")
	case t.LeaseDuration > MaxLeaseDuration:
		return fmt.Errorf("lease duration exceeds the %v ceiling", MaxLeaseDuration)
	case t.LeaseDuration%time.Second != 0:
		return errors.New("lease duration must be a whole number of seconds")
	}
	return nil
}

// RequestTimeout bounds each Lease request below the retry period.
func (t Timings) RequestTimeout() time.Duration { return t.RetryPeriod / 2 }

// DrainBound is how long in-flight duty work may run after the gate closes. It is
// the margin between the lease duration and the renew deadline: a successor cannot
// acquire before the lease duration passed, and the gate closed at the deadline.
func (t Timings) DrainBound() time.Duration { return t.LeaseDuration - t.RenewDeadline }

// Clock is the time source. Now must carry a monotonic reading (time.Now does).
type Clock interface {
	Now() time.Time
	After(d time.Duration) <-chan time.Time
}

// RealClock is the system clock.
type RealClock struct{}

// Now implements Clock.
func (RealClock) Now() time.Time { return time.Now() }

// After implements Clock.
func (RealClock) After(d time.Duration) <-chan time.Time { return time.After(d) }

// Token is the leadership fencing token: Epoch is the Lease's leaseTransitions, the
// reporterEpoch of ctr-pool-status-report.
type Token struct {
	Epoch  int64
	Holder string
}

// Gate is the leadership gate of one term. It is open only while the term is held
// and the current time is before the last successful renewal request's start plus
// renewDeadline. It is evaluated at every check, so a hung renewal cannot extend
// it. A closed gate never reopens: a later term gets a new Gate.
type Gate struct {
	clock    Clock
	mu       sync.Mutex
	closed   bool
	deadline time.Time
	token    Token
}

// Token returns the fencing token when the gate is open.
func (g *Gate) Token() (Token, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.closed || !g.clock.Now().Before(g.deadline) {
		return Token{}, false
	}
	return g.token, true
}

// Open reports whether the gate is open now.
func (g *Gate) Open() bool { _, ok := g.Token(); return ok }

// extend moves the deadline forward and reports whether the gate was still open to
// be extended. An expired gate is never reopened: a renewal that completes after the
// old deadline cannot undo a closure that duties and standbys may already rely on.
func (g *Gate) extend(deadline time.Time) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.closed || !g.clock.Now().Before(g.deadline) {
		return false
	}
	if deadline.After(g.deadline) {
		g.deadline = deadline
	}
	return true
}

func (g *Gate) close() {
	g.mu.Lock()
	g.closed = true
	g.mu.Unlock()
}

func (g *Gate) remaining() time.Duration {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.deadline.Sub(g.clock.Now())
}

// Duty is singleton work. It runs only while its gate is open, must stop when ctx
// ends, and must check the gate before every write (see Admission). Its in-flight
// work must finish within Timings.DrainBound after ctx ends.
type Duty func(ctx context.Context, g *Gate)

// Config configures an Elector.
type Config struct {
	// Namespace is the release namespace the Client is bound to. It is used only to
	// write the Lease's owner annotation; the Client alone decides where requests go.
	Namespace string
	Identity  string // unique per process incarnation; see NewIdentity
	Timings   Timings
	Client    Client
	Clock     Clock
	Duties    []Duty
	Logf      func(format string, args ...any) // optional; never receives credentials
}

// NewIdentity builds a holder identity from POD_NAME and a per-process random
// suffix, so a restarted container never reuses a previous incarnation's hold.
func NewIdentity(podName string, random io.Reader) (string, error) {
	if !validPodName(podName) {
		return "", errors.New("POD_NAME is absent or not a valid pod name")
	}
	var b [8]byte
	if _, err := io.ReadFull(random, b[:]); err != nil {
		return "", errors.New("no randomness for the holder identity")
	}
	return podName + "_" + hex.EncodeToString(b[:]), nil
}

var dnsLabelPattern = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// validPodName accepts a DNS subdomain: at most 253 characters of dot-separated
// labels, each a DNS label of at most 63 characters (so a..b and a.-b are refused).
func validPodName(name string) bool {
	if name == "" || len(name) > 253 {
		return false
	}
	for _, label := range strings.Split(name, ".") {
		if len(label) > 63 || !dnsLabelPattern.MatchString(label) {
			return false
		}
	}
	return true
}

// Elector campaigns for, renews and releases the leader Lease.
type Elector struct {
	cfg    Config
	clock  Clock
	client Client
	logf   func(string, ...any)

	mu      sync.Mutex // serialises Tick and Shutdown
	stopped bool
	current atomic.Pointer[Gate]

	// Leader state.
	rec    Lease
	cancel context.CancelFunc
	wg     sync.WaitGroup

	// Candidate state: what this process has observed of the Lease.
	obs     Lease
	obsAt   time.Time // local reading when obs was first seen
	haveObs bool
	barrier barrier

	// History, kept across terms so a recreated Lease continues the fencing token.
	haveHistory    bool
	maxTransitions int64
	knownDuration  time.Duration
}

// barrier is the recovery barrier for lost Lease history. It starts at the first
// successful API response after a 404 the process cannot account for, and a
// failure restarts it from the next success.
type barrier struct {
	active  bool
	started bool
	at      time.Time
}

// New validates cfg and returns an Elector that has not contacted the API.
func New(cfg Config) (*Elector, error) {
	switch {
	case cfg.Identity == "":
		return nil, errors.New("leader election needs a holder identity")
	case cfg.Client == nil || cfg.Clock == nil:
		return nil, errors.New("leader election needs a Lease client and a clock")
	case !validNamespace(cfg.Namespace):
		return nil, errors.New("leader election needs the release namespace")
	}
	if err := cfg.Timings.Validate(); err != nil {
		return nil, err
	}
	logf := cfg.Logf
	if logf == nil {
		logf = func(string, ...any) {}
	}
	return &Elector{cfg: cfg, clock: cfg.Clock, client: cfg.Client, logf: logf}, nil
}

// Token returns the current term's fencing token when the gate is open.
func (e *Elector) Token() (Token, bool) {
	if g := e.current.Load(); g != nil {
		return g.Token()
	}
	return Token{}, false
}

// Run campaigns until ctx ends, then shuts down gracefully. Ticks start one
// retry period apart, start to start: the time a tick took is part of that period
// and never added to it, so slow requests cannot stretch the polling interval
// that the takeover bound (leaseDuration + 2 x retryPeriod) is built on. A
// candidate whose observation becomes eligible sooner wakes exactly then, and
// takes over without a further read (see campaign), so neither the poll phase
// nor a read's latency is added to the wait after the observation.
func (e *Elector) Run(ctx context.Context) {
	for ctx.Err() == nil {
		started := e.clock.Now()
		e.Tick(ctx)
		wait := max(e.cfg.Timings.RetryPeriod-e.clock.Now().Sub(started), 0)
		if g := e.current.Load(); g != nil {
			if rem := g.remaining(); rem < wait {
				wait = max(rem, 0)
			}
		} else if until, ok := e.untilEligible(); ok && until > 0 && until < wait {
			wait = until
		}
		select {
		case <-ctx.Done():
		case <-e.clock.After(wait):
		}
	}
	e.Shutdown()
}

// Tick runs one election step at the current time: the holder renews, a candidate
// observes the Lease and acquires it when that is safe. It is the unit tests drive.
func (e *Elector) Tick(ctx context.Context) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.stopped {
		return
	}
	if g := e.current.Load(); g != nil {
		if g.Open() {
			e.renew(ctx, g)
		} else {
			e.stepDown("the renew deadline passed")
		}
		return
	}
	e.campaign(ctx)
}

// eligible reports whether the stored observation of an owned Lease has stood for
// long enough, on the local clock, to be taken over. The caller holds e.mu.
func (e *Elector) eligible(now time.Time) (time.Duration, bool) {
	if !e.haveObs || !owned(e.obs) || e.obs.Holder == "" {
		return 0, false
	}
	return e.wait(time.Duration(e.obs.LeaseDurationSeconds)*time.Second) - now.Sub(e.obsAt), true
}

// untilEligible is how long until a candidate's observation may be taken over.
func (e *Elector) untilEligible() (time.Duration, bool) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.stopped || e.current.Load() != nil {
		return 0, false
	}
	return e.eligible(e.clock.Now())
}

func (e *Elector) secs() int64 { return int64(e.cfg.Timings.LeaseDuration / time.Second) }

func (e *Elector) stamp(t time.Time) string { return t.UTC().Format(microTimeLayout) }

var errGateExpired = errors.New("the leadership gate expired while a request was pending")

// call runs one Lease request with a timeout read from the injected clock, so a
// hung request returns an error instead of holding the loop. A non-nil until ends
// the wait early with errGateExpired, so a holder acts on its gate deadline instead
// of waiting for the request timeout.
func (e *Elector) call(ctx context.Context, until <-chan time.Time, fn func(context.Context) (Lease, error)) (Lease, error) {
	cctx, cancel := context.WithCancel(ctx)
	defer cancel()
	type result struct {
		l   Lease
		err error
	}
	ch := make(chan result, 1)
	go func() { l, err := fn(cctx); ch <- result{l, err} }()
	select {
	case r := <-ch:
		return r.l, r.err
	case <-until:
		return Lease{}, errGateExpired
	case <-e.clock.After(e.cfg.Timings.RequestTimeout()):
		return Lease{}, errors.New("lease request timed out")
	case <-ctx.Done():
		return Lease{}, ctx.Err()
	}
}

func (e *Elector) renew(ctx context.Context, g *Gate) {
	start := e.clock.Now()
	rec := e.rec
	rec.RenewTime = e.stamp(start)
	// The gate deadline, not the request timeout, bounds the wait: the gate closes
	// and the duties are cancelled at the deadline even while the request is pending.
	got, err := e.call(ctx, e.clock.After(g.remaining()), func(c context.Context) (Lease, error) { return e.client.Update(c, rec) })
	switch {
	case err == nil && got.UID == e.rec.UID && got.Holder == e.cfg.Identity:
		e.rec = got
		if !g.extend(start.Add(e.cfg.Timings.RenewDeadline)) {
			e.stepDown("the renew deadline passed before the renewal completed")
			return
		}
	case err == nil:
		e.stepDown("another holder or Lease replaced ours")
		return
	case errors.Is(err, ErrNotFound), errors.Is(err, ErrConflict):
		e.stepDown("the Lease was deleted or changed by another writer")
		return
	}
	if !g.Open() {
		e.stepDown("the renew deadline passed")
	}
}

func (e *Elector) campaign(ctx context.Context) {
	// An observation that has already stood for the full wait is acted on without
	// another read: the update carries its resourceVersion, so a changed record
	// answers 409 and nothing is overwritten. This keeps the read's latency out of
	// the time from the observation to the takeover.
	if until, ok := e.eligible(e.clock.Now()); ok && until <= 0 {
		e.takeOver(ctx, e.obs)
		return
	}
	l, err := e.call(ctx, nil, func(c context.Context) (Lease, error) { return e.client.Get(c) })
	now := e.clock.Now()
	switch {
	case err == nil:
		e.observe(ctx, l, now)
	case errors.Is(err, ErrNotFound):
		e.absent(ctx, now)
	default:
		e.failure()
	}
}

// failure records an API failure: nothing is learned, continuity of observation is
// broken, and a running barrier restarts at the next successful response.
func (e *Elector) failure() {
	if e.barrier.active {
		e.barrier.started = false
	}
}

func (e *Elector) noteHistory(l Lease) {
	e.haveHistory = true
	e.maxTransitions = max(e.maxTransitions, l.LeaseTransitions)
	if l.LeaseDurationSeconds > 0 {
		e.knownDuration = time.Duration(l.LeaseDurationSeconds) * time.Second
	}
}

func owned(l Lease) bool {
	return l.Labels[OwnerKey] == OwnerValue && l.Annotations[VersionKey] == VersionValue
}

func sameRecord(a, b Lease) bool {
	return a.UID == b.UID && a.ResourceVersion == b.ResourceVersion && a.Holder == b.Holder && a.RenewTime == b.RenewTime
}

// wait is the greater of the local lease duration and the record's own positive
// leaseDurationSeconds.
func (e *Elector) wait(recorded time.Duration) time.Duration {
	return max(e.cfg.Timings.LeaseDuration, recorded)
}

func (e *Elector) observe(ctx context.Context, l Lease, now time.Time) {
	e.noteHistory(l)
	e.barrier = barrier{}
	if !e.haveObs || !sameRecord(e.obs, l) {
		e.obs, e.obsAt, e.haveObs = l, now, true
	}
	if !owned(l) {
		return // never take over a Lease without our ownership metadata
	}
	if l.Holder != "" && now.Sub(e.obsAt) < e.wait(time.Duration(l.LeaseDurationSeconds)*time.Second) {
		return
	}
	e.takeOver(ctx, l)
}

// absent handles a 404. Every 404 starts the recovery barrier, however closely the
// Lease was polled before: polling cannot show that nobody renewed between the last
// read and the deletion, and that renewal opens the holder's gate for up to
// renewDeadline after the Lease vanished. The holder's last possible renewal is
// no later than this first successful response, so waiting a full lease duration
// from it, measured on this process's own clock, outlasts every gate. A failure
// restarts the barrier from the next success (see failure); a Lease that reappears
// cancels it (see observe).
func (e *Elector) absent(ctx context.Context, now time.Time) {
	if !e.barrier.active {
		e.barrier = barrier{active: true}
		e.haveObs = false
		e.logf("leader: the Lease is absent; starting the recovery barrier")
	}
	if !e.barrier.started {
		e.barrier.started, e.barrier.at = true, now
	}
	if now.Sub(e.barrier.at) >= e.wait(e.knownDuration) {
		e.create(ctx)
	}
}

func (e *Elector) ownerAnnotation() string {
	b, _ := json.Marshal([]string{e.cfg.Namespace, LeaseName})
	return string(b)
}

func (e *Elector) create(ctx context.Context) {
	start := e.clock.Now()
	var transitions int64
	if e.haveHistory {
		transitions = e.maxTransitions + 1 // never reset the fencing token
	}
	l := Lease{
		Name:                 LeaseName,
		Labels:               map[string]string{OwnerKey: OwnerValue},
		Annotations:          map[string]string{VersionKey: VersionValue, OwnerKey: e.ownerAnnotation()},
		Holder:               e.cfg.Identity,
		LeaseDurationSeconds: e.secs(),
		AcquireTime:          e.stamp(start),
		RenewTime:            e.stamp(start),
		LeaseTransitions:     transitions,
	}
	got, err := e.call(ctx, nil, func(c context.Context) (Lease, error) { return e.client.Create(c, l) })
	switch {
	case err == nil:
		e.lead(got, start)
	case errors.Is(err, ErrConflict):
		// Someone else created it: the next Get observes it. A definite answer.
	default:
		e.failure()
	}
}

func (e *Elector) takeOver(ctx context.Context, l Lease) {
	start := e.clock.Now()
	n := l
	n.Holder = e.cfg.Identity
	n.LeaseDurationSeconds = e.secs()
	n.AcquireTime, n.RenewTime = e.stamp(start), e.stamp(start)
	n.LeaseTransitions = max(l.LeaseTransitions, e.maxTransitions) + 1
	got, err := e.call(ctx, nil, func(c context.Context) (Lease, error) { return e.client.Update(c, n) })
	switch {
	case err == nil:
		e.lead(got, start)
	case errors.Is(err, ErrConflict):
		// Another writer won; the next Get observes the new record.
		e.haveObs = false
	case errors.Is(err, ErrNotFound):
		// Deleted since we observed it. The 404 is a successful API response, so the
		// recovery barrier starts here, from this reading.
		e.haveObs = false
		e.absent(ctx, e.clock.Now())
	default:
		e.failure()
	}
}

// lead opens a new term's gate, anchored at the START of the request that won the
// Lease, and starts the duties behind it.
func (e *Elector) lead(got Lease, start time.Time) {
	e.noteHistory(got)
	g := &Gate{clock: e.clock, deadline: start.Add(e.cfg.Timings.RenewDeadline), token: Token{Epoch: got.LeaseTransitions, Holder: e.cfg.Identity}}
	if !g.Open() {
		return // the request took longer than the renew deadline; do not lead
	}
	e.rec = got
	e.haveObs, e.barrier = false, barrier{}
	e.current.Store(g)
	ctx, cancel := context.WithCancel(context.Background())
	e.cancel = cancel
	for _, d := range e.cfg.Duties {
		e.wg.Add(1)
		go func() {
			defer e.wg.Done()
			d(ctx, g)
		}()
	}
	e.logf("leader: acquired the Lease (epoch %d)", got.LeaseTransitions)
}

// stepDown closes the gate first, then cancels the duties, and returns to
// candidate with no observation history: a later 404 starts the barrier.
func (e *Elector) stepDown(reason string) {
	if g := e.current.Swap(nil); g != nil {
		g.close()
	}
	if e.cancel != nil {
		e.cancel()
		e.cancel = nil
	}
	e.haveObs, e.barrier = false, barrier{}
	e.logf("leader: stopped leading: %s", reason)
}

func (e *Elector) drain() bool {
	done := make(chan struct{})
	go func() { e.wg.Wait(); close(done) }()
	select {
	case <-done:
		return true
	case <-e.clock.After(e.cfg.Timings.DrainBound()):
		return false
	}
}

// Shutdown is the graceful stop. It closes the gate first, cancels the duties and
// drains them within Timings.DrainBound, then releases the Lease by an update with
// a resourceVersion precondition (never a delete, so leaseTransitions survives). A
// non-holder, or a failed drain, releases nothing and the Lease expires normally.
func (e *Elector) Shutdown() {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.stopped {
		return
	}
	e.stopped = true
	g := e.current.Swap(nil)
	if g == nil {
		return
	}
	wasOpen := g.Open()
	g.close()
	if e.cancel != nil {
		e.cancel()
		e.cancel = nil
	}
	if !e.drain() {
		e.logf("leader: duties did not drain in time; the Lease is not released")
		return
	}
	if !wasOpen {
		return // we may no longer hold it
	}
	rec := e.rec
	rec.Holder = ""
	rec.RenewTime = e.stamp(e.clock.Now())
	if _, err := e.call(context.Background(), nil, func(c context.Context) (Lease, error) { return e.client.Update(c, rec) }); err != nil {
		e.logf("leader: the Lease was not released; it will expire")
	}
}
