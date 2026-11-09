package leader

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"sync"
	"testing"
	"time"
)

const (
	relNS = "falcone-system"
	poolA = "pool-a"
	poolB = "pool-b"
)

var errDown = errors.New("request failed")

// --- clocks ------------------------------------------------------------------

// fakeClock is a manual clock. After channels fire only when Advance passes them.
type fakeClock struct {
	mu      sync.Mutex
	now     time.Time
	waiters []fakeWaiter
}

type fakeWaiter struct {
	at time.Time
	ch chan time.Time
}

func newFakeClock(start time.Time) *fakeClock { return &fakeClock{now: start} }

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) After(d time.Duration) <-chan time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	ch := make(chan time.Time, 1)
	c.waiters = append(c.waiters, fakeWaiter{at: c.now.Add(d), ch: ch})
	return ch
}

func (c *fakeClock) Advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
	kept := c.waiters[:0]
	for _, w := range c.waiters {
		if w.at.After(c.now) {
			kept = append(kept, w)
		} else {
			w.ch <- c.now
		}
	}
	c.waiters = kept
}

// driftClock is one replica's view of the shared clock running at rate times real speed.
type driftClock struct {
	base   *fakeClock
	origin time.Time
	start  time.Time
	rate   float64
}

func newDriftClock(base *fakeClock, rate float64) *driftClock {
	return &driftClock{base: base, origin: base.Now(), start: base.Now(), rate: rate}
}

func (c *driftClock) Now() time.Time {
	elapsed := c.base.Now().Sub(c.origin)
	return c.start.Add(time.Duration(float64(elapsed) * c.rate))
}

func (c *driftClock) After(time.Duration) <-chan time.Time { return nil }

// --- Lease API ---------------------------------------------------------------

// fakeAPI is the API server's view of the one Lease, shared by every replica.
type fakeAPI struct {
	mu     sync.Mutex
	lease  *Lease
	rv     int
	uids   int
	ops    []fakeOp
	events []string
	writes int
}

type fakeOp struct{ verb, namespace string }

func newFakeAPI() *fakeAPI { return &fakeAPI{} }

func (a *fakeAPI) record(event string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.events = append(a.events, event)
}

func (a *fakeAPI) snapshot() *Lease {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.lease == nil {
		return nil
	}
	return a.lease.Clone()
}

func (a *fakeAPI) writeCount() int {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.writes
}

// seed stores l as written by an operator or an older controller, with a fresh UID.
func (a *fakeAPI) seed(l *Lease) {
	a.mu.Lock()
	defer a.mu.Unlock()
	c := l.Clone()
	a.uids++
	a.rv++
	c.UID, c.ResourceVersion = fmt.Sprintf("uid-%d", a.uids), strconv.Itoa(a.rv)
	a.lease = c
}

// del is an operator deleting the Lease by hand.
func (a *fakeAPI) del() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.lease = nil
}

// touch changes the Lease without changing its content, as any concurrent writer does.
func (a *fakeAPI) touch() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rv++
	a.lease.ResourceVersion = strconv.Itoa(a.rv)
}

func (a *fakeAPI) epoch() string { return a.snapshot().Annotations[EpochAnnotation] }

// validLease is a well-formed, released Lease at the given epoch.
func validLease(epoch string) *Lease {
	l := newLease(relNS, "", DefaultLeaseDuration, time.Unix(0, 0))
	l.Annotations[EpochAnnotation] = epoch
	return l
}

// fakeClient is one replica's connection to the fakeAPI.
type fakeClient struct {
	api       *fakeAPI
	namespace string

	down            bool // every call fails
	createNotFound  bool // Create answers 404
	getNotFound     bool // Get answers 404 although the Lease may exist
	dropCreateReply bool // Create lands, but the reply is lost
	dropUpdateReply bool // Update lands, but the reply is lost
	afterGet        func()
	gets            int
}

var _ LeaseClient = (*fakeClient)(nil)

func (a *fakeAPI) client() *fakeClient { return &fakeClient{api: a, namespace: relNS} }

func (c *fakeClient) Get(context.Context) (*Lease, error) {
	c.api.mu.Lock()
	c.api.ops = append(c.api.ops, fakeOp{"get", c.namespace})
	c.gets++
	var out *Lease
	var err error
	switch {
	case c.down:
		err = errDown
	case c.getNotFound || c.api.lease == nil:
		err = ErrNotFound
	default:
		out = c.api.lease.Clone()
	}
	c.api.mu.Unlock()
	if c.afterGet != nil && err == nil {
		c.afterGet()
	}
	return out, err
}

func (c *fakeClient) Create(_ context.Context, l *Lease) (*Lease, error) {
	c.api.mu.Lock()
	defer c.api.mu.Unlock()
	c.api.ops = append(c.api.ops, fakeOp{"create", c.namespace})
	switch {
	case c.down:
		return nil, errDown
	case c.createNotFound:
		return nil, ErrNotFound
	case c.api.lease != nil:
		return nil, ErrAlreadyExists
	}
	stored := l.Clone()
	c.api.uids++
	c.api.rv++
	stored.UID, stored.ResourceVersion, stored.Namespace = fmt.Sprintf("uid-%d", c.api.uids), strconv.Itoa(c.api.rv), c.namespace
	c.api.lease = stored
	c.api.writes++
	c.api.events = append(c.api.events, "create")
	if c.dropCreateReply {
		return nil, errDown
	}
	return stored.Clone(), nil
}

func (c *fakeClient) Update(_ context.Context, l *Lease) (*Lease, error) {
	c.api.mu.Lock()
	defer c.api.mu.Unlock()
	c.api.ops = append(c.api.ops, fakeOp{"update", c.namespace})
	switch {
	case c.down:
		return nil, errDown
	case c.api.lease == nil:
		return nil, ErrNotFound
	case c.api.lease.ResourceVersion != l.ResourceVersion:
		return nil, ErrConflict
	}
	stored := c.api.lease
	stored.HolderIdentity, stored.LeaseDurationSeconds = l.HolderIdentity, l.LeaseDurationSeconds
	stored.AcquireTime, stored.RenewTime, stored.LeaseTransitions = l.AcquireTime, l.RenewTime, l.LeaseTransitions
	if epoch, ok := l.Annotations[EpochAnnotation]; ok {
		stored.Annotations[EpochAnnotation] = epoch
	}
	c.api.rv++
	stored.ResourceVersion = strconv.Itoa(c.api.rv)
	c.api.writes++
	c.api.events = append(c.api.events, "update:"+stored.HolderIdentity)
	if c.dropUpdateReply {
		return nil, errDown
	}
	return stored.Clone(), nil
}

// --- receiver ----------------------------------------------------------------

// fakeReceiver models the receiver of ctr-pool-status-report for one singleton: it
// records the lineage, epoch and sequence of the latest accepted write, refuses a
// different lineage whatever its epoch, refuses a lower epoch, and within one
// lineage and epoch accepts only a higher sequence. Reset is the operator recovery.
type fakeReceiver struct {
	mu       sync.Mutex
	lineage  string
	epoch    int64
	seq      int64
	accepted []Stamp
	refused  []Stamp
}

var _ Sink = (*fakeReceiver)(nil)

func (r *fakeReceiver) Write(_ context.Context, s Stamp, _ []byte) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	switch {
	case r.lineage != "" && s.Lineage != r.lineage,
		s.Epoch < r.epoch,
		s.Epoch == r.epoch && s.Seq <= r.seq:
		r.refused = append(r.refused, s)
		// The refusal discloses the receiver's epoch in its message, as a real one might.
		return fmt.Errorf("refused, receiver is at epoch %d: %w", r.epoch, &FencedError{})
	}
	r.lineage, r.epoch, r.seq = s.Lineage, s.Epoch, s.Seq
	r.accepted = append(r.accepted, s)
	return nil
}

// reset is the operator clearing the recorded lineage and epoch for this singleton.
func (r *fakeReceiver) reset() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.lineage, r.epoch, r.seq = "", 0, 0
}

func (r *fakeReceiver) holds() Stamp {
	r.mu.Lock()
	defer r.mu.Unlock()
	return Stamp{Lineage: r.lineage, Epoch: r.epoch, Seq: r.seq}
}

func (r *fakeReceiver) acceptedCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.accepted)
}

// --- instances ---------------------------------------------------------------

type instance struct {
	id     string
	e      *Elector
	client *fakeClient
	clock  Clock
}

const tick = DefaultRetryPeriod

func newInstance(t *testing.T, api *fakeAPI, clock Clock, id string) *instance {
	t.Helper()
	client := api.client()
	e, err := New(Config{
		Identity: id, Namespace: relNS,
		LeaseDuration: DefaultLeaseDuration, RenewDeadline: DefaultRenewDeadline, RetryPeriod: DefaultRetryPeriod,
		Lease: client, Clock: clock,
	})
	if err != nil {
		t.Fatal(err)
	}
	return &instance{id: id, e: e, client: client, clock: clock}
}

func (i *instance) tick() { i.e.Tick(context.Background()) }

func (i *instance) leads() bool { return i.e.Term() != nil }

func (i *instance) publish(r *fakeReceiver) error {
	return i.e.Publish(context.Background(), r, []byte(i.id))
}

// run advances the shared clock in retry-period steps for d, ticking each live
// instance after every step.
func run(clock *fakeClock, d time.Duration, instances ...*instance) {
	for elapsed := time.Duration(0); elapsed < d; elapsed += tick {
		clock.Advance(tick)
		for _, i := range instances {
			i.tick()
		}
	}
}

var epoch0 = time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)
