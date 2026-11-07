package leader_test

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

// fakeWriter is the Kubernetes write boundary; it counts what reaches it.
type fakeWriter struct {
	mu             sync.Mutex
	applies, drops int
}

func (w *fakeWriter) Apply(context.Context, leader.Object) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.applies++
	return nil
}

func (w *fakeWriter) Delete(context.Context, leader.Object) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.drops++
	return nil
}

func (w *fakeWriter) counts() (int, int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.applies, w.drops
}

func guarded(clock *fakeClock, l leader.Leadership, w *fakeWriter, d func() leader.DesiredState) leader.GuardedWriter {
	return leader.GuardedWriter{Admission: leader.Admission{Leadership: l, Clock: clock}, Desired: d, Inner: w}
}

func writeBoth(g leader.GuardedWriter) (error, error) {
	obj := leader.Object{Kind: "Deployment", Namespace: poolA, Name: "pool-x"}
	return g.Apply(context.Background(), obj), g.Delete(context.Background(), obj)
}

func TestWritesAreAdmittedOnlyWithAnOpenGateAndFreshDesiredState(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	holdFirst(t, clock, a)
	w := &fakeWriter{}
	fresh := func() leader.DesiredState { return leader.DesiredState{Available: true, ObservedAt: clock.Now()} }
	g := guarded(clock, a.el, w, fresh)
	if e1, e2 := writeBoth(g); e1 != nil || e2 != nil {
		t.Fatalf("a leader with fresh state was refused: %v %v", e1, e2)
	}
	if ap, dr := w.counts(); ap != 1 || dr != 1 {
		t.Fatalf("writer saw %d applies, %d deletes; want 1, 1", ap, dr)
	}

	t.Run("stale desired state", func(t *testing.T) {
		w := &fakeWriter{}
		observed := clock.Now().Add(-(leader.DefaultMaxDesiredStateAge + sec))
		e1, e2 := writeBoth(guarded(clock, a.el, w, func() leader.DesiredState { return leader.DesiredState{Available: true, ObservedAt: observed} }))
		if !errors.Is(e1, leader.ErrDesiredStateStale) || !errors.Is(e2, leader.ErrDesiredStateStale) {
			t.Fatalf("errors = %v, %v; want stale", e1, e2)
		}
		if ap, dr := w.counts(); ap != 0 || dr != 0 {
			t.Fatalf("writes reached the writer on stale data: %d %d", ap, dr)
		}
	})
	t.Run("unavailable desired state", func(t *testing.T) {
		for name, ds := range map[string]leader.DesiredState{
			"flag down":  {Available: false, ObservedAt: clock.Now()},
			"never read": {Available: true},
		} {
			w := &fakeWriter{}
			e1, e2 := writeBoth(guarded(clock, a.el, w, func() leader.DesiredState { return ds }))
			if !errors.Is(e1, leader.ErrDesiredStateUnavailable) || !errors.Is(e2, leader.ErrDesiredStateUnavailable) {
				t.Fatalf("%s: errors = %v, %v", name, e1, e2)
			}
			if ap, dr := w.counts(); ap != 0 || dr != 0 {
				t.Fatalf("%s: writes reached the writer: %d %d", name, ap, dr)
			}
		}
	})
	t.Run("no desired state source", func(t *testing.T) {
		w := &fakeWriter{}
		e1, _ := writeBoth(guarded(clock, a.el, w, nil))
		if e1 == nil {
			t.Fatal("a writer without a desired state source was admitted")
		}
		if ap, dr := w.counts(); ap != 0 || dr != 0 {
			t.Fatal("writes reached the writer")
		}
	})
}

func TestNoWritesAfterLeadershipIsLostOrTheAPIIsUnreachable(t *testing.T) {
	for _, tc := range []struct {
		name string
		lose func(clock *fakeClock, store *fakeStore, a *instance)
	}{
		{"leader lost to deletion", func(clock *fakeClock, store *fakeStore, a *instance) {
			store.remove()
			clock.Advance(2 * sec)
			a.tick()
		}},
		{"API server unreachable past the renew deadline", func(clock *fakeClock, store *fakeStore, a *instance) {
			a.client.setDown(true)
			for i := 0; i < 6; i++ { // 12 seconds, more than renewDeadline
				clock.Advance(2 * sec)
				a.tick()
			}
		}},
		{"API server unreachable and no tick runs at all", func(clock *fakeClock, store *fakeStore, a *instance) {
			clock.Advance(defaults.RenewDeadline) // the gate is evaluated on every check
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			clock, store := newFakeClock(), &fakeStore{}
			a := newInstance(t, clock, store, "a", defaults)
			holdFirst(t, clock, a)
			w := &fakeWriter{}
			fresh := func() leader.DesiredState { return leader.DesiredState{Available: true, ObservedAt: clock.Now()} }
			var gate *leader.Gate
			eventually(t, "the duty to start", func() bool {
				a.duty.mu.Lock()
				defer a.duty.mu.Unlock()
				if len(a.duty.gates) == 1 {
					gate = a.duty.gates[0]
				}
				return gate != nil
			})
			dutyWriter := guarded(clock, gate, w, fresh)
			if e1, _ := writeBoth(dutyWriter); e1 != nil {
				t.Fatalf("leader refused before the fault: %v", e1)
			}
			applies, deletes := w.counts()
			tc.lose(clock, store, a)
			e1, e2 := writeBoth(dutyWriter)
			if !errors.Is(e1, leader.ErrNotLeader) || !errors.Is(e2, leader.ErrNotLeader) {
				t.Fatalf("errors = %v, %v; want not leader", e1, e2)
			}
			if ap, dr := w.counts(); ap != applies || dr != deletes {
				t.Fatalf("writes reached the writer after the fault: %d/%d -> %d/%d", applies, deletes, ap, dr)
			}
		})
	}
}

func TestAnInstanceThatNeverLedWritesNothing(t *testing.T) {
	// A failed preflight leaves no elector at all, so there is no leadership to ask.
	clock := newFakeClock()
	w := &fakeWriter{}
	fresh := func() leader.DesiredState { return leader.DesiredState{Available: true, ObservedAt: clock.Now()} }
	e1, e2 := writeBoth(guarded(clock, nil, w, fresh))
	if !errors.Is(e1, leader.ErrNotLeader) || !errors.Is(e2, leader.ErrNotLeader) {
		t.Fatalf("errors = %v, %v", e1, e2)
	}
	if ap, dr := w.counts(); ap != 0 || dr != 0 {
		t.Fatal("a writer behind no leadership was reached")
	}
}

type fakeSink struct {
	mu       sync.Mutex
	payloads [][]byte
}

func (s *fakeSink) Publish(_ context.Context, p []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.payloads = append(s.payloads, append([]byte(nil), p...))
	return nil
}

func (s *fakeSink) all() [][]byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([][]byte(nil), s.payloads...)
}

var (
	reportScope = poolstatus.Scope{PoolRef: poolstatus.PoolRef{Namespace: poolA, Name: "pool-x"}, OwnerAnnotation: `["tenant","profile"]`}
	baseReport  = poolstatus.Report{ObservedGeneration: 3, DesiredReplicas: 1, ReadyReplicas: 1, ReadyCapacity: 1, RuntimeMode: "wasm"}
)

func TestOnlyTheLeaderPublishesAValidReportWithTheTokenEpoch(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	sinkA, sinkB := &fakeSink{}, &fakeSink{}
	ra := &leader.Reporter{Leadership: a.el, Sink: sinkA}
	rb := &leader.Reporter{Leadership: b.el, Sink: sinkB}
	for s := 0; s < 40; s += 2 {
		a.tick()
		b.tick()
		clock.Advance(2 * sec)
	}
	leaderRep, leaderSink, otherRep, otherSink, lead := ra, sinkA, rb, sinkB, a
	if b.open() {
		leaderRep, leaderSink, otherRep, otherSink, lead = rb, sinkB, ra, sinkA, b
	}
	ctx := context.Background()
	if err := otherRep.Publish(ctx, baseReport, reportScope, nil, poolstatus.Options{}); !errors.Is(err, leader.ErrNotLeader) {
		t.Fatalf("non-leader publish error = %v", err)
	}
	if n := len(otherSink.all()); n != 0 {
		t.Fatalf("non-leader published %d reports", n)
	}
	for i := 0; i < 2; i++ {
		if err := leaderRep.Publish(ctx, baseReport, reportScope, nil, poolstatus.Options{}); err != nil {
			t.Fatal(err)
		}
	}
	got := leaderSink.all()
	if len(got) != 2 {
		t.Fatalf("leader published %d reports, want 2", len(got))
	}
	tok, _ := lead.el.Token()
	for i, p := range got {
		if err := poolstatus.ValidateJSON(p); err != nil {
			t.Fatalf("report %d violates the contract: %v", i, err)
		}
		var r poolstatus.Report
		if err := json.Unmarshal(p, &r); err != nil {
			t.Fatal(err)
		}
		if r.ReporterEpoch != tok.Epoch || r.ObservationSeq != int64(i+1) {
			t.Fatalf("report %d: reporterEpoch %d seq %d; want %d, %d", i, r.ReporterEpoch, r.ObservationSeq, tok.Epoch, i+1)
		}
	}

	// Leadership lost: nothing more is published.
	lead.client.setDown(true)
	for i := 0; i < 8; i++ {
		clock.Advance(2 * sec)
		lead.tick()
	}
	if err := leaderRep.Publish(ctx, baseReport, reportScope, nil, poolstatus.Options{}); !errors.Is(err, leader.ErrNotLeader) {
		t.Fatalf("publish after loss = %v", err)
	}
	if n := len(leaderSink.all()); n != 2 {
		t.Fatalf("a former leader published: %d reports", n)
	}
}

func TestObservationSeqRestartsWhenTheEpochRises(t *testing.T) {
	clock, store := newFakeClock(), &fakeStore{}
	a := newInstance(t, clock, store, "a", defaults)
	b := newInstance(t, clock, store, "b", defaults)
	holdFirst(t, clock, a)
	sinkA, sinkB := &fakeSink{}, &fakeSink{}
	ra := &leader.Reporter{Leadership: a.el, Sink: sinkA}
	rb := &leader.Reporter{Leadership: b.el, Sink: sinkB}
	ctx := context.Background()
	for i := 0; i < 3; i++ {
		if err := ra.Publish(ctx, baseReport, reportScope, nil, poolstatus.Options{}); err != nil {
			t.Fatal(err)
		}
	}
	a.el.Shutdown() // graceful release; B takes over at epoch + 1
	clock.Advance(2 * sec)
	b.tick()
	if err := rb.Publish(ctx, baseReport, reportScope, nil, poolstatus.Options{}); err != nil {
		t.Fatal(err)
	}
	var first, second poolstatus.Report
	_ = json.Unmarshal(sinkA.all()[2], &first)
	_ = json.Unmarshal(sinkB.all()[0], &second)
	if second.ReporterEpoch <= first.ReporterEpoch || second.ObservationSeq != 1 || first.ObservationSeq != 3 {
		t.Fatalf("epochs/seqs: first %d/%d second %d/%d", first.ReporterEpoch, first.ObservationSeq, second.ReporterEpoch, second.ObservationSeq)
	}
	_ = time.Second
}
