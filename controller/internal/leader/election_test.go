package leader_test

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

const sec = time.Second

var defaults = leader.DefaultTimings

// holdFirst ticks inst every retry period until it holds, with a hard stop.
func holdFirst(t *testing.T, clock *fakeClock, inst *instance) {
	t.Helper()
	for n := 0; n < 40 && !inst.open(); n++ {
		inst.tick()
		if inst.open() {
			return
		}
		clock.Advance(2 * sec)
	}
	if !inst.open() {
		t.Fatalf("%s never acquired", inst.name)
	}
}

func TestDefaultTimingsAreTheContractDefaults(t *testing.T) {
	if defaults.LeaseDuration != 15*sec || defaults.RenewDeadline != 10*sec || defaults.RetryPeriod != 2*sec {
		t.Fatalf("defaults = %+v, want 15s/10s/2s", defaults)
	}
	if err := defaults.Validate(); err != nil {
		t.Fatal(err)
	}
}

// TestExactlyOneInstanceHoldsAndRunsTheDuty drives two replicas against one store.
func TestExactlyOneInstanceHoldsAndRunsTheDuty(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	for s := 0; s < 60; s += 2 {
		a.tick()
		b.tick()
		if a.open() && b.open() {
			t.Fatalf("t=%ds: both gates are open", s)
		}
		clock.Advance(2 * sec)
	}
	if !a.open() && !b.open() {
		t.Fatal("nobody holds the Lease after 60s")
	}
	holder, other := a, b
	if b.open() {
		holder, other = b, a
	}
	holder.duty.await(t, "the leader's duty to run", func() bool { r, _, _ := holder.duty.snapshot(); return r == 1 })
	if r, s, _ := other.duty.snapshot(); r != 0 || s != 0 {
		t.Fatalf("the non-leader ran its duty: running %d started %d", r, s)
	}
	l := store.current()
	if l == nil || l.Holder != holder.name+"-0123abcd" {
		t.Fatalf("stored holder = %+v, want %s", l, holder.name)
	}
	if l.Labels["falcone.ai/owner"] != "pool-controller" || l.Annotations["falcone.ai/ownership-version"] != "1" {
		t.Fatalf("Lease lacks the ownership metadata: %+v %+v", l.Labels, l.Annotations)
	}
	if l.LeaseDurationSeconds != 15 {
		t.Fatalf("leaseDurationSeconds = %d, want 15", l.LeaseDurationSeconds)
	}
}

// TestHandoverTimingAfterRenewalsStop covers the takeover window: B acquires no
// earlier than leaseDuration after the last record change it observed, and no later
// than leaseDuration + 2 x retryPeriod after A's last successful renewal. A's gate
// closes at its last renewal start + renewDeadline and its duty is cancelled.
func TestHandoverTimingAfterRenewalsStop(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	var lastRenew time.Duration
	var prevRV string
	acquiredAt := time.Duration(-1)
	partitioned := false
	for step := 0; step < 80; step++ {
		if !partitioned && step == 10 {
			a.client.setDown(true) // A stops renewing: its renewals fail from here
			partitioned = true
		}
		a.tick()
		b.tick()
		if l := store.current(); l != nil && l.ResourceVersion != prevRV {
			prevRV = l.ResourceVersion
			if l.Holder == "a-0123abcd" {
				lastRenew = clock.since()
			}
			if l.Holder == "b-0123abcd" && acquiredAt < 0 {
				acquiredAt = clock.since()
			}
		}
		if a.open() && b.open() {
			t.Fatalf("at %v both gates are open", clock.since())
		}
		if acquiredAt >= 0 {
			break
		}
		clock.Advance(2 * sec)
	}
	if acquiredAt < 0 {
		t.Fatal("B never took over")
	}
	if got := acquiredAt - lastRenew; got < defaults.LeaseDuration || got > defaults.LeaseDuration+2*defaults.RetryPeriod {
		t.Fatalf("takeover %v after A's last renewal; want within [%v, %v]", got, defaults.LeaseDuration, defaults.LeaseDuration+2*defaults.RetryPeriod)
	}
	if a.open() {
		t.Fatal("A's gate is still open after B took over")
	}
	a.duty.await(t, "A's duty to be cancelled", func() bool { r, _, c := a.duty.snapshot(); return r == 0 && c == 1 })
	b.duty.await(t, "B's duty to run", func() bool { r, _, _ := b.duty.snapshot(); return r == 1 })
	if tok, ok := b.el.Token(); !ok || tok.Epoch != 1 {
		t.Fatalf("B's token = %+v, %v; want epoch 1 (one transition)", tok, ok)
	}
}

// TestDeletedLeaseDoesNotOpenASecondGate is the second-review split-brain: B sees
// A's Lease at t=0, loses API access while A renews through t=14, the Lease is
// deleted at t=15 and B sees 404 at t=16 while A is partitioned and keeps a valid
// gate until t=24. B must send no create and open no gate before t=31.
func TestDeletedLeaseDoesNotOpenASecondGate(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	// Align on the exact timeline: B's first tick is t=0, A renews at every even t.
	t0 := clock.Now()
	at := func() time.Duration { return clock.Now().Sub(t0) }
	// Move to a tick boundary shared with A's renewal cadence: A ticks at t0 now.
	for step := 0; step <= 40; step++ {
		s := at()
		switch s {
		case 2 * sec:
			b.client.setDown(true) // B loses API access after observing A at t=0
		case 16 * sec:
			b.client.setDown(false) // B regains access
			a.client.setDown(true)  // A is partitioned from t=16 on
		}
		if s == 15*sec {
			store.remove() // the Lease is deleted
		}
		if s%(2*sec) == 0 {
			a.tick()
			b.tick()
		}
		aOpen, bOpen := a.open(), b.open()
		if aOpen && bOpen {
			t.Fatalf("t=%v: both gates are open", s)
		}
		if wantA := s < 24*sec; aOpen != wantA {
			t.Fatalf("t=%v: A's gate open = %v, want %v (last renewal at 14, renewDeadline 10)", s, aOpen, wantA)
		}
		if creates, _, _ := b.client.counts(); s < 31*sec && (creates != 0 || bOpen) {
			t.Fatalf("t=%v: B sent %d creates, gate open %v; want none before t=31", s, creates, bOpen)
		}
		if s == 32*sec && !bOpen {
			t.Fatalf("t=%v: B should have acquired after the barrier", s)
		}
		clock.Advance(sec)
	}
	if l := store.current(); l == nil || l.Holder != "b-0123abcd" {
		t.Fatalf("final Lease = %+v, want held by B", l)
	}
	// B recreated a deleted Lease, so its fencing token continues past the highest
	// leaseTransitions it knew (A took the empty store at 0, so B must be at 1 or more).
	if tok, ok := b.el.Token(); !ok || tok.Epoch < 1 {
		t.Fatalf("B's epoch = %+v, want a value above A's", tok)
	}
}

// TestDeletionAfterAnUnseenRenewalDoesNotOpenASecondGate is the second checker's
// split-brain: B reads A's unchanged record at 0, 2, 4, 6 and 8 (A's renewals at 2,
// 4 and 6 failed), A renews right after B's read at 8, the Lease is deleted at 9 and
// A is partitioned. B's reads are continuous, yet it cannot know about the renewal
// at 8: A's gate stays open until 18. B must treat the 404 at 10 as lost history,
// create nothing before 25 (first success plus leaseDuration) and never overlap A.
func TestDeletionAfterAnUnseenRenewalDoesNotOpenASecondGate(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	t0 := clock.Now()
	a.client.setFault("update", errNetwork) // A's renewals at 2, 4 and 6 fail
	for step := 0; step <= 40; step++ {
		s := clock.Now().Sub(t0)
		switch s {
		case 8 * sec:
			a.client.setFault("update", nil) // B reads first, then A renews
		case 9 * sec:
			store.remove()
			a.client.setDown(true)
		}
		if s%(2*sec) == 0 {
			b.tick()
			a.tick()
		}
		aOpen, bOpen := a.open(), b.open()
		if aOpen && bOpen {
			t.Fatalf("t=%v: both gates are open", s)
		}
		if creates, _, _ := b.client.counts(); s < 25*sec && (creates != 0 || bOpen) {
			t.Fatalf("t=%v: B sent %d creates, gate open %v; want none before t=25", s, creates, bOpen)
		}
		if wantA := s < 18*sec; aOpen != wantA {
			t.Fatalf("t=%v: A's gate open = %v, want %v (last renewal at 8, renewDeadline 10)", s, aOpen, wantA)
		}
		clock.Advance(sec)
	}
	if !b.open() {
		t.Fatal("B should acquire once the recovery barrier passed")
	}
}

// TestBarrierOnNoHistory404 — a process with no history that sees 404 waits a full
// lease duration from its first successful API response before it creates.
func TestBarrierOnNoHistory404(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	b := newInstance(t, clock, store, "b", defaults)
	b.client.setDown(true)
	for s := time.Duration(0); s < 6*sec; s += 2 * sec { // API down: nothing is learned
		b.tick()
		clock.Advance(2 * sec)
	}
	b.client.setDown(false)
	first := clock.Now() // first successful response
	for clock.Now().Sub(first) < defaults.LeaseDuration {
		b.tick()
		if creates, _, _ := b.client.counts(); creates != 0 || b.open() {
			t.Fatalf("%v after the first success: created %d, open %v", clock.Now().Sub(first), creates, b.open())
		}
		clock.Advance(2 * sec)
	}
	b.tick()
	if !b.open() {
		t.Fatalf("B should acquire once leaseDuration passed (at %v)", clock.Now().Sub(first))
	}
}

// TestAPIErrorDuringBarrierRestartsIt: an error while the barrier runs restarts it
// from the next successful response.
func TestAPIErrorDuringBarrierRestartsIt(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	b := newInstance(t, clock, store, "b", defaults)
	b.tick() // barrier starts at 0
	clock.Advance(12 * sec)
	b.client.setFault("get", errNetwork)
	b.tick() // error at 12
	clock.Advance(2 * sec)
	b.client.setFault("get", nil)
	b.tick() // next success at 14 restarts the barrier
	restart := clock.Now()
	clock.Advance(2 * sec)
	b.tick() // 16: would have passed 15 from the original start
	if creates, _, _ := b.client.counts(); creates != 0 {
		t.Fatalf("B created %d times before the restarted barrier ended", creates)
	}
	for clock.Now().Sub(restart) < defaults.LeaseDuration {
		clock.Advance(sec)
		b.tick()
		if creates, _, _ := b.client.counts(); creates != 0 && clock.Now().Sub(restart) < defaults.LeaseDuration {
			t.Fatalf("created %v after the restart", clock.Now().Sub(restart))
		}
	}
	b.tick()
	if !b.open() {
		t.Fatal("B should acquire after the restarted barrier")
	}
}

// TestReappearingLeaseCancelsTheBarrier: the observed Lease ends the barrier and
// normal observation applies; a later 404 starts a barrier of its own.
func TestReappearingLeaseCancelsTheBarrier(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	b := newInstance(t, clock, store, "b", defaults)
	b.tick()
	clock.Advance(4 * sec)
	store.seed(leader.Lease{Name: leader.LeaseName, Holder: "other", LeaseDurationSeconds: 15,
		Labels:      map[string]string{"falcone.ai/owner": "pool-controller"},
		Annotations: map[string]string{"falcone.ai/ownership-version": "1"}})
	b.tick() // sees the Lease: the barrier is cancelled and normal observation applies
	clock.Advance(sec)
	b.tick() // still the same record, held by someone else
	if creates, _, _ := b.client.counts(); creates != 0 || b.open() {
		t.Fatal("B must not act on a Lease held by another instance")
	}
	store.remove()
	clock.Advance(sec)
	b.tick() // the 404 starts a new barrier here, whatever was observed before
	gone := clock.Now()
	for clock.Now().Sub(gone) < defaults.LeaseDuration {
		if creates, _, _ := b.client.counts(); creates != 0 || b.open() {
			t.Fatalf("created %v after the 404", clock.Now().Sub(gone))
		}
		clock.Advance(sec)
		b.tick()
	}
	if !b.open() {
		t.Fatal("B should acquire once leaseDuration passed since the 404")
	}
}

func TestHolderThatGetsNotFoundOrConflictLosesItsGateAtOnce(t *testing.T) {
	for _, tc := range []struct {
		name string
		edit func(*fakeStore)
	}{
		{"404 on renew", func(s *fakeStore) { s.remove() }},
		{"409 on renew", func(s *fakeStore) {
			l := s.current()
			s.seed(*l) // different uid/resourceVersion: our update conflicts
		}},
		{"another holder", func(s *fakeStore) {
			l := s.current()
			l.Holder = "intruder"
			s.seed(*l)
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			clock, store := newFakeClock(), &fakeStore{}
			a := newInstance(t, clock, store, "a", defaults)
			holdFirst(t, clock, a)
			tc.edit(store)
			clock.Advance(2 * sec)
			creates, _, _ := a.client.counts()
			a.tick()
			if a.open() {
				t.Fatal("gate still open after the Lease was lost")
			}
			if after, _, _ := a.client.counts(); after != creates {
				t.Fatalf("a holder recreated the Lease while its gate was open (%d -> %d creates)", creates, after)
			}
			a.duty.await(t, "the duty to be cancelled", func() bool { r, _, c := a.duty.snapshot(); return r == 0 && c == 1 })
			// Back to candidate: with the Lease gone it must wait out a barrier, not recreate at once.
			clock.Advance(2 * sec)
			a.tick()
			if a.open() {
				t.Fatal("candidate acquired immediately")
			}
		})
	}
}

func TestHungRenewalClosesTheGateAtItsDeadline(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	holdFirst(t, clock, a)
	clock.Advance(2 * sec)
	a.tick() // a good renewal; its start is the new anchor
	lastStart := clock.Now()
	clock.Advance(2 * sec)
	a.client.hang = make(chan struct{})
	done := make(chan struct{})
	base := clock.pending()
	go func() { a.tick(); close(done) }()
	clock.waitTimers(t, base+1)
	// the request is hung; advance to the renew deadline measured from the last renewal start
	for clock.Now().Sub(lastStart) < defaults.RenewDeadline-sec {
		clock.Advance(sec)
		if !a.open() {
			t.Fatalf("gate closed %v after the last renewal, before the deadline", clock.Now().Sub(lastStart))
		}
	}
	clock.Advance(sec)
	if a.open() {
		t.Fatalf("a hung renewal kept the gate open %v after the last renewal start", clock.Now().Sub(lastStart))
	}
	close(a.client.hang)
	<-done
}

// TestPendingRenewalDoesNotDelayCancellationPastTheGateDeadline runs the real Run
// loop. A renewal starts 0.5s before the gate expires and hangs. The duty must be
// cancelled at the deadline, not at the request timeout, and the late response must
// not reopen the gate. A standby, which saw the unchanged record from the last
// renewal, takes over no earlier than a lease duration later, after the old duty's
// drain bound.
func TestPendingRenewalDoesNotDelayCancellationPastTheGateDeadline(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	a.duty.stuck = true // the duty ignores cancellation until released
	holdFirst(t, clock, a)
	t0 := clock.since() // the acquisition request started here
	b.tick()            // b observes the record a wrote, at t0
	a.client.hang = make(chan struct{})
	clock.Advance(9500 * time.Millisecond) // 0.5s before the gate deadline, with no tick between
	if !a.open() {
		t.Fatal("gate closed before its deadline")
	}
	base := clock.pending()
	ctx, stop := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { a.el.Run(ctx); close(done) }()
	clock.waitTimers(t, base+2) // the request timeout and the gate-expiry timer
	clock.Advance(500 * time.Millisecond)
	a.duty.await(t, "the duty to be cancelled at the deadline", func() bool {
		running, _, cancelled := a.duty.snapshot()
		return running == 1 && a.duty.ctxDone() && cancelled == 0
	})
	if a.open() {
		t.Fatal("gate open at its deadline while the renewal was pending")
	}
	if got := clock.since() - t0; got != 10*sec {
		t.Fatalf("test is out of step: %v after the last renewal, want 10s", got)
	}
	close(a.client.hang) // the late response arrives; a stale success must not reopen the gate
	if a.open() {
		t.Fatal("a response after the deadline reopened the gate")
	}
	// The standby must not take over before a lease duration after its observation.
	clock.Advance(4 * sec) // t0+14s
	b.tick()
	if b.open() || a.open() {
		t.Fatalf("t0+14s: a open=%v b open=%v", a.open(), b.open())
	}
	close(a.duty.release) // the old duty finishes inside the drain bound (t0+15s)
	a.duty.await(t, "the old duty to finish", func() bool { r, _, c := a.duty.snapshot(); return r == 0 && c == 1 })
	clock.Advance(sec) // t0+15s
	b.tick()
	if !b.open() {
		t.Fatal("standby did not take over a lease duration after its observation")
	}
	if a.open() {
		t.Fatal("both gates open")
	}
	stop()
	<-done
}

// TestRenewalCompletingAfterTheOldDeadlineNeverReopensTheGate makes the renewal
// succeed only after the clock passed the gate deadline.
func TestRenewalCompletingAfterTheOldDeadlineNeverReopensTheGate(t *testing.T) {
	for i := 0; i < 50; i++ {
		clock, store := newFakeClock(), &fakeStore{}
		a := newInstance(t, clock, store, "a", defaults)
		holdFirst(t, clock, a)
		clock.Advance(9500 * time.Millisecond)
		a.client.afterUpdate = func() { clock.Advance(sec) } // the response arrives at deadline + 0.5s
		a.tick()
		if a.open() {
			t.Fatal("a renewal completing after the old deadline reopened the gate")
		}
		a.duty.await(t, "the duty to be cancelled", func() bool { r, _, c := a.duty.snapshot(); return r == 0 && c == 1 })
		a.client.afterUpdate = nil
		clock.Advance(2 * sec)
		a.tick()
		if a.open() {
			t.Fatal("gate reopened without a new acquisition")
		}
	}
}

func TestForeignLeaseWithoutOwnershipMetadataIsNeverTakenOver(t *testing.T) {
	for _, tc := range []struct {
		name        string
		labels, ann map[string]string
	}{
		{"no metadata", nil, nil},
		{"label only", map[string]string{"falcone.ai/owner": "pool-controller"}, nil},
		{"annotation only", nil, map[string]string{"falcone.ai/ownership-version": "1"}},
		{"wrong owner", map[string]string{"falcone.ai/owner": "somebody"}, map[string]string{"falcone.ai/ownership-version": "1"}},
		{"wrong version", map[string]string{"falcone.ai/owner": "pool-controller"}, map[string]string{"falcone.ai/ownership-version": "2"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			clock, store := newFakeClock(), &fakeStore{}
			store.seed(leader.Lease{Name: leader.LeaseName, Holder: "", LeaseDurationSeconds: 15, Labels: tc.labels, Annotations: tc.ann})
			b := newInstance(t, clock, store, "b", defaults)
			for s := 0; s < 120; s += 2 {
				b.tick()
				if b.open() {
					t.Fatalf("t=%ds: took over a Lease without ownership metadata", s)
				}
				clock.Advance(2 * sec)
			}
			if _, updates, _ := b.client.counts(); updates != 0 {
				t.Fatalf("sent %d updates to a foreign Lease", updates)
			}
			if creates, _, _ := b.client.counts(); creates != 0 {
				t.Fatalf("sent %d creates", creates)
			}
		})
	}
}

func TestConflictsDoNotOpenAGate(t *testing.T) {
	for _, op := range []string{"create", "update"} {
		t.Run(op, func(t *testing.T) {
			clock, store := newFakeClock(), &fakeStore{}
			b := newInstance(t, clock, store, "b", defaults)
			if op == "update" {
				store.seed(leader.Lease{Name: leader.LeaseName, Holder: "", LeaseDurationSeconds: 15,
					Labels:      map[string]string{"falcone.ai/owner": "pool-controller"},
					Annotations: map[string]string{"falcone.ai/ownership-version": "1"}})
			}
			b.client.setFault(op, leader.ErrConflict)
			for s := 0; s < 40; s += 2 {
				b.tick()
				if b.open() {
					t.Fatalf("t=%ds: a 409 on %s opened a gate", s, op)
				}
				clock.Advance(2 * sec)
			}
			c, u, _ := b.client.counts()
			if (op == "create" && c == 0) || (op == "update" && u == 0) {
				t.Fatalf("test never reached %s (creates %d updates %d)", op, c, u)
			}
		})
	}
}

func TestReleasedLeaseIsAcquiredOnTheNextRetry(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	b.tick() // B observes A
	a.el.Shutdown()
	if a.open() {
		t.Fatal("A's gate is open after shutdown")
	}
	l := store.current()
	if l == nil || l.Holder != "" {
		t.Fatalf("Lease after release = %+v, want an empty holder", l)
	}
	if l.LeaseTransitions != 0 {
		t.Fatalf("release changed leaseTransitions to %d", l.LeaseTransitions)
	}
	clock.Advance(2 * sec)
	b.tick()
	if !b.open() {
		t.Fatal("standby did not acquire the released Lease on its next retry")
	}
	if tok, _ := b.el.Token(); tok.Epoch != 1 {
		t.Fatalf("B's epoch = %d, want 1", tok.Epoch)
	}
}

func TestShutdownClosesGateThenCancelsDrainsAndReleases(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	holdFirst(t, clock, a)
	a.duty.await(t, "duty start", func() bool { r, _, _ := a.duty.snapshot(); return r == 1 })
	a.el.Shutdown()
	if r, _, c := a.duty.snapshot(); r != 0 || c != 1 {
		t.Fatalf("duty running %d cancelled %d after shutdown", r, c)
	}
	if _, ok := a.el.Token(); ok {
		t.Fatal("token still available after shutdown")
	}
	if l := store.current(); l == nil || l.Holder != "" {
		t.Fatalf("Lease not released: %+v", l)
	}
	before := store.current().ResourceVersion
	a.tick() // after shutdown a stopped elector sends nothing and stays closed
	if store.current().ResourceVersion != before || a.open() {
		t.Fatal("a stopped elector wrote or reopened")
	}
}

func TestShutdownWithAStuckDutyReleasesNothing(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	a.duty.stuck = true
	holdFirst(t, clock, a)
	a.duty.await(t, "duty start", func() bool { r, _, _ := a.duty.snapshot(); return r == 1 })
	before := store.current()
	done := make(chan struct{})
	base := clock.pending()
	go func() { a.el.Shutdown(); close(done) }()
	clock.waitTimers(t, base+1)
	if _, ok := a.el.Token(); ok {
		t.Fatal("the gate must close before the drain starts")
	}
	clock.Advance(defaults.LeaseDuration - defaults.RenewDeadline) // the drain bound
	<-done
	got := store.current()
	if got.Holder != before.Holder || got.ResourceVersion != before.ResourceVersion {
		t.Fatalf("a failed drain released the Lease: %+v", got)
	}
	close(a.duty.release)
}

func TestShutdownByANonHolderReleasesNothing(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	b.tick()
	before := store.current()
	b.el.Shutdown()
	if after := store.current(); after.ResourceVersion != before.ResourceVersion || after.Holder != before.Holder {
		t.Fatal("a non-holder changed the Lease on shutdown")
	}
	if _, u, _ := b.client.counts(); u != 0 {
		t.Fatalf("non-holder sent %d updates", u)
	}
}

func TestCreatesStayInTheElectorsOwnNamespaceMetadata(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	holdFirst(t, clock, a)
	l := store.current()
	if l.Name != leader.LeaseName || l.Annotations["falcone.ai/owner"] != `["`+releaseNS+`","`+leader.LeaseName+`"]` {
		t.Fatalf("created Lease = %+v", l)
	}
}

func TestInvalidTimingsFailClosed(t *testing.T) {
	good := leader.DefaultTimings
	bad := map[string]leader.Timings{
		"zero lease":         {0, good.RenewDeadline, good.RetryPeriod},
		"zero renew":         {good.LeaseDuration, 0, good.RetryPeriod},
		"zero retry":         {good.LeaseDuration, good.RenewDeadline, 0},
		"negative":           {good.LeaseDuration, good.RenewDeadline, -sec},
		"lease == renew":     {10 * sec, 10 * sec, 2 * sec},
		"renew == retry":     {15 * sec, 2 * sec, 2 * sec},
		"lease < renew":      {5 * sec, 10 * sec, 2 * sec},
		"above ceiling":      {leader.MaxLeaseDuration + sec, 10 * sec, 2 * sec},
		"fractional seconds": {15*sec + time.Millisecond, 10 * sec, 2 * sec},
	}
	for name, tm := range bad {
		if tm.Validate() == nil {
			t.Errorf("%s: Validate accepted %+v", name, tm)
		}
		clock, store := newFakeClock(), &fakeStore{}
		_, err := leader.New(leader.Config{Namespace: releaseNS, Identity: "x", Timings: tm, Client: store.client("x"), Clock: clock})
		if err == nil {
			t.Errorf("%s: New accepted %+v", name, tm)
		}
	}
	if err := (leader.Timings{LeaseDuration: leader.MaxLeaseDuration, RenewDeadline: 10 * sec, RetryPeriod: 2 * sec}).Validate(); err != nil {
		t.Errorf("the ceiling itself is valid: %v", err)
	}
}

func TestNewRejectsAMissingIdentityClientOrNamespace(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	ok := leader.Config{Namespace: releaseNS, Identity: "x", Timings: defaults, Client: store.client("x"), Clock: clock}
	if _, err := leader.New(ok); err != nil {
		t.Fatal(err)
	}
	for name, edit := range map[string]func(*leader.Config){
		"identity":  func(c *leader.Config) { c.Identity = "" },
		"client":    func(c *leader.Config) { c.Client = nil },
		"namespace": func(c *leader.Config) { c.Namespace = "" },
		"clock":     func(c *leader.Config) { c.Clock = nil },
	} {
		cfg := ok
		edit(&cfg)
		if _, err := leader.New(cfg); err == nil {
			t.Errorf("%s: New accepted the config", name)
		}
	}
}

func TestIdentityHasARandomSuffixPerProcess(t *testing.T) {
	id1, err1 := leader.NewIdentity("controller-7d9f", bytesReader{1, 2, 3, 4, 5, 6, 7, 8})
	id2, err2 := leader.NewIdentity("controller-7d9f", bytesReader{9, 9, 9, 9, 9, 9, 9, 9})
	if err1 != nil || err2 != nil || id1 == id2 || id1[:len("controller-7d9f_")] != "controller-7d9f_" {
		t.Fatalf("identities %q %q (%v %v)", id1, id2, err1, err2)
	}
	if _, err := leader.NewIdentity("Bad Name", bytesReader{1, 2, 3, 4, 5, 6, 7, 8}); err == nil {
		t.Fatal("an invalid POD_NAME was accepted")
	}
	if _, err := leader.NewIdentity("", bytesReader{1, 2, 3, 4, 5, 6, 7, 8}); err == nil {
		t.Fatal("an empty POD_NAME was accepted")
	}
}

func TestIdentityRefusesMalformedDNSSubdomains(t *testing.T) {
	long63, long64 := strings.Repeat("a", 63), strings.Repeat("a", 64)
	for name, pod := range map[string]string{
		"empty label":            "a..b",
		"label starts with -":    "a.-b",
		"label ends with -":      "a-.b",
		"leading dot":            ".a",
		"trailing dot":           "a.",
		"upper case":             "Pod",
		"label over 63":          long64,
		"name over 253":          strings.Repeat(long63[:50]+".", 6) + "a",
		"underscore (separator)": "a_b",
	} {
		if _, err := leader.NewIdentity(pod, bytesReader{1, 2, 3, 4, 5, 6, 7, 8}); err == nil {
			t.Errorf("%s: %q was accepted", name, pod)
		}
	}
	for _, pod := range []string{"a", "a.b", "controller-7d9f6c-abcde", "a-b.c-d", long63} {
		if _, err := leader.NewIdentity(pod, bytesReader{1, 2, 3, 4, 5, 6, 7, 8}); err != nil {
			t.Errorf("%q was refused: %v", pod, err)
		}
	}
}

type bytesReader []byte

func (b bytesReader) Read(p []byte) (int, error) { copy(p, b); return len(p), nil }

var _ = errors.New
var _ = context.Background

// slowClient makes every successful request take d of fake time, as a slow but
// healthy API server does, and records when each update was sent.
type slowClient struct {
	leader.Client
	clock   *fakeClock
	d       time.Duration
	mu      sync.Mutex
	updates []time.Duration
}

func (c *slowClient) firstUpdate() (time.Duration, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.updates) == 0 {
		return 0, false
	}
	return c.updates[0], true
}

func (c *slowClient) Get(ctx context.Context) (leader.Lease, error) {
	l, err := c.Client.Get(ctx)
	c.clock.Advance(c.d)
	return l, err
}

func (c *slowClient) Update(ctx context.Context, l leader.Lease) (leader.Lease, error) {
	c.mu.Lock()
	c.updates = append(c.updates, c.clock.since())
	c.mu.Unlock()
	return c.Client.Update(ctx, l)
}

// TestRunLoopTakeoverBoundHoldsWithSlowRequests: request latency is part of the
// polling interval, not added to it. With every request taking 0.99s, B polls
// every retry period (start to start), so it acquires no later than leaseDuration
// + 2 x retryPeriod after A's last renewal.
func TestRunLoopTakeoverBoundHoldsWithSlowRequests(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	holdFirst(t, clock, a) // A never renews again after this acquisition
	lastRenew := clock.since()
	b := newInstance(t, clock, store, "b", defaults)
	slow := &slowClient{Client: b.client, clock: clock, d: 990 * time.Millisecond}
	el, err := leader.New(leader.Config{
		Namespace: releaseNS, Identity: "b-0123abcd", Timings: defaults, Client: slow, Clock: clock,
	})
	if err != nil {
		t.Fatal(err)
	}
	clock.Advance(1500 * time.Millisecond) // B's polling phase relative to A's renewal
	ctx, stop := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { el.Run(ctx); close(done) }()
	seen := 0
	var sentAt time.Duration
	for {
		if at, ok := slow.firstUpdate(); ok {
			sentAt = at
			break
		}
		// Wait for Run to block on its polling wait: a new After call that is not
		// a request timeout. Then let that wait elapse.
		var d time.Duration
		clock.changed.await(t, "the Run loop to wait for its next poll", func() bool {
			n, last := clock.afterCalls()
			d = last
			return n > seen && last != defaults.RequestTimeout()
		})
		seen, _ = clock.afterCalls()
		clock.Advance(d)
	}
	limit := defaults.LeaseDuration + 2*defaults.RetryPeriod
	if got := sentAt - lastRenew; got > limit {
		t.Fatalf("B sent its takeover %v after A's last renewal; want within %v", got, limit)
	}
	stop()
	<-done
}
