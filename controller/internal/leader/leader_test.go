package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"math"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

type timer struct {
	at      time.Time
	f       func()
	stopped bool
}
type fakeClock struct {
	mu     sync.Mutex
	now    time.Time
	timers []*timer
	waits  chan time.Duration
}

func clock() *fakeClock             { return &fakeClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)} }
func (c *fakeClock) Now() time.Time { c.mu.Lock(); defer c.mu.Unlock(); return c.now }
func (c *fakeClock) AfterFunc(d time.Duration, f func()) func() {
	c.mu.Lock()
	defer c.mu.Unlock()
	t := &timer{at: c.now.Add(d), f: f}
	c.timers = append(c.timers, t)
	return func() { c.mu.Lock(); t.stopped = true; c.mu.Unlock() }
}
func (c *fakeClock) After(d time.Duration) <-chan time.Time {
	ch := make(chan time.Time, 1)
	c.AfterFunc(d, func() { ch <- c.Now() })
	if c.waits != nil {
		c.waits <- d
	}
	return ch
}
func (c *fakeClock) advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	var calls []func()
	for _, t := range c.timers {
		if !t.stopped && !t.at.After(c.now) {
			t.stopped = true
			calls = append(calls, t.f)
		}
	}
	c.mu.Unlock()
	for _, f := range calls {
		f()
	}
}

type store struct {
	mu        sync.Mutex
	lease     *leader.Lease
	rev       int
	writes    int
	reads     int
	onUpdate  func()
	fail      bool
	conflict  func()
	onGet     func()
	normalize bool
	onCreate  func()
}

func copyLease(l leader.Lease) leader.Lease {
	b, _ := json.Marshal(l)
	var r leader.Lease
	_ = json.Unmarshal(b, &r)
	return r
}
func (s *store) Get(context.Context) (leader.Lease, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reads++
	if s.onGet != nil {
		s.onGet()
	}
	if s.fail {
		return leader.Lease{}, errors.New("unavailable")
	}
	if s.lease == nil {
		return leader.Lease{}, leader.ErrNotFound
	}
	return copyLease(*s.lease), nil
}
func (s *store) normalizeTimes(l *leader.Lease) {
	if s.normalize {
		for _, stamp := range []*string{&l.Spec.AcquireTime, &l.Spec.RenewTime} {
			if parsed, err := time.Parse(time.RFC3339Nano, *stamp); err == nil {
				*stamp = parsed.Truncate(time.Microsecond).UTC().Format("2006-01-02T15:04:05.000000Z07:00")
			}
		}
	}
}
func (s *store) Create(_ context.Context, l leader.Lease) (leader.Lease, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.fail {
		return l, errors.New("unavailable")
	}
	if s.lease != nil {
		return l, leader.ErrConflict
	}
	s.rev++
	l.Metadata.ResourceVersion = strconv.Itoa(s.rev)
	s.normalizeTimes(&l)
	s.lease = &l
	s.writes++
	if s.onCreate != nil {
		s.onCreate()
	}
	return copyLease(l), nil
}
func (s *store) Update(_ context.Context, l leader.Lease) (leader.Lease, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.fail {
		return l, errors.New("unavailable")
	}
	if s.conflict != nil {
		f := s.conflict
		s.conflict = nil
		f()
		return l, leader.ErrConflict
	}
	if s.lease == nil || l.Metadata.ResourceVersion != s.lease.Metadata.ResourceVersion {
		return l, leader.ErrConflict
	}
	s.rev++
	l.Metadata.ResourceVersion = strconv.Itoa(s.rev)
	s.normalizeTimes(&l)
	s.lease = &l
	s.writes++
	if s.onUpdate != nil {
		s.onUpdate()
	}
	return copyLease(l), nil
}
func elector(t *testing.T, s *store, c *fakeClock, id string) *leader.Elector {
	t.Helper()
	e, err := leader.New(leader.Config{Namespace: "release", Identity: id, LeaseDuration: 15 * time.Second, RenewDeadline: 10 * time.Second, RetryPeriod: 2 * time.Second}, s, c, nil)
	if err != nil {
		t.Fatal(err)
	}
	return e
}
func epoch(t *testing.T, e *leader.Elector) int64 {
	t.Helper()
	n, ok := e.Gate().Epoch()
	if !ok {
		t.Fatal("expected leader")
	}
	return n
}

func TestOneLeaderPublishesFencedReportAndCrashHandover(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	b := elector(t, s, c, "b")
	ctx := context.Background()
	a.Tick(ctx)
	b.Tick(ctx)
	n := epoch(t, a)
	if _, ok := b.Gate().Epoch(); ok {
		t.Fatal("two leaders")
	}
	if s.writes != 1 {
		t.Fatal("standby wrote")
	}
	data, err := poolstatus.Encode(poolstatus.Report{ReporterEpoch: n, RuntimeMode: "wasm"}, poolstatus.Scope{PoolRef: poolstatus.PoolRef{Namespace: "pool-a", Name: "falcone-pool-abc"}, OwnerAnnotation: `["tenant","profile"]`}, nil, poolstatus.Options{})
	if err != nil {
		t.Fatal(err)
	}
	var report poolstatus.Report
	_ = json.Unmarshal(data, &report)
	if report.ReporterEpoch != s.lease.Spec.LeaseTransitions {
		t.Fatal("unfenced report")
	}
	old := a.Gate()
	c.advance(10 * time.Second)
	if _, ok := old.Epoch(); ok {
		t.Fatal("stalled loop gate remained open")
	}
	select {
	case <-old.Context().Done():
	default:
		t.Fatal("duties not cancelled")
	}
	b.Tick(ctx)
	c.advance(4 * time.Second)
	b.Tick(ctx)
	if s.writes != 1 {
		t.Fatal("early acquisition")
	}
	c.advance(time.Second)
	b.Tick(ctx)
	if epoch(t, b) <= n {
		t.Fatal("epoch not advanced")
	}
}

func TestRenewalFailureCancelsAndNeverReopensOldGate(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	ctx := context.Background()
	a.Tick(ctx)
	g := a.Gate()
	n := epoch(t, a)
	c.advance(2 * time.Second)
	a.Tick(ctx)
	if epoch(t, a) != n {
		t.Fatal("renewal changed epoch")
	}
	s.fail = true
	c.advance(9 * time.Second)
	a.Tick(ctx)
	if _, ok := g.Epoch(); !ok {
		t.Fatal("gate closed before renew deadline")
	}
	c.advance(time.Second)
	if _, ok := g.Epoch(); ok {
		t.Fatal("gate open at renew deadline")
	}
	select {
	case <-g.Context().Done():
	default:
		t.Fatal("inflight duty not cancelled")
	}
	s.fail = false
	c.advance(5 * time.Second)
	a.Tick(ctx)
	if epoch(t, a) <= n {
		t.Fatal("reacquisition reused epoch")
	}
	if _, ok := g.Epoch(); ok {
		t.Fatal("old gate reopened")
	}
}
func TestReleaseHandoverAndRestartEpoch(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "pod-process-1")
	b := elector(t, s, c, "standby")
	ctx := context.Background()
	a.Tick(ctx)
	b.Tick(ctx)
	n := epoch(t, a)
	a.Shutdown(ctx)
	if s.lease.Spec.HolderIdentity != "" || s.lease.Spec.LeaseTransitions != n {
		t.Fatal("release removed fencing history")
	}
	b.Tick(ctx)
	if epoch(t, b) <= n {
		t.Fatal("release handover did not advance epoch")
	}
	second := epoch(t, b)
	b.Shutdown(ctx)
	restarted := elector(t, s, c, "pod-process-2")
	restarted.Tick(ctx)
	if epoch(t, restarted) <= second {
		t.Fatal("restart reused epoch")
	}
}
func TestForeignOwnershipNeverAdopted(t *testing.T) {
	for name, edit := range map[string]func(*leader.Lease){
		"label":           func(l *leader.Lease) { l.Metadata.Labels = nil },
		"annotations":     func(l *leader.Lease) { l.Metadata.Annotations = nil },
		"owner namespace": func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = `["pool-a","leader"]` },
		"scaler duty":     func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = `["release","scaler"]` },
		"version":         func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/ownership-version"] = "2" },
		"name":            func(l *leader.Lease) { l.Metadata.Name = "falcone-pool-controller-scaler" },
		"namespace":       func(l *leader.Lease) { l.Metadata.Namespace = "pool-a" },
	} {
		t.Run(name, func(t *testing.T) {
			c := clock()
			s := &store{}
			a := elector(t, s, c, "a")
			a.Tick(context.Background())
			g := a.Gate()
			edit(s.lease)
			a.Tick(context.Background()) // Still before the local renew deadline.
			if _, ok := g.Epoch(); ok || g.Context().Err() == nil || s.writes != 1 {
				t.Fatal("current holder retained authority after ownership became unverified")
			}
			c.advance(time.Hour)
			var logs []string
			b, err := leader.New(leader.Config{Namespace: "release", Identity: "b", LeaseDuration: 15 * time.Second, RenewDeadline: 10 * time.Second, RetryPeriod: 2 * time.Second}, s, c, func(s string) { logs = append(logs, s) })
			if err != nil {
				t.Fatal(err)
			}
			b.Tick(context.Background())
			c.advance(time.Hour)
			b.Tick(context.Background())
			if s.writes != 1 {
				t.Fatal("adopted foreign lease")
			}
			if _, ok := b.Gate().Epoch(); ok {
				t.Fatal("unverified leadership")
			}
			if len(logs) != 2 || logs[0] != "leader lease ownership unverified" || logs[1] != logs[0] {
				t.Fatal(logs)
			}
		})
	}
}
func TestConflictRereadsAndStepsDown(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	g := a.Gate()
	reads := s.reads
	s.conflict = func() { s.lease.Spec.HolderIdentity = "foreign"; s.lease.Spec.LeaseTransitions++ }
	c.advance(time.Second)
	a.Tick(context.Background())
	if _, ok := g.Epoch(); ok {
		t.Fatal("conflict retained leadership")
	}
	if s.reads != reads+2 {
		t.Fatal("conflict did not immediately re-read the Lease")
	}
	if s.writes != 1 {
		t.Fatal("conflict forced write")
	}
	a.Tick(context.Background())
	if s.writes != 1 {
		t.Fatal("foreign holder overwritten")
	}
}
func TestRemoteClockIsNeverUsedForExpiration(t *testing.T) {
	for _, stamp := range []string{"1900-01-01T00:00:00Z", "9999-01-01T00:00:00Z"} {
		c := clock()
		s := &store{}
		a := elector(t, s, c, "a")
		a.Tick(context.Background())
		s.lease.Spec.RenewTime = stamp
		b := elector(t, s, c, "b")
		b.Tick(context.Background())
		c.advance(14 * time.Second)
		b.Tick(context.Background())
		if s.writes != 1 {
			t.Fatal("used remote timestamp")
		}
		c.advance(time.Second)
		b.Tick(context.Background())
		epoch(t, b)
	}
}
func TestMissingLeaseRetainsObservedEpochAndIdentityChanges(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	first := epoch(t, a)
	s.lease = nil
	a.Tick(context.Background())
	if epoch(t, a) <= first {
		t.Fatal("missing lease reset observed epoch")
	}
	one, err := leader.NewIdentity()
	if err != nil {
		t.Fatal(err)
	}
	two, err := leader.NewIdentity()
	if err != nil || one == two {
		t.Fatal("process identities reused")
	}
}

func await(t *testing.T, ch <-chan struct{}) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(5 * time.Second):
		t.Fatal("duty did not progress")
	}
}
func TestRunStopsDutiesBeforeReleasingAndBoundsShutdown(t *testing.T) {
	for _, cooperate := range []bool{true, false} {
		t.Run(strconv.FormatBool(cooperate), func(t *testing.T) {
			c := clock()
			c.waits = make(chan time.Duration, 10)
			s := &store{}
			a := elector(t, s, c, "a")
			ctx, cancel := context.WithCancel(context.Background())
			started := make(chan struct{})
			cancelled := make(chan struct{})
			unblock := make(chan struct{})
			finished := make(chan struct{})
			go func() {
				a.Run(ctx, func(ctx context.Context, g *leader.Gate) {
					if _, ok := g.Epoch(); !ok {
						t.Error("duty started without leadership")
					}
					close(started)
					<-ctx.Done()
					close(cancelled)
					if !cooperate {
						<-unblock
					}
				})
				close(finished)
			}()
			await(t, started)
			select {
			case d := <-c.waits:
				if d != 2*time.Second {
					t.Fatal(d)
				}
			case <-time.After(5 * time.Second):
				t.Fatal("run loop not waiting")
			}
			cancel()
			await(t, cancelled)
			if !cooperate {
				select {
				case d := <-c.waits:
					if d != 10*time.Second {
						t.Fatal(d)
					}
				case <-time.After(5 * time.Second):
					t.Fatal("shutdown not bounded")
				}
				c.advance(10 * time.Second)
			}
			await(t, finished)
			close(unblock)
			if cooperate {
				if s.lease.Spec.HolderIdentity != "" {
					t.Fatal("graceful shutdown did not release")
				}
			} else if s.lease.Spec.HolderIdentity != "a" || s.writes != 1 {
				t.Fatal("released while duties still running")
			}
		})
	}
}
func TestFailedReleasePreservesLeaseUntilExpiry(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	b := elector(t, s, c, "b")
	ctx := context.Background()
	a.Tick(ctx)
	b.Tick(ctx)
	s.fail = true
	a.Shutdown(ctx)
	if _, ok := a.Gate().Epoch(); ok {
		t.Fatal("release failure reopened gate")
	}
	s.fail = false
	c.advance(14 * time.Second)
	b.Tick(ctx)
	if s.writes != 1 {
		t.Fatal("early failover")
	}
	c.advance(time.Second)
	b.Tick(ctx)
	epoch(t, b)
}

func TestReplacementLeaseDoesNotOverlapCancelledDuties(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		c := clock()
		c.waits = make(chan time.Duration, 10)
		s := &store{}
		gets := 0
		s.onGet = func() {
			gets++
			if gets == 2 {
				s.lease = nil
			}
		}
		a := elector(t, s, c, "a")
		ctx, cancel := context.WithCancel(context.Background())
		var starts atomic.Int32
		unblock := make(chan struct{})
		finished := make(chan struct{})
		started := make(chan struct{}, 2)
		go func() {
			a.Run(ctx, func(ctx context.Context, _ *leader.Gate) {
				starts.Add(1)
				started <- struct{}{}
				<-ctx.Done()
				<-unblock
			})
			close(finished)
		}()
		await(t, started)
		<-c.waits
		c.advance(2 * time.Second)
		<-c.waits
		// Wait until every duty goroutine has either started or blocked.
		synctest.Wait()
		count := starts.Load()
		cancel()
		<-c.waits
		c.advance(10 * time.Second)
		await(t, finished)
		close(unblock)
		if count != 1 {
			t.Fatalf("started %d overlapping duties after Lease replacement", count)
		}
	})
}

func TestTwoRunningInstancesOnlyLeaderReconcilesAndPublishes(t *testing.T) {
	c := clock()
	c.waits = make(chan time.Duration, 10)
	s := &store{}
	a := elector(t, s, c, "a")
	b := elector(t, s, c, "b")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var writes, publishes atomic.Int32
	reports := make(chan []byte, 2)
	done := make(chan struct{}, 2)
	duty := func(ctx context.Context, g *leader.Gate) {
		n, ok := g.Epoch()
		if !ok {
			t.Error("unfenced reconcile")
			return
		}
		writes.Add(1)
		data, err := poolstatus.Encode(poolstatus.Report{ReporterEpoch: n, RuntimeMode: "wasm"}, poolstatus.Scope{PoolRef: poolstatus.PoolRef{Namespace: "pool-a", Name: "falcone-pool-abc"}, OwnerAnnotation: `["tenant","profile"]`}, nil, poolstatus.Options{})
		if err != nil {
			t.Error(err)
			return
		}
		if _, ok := g.Epoch(); ok {
			publishes.Add(1)
			reports <- data
		}
		<-ctx.Done()
	}
	go func() { a.Run(ctx, duty); done <- struct{}{} }()
	go func() { b.Run(ctx, duty); done <- struct{}{} }()
	<-c.waits
	<-c.waits
	var report poolstatus.Report
	select {
	case data := <-reports:
		if json.Unmarshal(data, &report) != nil {
			t.Fatal("bad report")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no leader publication")
	}
	cancel()
	await(t, done)
	await(t, done)
	if writes.Load() != 1 || publishes.Load() != 1 || report.ReporterEpoch != s.lease.Spec.LeaseTransitions {
		t.Fatalf("writes=%d publishes=%d epoch=%d", writes.Load(), publishes.Load(), report.ReporterEpoch)
	}
}

func TestGateChecksClockWhenWatchdogIsDelayed(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	g := a.Gate()
	// Move the injected local clock without delivering timer callbacks.
	c.mu.Lock()
	c.now = c.now.Add(10 * time.Second)
	c.mu.Unlock()
	if _, ok := g.Epoch(); ok {
		t.Fatal("gate depended on watchdog scheduling")
	}
	select {
	case <-g.Context().Done():
	default:
		t.Fatal("gate did not cancel inflight work")
	}
}

func TestNilGateHasNoDutyAuthority(t *testing.T) {
	var g *leader.Gate
	if _, ok := g.Epoch(); ok || g.Context().Err() == nil {
		t.Fatal("absent gate must deny authority and provide a cancelled duty context")
	}
}

func TestKubernetesMicroTimeResponsesPermitAcquisitionAndRenewal(t *testing.T) {
	c := clock()
	c.now = c.now.Add(123456789 * time.Nanosecond)
	s := &store{normalize: true}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	n := epoch(t, a)
	c.advance(2 * time.Second)
	a.Tick(context.Background())
	if epoch(t, a) != n {
		t.Fatal("canonical timestamp changed tenure")
	}
}

func TestRunRetryCadenceIncludesRequestTime(t *testing.T) {
	c := clock()
	c.waits = make(chan time.Duration, 10)
	s := &store{}
	s.onGet = func() { c.advance(500 * time.Millisecond) }
	a := elector(t, s, c, "a")
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { a.Run(ctx, func(ctx context.Context, _ *leader.Gate) { <-ctx.Done() }); close(done) }()
	var delay time.Duration
	select {
	case delay = <-c.waits:
	case <-time.After(5 * time.Second):
		t.Fatal("no retry scheduled")
	}
	cancel()
	await(t, done)
	if delay != 1500*time.Millisecond {
		t.Fatalf("retry adds request latency: delay=%s, want 1.5s", delay)
	}
}

func TestStandbyWaitsFullDurationFromLastObservedRenewal(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	b := elector(t, s, c, "b")
	ctx := context.Background()
	a.Tick(ctx)
	b.Tick(ctx)
	c.advance(2 * time.Second)
	a.Tick(ctx)
	c.advance(2 * time.Second)
	b.Tick(ctx) // Renewal observed two seconds after it was written.
	c.advance(14 * time.Second)
	b.Tick(ctx)
	if s.writes != 2 {
		t.Fatal("acquired before duration from local observation")
	}
	c.advance(2 * time.Second)
	b.Tick(ctx)
	epoch(t, b)
	if s.writes != 3 {
		t.Fatal("did not acquire within duration plus retry")
	}
}

func TestDeletedLeaseWaitsForLastObservedHolderToExpire(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	b := elector(t, s, c, "b")
	ctx := context.Background()
	a.Tick(ctx)
	b.Tick(ctx)
	c.advance(2 * time.Second)
	a.Tick(ctx)
	c.advance(2 * time.Second)
	b.Tick(ctx) // Observe a renewal at t=4, independently of its remote timestamp.
	old := a.Gate()
	n := epoch(t, a)
	s.lease = nil
	c.advance(time.Second)
	b.Tick(ctx)
	if _, ok := old.Epoch(); !ok {
		t.Fatal("reproduction requires the partitioned holder's gate to remain open")
	}
	if _, ok := b.Gate().Epoch(); ok {
		t.Fatal("deleted Lease opened a second gate before the observed holder expired")
	}
	if s.writes != 2 || s.lease != nil {
		t.Fatal("standby created a replacement while the old holder could still write")
	}
	c.advance(13 * time.Second) // t=18: only 14s since the observation.
	b.Tick(ctx)
	if s.writes != 2 || s.lease != nil {
		t.Fatal("404 reset or bypassed the observation-based expiry wait")
	}
	c.advance(2 * time.Second) // Next retry after expiry at t=19.
	b.Tick(ctx)
	if epoch(t, b) != n+1 || s.writes != 3 {
		t.Fatal("replacement did not acquire within leaseDuration + retryPeriod with a fresh epoch")
	}
	if _, ok := old.Epoch(); ok {
		t.Fatal("replacement overlaps the old holder")
	}
}

func TestCancelledCampaignDoesNotWriteLease(t *testing.T) {
	for _, duringRead := range []bool{false, true} {
		c := clock()
		s := &store{}
		a := elector(t, s, c, "a")
		ctx, cancel := context.WithCancel(context.Background())
		if duringRead {
			s.onGet = cancel
		} else {
			cancel()
		}
		a.Tick(ctx)
		cancel()
		if s.writes != 0 {
			t.Fatal("cancelled campaign wrote Lease")
		}
		if _, ok := a.Gate().Epoch(); ok {
			t.Fatal("cancelled campaign opened gate")
		}
	}
}

func TestShutdownReleasesAcquisitionConfirmedDuringCancellation(t *testing.T) {
	c := clock()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s := &store{onCreate: cancel}
	a := elector(t, s, c, "a")
	a.Run(ctx, func(context.Context, *leader.Gate) { t.Error("started duties after shutdown") })
	if s.lease == nil || s.lease.Spec.HolderIdentity != "" || s.lease.Spec.LeaseTransitions != 0 {
		t.Fatal("shutdown left a confirmed acquisition held")
	}
}

// A plain successful Get must revoke authority, without relying on a conflict
// or waiting for the local watchdog.
func TestForeignHolderOnReadImmediatelyFencesDuties(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	g := a.Gate()
	c.advance(time.Second)
	s.lease.Spec.HolderIdentity = "b"
	s.lease.Spec.LeaseTransitions++
	a.Tick(context.Background())
	if _, ok := g.Epoch(); ok || g.Context().Err() == nil {
		t.Fatal("foreign holder read did not immediately fence writes and publishes")
	}
	if s.writes != 1 {
		t.Fatal("wrote after observing a foreign holder")
	}
}

func TestStaleShutdownCannotReleaseAnotherTenure(t *testing.T) {
	t.Run("another holder acquired", func(t *testing.T) {
		c := clock()
		s := &store{}
		a := elector(t, s, c, "a")
		b := elector(t, s, c, "b")
		ctx := context.Background()
		a.Tick(ctx)
		b.Tick(ctx)
		c.advance(15 * time.Second)
		b.Tick(ctx)
		n := epoch(t, b)
		writes := s.writes
		a.Shutdown(ctx)
		if s.writes != writes || s.lease.Spec.HolderIdentity != "b" || s.lease.Spec.LeaseTransitions != n {
			t.Fatal("old leader released a newly acquired tenure")
		}
	})
	// Isolate each guard: a foreign holder with the old epoch, and a newer
	// epoch with the old identity, must each independently prevent release.
	for _, sameIdentity := range []bool{false, true} {
		t.Run(strconv.FormatBool(sameIdentity), func(t *testing.T) {
			c := clock()
			s := &store{}
			a := elector(t, s, c, "a")
			a.Tick(context.Background())
			s.lease.Spec.HolderIdentity = "b"
			if sameIdentity {
				s.lease.Spec.HolderIdentity = "a"
				s.lease.Spec.LeaseTransitions++
			}
			before := copyLease(*s.lease)
			a.Shutdown(context.Background())
			if s.writes != 1 || s.lease.Spec != before.Spec {
				t.Fatal("stale shutdown wrote another holder or epoch's Lease")
			}
			if _, ok := a.Gate().Epoch(); ok {
				t.Fatal("shutdown retained authority")
			}
		})
	}
}

func TestRenewDeadlineIncludesReadAndWriteLatency(t *testing.T) {
	for _, operation := range []string{"get", "update"} {
		t.Run(operation, func(t *testing.T) {
			c := clock()
			s := &store{}
			a := elector(t, s, c, "a")
			a.Tick(context.Background())
			g := a.Gate()
			c.advance(2 * time.Second) // Renewal starts at t=2; deadline must be t=12.
			latency := func() { c.advance(3 * time.Second) }
			if operation == "get" {
				s.onGet = latency
			} else {
				s.onUpdate = latency
			}
			a.Tick(context.Background()) // Completes at t=5.
			c.advance(6 * time.Second)   // t=11.
			if _, ok := g.Epoch(); !ok {
				t.Fatal("renewal closed the gate early")
			}
			c.advance(time.Second) // t=12, not t=15.
			if _, ok := g.Epoch(); ok || g.Context().Err() == nil {
				t.Fatal("API latency extended the renewal deadline")
			}
		})
	}
}

// This API boundary returns a different record than the write it accepted.
type changedReplyClient struct {
	leader.Client
	change func(*leader.Lease)
}

func (c changedReplyClient) Create(ctx context.Context, l leader.Lease) (leader.Lease, error) {
	result, err := c.Client.Create(ctx, l)
	c.change(&result)
	return result, err
}
func (c changedReplyClient) Update(ctx context.Context, l leader.Lease) (leader.Lease, error) {
	result, err := c.Client.Update(ctx, l)
	c.change(&result)
	return result, err
}

func TestWriteResponseMustConfirmOwnedTenure(t *testing.T) {
	for _, renew := range []bool{false, true} {
		for name, change := range map[string]func(*leader.Lease){
			"holder":    func(l *leader.Lease) { l.Spec.HolderIdentity = "b" },
			"epoch":     func(l *leader.Lease) { l.Spec.LeaseTransitions++ },
			"ownership": func(l *leader.Lease) { l.Metadata.Labels = nil },
		} {
			t.Run(strconv.FormatBool(renew)+"/"+name, func(t *testing.T) {
				c := clock()
				s := &store{}
				alter := false
				client := changedReplyClient{Client: s, change: func(l *leader.Lease) {
					if alter {
						change(l)
					}
				}}
				a, err := leader.New(leader.Config{Namespace: "release", Identity: "a", LeaseDuration: 15 * time.Second, RenewDeadline: 10 * time.Second, RetryPeriod: 2 * time.Second}, client, c, nil)
				if err != nil {
					t.Fatal(err)
				}
				if renew {
					a.Tick(context.Background())
					epoch(t, a)
					c.advance(time.Second)
				}
				alter = true
				a.Tick(context.Background())
				if _, ok := a.Gate().Epoch(); ok {
					t.Fatal("unconfirmed write response authorized leadership")
				}
				if g := a.Gate(); g != nil && g.Context().Err() == nil {
					t.Fatal("unconfirmed renewal did not cancel duties")
				}
			})
		}
	}
}

func TestEpochExhaustionNeverWritesOrAcquires(t *testing.T) {
	c := clock()
	s := &store{}
	a := elector(t, s, c, "a")
	a.Tick(context.Background())
	s.lease.Spec.LeaseTransitions = math.MaxInt32
	s.lease.Spec.HolderIdentity = ""
	b := elector(t, s, c, "b")
	b.Tick(context.Background())
	if s.writes != 1 {
		t.Fatal("exhausted epoch was incremented")
	}
	if _, ok := b.Gate().Epoch(); ok {
		t.Fatal("acquired an exhausted epoch")
	}
}
