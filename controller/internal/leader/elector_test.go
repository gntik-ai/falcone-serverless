package leader

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"
	"time"
)

func mustLead(t *testing.T, i *instance) *Term {
	t.Helper()
	term := i.e.Term()
	if term == nil {
		t.Fatalf("%s must lead, status %+v", i.id, i.e.Status())
	}
	return term
}

func requireFenced(t *testing.T, err error) {
	t.Helper()
	var fenced *FencedError
	if !errors.As(err, &fenced) {
		t.Fatalf("want a fence refusal, got %v", err)
	}
}

// --- acquisition, renewal and handover --------------------------------------

func TestTwoInstancesExactlyOneLeadsAndPublishes(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick()
	b.tick()
	for range 30 {
		clock.Advance(tick)
		a.tick()
		b.tick()
		if !a.leads() || b.leads() {
			t.Fatalf("exactly controller-0 must lead: a=%v b=%v", a.leads(), b.leads())
		}
		if err := a.publish(rec); err != nil {
			t.Fatal(err)
		}
		if err := b.publish(rec); !errors.Is(err, ErrNotLeader) {
			t.Fatalf("the standby published: %v", err)
		}
	}
	if got, want := rec.holds(), (Stamp{Lineage: "uid-1", Epoch: 1, Seq: 30}); got != want {
		t.Fatalf("receiver holds %+v, want %+v from the leader alone", got, want)
	}
	if rec.acceptedCount() != 30 || api.snapshot().HolderIdentity != "controller-0" {
		t.Fatalf("accepted %d, holder %q", rec.acceptedCount(), api.snapshot().HolderIdentity)
	}
	if s := b.e.Status(); !s.Ready || s.Leading || s.Reason != ReasonStandby {
		t.Fatalf("standby status %+v", s)
	}
}

func TestStandbyAcquiresWithinLeaseDurationPlusRetryPeriodAndFencesTheOldTerm(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick()
	b.tick()
	run(clock, 10*time.Second, a, b)
	termA := mustLead(t, a)
	if err := a.publish(rec); err != nil {
		t.Fatal(err)
	}
	delayed, err := termA.Stamp() // taken, but sent only after the handover
	if err != nil {
		t.Fatal(err)
	}
	lastRenewal := clock.Now() // A stops renewing here

	for !b.leads() {
		clock.Advance(tick)
		b.tick()
		if clock.Now().Sub(lastRenewal) > 10*time.Minute {
			t.Fatal("the standby never took over")
		}
	}
	if took := clock.Now().Sub(lastRenewal); took < DefaultLeaseDuration || took > DefaultLeaseDuration+DefaultRetryPeriod {
		t.Fatalf("takeover after %s, want within [%s, %s]", took, DefaultLeaseDuration, DefaultLeaseDuration+DefaultRetryPeriod)
	}
	if s := b.e.Status(); s.Epoch != 2 || s.Lineage != "uid-1" {
		t.Fatalf("new term %+v, want epoch 2 on the same lineage", s)
	}
	if err := b.publish(rec); err != nil {
		t.Fatalf("the new leader's report was refused: %v", err)
	}
	requireFenced(t, rec.Write(context.Background(), delayed, nil))
	requireFenced(t, rec.Write(context.Background(), Stamp{Lineage: "uid-1", Epoch: 1, Seq: 1000}, nil))

	a.tick() // the former leader resumes: its gate closed, and it cannot write or publish
	if a.leads() || a.e.Status().Reason != ReasonLeadershipLost {
		t.Fatalf("the former leader still leads: %+v", a.e.Status())
	}
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("the former leader published: %v", err)
	}
	select {
	case <-termA.Context().Done():
	default:
		t.Fatal("the former leader's duty work was not cancelled")
	}
}

func TestLeaseLivesOnlyInTheReleaseNamespace(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick()
	b.tick()
	run(clock, time.Minute, a, b)
	api.del()
	run(clock, time.Minute, a, b)
	if len(api.ops) == 0 {
		t.Fatal("no Lease operation was recorded")
	}
	for _, op := range api.ops {
		if op.namespace != relNS {
			t.Fatalf("Lease %s in %q, want only the release namespace %q (pool namespaces %s, %s)", op.verb, op.namespace, relNS, poolA, poolB)
		}
	}
}

// --- epochs ------------------------------------------------------------------

func epochsAcrossHandover(t *testing.T, start time.Time) []string {
	t.Helper()
	clock, api := newFakeClock(start), newFakeAPI()
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick()
	b.tick()
	got := []string{api.epoch()}
	run(clock, 30*time.Second, b) // A stops: B takes over
	got = append(got, api.epoch())
	return got
}

func TestEpochNeverUsesTheWallClock(t *testing.T) {
	for _, start := range []time.Time{time.Unix(0, 0), epoch0, time.Date(2999, 12, 31, 0, 0, 0, 0, time.UTC)} {
		if got := epochsAcrossHandover(t, start); !reflect.DeepEqual(got, []string{"1", "2"}) {
			t.Fatalf("clock %s: epochs %v, want [1 2]", start, got)
		}
	}
}

func TestFreshInstallBootstrapsAtEpochOneWithFullOwnershipMetadata(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	term := mustLead(t, a)
	l := api.snapshot()
	if term.Epoch != 1 || term.Lineage != l.UID || l.UID == "" {
		t.Fatalf("term %+v on Lease %q", term, l.UID)
	}
	if l.Name != LeaseName || l.Namespace != relNS || l.HolderIdentity != "controller-0" {
		t.Fatalf("lease %+v", l)
	}
	wantLabels := map[string]string{"falcone.ai/owner": "pool-controller"}
	wantAnnotations := map[string]string{
		"falcone.ai/ownership-version": "1",
		"falcone.ai/owner":             `["falcone-system","leader"]`,
		"falcone.ai/leader-epoch":      "1",
	}
	if !reflect.DeepEqual(l.Labels, wantLabels) || !reflect.DeepEqual(l.Annotations, wantAnnotations) {
		t.Fatalf("labels %v annotations %v", l.Labels, l.Annotations)
	}
	if err := VerifyOwnership(l, relNS); err != nil {
		t.Fatal(err)
	}
}

func TestNewTermWritesTheRecordedEpochPlusOneAcrossHandoverAndRestart(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	api.seed(validLease("41"))
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	if term := mustLead(t, a); term.Epoch != 42 || api.epoch() != "42" {
		t.Fatalf("epoch %d, recorded %s, want 42", term.Epoch, api.epoch())
	}
	if err := a.publish(rec); err != nil {
		t.Fatal(err)
	}
	delayed, _ := a.e.Term().Stamp()

	// Handover: the standby takes over when A stops renewing, at the recorded epoch + 1.
	b := newInstance(t, api, clock, "controller-1")
	b.tick()
	run(clock, 30*time.Second, b)
	if term := mustLead(t, b); term.Epoch != 43 {
		t.Fatalf("handover epoch %d, want 43", term.Epoch)
	}
	if err := b.publish(rec); err != nil {
		t.Fatal(err)
	}

	// Restart: a fresh process with no memory reads the Lease and publishes at epoch + 1.
	b2 := newInstance(t, api, clock, "controller-1")
	b2.tick()
	run(clock, 30*time.Second, b2)
	if term := mustLead(t, b2); term.Epoch != 44 {
		t.Fatalf("restart epoch %d, want 44", term.Epoch)
	}
	if err := b2.publish(rec); err != nil {
		t.Fatal(err)
	}
	requireFenced(t, rec.Write(context.Background(), delayed, nil))
	requireFenced(t, rec.Write(context.Background(), Stamp{Lineage: "uid-1", Epoch: 43, Seq: 99}, nil))
}

func TestTakeoverUsesOptimisticConcurrency(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	api.seed(validLease("7")) // released
	b := newInstance(t, api, clock, "controller-1")
	b.client.afterGet = api.touch // another writer changes the Lease between Get and Update
	b.tick()
	if b.leads() || api.epoch() != "7" || api.writeCount() != 0 {
		t.Fatalf("a stale update won: leads=%v epoch=%s writes=%d", b.leads(), api.epoch(), api.writeCount())
	}
	b.client.afterGet = nil
	b.tick()
	if term := mustLead(t, b); term.Epoch != 8 {
		t.Fatalf("epoch %d, want 8", term.Epoch)
	}
}

func TestMissingOrMalformedEpochFailsClosedAndIsNeverModified(t *testing.T) {
	bad := map[string]*string{"missing": nil}
	for name, v := range map[string]string{
		"empty": "", "text": "abc", "zero": "0", "negative": "-1", "plus": "+1", "leading zero": "01",
		"space": " 1", "float": "1.0", "hex": "0x1", "overflow": "9223372036854775808",
		"exhausted": "9223372036854775807",
	} {
		bad[name] = &v
	}
	for name, value := range bad {
		for _, held := range []bool{false, true} {
			clock, api := newFakeClock(epoch0), newFakeAPI()
			l := validLease("1")
			if value == nil {
				delete(l.Annotations, EpochAnnotation)
			} else {
				l.Annotations[EpochAnnotation] = *value
			}
			if held {
				l.HolderIdentity = "someone-else"
			}
			api.seed(l)
			before := api.snapshot()
			a := newInstance(t, api, clock, "controller-0")
			run(clock, time.Minute, a)
			if a.leads() || a.e.Status().Reason != ReasonEpochInvalid || a.e.Status().Ready {
				t.Fatalf("%s: status %+v", name, a.e.Status())
			}
			if api.writeCount() != 0 || !reflect.DeepEqual(api.snapshot(), before) {
				t.Fatalf("%s: the Lease was modified", name)
			}
		}
	}
}

// --- ownership ---------------------------------------------------------------

func TestOwnershipIsVerifiedBeforeAnyTakeoverOrUpdate(t *testing.T) {
	defects := map[string]func(l *Lease){
		"label missing":              func(l *Lease) { delete(l.Labels, OwnerLabel) },
		"label foreign":              func(l *Lease) { l.Labels[OwnerLabel] = "someone" },
		"version missing":            func(l *Lease) { delete(l.Annotations, VersionAnnotation) },
		"version other":              func(l *Lease) { l.Annotations[VersionAnnotation] = "2" },
		"owner annotation missing":   func(l *Lease) { delete(l.Annotations, OwnerAnnotation) },
		"owner annotation malformed": func(l *Lease) { l.Annotations[OwnerAnnotation] = "SECRET-OWNER-VALUE [" },
		"owner annotation object":    func(l *Lease) { l.Annotations[OwnerAnnotation] = `{"owner":"SECRET-OWNER-VALUE"}` },
		"owner annotation numbers":   func(l *Lease) { l.Annotations[OwnerAnnotation] = `[1,2]` },
		"owner other namespace":      func(l *Lease) { l.Annotations[OwnerAnnotation] = `["SECRET-OWNER-VALUE","leader"]` },
		"owner other duty":           func(l *Lease) { l.Annotations[OwnerAnnotation] = `["falcone-system","scaler"]` },
		"owner too long":             func(l *Lease) { l.Annotations[OwnerAnnotation] = `["falcone-system","leader","x"]` },
		"owner too short":            func(l *Lease) { l.Annotations[OwnerAnnotation] = `["falcone-system"]` },
		"owner empty":                func(l *Lease) { l.Annotations[OwnerAnnotation] = `` },
	}
	for name, mutate := range defects {
		for _, held := range []bool{false, true} {
			clock, api := newFakeClock(epoch0), newFakeAPI()
			l := validLease("3")
			mutate(l)
			if held {
				l.HolderIdentity = "someone-else"
			}
			api.seed(l)
			before := api.snapshot()
			a := newInstance(t, api, clock, "controller-0")
			run(clock, time.Minute, a)
			s := a.e.Status()
			if a.leads() || s.Reason != ReasonOwnershipMismatch || s.Ready {
				t.Fatalf("%s (held=%v): status %+v", name, held, s)
			}
			if strings.Contains(s.Detail, "SECRET") {
				t.Fatalf("%s: the status echoes a value from the foreign object: %q", name, s.Detail)
			}
			if api.writeCount() != 0 || !reflect.DeepEqual(api.snapshot(), before) {
				t.Fatalf("%s (held=%v): the Lease was modified", name, held)
			}
		}
	}
	clock, api := newFakeClock(epoch0), newFakeAPI()
	api.seed(validLease("3"))
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	if !a.leads() {
		t.Fatalf("a Lease with full ownership must be taken over: %+v", a.e.Status())
	}
}

// --- fencing across races ----------------------------------------------------

// A pause after the gate was checked and the stamp taken, before the write is sent: the
// old term's report arrives after a newer term has published.
func TestRacePauseBetweenTheGateCheckAndTheWriteIsRefusedByTheReceiver(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick()
	b.tick()
	stamp, err := mustLead(t, a).Stamp()
	if err != nil {
		t.Fatal(err)
	}
	a.client.down = true // the paused process also cannot renew
	run(clock, 30*time.Second, b)
	mustLead(t, b)
	if err := b.publish(rec); err != nil {
		t.Fatal(err)
	}
	clock.Advance(tick)
	requireFenced(t, rec.Write(context.Background(), stamp, []byte("late")))
	if rec.holds().Epoch != 2 || rec.acceptedCount() != 1 {
		t.Fatalf("receiver %+v after %d accepted writes", rec.holds(), rec.acceptedCount())
	}
}

// A replica whose clock runs slow keeps its gate open after a faster standby took over.
// Both believe they lead; only the new epoch is accepted.
func TestRaceClockRateDriftLeavesAnOverlapThatTheFenceClosesAtTheReceiver(t *testing.T) {
	base, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	slow := newDriftClock(base, 0.5)
	a, b := newInstance(t, api, slow, "controller-0"), newInstance(t, api, base, "controller-1")
	a.tick()
	b.tick()
	a.client.down = true // A stops renewing
	var both bool
	for elapsed := time.Duration(0); elapsed < 18*time.Second; elapsed += tick {
		base.Advance(tick)
		b.tick()
		both = both || (a.leads() && b.leads())
	}
	if !both {
		t.Fatal("the scenario must reach the overlap window: both instances believe they lead")
	}
	if err := b.publish(rec); err != nil {
		t.Fatal(err)
	}
	requireFenced(t, a.publish(rec)) // A's gate is still open on its slow clock
	if rec.holds().Epoch != 2 {
		t.Fatalf("receiver %+v", rec.holds())
	}
}

// The Create (or the takeover Update) lands but its reply is lost: the instance does not lead,
// so no report of that epoch ever exists; the standby takes over with a higher epoch.
func TestRaceAmbiguousCreateAndUpdateNeverProduceAcceptedStaleReports(t *testing.T) {
	for _, name := range []string{"create", "update"} {
		t.Run(name, func(t *testing.T) {
			clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
			if name == "update" {
				api.seed(validLease("4"))
			}
			a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
			a.client.dropCreateReply, a.client.dropUpdateReply = true, true
			a.tick()
			if a.leads() || a.e.Status().Reason != ReasonLeaseUnavailable {
				t.Fatalf("an unanswered write made the instance lead: %+v", a.e.Status())
			}
			landed := api.epoch()
			if landed != "1" && landed != "5" || api.snapshot().HolderIdentity != "controller-0" {
				t.Fatalf("the write must have landed: epoch %s holder %q", landed, api.snapshot().HolderIdentity)
			}
			a.client.dropCreateReply, a.client.dropUpdateReply = false, false
			b.tick()
			if b.leads() {
				t.Fatal("B took a Lease that is held and not expired")
			}
			run(clock, 30*time.Second, a, b)
			if a.leads() == b.leads() {
				t.Fatalf("exactly one of them must lead: a=%v b=%v", a.leads(), b.leads())
			}
			for _, i := range []*instance{a, b} {
				if i.leads() {
					if err := i.publish(rec); err != nil {
						t.Fatal(err)
					}
					if rec.holds().Epoch <= mustEpoch(t, landed) {
						t.Fatalf("the new term's epoch %d does not exceed the landed epoch %s", rec.holds().Epoch, landed)
					}
				}
			}
		})
	}
}

func mustEpoch(t *testing.T, s string) int64 {
	t.Helper()
	l := validLease(s)
	e, err := EpochOf(l)
	if err != nil {
		t.Fatal(err)
	}
	return e
}

func TestRaceRepeated404sNeverLeadAndWriteNothingUnlessCreateSucceeds(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a := newInstance(t, api, clock, "controller-0")
	a.client.createNotFound = true
	run(clock, time.Minute, a)
	if a.leads() || api.writeCount() != 0 || rec.acceptedCount() != 0 {
		t.Fatalf("leads=%v writes=%d", a.leads(), api.writeCount())
	}
	if s := a.e.Status(); s.Reason != ReasonLeaseUnavailable || s.Ready {
		t.Fatalf("status %+v", s)
	}
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
		t.Fatal(err)
	}
}

// The pause lands between the deadline calculation and the term's context: the term is born
// past its deadline, opens no gate, and its epoch never reaches the receiver while the
// standby that took over publishes at the next one.
func TestRacePauseBetweenDeadlineAndContextOpensNoTermAndTheNewEpochIsAccepted(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	held := validLease("4")
	held.HolderIdentity = "ghost"
	api.seed(held)
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick() // both observe the ghost's Lease at t=0
	b.tick()
	clock.Advance(DefaultLeaseDuration)
	a.e.beforeTerm = func() {
		a.e.beforeTerm = nil
		// A has taken the Lease (epoch 5) and computed its deadline; the process now pauses.
		// B sees the Lease unchanged for a lease duration and takes over at epoch 6.
		for range 9 {
			clock.Advance(tick)
			b.tick()
		}
	}
	// A's observation is a lease duration old, so it takes the Lease over now.
	a.tick()
	if a.leads() {
		t.Fatalf("A opened a gate past its deadline: %+v", a.e.Status())
	}
	if got := a.e.Status().Reason; got != ReasonLeaseUnavailable {
		t.Fatalf("A status %q", got)
	}
	if err := b.publish(rec); err != nil {
		t.Fatalf("the new leader's report was refused: %v", err)
	}
	if got := rec.holds(); got.Epoch != 6 || got.Lineage != "uid-1" {
		t.Fatalf("receiver %+v, want epoch 6 from B", got)
	}
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("the paused instance published: %v", err)
	}
	if api.snapshot().HolderIdentity != "controller-1" {
		t.Fatalf("holder %q", api.snapshot().HolderIdentity)
	}
}

// The Create lands but its successful reply is delayed past the deadline while the standby
// takes over. The late reply opens no term and the receiver accepts only the new epoch.
func TestRaceDelayedSuccessfulCreateOpensNoTermAndTheNewEpochIsAccepted(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.client.afterCreate = func() {
		a.client.afterCreate = nil
		for range 9 { // 18 s: B sees the new Lease unchanged for a lease duration and takes it
			clock.Advance(tick)
			b.tick()
		}
	}
	a.tick()
	if a.leads() {
		t.Fatalf("a reply that arrived after the deadline opened a gate: %+v", a.e.Status())
	}
	mustLead(t, b)
	if err := b.publish(rec); err != nil {
		t.Fatalf("the new leader's report was refused: %v", err)
	}
	if rec.holds().Epoch != 2 {
		t.Fatalf("receiver %+v, want epoch 2", rec.holds())
	}
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("the delayed instance published: %v", err)
	}
	if rec.acceptedCount() != 1 {
		t.Fatalf("accepted %d reports", rec.acceptedCount())
	}
}

// A refusal that arrives after the process re-acquired the Lease still stops every term:
// the term that was refused is not the current one.
func TestFenceRefusalAfterReacquisitionStopsTheCurrentTermToo(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	termA := mustLead(t, a)
	inFlight := make(chan struct{})
	release := make(chan struct{})
	sink := blockingSink{started: inFlight, release: release}
	result := make(chan error, 1)
	go func() { result <- a.e.Publish(context.Background(), termA, sink, nil) }()
	<-inFlight

	a.client.down = true // renewals fail until the gate closes, then the API returns
	run(clock, 12*time.Second, a)
	if a.leads() {
		t.Fatal("A must have lost its term")
	}
	a.client.down = false
	run(clock, 40*time.Second, a)
	termB := mustLead(t, a)
	if termB == termA || termB.Epoch != 2 {
		t.Fatalf("the re-acquired term %+v", termB)
	}

	close(release) // the delayed refusal of term A arrives now
	if err := <-result; !errors.Is(err, ErrFenceRejected) {
		t.Fatalf("publish returned %v", err)
	}
	if a.leads() {
		t.Fatal("the re-acquired term still leads after a refusal")
	}
	if _, err := termB.Stamp(); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("the re-acquired term can still stamp writes: %v", err)
	}
	select {
	case <-termB.Context().Done():
	default:
		t.Fatal("the re-acquired term's duty work was not cancelled")
	}
	if s := a.e.Status(); s.Reason != ReasonFenceRejected || s.Ready || s.Leading {
		t.Fatalf("status %+v", s)
	}
	a.tick() // a stopped instance releases the Lease and never campaigns again
	if api.snapshot().HolderIdentity != "" {
		t.Fatal("the stopped instance still holds the Lease")
	}
	run(clock, time.Minute, a)
	if a.leads() || a.e.Status().Reason != ReasonFenceRejected {
		t.Fatalf("status %+v", a.e.Status())
	}
}

// A refusal that arrives while a takeover is still being answered opens no term.
func TestFenceRefusalDuringAnInFlightAcquisitionOpensNoTerm(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	termA := mustLead(t, a)
	a.client.down = true
	run(clock, 12*time.Second, a)
	a.client.down = false
	refused := false
	a.e.beforeTerm = func() {
		a.e.beforeTerm = nil
		refused = true
		a.e.reject(termA) // the delayed refusal lands between the write and the gate
	}
	for !refused {
		clock.Advance(tick)
		a.tick()
		if clock.Now().Sub(epoch0) > 10*time.Minute {
			t.Fatal("the takeover never happened")
		}
	}
	if a.leads() || a.e.Status().Reason != ReasonFenceRejected {
		t.Fatalf("a term opened after the refusal: %+v", a.e.Status())
	}
	if a.e.Term() != nil {
		t.Fatal("a term is open after the refusal")
	}
}

// A duty that paused before publishing resumes after the process re-acquired the Lease: its
// payload came from the old term, so it can neither be stamped with the new epoch nor sent.
func TestPausedDutyCannotPublishUnderTheReacquiredTerm(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	termA := mustLead(t, a)
	a.client.down = true
	run(clock, 12*time.Second, a)
	a.client.down = false
	run(clock, 40*time.Second, a)
	termB := mustLead(t, a)
	if termB.Epoch != 2 {
		t.Fatalf("epoch %d", termB.Epoch)
	}
	if err := a.e.Publish(context.Background(), termA, rec, []byte("stale result")); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("a duty of the old term published: %v", err)
	}
	if rec.acceptedCount() != 0 {
		t.Fatalf("a stale payload reached the receiver: %+v", rec.accepted)
	}
	if err := a.e.Publish(context.Background(), termB, rec, []byte("current")); err != nil {
		t.Fatal(err)
	}
	if got, want := rec.holds(), (Stamp{Lineage: "uid-1", Epoch: 2, Seq: 1}); got != want {
		t.Fatalf("receiver %+v, want %+v", got, want)
	}
}

type blockingSink struct{ started, release chan struct{} }

func (s blockingSink) Write(context.Context, Stamp, []byte) error {
	close(s.started)
	<-s.release
	return &FencedError{}
}

// --- deletion and lineage ----------------------------------------------------

func TestDeletedLeaseIsFencedAtEveryEpochUntilTheOperatorResetsTheReceiver(t *testing.T) {
	for name, recreated := range map[string]*string{
		"absent, so epoch 1 (lower)": nil,
		"epoch equal to the old":     ptr("2"),  // the new term reaches 3, the old one
		"epoch above the old":        ptr("20"), // the new term reaches 21
	} {
		t.Run(name, func(t *testing.T) {
			clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
			api.seed(validLease("2"))
			old := newInstance(t, api, clock, "controller-0")
			old.tick()
			oldTerm := mustLead(t, old)
			if oldTerm.Epoch != 3 {
				t.Fatalf("old epoch %d", oldTerm.Epoch)
			}
			for range 5 {
				if err := old.publish(rec); err != nil {
					t.Fatal(err)
				}
			}
			delayedOld, _ := oldTerm.Stamp()
			held := rec.holds()

			// An operator deletes the Lease by hand; the controller or the operator recreates it.
			api.del()
			if recreated != nil {
				api.seed(validLease(*recreated))
			}
			fresh := newInstance(t, api, clock, "controller-1")
			delayedNew := Stamp{}
			for range 3 {
				fresh.tick()
				if fresh.leads() {
					break
				}
				clock.Advance(tick)
			}
			termNew := mustLead(t, fresh)
			if termNew.Lineage == oldTerm.Lineage {
				t.Fatal("the recreated Lease must be a new lineage")
			}
			delayedNew, _ = termNew.Stamp() // taken before shutdown, sent after the takeover below

			if err := fresh.publish(rec); !errors.Is(err, ErrFenceRejected) {
				t.Fatalf("the recreated Lease's report was accepted or failed differently: %v", err)
			}
			if rec.holds() != held {
				t.Fatalf("the receiver changed: %+v, was %+v", rec.holds(), held)
			}
			if s := fresh.e.Status(); s.Reason != ReasonFenceRejected || s.Ready || s.Leading {
				t.Fatalf("the controller must end stopped with FenceRejected: %+v", s)
			}
			epochBefore := api.epoch()
			if fresh.leads() {
				t.Fatal("duty writes continue after a refusal")
			}
			select {
			case <-termNew.Context().Done():
			default:
				t.Fatal("the duty context was not cancelled")
			}

			// Time passes, the Lease is released and expires: no retry, no higher epoch, still FenceRejected.
			for range 40 {
				clock.Advance(tick)
				fresh.tick()
				if err := fresh.publish(rec); !errors.Is(err, ErrFenceRejected) {
					t.Fatalf("after the refusal: %v", err)
				}
			}
			if api.epoch() != epochBefore || fresh.e.Status().Reason != ReasonFenceRejected {
				t.Fatalf("epoch %s -> %s, status %+v", epochBefore, api.epoch(), fresh.e.Status())
			}
			if api.snapshot().HolderIdentity != "" {
				t.Fatal("a stopped instance must have released the Lease")
			}

			// Reports delayed across the shutdown and takeover are refused at any epoch.
			requireFenced(t, rec.Write(context.Background(), delayedNew, nil))
			for _, epoch := range []int64{1, termNew.Epoch, oldTerm.Epoch, 99} {
				requireFenced(t, rec.Write(context.Background(), Stamp{Lineage: termNew.Lineage, Epoch: epoch, Seq: 1000}, nil))
			}
			if rec.holds() != held {
				t.Fatalf("the receiver changed: %+v", rec.holds())
			}

			// Operator recovery: reset the receiver, restart the controller. A new term is
			// accepted, and a report delayed from the deleted lineage is still refused.
			rec.reset(api.snapshot().UID)
			// A report delayed from the deleted lineage arrives first, right after the reset:
			// it must neither be accepted nor reclaim the receiver for the old lineage.
			requireFenced(t, rec.Write(context.Background(), delayedOld, nil))
			requireFenced(t, rec.Write(context.Background(), Stamp{Lineage: oldTerm.Lineage, Epoch: 1, Seq: 1}, nil))
			if rec.holds().Lineage != termNew.Lineage {
				t.Fatalf("the old lineage reclaimed the receiver: %+v", rec.holds())
			}
			restarted := newInstance(t, api, clock, "controller-1")
			for range 10 {
				restarted.tick()
				if restarted.leads() {
					break
				}
				clock.Advance(tick)
			}
			if term := mustLead(t, restarted); term.Lineage != termNew.Lineage || term.Epoch <= termNew.Epoch {
				t.Fatalf("restarted term %+v after %+v", term, termNew)
			}
			if err := restarted.publish(rec); err != nil {
				t.Fatalf("after the reset a new term must be accepted: %v", err)
			}
			requireFenced(t, rec.Write(context.Background(), delayedOld, nil))
			requireFenced(t, rec.Write(context.Background(), Stamp{Lineage: oldTerm.Lineage, Epoch: 99, Seq: 99}, nil))
		})
	}
}

func ptr(s string) *string { return &s }

func TestRenewalAfterDeletionStepsDownAndTheNewLineageIsRefused(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a := newInstance(t, api, clock, "controller-0")
	a.tick()
	if err := a.publish(rec); err != nil {
		t.Fatal(err)
	}
	api.del()
	clock.Advance(tick)
	a.client.getNotFound = true
	a.tick() // the renewal answers 404: the gate closes at once, not after the renew deadline
	if a.leads() || a.e.Status().Reason != ReasonLeadershipLost {
		t.Fatalf("status %+v", a.e.Status())
	}
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
		t.Fatal(err)
	}
	a.client.getNotFound = false
	clock.Advance(tick)
	a.tick() // bootstrap: epoch 1 on a new lineage
	if term := mustLead(t, a); term.Epoch != 1 || term.Lineage == "uid-1" {
		t.Fatalf("term %+v", term)
	}
	if err := a.publish(rec); !errors.Is(err, ErrFenceRejected) {
		t.Fatalf("the receiver must refuse the new lineage at epoch 1: %v", err)
	}
	if rec.holds().Lineage != "uid-1" {
		t.Fatalf("receiver %+v", rec.holds())
	}
}

// --- gate timing -------------------------------------------------------------

func TestRenewalFailureClosesTheGateAtTheRenewDeadlineAndCancelsDutyWork(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	a.tick() // renewed at t=0
	b.tick()
	term := mustLead(t, a)
	a.client.down = true
	for elapsed := tick; elapsed < DefaultRenewDeadline; elapsed += tick {
		clock.Advance(tick)
		a.tick()
		b.tick()
		if !a.leads() {
			t.Fatalf("the gate closed early, at %s", elapsed)
		}
		if err := a.publish(rec); err != nil {
			t.Fatal(err)
		}
	}
	clock.Advance(tick) // t = renewDeadline
	if a.leads() {
		t.Fatal("the gate must be closed at the renew deadline, even before the next Tick")
	}
	a.tick()
	select {
	case <-term.Context().Done():
	default:
		t.Fatal("duty work was not cancelled when the gate closed")
	}
	writes := rec.acceptedCount()
	if err := a.publish(rec); !errors.Is(err, ErrNotLeader) || rec.acceptedCount() != writes {
		t.Fatalf("a closed gate wrote: %v", err)
	}
	if s := a.e.Status(); s.Reason != ReasonLeadershipLost || s.Ready {
		t.Fatalf("status %+v", s)
	}
}

// With the last renewal at t=0, a failing renewal that returns at t=7 and a retry from t=9
// that succeeds at t=13: the gate closes at t=10 and the duty context is cancelled at once,
// before any later Tick, and the late success does not reopen it.
func TestGateExpiryCancelsDutyWorkWithoutATickAndALateRenewalDoesNotReviveIt(t *testing.T) {
	clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
	a := newInstance(t, api, clock, "controller-0")
	a.tick() // t=0: leads, renewed at t=0
	term := mustLead(t, a)
	calls := 0
	a.client.beforeUpdate = func() error {
		calls++
		if calls == 1 { // a slow failure, back at t=7
			clock.Advance(7 * time.Second)
			return errDown
		}
		clock.Advance(4 * time.Second) // the retry from t=9 returns at t=13
		select {
		case <-term.Context().Done():
		case <-time.After(10 * time.Second):
			t.Error("the gate closed at t=10 without cancelling the duty context")
		}
		return nil
	}
	a.tick() // renewal fails at t=7: the gate is still open
	if !a.leads() {
		t.Fatal("the gate closed before the renew deadline")
	}
	clock.Advance(2 * time.Second) // t=9
	a.tick()                       // the renewal lands at t=13, past the deadline
	if a.leads() || a.e.Status().Reason != ReasonLeadershipLost {
		t.Fatalf("the late renewal reopened the gate: %+v", a.e.Status())
	}
	if _, err := term.Stamp(); !errors.Is(err, ErrNotLeader) {
		t.Fatalf("an expired term can still stamp writes: %v", err)
	}
	if err := a.e.Publish(context.Background(), term, rec, nil); !errors.Is(err, ErrNotLeader) || rec.acceptedCount() != 0 {
		t.Fatalf("an expired term published: %v", err)
	}
	if api.snapshot().HolderIdentity != "" {
		t.Fatal("the instance that lost its gate must hand the Lease back")
	}
}

func TestDegradedStatesProduceNoWritesAndNoReports(t *testing.T) {
	for name, setup := range map[string]func(api *fakeAPI, i *instance){
		"API server unavailable": func(_ *fakeAPI, i *instance) { i.client.down = true },
		"foreign Lease":          func(api *fakeAPI, _ *instance) { l := validLease("1"); delete(l.Labels, OwnerLabel); api.seed(l) },
		"unusable epoch":         func(api *fakeAPI, _ *instance) { l := validLease("x"); api.seed(l) },
	} {
		t.Run(name, func(t *testing.T) {
			clock, api, rec := newFakeClock(epoch0), newFakeAPI(), &fakeReceiver{}
			a := newInstance(t, api, clock, "controller-0")
			setup(api, a)
			run(clock, time.Minute, a)
			if err := a.publish(rec); !errors.Is(err, ErrNotLeader) {
				t.Fatal(err)
			}
			if api.writeCount() != 0 || rec.acceptedCount() != 0 || a.e.Status().Ready {
				t.Fatalf("writes=%d accepted=%d status=%+v", api.writeCount(), rec.acceptedCount(), a.e.Status())
			}
		})
	}
}

// --- shutdown ----------------------------------------------------------------

func TestSIGTERMStopsDutiesThenReleasesTheLeaseSoAStandbyTakesOverAtOnce(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a, b := newInstance(t, api, clock, "controller-0"), newInstance(t, api, clock, "controller-1")
	ctx, cancel := context.WithCancel(context.Background())
	started := make(chan *Term)
	done := make(chan error)
	go func() {
		done <- a.e.Run(ctx, func(dutyCtx context.Context, term *Term) {
			started <- term
			<-dutyCtx.Done()
			api.record("duties stopped")
		})
	}()
	term := <-started
	if term.Epoch != 1 {
		t.Fatalf("epoch %d", term.Epoch)
	}
	cancel() // SIGTERM
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if got := api.events; !reflect.DeepEqual(got, []string{"create", "duties stopped", "update:"}) {
		t.Fatalf("events %v: duties must stop before the Lease is released", got)
	}
	if l := api.snapshot(); l.HolderIdentity != "" || l.Annotations[EpochAnnotation] != "1" {
		t.Fatalf("released Lease %+v must keep its epoch", l)
	}
	if a.leads() || a.e.Status().Reason != ReasonStopped {
		t.Fatalf("status %+v", a.e.Status())
	}
	b.tick() // no clock advance: the standby does not wait for expiry
	if got := mustLead(t, b); got.Epoch != 2 {
		t.Fatalf("epoch %d", got.Epoch)
	}
}

func TestShutdownWithoutLeadershipTouchesNoLease(t *testing.T) {
	clock, api := newFakeClock(epoch0), newFakeAPI()
	a := newInstance(t, api, clock, "controller-0")
	a.client.down = true
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error)
	go func() {
		done <- a.e.Run(ctx, func(context.Context, *Term) { t.Error("a duty started without the Lease") })
	}()
	cancel()
	<-done
	if api.writeCount() != 0 {
		t.Fatal("a standby wrote while shutting down")
	}
}

// --- configuration -----------------------------------------------------------

func TestConfigValidation(t *testing.T) {
	good := Config{
		Identity: "controller-0", Namespace: relNS,
		LeaseDuration: DefaultLeaseDuration, RenewDeadline: DefaultRenewDeadline, RetryPeriod: DefaultRetryPeriod,
		Lease: newFakeAPI().client(),
	}
	if err := good.Validate(); err != nil {
		t.Fatal(err)
	}
	for name, edit := range map[string]func(c *Config){
		"empty identity":      func(c *Config) { c.Identity = "" },
		"upper-case identity": func(c *Config) { c.Identity = "Controller-0" },
		"underscore identity": func(c *Config) { c.Identity = "controller_0" },
		"slash identity":      func(c *Config) { c.Identity = "a/b" },
		"long identity":       func(c *Config) { c.Identity = strings.Repeat("a", 254) },
		"empty namespace":     func(c *Config) { c.Namespace = "" },
		"dotted namespace":    func(c *Config) { c.Namespace = "a.b" },
		"zero retry":          func(c *Config) { c.RetryPeriod = 0 },
		"renew <= retry":      func(c *Config) { c.RenewDeadline = c.RetryPeriod },
		"lease <= renew":      func(c *Config) { c.LeaseDuration = c.RenewDeadline },
		"no client":           func(c *Config) { c.Lease = nil },
	} {
		c := good
		edit(&c)
		if _, err := New(c); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}
