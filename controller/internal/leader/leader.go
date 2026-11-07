// Package leader fences singleton duties with the owned release-namespace Lease.
// A Gate must be checked immediately before every write or publish. It does not
// authorize scaler duties or validate desired state and pool ownership.
package leader

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"math"
	"sync"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/preflight"
)

var (
	ErrNotFound = errors.New("lease not found")
	ErrConflict = errors.New("lease conflict")
)

// Config timings are ordered, bounded and shared by all replicas.
type Config struct {
	Namespace, Identity                       string
	LeaseDuration, RenewDeadline, RetryPeriod time.Duration
}

func (c Config) Validate() error {
	if !preflight.ValidNamespace(c.Namespace) || c.Identity == "" || c.LeaseDuration > 300*time.Second || c.RetryPeriod <= 0 || c.LeaseDuration <= c.RenewDeadline || c.RenewDeadline <= c.RetryPeriod {
		return errors.New("invalid leader configuration")
	}
	for _, d := range []time.Duration{c.LeaseDuration, c.RenewDeadline, c.RetryPeriod} {
		if d%time.Second != 0 {
			return errors.New("invalid leader configuration")
		}
	}
	return nil
}

// NewIdentity is independent of pod name and changes on every process start.
func NewIdentity() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", errors.New("leader identity unavailable")
	}
	return hex.EncodeToString(b[:]), nil
}

type Metadata struct {
	Name            string            `json:"name"`
	Namespace       string            `json:"namespace"`
	ResourceVersion string            `json:"resourceVersion,omitempty"`
	UID             string            `json:"uid,omitempty"`
	Labels          map[string]string `json:"labels"`
	Annotations     map[string]string `json:"annotations"`
}
type Spec struct {
	HolderIdentity       string `json:"holderIdentity"`
	LeaseDurationSeconds int64  `json:"leaseDurationSeconds"`
	AcquireTime          string `json:"acquireTime,omitempty"`
	RenewTime            string `json:"renewTime,omitempty"`
	LeaseTransitions     int64  `json:"leaseTransitions"`
}
type Lease struct {
	APIVersion string   `json:"apiVersion"`
	Kind       string   `json:"kind"`
	Metadata   Metadata `json:"metadata"`
	Spec       Spec     `json:"spec"`
	// Raw preserves fields this version does not interpret on update.
	Raw map[string]json.RawMessage `json:"-"`
}

type Client interface {
	Get(context.Context) (Lease, error)
	Create(context.Context, Lease) (Lease, error)
	Update(context.Context, Lease) (Lease, error)
}
type Clock interface {
	Now() time.Time
	After(time.Duration) <-chan time.Time
	AfterFunc(time.Duration, func()) func()
}
type RealClock struct{}

func (RealClock) Now() time.Time                         { return time.Now() }
func (RealClock) After(d time.Duration) <-chan time.Time { return time.After(d) }
func (RealClock) AfterFunc(d time.Duration, f func()) func() {
	t := time.AfterFunc(d, f)
	return func() { t.Stop() }
}

// Gate is scoped to one acquisition. An expired Gate can never be reopened.
type Gate struct {
	mu         sync.Mutex
	clock      Clock
	epoch      int64
	until      time.Time
	ctx        context.Context
	cancel     context.CancelFunc
	stop       func()
	generation uint64
}

func (g *Gate) Context() context.Context {
	if g == nil {
		return context.Background()
	}
	return g.ctx
}
func (g *Gate) Epoch() (int64, bool) {
	if g == nil {
		return 0, false
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.clock.Now().Before(g.until) {
		g.cancel()
	}
	return g.epoch, g.ctx.Err() == nil
}
func (g *Gate) close() {
	if g == nil {
		return
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	g.cancel()
	if g.stop != nil {
		g.stop()
	}
}
func (g *Gate) extend(until time.Time) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.ctx.Err() != nil {
		return
	}
	if g.stop != nil {
		g.stop()
	}
	g.until = until
	g.generation++
	generation := g.generation
	g.stop = g.clock.AfterFunc(until.Sub(g.clock.Now()), func() {
		g.mu.Lock()
		defer g.mu.Unlock()
		if generation == g.generation {
			g.cancel()
		}
	})
}

// Elector's Tick, Run and Shutdown are serialized by its caller. Gates may be
// checked concurrently. API request time counts against the local renew window.
type Elector struct {
	config     Config
	client     Client
	clock      Clock
	log        func(string)
	gate       *Gate
	observed   Spec
	seen       bool
	observedAt time.Time
	highest    int64
	heldEpoch  int64
}

func New(c Config, client Client, clock Clock, log func(string)) (*Elector, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}
	if client == nil || clock == nil {
		return nil, errors.New("invalid leader dependencies")
	}
	if log == nil {
		log = func(string) {}
	}
	return &Elector{config: c, client: client, clock: clock, log: log, highest: -1, heldEpoch: -1}, nil
}
func (e *Elector) Gate() *Gate { return e.gate }
func (e *Elector) verified(l Lease) bool {
	var owner []string
	err := json.Unmarshal([]byte(l.Metadata.Annotations["falcone.ai/owner"]), &owner)
	return l.Metadata.Name == preflight.LeaderLeaseName && l.Metadata.Namespace == e.config.Namespace && l.Metadata.ResourceVersion != "" && l.Metadata.Labels["falcone.ai/owner"] == "pool-controller" && l.Metadata.Annotations["falcone.ai/ownership-version"] == "1" && err == nil && len(owner) == 2 && owner[0] == e.config.Namespace && owner[1] == "leader" && l.Spec.LeaseTransitions >= 0 && l.Spec.LeaseTransitions <= math.MaxInt32 && l.Spec.LeaseDurationSeconds > 0 && l.Spec.LeaseDurationSeconds <= 300
}
func (e *Elector) observe(l Lease) {
	if !e.seen || l.Spec != e.observed {
		e.observedAt = e.clock.Now()
		e.observed = l.Spec
		e.seen = true
	}
	if l.Spec.LeaseTransitions > e.highest {
		e.highest = l.Spec.LeaseTransitions
	}
	if l.Spec.HolderIdentity != e.config.Identity || e.gate != nil && l.Spec.LeaseTransitions != e.gate.epoch {
		e.gate.close()
	}
}
func (e *Elector) fresh() Lease {
	owner, _ := json.Marshal([]string{e.config.Namespace, "leader"})
	return Lease{APIVersion: "coordination.k8s.io/v1", Kind: "Lease", Metadata: Metadata{Name: preflight.LeaderLeaseName, Namespace: e.config.Namespace, Labels: map[string]string{"falcone.ai/owner": "pool-controller"}, Annotations: map[string]string{"falcone.ai/owner": string(owner), "falcone.ai/ownership-version": "1"}}}
}

// Lease timestamps use Kubernetes MicroTime's canonical UTC precision so the
// server's JSON normalization does not change the record we just wrote.
func leaseTime(t time.Time) string {
	return t.UTC().Truncate(time.Microsecond).Format("2006-01-02T15:04:05.000000Z07:00")
}

// Tick attempts one renewal/acquisition. Remote timestamps are opaque record
// fields: only elapsed local time since an observed change decides expiration.
func (e *Elector) Tick(ctx context.Context) {
	if ctx.Err() != nil {
		e.gate.close()
		return
	}
	start := e.clock.Now()
	l, err := e.client.Get(ctx)
	if ctx.Err() != nil {
		e.gate.close()
		return
	}
	missing := errors.Is(err, ErrNotFound)
	if err != nil && !missing {
		return
	}
	if !missing {
		if !e.verified(l) {
			e.gate.close()
			e.log("leader lease ownership unverified")
			return
		}
		e.observe(l)
	} else {
		e.gate.close()
		l = e.fresh()
	}
	_, active := e.gate.Epoch()
	renew := !missing && active && l.Spec.HolderIdentity == e.config.Identity
	if !renew && !missing && l.Spec.HolderIdentity != "" && start.Sub(e.observedAt) < time.Duration(l.Spec.LeaseDurationSeconds)*time.Second {
		return
	}
	if !renew {
		if e.highest >= math.MaxInt32 {
			e.gate.close()
			e.log("leader epoch exhausted")
			return
		}
		l.Spec.LeaseTransitions = e.highest + 1
		l.Spec.AcquireTime = leaseTime(start)
	}
	l.Spec.HolderIdentity = e.config.Identity
	l.Spec.LeaseDurationSeconds = int64(e.config.LeaseDuration / time.Second)
	l.Spec.RenewTime = leaseTime(start)
	var result Lease
	if missing {
		result, err = e.client.Create(ctx, l)
	} else {
		result, err = e.client.Update(ctx, l)
	}
	if err != nil {
		if errors.Is(err, ErrConflict) {
			e.gate.close()
			if reread, readErr := e.client.Get(ctx); readErr == nil && e.verified(reread) {
				e.observe(reread)
			}
		}
		return
	}
	if !e.verified(result) || result.Spec != l.Spec {
		e.gate.close()
		return
	}
	e.observe(result)
	e.heldEpoch = result.Spec.LeaseTransitions
	if ctx.Err() != nil || !e.clock.Now().Before(start.Add(e.config.RenewDeadline)) {
		e.gate.close()
		return
	}
	if renew {
		if _, ok := e.gate.Epoch(); !ok {
			return
		}
	} else {
		ctx, cancel := context.WithCancel(context.Background())
		e.gate = &Gate{clock: e.clock, epoch: result.Spec.LeaseTransitions, ctx: ctx, cancel: cancel}
	}
	e.gate.extend(start.Add(e.config.RenewDeadline))
}

// Run starts fresh duties per acquisition. It never overlaps old duties with a
// new tenure, including duties that ignore cancellation. Production duties are
// deliberately no-ops until DEP-03 supplies the desired-state/reconcile seams.
func (e *Elector) Run(ctx context.Context, duty func(context.Context, *Gate)) {
	var running *Gate
	var done <-chan struct{}
	for ctx.Err() == nil {
		next := e.clock.Now().Add(e.config.RetryPeriod)
		if running != nil {
			if _, ok := running.Epoch(); !ok {
				select {
				case <-done:
					running = nil
					done = nil
				default:
					select {
					case <-ctx.Done():
						continue
					case <-e.clock.After(e.config.RetryPeriod):
						continue
					}
				}
			}
		}
		e.Tick(ctx)
		if g := e.Gate(); g != running {
			stopped := running == nil
			if !stopped {
				select {
				case <-done:
					stopped = true
				default:
				}
			}
			if _, ok := g.Epoch(); ok && stopped {
				running = g
				ch := make(chan struct{})
				done = ch
				go func() { defer close(ch); duty(g.Context(), g) }()
			}
		}
		// Requests consume the retry period rather than extending every interval.
		if delay := next.Sub(e.clock.Now()); delay > 0 {
			select {
			case <-ctx.Done():
			case <-e.clock.After(delay):
			}
		}
	}
	e.gate.close()
	if done != nil {
		select {
		case <-done:
		case <-e.clock.After(e.config.RenewDeadline):
			return
		}
	}
	e.Shutdown(context.Background())
}

// Shutdown releases only this process's verified tenure, preserving the object
// and epoch. Call only after duties have stopped. Conflicts never force a write.
func (e *Elector) Shutdown(ctx context.Context) {
	e.gate.close()
	if e.heldEpoch < 0 {
		return
	}
	l, err := e.client.Get(ctx)
	if err != nil || !e.verified(l) || l.Spec.HolderIdentity != e.config.Identity || l.Spec.LeaseTransitions != e.heldEpoch {
		return
	}
	l.Spec.HolderIdentity = ""
	if _, err = e.client.Update(ctx, l); errors.Is(err, ErrConflict) {
		if current, readErr := e.client.Get(ctx); readErr == nil && e.verified(current) {
			e.observe(current)
		}
	}
}
