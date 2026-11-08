package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

// The public seams are campaign Step, Gate.Write and the Kubernetes client.
// No election internals or real sleeps are used to observe duty writes.
type timer struct {
	at      time.Time
	fn      func()
	stopped bool
}

func (t *timer) Stop() bool { old := !t.stopped; t.stopped = true; return old }

type clock struct {
	now    time.Time
	timers []*timer
}

func newClock() *clock          { return &clock{now: time.Date(2026, 11, 8, 0, 0, 0, 0, time.UTC)} }
func (c *clock) Now() time.Time { return c.now }
func (c *clock) AfterFunc(d time.Duration, fn func()) leader.Timer {
	t := &timer{at: c.now.Add(d), fn: fn}
	c.timers = append(c.timers, t)
	return t
}
func (c *clock) advance(d time.Duration) {
	c.now = c.now.Add(d)
	for _, t := range c.timers {
		if !t.stopped && !t.at.After(c.now) {
			t.stopped = true
			t.fn()
		}
	}
}

type fakeAPI struct {
	mu           sync.Mutex
	lease        *leader.Lease
	version      int
	calls        []string
	writes       int
	fail         bool
	afterUpdate  func()
	beforeUpdate func()
	afterCreate  func() error
	missing      bool
	beforeGet    func()
}

func clone(l leader.Lease) leader.Lease {
	b, _ := json.Marshal(l)
	var out leader.Lease
	_ = json.Unmarshal(b, &out)
	return out
}
func seed() *fakeAPI {
	f := &fakeAPI{}
	f.lease = &leader.Lease{APIVersion: "coordination.k8s.io/v1", Kind: "Lease", Metadata: leader.Metadata{
		Name: leader.LeaseName, Namespace: "falcone-system", UID: "uid-1", ResourceVersion: "1",
		Labels:      map[string]string{"falcone.ai/owner": "pool-controller"},
		Annotations: map[string]string{"falcone.ai/owner": `["falcone-system","leader"]`, "falcone.ai/ownership-version": "1", leader.EpochAnnotation: "0", leader.EpochUIDAnnotation: "uid-1"},
	}}
	f.version = 1
	return f
}
func (f *fakeAPI) Get(_ context.Context, ns, name string) (leader.Lease, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "get "+ns+"/"+name)
	if f.beforeGet != nil {
		f.beforeGet()
	}
	if f.fail {
		return leader.Lease{}, errors.New("API unreachable")
	}
	if f.lease == nil || f.missing {
		return leader.Lease{}, leader.ErrNotFound
	}
	return clone(*f.lease), nil
}
func (f *fakeAPI) Create(_ context.Context, ns string, l leader.Lease) (leader.Lease, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "create "+ns+"/"+l.Metadata.Name)
	if f.fail {
		return leader.Lease{}, errors.New("API unreachable")
	}
	if f.lease != nil {
		return leader.Lease{}, leader.ErrConflict
	}
	f.version++
	l.Metadata.UID = "uid-recreated"
	l.Metadata.ResourceVersion = fmt.Sprint(f.version)
	f.lease = &l
	f.writes++
	if f.afterCreate != nil {
		if err := f.afterCreate(); err != nil {
			return leader.Lease{}, err
		}
	}
	return clone(l), nil
}
func (f *fakeAPI) Update(_ context.Context, ns string, l leader.Lease) (leader.Lease, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "update "+ns+"/"+l.Metadata.Name)
	if f.fail {
		return leader.Lease{}, errors.New("API unreachable")
	}
	if f.beforeUpdate != nil {
		f.beforeUpdate()
	}
	if f.lease == nil || l.Metadata.ResourceVersion != f.lease.Metadata.ResourceVersion || l.Metadata.UID != f.lease.Metadata.UID {
		return leader.Lease{}, leader.ErrConflict
	}
	f.version++
	l.Metadata.ResourceVersion = fmt.Sprint(f.version)
	f.lease = &l
	f.writes++
	if f.afterUpdate != nil {
		f.afterUpdate()
	}
	return clone(l), nil
}
func candidate(t *testing.T, api *fakeAPI, c *clock, name string) *leader.Elector {
	t.Helper()
	cfg := leader.DefaultConfig("falcone-system", name)
	cfg.PreflightPassed = true
	e, err := leader.New(cfg, api, c)
	if err != nil {
		t.Fatal(err)
	}
	return e
}
func write(t *testing.T, g *leader.Gate, fresh bool, tokens *[]leader.Fence) error {
	t.Helper()
	if g == nil {
		return errors.New("no gate")
	}
	return g.Write(fresh, func(ctx context.Context, token leader.Fence) error {
		if ctx.Err() != nil {
			t.Fatal("cancelled duty called")
		}
		*tokens = append(*tokens, token)
		return nil
	})
}

func TestTwoInstancesRenewAndHandOverWithFencing(t *testing.T) {
	f, c := seed(), newClock()
	a, b := candidate(t, f, c, "pod-a"), candidate(t, f, c, "pod-b")
	ctx := context.Background()
	if err := a.Step(ctx); err != nil {
		t.Fatal(err)
	}
	_ = b.Step(ctx)
	var reports []leader.Fence
	if err := write(t, a.Gate(), true, &reports); err != nil {
		t.Fatal(err)
	}
	if err := write(t, b.Gate(), true, &reports); err == nil {
		t.Fatal("standby wrote")
	}
	if reports[0].Epoch != 1 || reports[0].Seq != 1 {
		t.Fatalf("first fence: %+v", reports)
	}
	c.advance(2 * time.Second)
	if err := a.Step(ctx); err != nil {
		t.Fatal(err)
	}
	_ = b.Step(ctx)
	lastRenewal := c.Now()
	old := a.Gate()
	duty := old.Context()
	// A loses API access; its independent renewal timer must cancel duty work.
	f.fail = true
	c.advance(10 * time.Second)
	_ = a.Step(ctx)
	if duty.Err() == nil {
		t.Fatal("renewal failure did not cancel duty")
	}
	if err := write(t, old, true, &reports); err == nil {
		t.Fatal("expired leader wrote")
	}
	f.fail = false
	// B retries on its configured cadence. The observed last renewal at t=2
	// expires at t=17: every retry through t=16 must refuse takeover and writes.
	for i := 0; i < 3; i++ {
		writes := f.writes
		if err := b.Step(ctx); !errors.Is(err, leader.ErrStandby) || f.writes != writes {
			t.Fatalf("standby wrote before observed expiry at %v: %v", c.Now(), err)
		}
		if err := write(t, b.Gate(), true, &reports); err == nil {
			t.Fatal("standby duty wrote before observed expiry")
		}
		c.advance(2 * time.Second)
	}
	if err := b.Step(ctx); err != nil {
		t.Fatal(err)
	}
	if elapsed := c.Now().Sub(lastRenewal); elapsed > 17*time.Second {
		t.Fatalf("handover took %v, exceeds leaseDuration + retryPeriod", elapsed)
	}
	if err := write(t, b.Gate(), true, &reports); err != nil {
		t.Fatal(err)
	}
	if reports[1].Epoch != 2 || reports[1].Seq != 1 {
		t.Fatalf("handover fence: %+v", reports)
	}
	for _, call := range f.calls {
		if call != "get falcone-system/"+leader.LeaseName && call != "update falcone-system/"+leader.LeaseName {
			t.Fatalf("wrong namespace call: %s", call)
		}
	}
}

func TestPausedDeadlineCannotRenewTheSameTerm(t *testing.T) {
	f, c := seed(), newClock()
	e := candidate(t, f, c, "pod-a")
	ctx := context.Background()
	if err := e.Step(ctx); err != nil {
		t.Fatal(err)
	}
	old := e.Gate()
	// Model a process pause: the clock advances while timer delivery is paused.
	c.now = c.now.Add(11 * time.Second)
	if err := e.Step(ctx); err == nil {
		t.Fatal("resurrected an expired term before timers ran")
	}
	if old.Context().Err() == nil {
		t.Fatal("expired duty context still live")
	}
}

func TestUnverifiedOwnershipAndEpochNeverWritten(t *testing.T) {
	for name, damage := range map[string]func(*leader.Lease){
		"missing owner":   func(l *leader.Lease) { delete(l.Metadata.Annotations, "falcone.ai/owner") },
		"malformed owner": func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = "[" },
		"null owner":      func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = "null" },
		"wrong namespace": func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = `["pool-a","leader"]` },
		"wrong duty":      func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/owner"] = `["falcone-system","scaler"]` },
		"extra identity": func(l *leader.Lease) {
			l.Metadata.Annotations["falcone.ai/owner"] = `["falcone-system","leader","extra"]`
		},
		"wrong label":             func(l *leader.Lease) { l.Metadata.Labels["falcone.ai/owner"] = "other" },
		"wrong version":           func(l *leader.Lease) { l.Metadata.Annotations["falcone.ai/ownership-version"] = "2" },
		"no UID":                  func(l *leader.Lease) { l.Metadata.UID = "" },
		"no resourceVersion":      func(l *leader.Lease) { l.Metadata.ResourceVersion = "" },
		"missing epoch":           func(l *leader.Lease) { delete(l.Metadata.Annotations, leader.EpochAnnotation) },
		"malformed epoch":         func(l *leader.Lease) { l.Metadata.Annotations[leader.EpochAnnotation] = "1.0" },
		"negative epoch":          func(l *leader.Lease) { l.Metadata.Annotations[leader.EpochAnnotation] = "-1" },
		"overflow epoch":          func(l *leader.Lease) { l.Metadata.Annotations[leader.EpochAnnotation] = "9223372036854775807" },
		"copied epoch on new UID": func(l *leader.Lease) { l.Metadata.UID = "new-uid" },
	} {
		t.Run(name, func(t *testing.T) {
			f, c := seed(), newClock()
			damage(f.lease)
			e := candidate(t, f, c, "pod-a")
			for i := 0; i < 3; i++ {
				_ = e.Step(context.Background())
				c.advance(time.Minute)
			}
			_ = e.Release(context.Background())
			if f.writes != 0 || e.Gate() != nil {
				t.Fatalf("adopted unverified Lease: writes %d", f.writes)
			}
		})
	}
}

func TestDegradedDutiesDoNotWrite(t *testing.T) {
	f, c := seed(), newClock()
	e := candidate(t, f, c, "pod-a")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := e.Step(ctx); err != nil {
		t.Fatal(err)
	}
	var reports []leader.Fence
	// Both unavailable and stale desired state are represented by false; neither
	// authorizes reconciliation, resource deletion or a status write.
	for _, mode := range []string{"unavailable desired state", "stale desired state"} {
		t.Run(mode, func(t *testing.T) {
			if err := write(t, e.Gate(), false, &reports); err == nil {
				t.Fatal("degraded write accepted")
			}
		})
	}
	f.fail = true
	c.advance(10 * time.Second)
	_ = e.Step(ctx)
	if err := write(t, e.Gate(), true, &reports); err == nil {
		t.Fatal("wrote after API renewal deadline")
	}
	cancel()
	if err := write(t, e.Gate(), true, &reports); err == nil {
		t.Fatal("wrote after cancellation")
	}
	if len(reports) != 0 {
		t.Fatalf("degraded writes: %v", reports)
	}
	bad := leader.DefaultConfig("falcone-system", "pod-b")
	if _, err := leader.New(bad, f, c); err == nil {
		t.Fatal("failed preflight allowed campaigning")
	}
}

// Reference receiver: authorization and desired generation checks precede the
// epoch guard, then lexicographic (generation,epoch,seq) ordering (contract G9).
type observation struct {
	generation int64
	fence      leader.Fence
}
type receiver struct{ stored observation }

func (r *receiver) accept(o observation) bool {
	if o.generation > 7 || o.fence.Epoch < r.stored.fence.Epoch {
		return false
	}
	s := r.stored
	if o.generation < s.generation || (o.generation == s.generation && (o.fence.Epoch < s.fence.Epoch || (o.fence.Epoch == s.fence.Epoch && o.fence.Seq <= s.fence.Seq))) {
		return false
	}
	r.stored = o
	return true
}
func checkFenced(t *testing.T, old, new leader.Fence) {
	t.Helper()
	r := receiver{}
	if !r.accept(observation{5, old}) || !r.accept(observation{5, new}) {
		t.Fatal("receiver refused current terms")
	}
	old.Seq = 999999
	if r.accept(observation{7, old}) {
		t.Fatal("receiver accepted superseded epoch with higher generation/sequence")
	}
}
func emitted(t *testing.T, g *leader.Gate) leader.Fence {
	t.Helper()
	var tokens []leader.Fence
	if err := write(t, g, true, &tokens); err != nil {
		t.Fatal(err)
	}
	return tokens[0]
}

func TestRestartAndGracefulReleaseKeepEpoch(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "same-pod")
	ctx, cancel := context.WithCancel(context.Background())
	if err := a.Step(ctx); err != nil {
		t.Fatal(err)
	}
	old := emitted(t, a.Gate())
	cancel()
	if a.Gate().Context().Err() == nil {
		t.Fatal("shutdown did not cancel duty")
	}
	if err := a.Release(context.Background()); err != nil {
		t.Fatal(err)
	}
	if f.lease.Spec.HolderIdentity != "" || f.lease.Metadata.Annotations[leader.EpochAnnotation] != "1" {
		t.Fatal("release discarded epoch or kept holder")
	}
	b := candidate(t, f, c, "same-pod") // No election memory survives this restart.
	if err := b.Step(context.Background()); err != nil {
		t.Fatal(err)
	}
	new := emitted(t, b.Gate())
	checkFenced(t, old, new)
	writes := f.writes
	if err := a.Release(context.Background()); err == nil || f.writes != writes {
		t.Fatal("released another holder's term")
	}
}

func TestRestartWithoutReleaseUsesNewTerm(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "same-pod")
	ctx := context.Background()
	_ = a.Step(ctx)
	old := emitted(t, a.Gate())
	b := candidate(t, f, c, "same-pod")
	if err := b.Step(ctx); err == nil {
		t.Fatal("restart renewed former process term")
	}
	c.advance(15 * time.Second)
	if err := b.Step(ctx); err != nil {
		t.Fatal(err)
	}
	new := emitted(t, b.Gate())
	if new.Epoch != 2 || new.Seq != 1 {
		t.Fatalf("restart reused fence: %+v", new)
	}
	checkFenced(t, old, new)
}

func TestLeaseDeletionRequiresTrustedContinuityEvenAfterRestart(t *testing.T) {
	for _, timings := range []struct {
		name         string
		lease, renew time.Duration
	}{{"defaults", 15 * time.Second, 10 * time.Second}, {"unequal", 60 * time.Second, 50 * time.Second}} {
		t.Run(timings.name, func(t *testing.T) {
			f, ca, cb := seed(), newClock(), newClock()
			cfg := leader.DefaultConfig("falcone-system", "pod-a")
			cfg.PreflightPassed = true
			cfg.LeaseDuration = timings.lease
			cfg.RenewDeadline = timings.renew
			a, err := leader.New(cfg, f, ca)
			if err != nil {
				t.Fatal(err)
			}
			ctx := context.Background()
			_ = a.Step(ctx)
			old := emitted(t, a.Gate())
			// B misses A's renewals, then finds a deleted Lease. A's local
			// clock is partitioned/paused; its gate can still believe it leads.
			b := candidate(t, f, cb, "pod-b")
			_ = b.Step(ctx)
			ca.advance(2 * time.Second)
			_ = a.Step(ctx)
			f.lease = nil
			for i := 0; i < 4; i++ {
				if err := b.Step(ctx); !errors.Is(err, leader.ErrHistory) {
					t.Fatalf("unknown history: %v", err)
				}
				cb.advance(time.Minute)
			}
			if b.Gate() != nil {
				t.Fatal("deleted Lease silently seeded epoch")
			}
			// Copied metadata on a recreated object is invalid even to a fresh
			// process; its UID binding still names the deleted incarnation.
			copied := clone(*seed().lease)
			copied.Metadata.UID = "uid-recreated"
			f.lease = &copied
			fresh := candidate(t, f, cb, "pod-restart")
			writes := f.writes
			if err := fresh.Step(ctx); !errors.Is(err, leader.ErrHistory) || f.writes != writes {
				t.Fatal("restart adopted copied epoch")
			}
			// An external trusted recovery has established a floor >= every
			// prior persisted epoch and bound it to the replacement's UID.
			f.lease.Metadata.Annotations[leader.EpochAnnotation] = "1"
			f.lease.Metadata.Annotations[leader.EpochUIDAnnotation] = f.lease.Metadata.UID
			fresh = candidate(t, f, cb, "pod-restart-2")
			if err := fresh.Step(ctx); err != nil {
				t.Fatal(err)
			}
			new := emitted(t, fresh.Gate())
			checkFenced(t, old, new)
			// Actual old leader can still issue a write in the overlap window;
			// its token is rejected after the newer epoch has published.
			stale := emitted(t, a.Gate())
			r := receiver{}
			if !r.accept(observation{5, new}) || r.accept(observation{7, stale}) {
				t.Fatal("overlap was not fenced")
			}
		})
	}
}

func TestClockDriftOverlapIsFenced(t *testing.T) {
	f, ca, cb := seed(), newClock(), newClock()
	a, b := candidate(t, f, ca, "pod-a"), candidate(t, f, cb, "pod-b")
	ctx := context.Background()
	_ = a.Step(ctx)
	old := emitted(t, a.Gate())
	_ = b.Step(ctx)
	// A's slow clock still considers its deadline valid when B's lease expires.
	ca.advance(time.Second)
	cb.advance(15 * time.Second)
	if err := b.Step(ctx); err != nil {
		t.Fatal(err)
	}
	new := emitted(t, b.Gate())
	checkFenced(t, old, new)
	stale := emitted(t, a.Gate())
	r := receiver{}
	_ = r.accept(observation{5, new})
	if r.accept(observation{7, stale}) {
		t.Fatal("drift bypassed epoch guard")
	}
}

func TestDelayedAcquisitionCannotOpenExpiredGate(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "pod-a")
	f.afterUpdate = func() { c.advance(11 * time.Second) }
	if err := a.Step(context.Background()); err == nil || a.Gate() != nil {
		t.Fatal("late acquisition opened a gate")
	}
	// The successful but delayed write still consumed its persisted epoch.
	f.afterUpdate = nil
	b := candidate(t, f, c, "pod-b")
	_ = b.Step(context.Background())
	c.advance(15 * time.Second)
	if err := b.Step(context.Background()); err != nil {
		t.Fatal(err)
	}
	if got := emitted(t, b.Gate()); got.Epoch != 2 {
		t.Fatalf("late term epoch reused: %+v", got)
	} else {
		checkFenced(t, leader.Fence{Epoch: 1, Seq: 999}, got)
	}
}

func TestRepeated404AndAmbiguousCreateNeverEstablishFloor(t *testing.T) {
	for _, ambiguous := range []bool{false, true} {
		t.Run(fmt.Sprint(ambiguous), func(t *testing.T) {
			f, c := seed(), newClock()
			former := candidate(t, f, c, "former-pod")
			if err := former.Step(context.Background()); err != nil {
				t.Fatal(err)
			}
			old := emitted(t, former.Gate())
			f.lease = nil
			f.afterCreate = func() error {
				c.advance(time.Minute)
				if ambiguous {
					return errors.New("response lost")
				}
				return nil
			}
			a := candidate(t, f, c, "pod-a")
			_ = a.Step(context.Background())
			f.missing = true
			for i := 0; i < 4; i++ {
				_ = a.Step(context.Background())
				c.advance(time.Minute)
			}
			b := candidate(t, f, c, "pod-restart")
			_ = b.Step(context.Background())
			if a.Gate() != nil || b.Gate() != nil || f.lease.Metadata.Annotations[leader.EpochAnnotation] != "" || f.writes != 2 {
				t.Fatal("unknown history became publishable")
			}
			f.missing = false
			f.lease.Metadata.Annotations[leader.EpochAnnotation] = "1"
			f.lease.Metadata.Annotations[leader.EpochUIDAnnotation] = f.lease.Metadata.UID
			recovered := candidate(t, f, c, "recovered-pod")
			if err := recovered.Step(context.Background()); err != nil {
				t.Fatal(err)
			}
			checkFenced(t, old, emitted(t, recovered.Gate()))
		})
	}
}

func TestEveryUpdateUsesTheSameFreshVerifiedRead(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "pod-a")
	ctx := context.Background()
	_ = a.Step(ctx)
	// An unrelated metadata write between renewals must not cause a permanent
	// conflict: use the newly read version and preserve its other annotations.
	f.version++
	f.lease.Metadata.ResourceVersion = fmt.Sprint(f.version)
	f.lease.Metadata.Annotations["example.com/keep"] = "yes"
	c.advance(time.Second)
	if err := a.Step(ctx); err != nil || f.lease.Metadata.Annotations["example.com/keep"] != "yes" {
		t.Fatal("did not use fresh verified read")
	}
	writes := f.writes
	f.beforeUpdate = func() {
		f.version++
		f.lease.Metadata.ResourceVersion = fmt.Sprint(f.version)
		f.lease.Metadata.Annotations["falcone.ai/owner"] = `["foreign","leader"]`
	}
	if err := a.Release(ctx); !errors.Is(err, leader.ErrConflict) || f.writes != writes {
		t.Fatal("release bypassed optimistic concurrency")
	}
	f.beforeUpdate = nil
	if err := a.Step(ctx); !errors.Is(err, leader.ErrOwnership) || f.writes != writes {
		t.Fatal("retried adoption without ownership check")
	}
}

func TestIdentityAndTimingValidationBeforeCampaign(t *testing.T) {
	for name, edit := range map[string]func(*leader.Config){
		"missing pod":        func(c *leader.Config) { c.PodName = "" },
		"invalid pod":        func(c *leader.Config) { c.PodName = "../../pod" },
		"empty pod label":    func(c *leader.Config) { c.PodName = "pod..name" },
		"namespace mismatch": func(c *leader.Config) { c.Namespace = "pool-a" },
		"missing namespace":  func(c *leader.Config) { c.Namespace = "" },
		"failed preflight":   func(c *leader.Config) { c.PreflightPassed = false },
		"unbounded lease":    func(c *leader.Config) { c.LeaseDuration = 61 * time.Second },
		"renew equals lease": func(c *leader.Config) { c.RenewDeadline = c.LeaseDuration },
		"retry equals renew": func(c *leader.Config) { c.RetryPeriod = c.RenewDeadline },
	} {
		t.Run(name, func(t *testing.T) {
			f := seed()
			cfg := leader.DefaultConfig("falcone-system", "pod-a")
			cfg.PreflightPassed = true
			edit(&cfg)
			if _, err := leader.New(cfg, f, newClock()); err == nil || len(f.calls) != 0 {
				t.Fatal("invalid startup reached Lease API")
			}
		})
	}
}

func TestRunCancellationClosesDutyAndReleasesLease(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "pod-a")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	var dutyErr error
	var dutyContext context.Context
	var token leader.Fence
	a.Run(ctx, func(g *leader.Gate) {
		defer close(done)
		dutyContext = g.Context()
		dutyErr = g.Write(true, func(_ context.Context, f leader.Fence) error { token = f; cancel(); return nil })
	}, nil)
	<-done
	if dutyErr != nil || dutyContext.Err() == nil || token.Epoch != 1 {
		t.Fatalf("duty not cancelled: token %+v, error %v", token, dutyErr)
	}
	if f.lease.Spec.HolderIdentity != "" || f.lease.Metadata.Annotations[leader.EpochAnnotation] != "1" {
		t.Fatal("shutdown did not preserve epoch and release holder")
	}
	var reports []leader.Fence
	if err := write(t, a.Gate(), true, &reports); err == nil {
		t.Fatal("shutdown gate wrote")
	}
	// Empty-holder release permits immediate standby acquisition.
	b := candidate(t, f, c, "pod-b")
	if err := b.Step(context.Background()); err != nil {
		t.Fatal(err)
	}
	if got := emitted(t, b.Gate()); got.Epoch != 2 {
		t.Fatalf("release reused epoch: %+v", got)
	}
}

// Advance retry scheduling synchronously so Run can be driven without sleeps or
// concurrent access to the fake clock. Duty deadlines remain ordinary timers.
type retryClock struct {
	*clock
	retry time.Duration
}

func (c retryClock) AfterFunc(d time.Duration, fn func()) leader.Timer {
	if d == c.retry {
		c.advance(d)
		fn()
		return &timer{stopped: true}
	}
	return c.clock.AfterFunc(d, fn)
}

func TestRunReportsOnlyErrorTransitions(t *testing.T) {
	f, c := seed(), newClock()
	cfg := leader.DefaultConfig("falcone-system", "pod-a")
	cfg.PreflightPassed = true
	e, err := leader.New(cfg, f, retryClock{clock: c, retry: cfg.RetryPeriod})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	delete(f.lease.Metadata.Annotations, leader.EpochAnnotation)
	reads := 0
	f.beforeGet = func() {
		reads++
		switch reads {
		case 4:
			// Healthy acquisition resets the error state between two runs of
			// the same degraded state.
			f.lease.Metadata.Annotations[leader.EpochAnnotation] = "0"
		case 5:
			delete(f.lease.Metadata.Annotations, leader.EpochAnnotation)
		case 8:
			cancel()
		}
	}
	var observed []error
	e.Run(ctx, nil, func(err error) { observed = append(observed, err) })
	if len(observed) != 2 || !errors.Is(observed[0], leader.ErrHistory) || !errors.Is(observed[1], leader.ErrHistory) {
		t.Fatalf("expected one diagnostic per transition into missing history, got %v", observed)
	}
}

func TestPoolStatusPublicationOverridesCallerFenceAndConsumesFailedSequence(t *testing.T) {
	f, c := seed(), newClock()
	a := candidate(t, f, c, "pod-a")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := a.Step(ctx); err != nil {
		t.Fatal(err)
	}
	input := poolstatus.Report{ReporterEpoch: 999, ObservationSeq: 999}
	var reports []poolstatus.Report
	publish := func(ctx context.Context, r poolstatus.Report) error {
		if ctx.Err() != nil {
			t.Fatal("cancelled publisher invoked")
		}
		reports = append(reports, r)
		return errors.New("delivery failed")
	}
	for i := 0; i < 2; i++ {
		if err := a.Gate().PublishReport(true, input, publish); err == nil {
			t.Fatal("delivery failure lost")
		}
	}
	if len(reports) != 2 || reports[0].ReporterEpoch != 1 || reports[0].ObservationSeq != 1 || reports[1].ReporterEpoch != 1 || reports[1].ObservationSeq != 2 {
		t.Fatalf("unfenced report or reused sequence: %+v", reports)
	}
	if err := a.Gate().PublishReport(false, input, publish); !errors.Is(err, leader.ErrStandby) {
		t.Fatal("stale desired state published")
	}
	cancel()
	if err := a.Gate().PublishReport(true, input, publish); !errors.Is(err, leader.ErrStandby) {
		t.Fatal("cancelled duty published")
	}
	if len(reports) != 2 || input.ReporterEpoch != 999 || input.ObservationSeq != 999 {
		t.Fatal("degraded write or caller mutation")
	}
}

func TestLongValidPodNameHasBoundedHolderIdentity(t *testing.T) {
	f, c := seed(), newClock()
	name := strings.Repeat("a", 63) + "." + strings.Repeat("b", 63) + "." + strings.Repeat("c", 63) + "." + strings.Repeat("d", 61)
	a := candidate(t, f, c, name)
	if err := a.Step(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.lease.Spec.HolderIdentity) > 128 {
		t.Fatal("valid pod name produced an oversized Lease holder")
	}
}
