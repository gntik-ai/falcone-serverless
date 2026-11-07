package leader_test

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

const (
	releaseNS = "falcone-system"
	poolA     = "pool-a"
	poolB     = "pool-b"
)

// signal is a broadcast: wait returns a channel that closes at the next notify, so
// a test blocks on the event it needs instead of polling or sleeping.
type signal struct {
	mu sync.Mutex
	ch chan struct{}
}

func (s *signal) wait() <-chan struct{} {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ch == nil {
		s.ch = make(chan struct{})
	}
	return s.ch
}

func (s *signal) notify() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ch != nil {
		close(s.ch)
		s.ch = nil
	}
}

// await blocks until cond holds, re-checking after every notify. The deadline only
// guards against a hung test; it is never what makes a test pass.
func (s *signal) await(t *testing.T, what string, cond func() bool) {
	t.Helper()
	guard := time.After(30 * time.Second)
	for {
		changed := s.wait() // subscribe before checking, so no notify is missed
		if cond() {
			return
		}
		select {
		case <-changed:
		case <-guard:
			t.Fatalf("timed out waiting for %s", what)
		}
	}
}

var epoch0 = time.Unix(1_800_000_000, 0).UTC()

// fakeClock is a manually advanced clock. Timers fire when Advance passes them.
type fakeClock struct {
	mu      sync.Mutex
	now     time.Time
	timers  []fakeTimer
	afters  []time.Duration // the duration of every After call, in order
	changed signal          // notified whenever a timer is registered
}

type fakeTimer struct {
	at time.Time
	ch chan time.Time
}

func newFakeClock() *fakeClock { return &fakeClock{now: epoch0} }

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) After(d time.Duration) <-chan time.Time {
	c.mu.Lock()
	ch := make(chan time.Time, 1)
	c.afters = append(c.afters, d)
	if d <= 0 {
		ch <- c.now
		c.mu.Unlock()
		c.changed.notify()
		return ch
	}
	c.timers = append(c.timers, fakeTimer{at: c.now.Add(d), ch: ch})
	c.mu.Unlock()
	c.changed.notify()
	return ch
}

func (c *fakeClock) Advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	var keep []fakeTimer
	for _, t := range c.timers {
		if !t.at.After(c.now) {
			t.ch <- c.now
		} else {
			keep = append(keep, t)
		}
	}
	c.timers = keep
	c.mu.Unlock()
}

// since is the fake time elapsed since the start of the test.
func (c *fakeClock) since() time.Duration { return c.Now().Sub(epoch0) }

// afterCalls returns how many After calls have been made, and the last duration.
func (c *fakeClock) afterCalls() (int, time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.afters) == 0 {
		return 0, 0
	}
	return len(c.afters), c.afters[len(c.afters)-1]
}

func (c *fakeClock) pending() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return len(c.timers)
}

// waitTimers blocks until n timers are pending, so a goroutine blocked on the
// clock is known to be waiting before the test advances time.
func (c *fakeClock) waitTimers(t *testing.T, n int) {
	t.Helper()
	c.changed.await(t, "pending timers", func() bool { return c.pending() >= n })
}

// fakeStore is one in-memory Lease store, the Kubernetes API's view of the Lease.
type fakeStore struct {
	mu      sync.Mutex
	lease   *leader.Lease
	counter int
	// requests records the operation of every request, with the namespace it hit.
	requests []string
	creates  int
	updates  int
	// onWrite lets a test observe the stored object after each successful write.
	writeCount int
}

func (s *fakeStore) clone(l leader.Lease) leader.Lease {
	out := l
	out.Labels = copyMap(l.Labels)
	out.Annotations = copyMap(l.Annotations)
	return out
}

func copyMap(m map[string]string) map[string]string {
	if m == nil {
		return nil
	}
	out := make(map[string]string, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}

func (s *fakeStore) bump(l *leader.Lease) {
	s.counter++
	l.ResourceVersion = "rv" + itoa(s.counter)
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var b []byte
	for ; n > 0; n /= 10 {
		b = append([]byte{byte('0' + n%10)}, b...)
	}
	return string(b)
}

// current returns a copy of the stored Lease, or nil when there is none.
func (s *fakeStore) current() *leader.Lease {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.lease == nil {
		return nil
	}
	c := s.clone(*s.lease)
	return &c
}

func (s *fakeStore) remove() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.lease = nil
}

// seed stores a Lease as another actor wrote it.
func (s *fakeStore) seed(l leader.Lease) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.counter++
	l.UID = "uid-seed-" + itoa(s.counter)
	s.bump(&l)
	c := s.clone(l)
	s.lease = &c
}

// storeClient is one instance's connection to the store, with fault injection.
type storeClient struct {
	store                  *fakeStore
	name                   string
	mu                     sync.Mutex
	down                   bool             // every request fails with a transport error
	fault                  map[string]error // per-operation error
	hang                   chan struct{}    // when set, a request blocks until it is closed or cancelled
	afterUpdate            func()           // when set, runs after an update was applied, before it returns
	creates, updates, gets int
}

var errNetwork = errors.New("injected network failure")

func (s *fakeStore) client(name string) *storeClient {
	return &storeClient{store: s, name: name, fault: map[string]error{}}
}

func (c *storeClient) setDown(v bool) { c.mu.Lock(); c.down = v; c.mu.Unlock() }
func (c *storeClient) setFault(op string, err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if err == nil {
		delete(c.fault, op)
	} else {
		c.fault[op] = err
	}
}
func (c *storeClient) counts() (creates, updates, gets int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.creates, c.updates, c.gets
}

func (c *storeClient) enter(ctx context.Context, op string) error {
	c.mu.Lock()
	switch op {
	case "create":
		c.creates++
	case "update":
		c.updates++
	case "get":
		c.gets++
	}
	down, fault, hang := c.down, c.fault[op], c.hang
	c.mu.Unlock()
	if hang != nil {
		select {
		case <-hang:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	if down {
		return errNetwork
	}
	return fault
}

func (c *storeClient) Get(ctx context.Context) (leader.Lease, error) {
	if err := c.enter(ctx, "get"); err != nil {
		return leader.Lease{}, err
	}
	s := c.store
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.lease == nil {
		return leader.Lease{}, leader.ErrNotFound
	}
	return s.clone(*s.lease), nil
}

func (c *storeClient) Create(ctx context.Context, l leader.Lease) (leader.Lease, error) {
	if err := c.enter(ctx, "create"); err != nil {
		return leader.Lease{}, err
	}
	s := c.store
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.lease != nil {
		return leader.Lease{}, leader.ErrConflict
	}
	l = s.clone(l)
	s.counter++
	l.UID = "uid-" + itoa(s.counter)
	s.bump(&l)
	stored := s.clone(l)
	s.lease = &stored
	return s.clone(l), nil
}

func (c *storeClient) Update(ctx context.Context, l leader.Lease) (leader.Lease, error) {
	if err := c.enter(ctx, "update"); err != nil {
		return leader.Lease{}, err
	}
	s := c.store
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.lease == nil {
		return leader.Lease{}, leader.ErrNotFound
	}
	if l.ResourceVersion != s.lease.ResourceVersion || (l.UID != "" && l.UID != s.lease.UID) {
		return leader.Lease{}, leader.ErrConflict
	}
	l = s.clone(l)
	l.UID = s.lease.UID
	s.bump(&l)
	stored := s.clone(l)
	s.lease = &stored
	if c.afterUpdate != nil {
		defer c.afterUpdate()
	}
	return s.clone(l), nil
}

// instance is one controller replica under test.
type instance struct {
	name   string
	client *storeClient
	el     *leader.Elector
	duty   *dutyProbe
}

// dutyProbe records the lifecycle of the injected singleton duty.
type dutyProbe struct {
	mu        sync.Mutex
	running   int
	started   int
	cancelled int
	gates     []*leader.Gate
	ctx       context.Context
	stuck     bool // ignore cancellation until released
	release   chan struct{}
	changed   signal // notified whenever the duty starts or stops
}

func newDutyProbe() *dutyProbe { return &dutyProbe{release: make(chan struct{})} }

func (p *dutyProbe) duty(ctx context.Context, g *leader.Gate) {
	p.mu.Lock()
	p.running++
	p.started++
	p.gates = append(p.gates, g)
	stuck := p.stuck
	p.ctx = ctx
	p.mu.Unlock()
	p.changed.notify()
	<-ctx.Done()
	p.changed.notify()
	if stuck {
		<-p.release
	}
	p.mu.Lock()
	p.running--
	p.cancelled++
	p.mu.Unlock()
	p.changed.notify()
}

// ctxDone reports whether the duty's context has been cancelled.
func (p *dutyProbe) ctxDone() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.ctx != nil && p.ctx.Err() != nil
}

func (p *dutyProbe) snapshot() (running, started, cancelled int) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.running, p.started, p.cancelled
}

// await blocks until cond holds, re-checked after every duty start or stop.
func (p *dutyProbe) await(t *testing.T, what string, cond func() bool) {
	t.Helper()
	p.changed.await(t, what, cond)
}

func newInstance(t *testing.T, clock *fakeClock, store *fakeStore, name string, timings leader.Timings) *instance {
	t.Helper()
	probe := newDutyProbe()
	client := store.client(name)
	el, err := leader.New(leader.Config{
		Namespace: releaseNS,
		Identity:  name + "-0123abcd",
		Timings:   timings,
		Client:    client,
		Clock:     clock,
		Duties:    []leader.Duty{probe.duty},
	})
	if err != nil {
		t.Fatal(err)
	}
	return &instance{name: name, client: client, el: el, duty: probe}
}

func (i *instance) open() bool {
	_, ok := i.el.Token()
	return ok
}

// tick runs one election iteration at the current fake time.
func (i *instance) tick() { i.el.Tick(context.Background()) }
