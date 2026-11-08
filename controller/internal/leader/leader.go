// Package leader gates singleton duties with a namespaced Lease. Gate deadlines
// are best effort; receivers MUST enforce fencing on every duty write.
package leader

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"math"
	"regexp"
	"strconv"
	"sync"
	"time"

	"github.com/gntik-ai/falcone-serverless/controller/internal/status/poolstatus"
)

const (
	LeaseName          = "falcone-pool-controller-leader"
	EpochAnnotation    = "falcone.ai/leader-epoch"
	EpochUIDAnnotation = "falcone.ai/leader-epoch-uid"
	owner              = "falcone.ai/owner"
	version            = "falcone.ai/ownership-version"
)

var (
	ErrNotFound  = errors.New("Lease not found")
	ErrConflict  = errors.New("Lease precondition conflict")
	ErrHistory   = errors.New("monotonic epoch continuity unavailable; publishing refused")
	ErrOwnership = errors.New("Lease ownership or identity is invalid")
	ErrStandby   = errors.New("singleton duty is not authorized")
	dnsLabel     = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)
	dnsSubdomain = regexp.MustCompile(`^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$`)
)

// Metadata retains ownership metadata and Kubernetes lifecycle fields on PUT.
type Metadata struct {
	Name              string            `json:"name"`
	Namespace         string            `json:"namespace"`
	UID               string            `json:"uid,omitempty"`
	ResourceVersion   string            `json:"resourceVersion,omitempty"`
	Labels            map[string]string `json:"labels,omitempty"`
	Annotations       map[string]string `json:"annotations,omitempty"`
	DeletionTimestamp *time.Time        `json:"deletionTimestamp,omitempty"`
	Finalizers        []string          `json:"finalizers,omitempty"`
	OwnerReferences   json.RawMessage   `json:"ownerReferences,omitempty"`
}
type LeaseSpec struct {
	HolderIdentity       string     `json:"holderIdentity,omitempty"`
	LeaseDurationSeconds int        `json:"leaseDurationSeconds,omitempty"`
	AcquireTime          *time.Time `json:"acquireTime,omitempty"`
	RenewTime            *time.Time `json:"renewTime,omitempty"`
}
type Lease struct {
	APIVersion string    `json:"apiVersion"`
	Kind       string    `json:"kind"`
	Metadata   Metadata  `json:"metadata"`
	Spec       LeaseSpec `json:"spec"`
}

// Client updates must use the UID and resourceVersion supplied by a fresh Get.
// Implementations return detached objects; callers may mutate them for Update.
type Client interface {
	Get(context.Context, string, string) (Lease, error)
	Create(context.Context, string, Lease) (Lease, error)
	Update(context.Context, string, Lease) (Lease, error)
}
type Timer interface{ Stop() bool }
type Clock interface {
	Now() time.Time
	AfterFunc(time.Duration, func()) Timer
}
type RealClock struct{}

func (RealClock) Now() time.Time                            { return time.Now() }
func (RealClock) AfterFunc(d time.Duration, f func()) Timer { return time.AfterFunc(d, f) }

type Config struct {
	Namespace           string // Downward-API POD_NAMESPACE, never a pool namespace.
	ControllerNamespace string // The exact namespace checked by preflight.
	PodName             string
	PreflightPassed     bool
	LeaseDuration       time.Duration
	RenewDeadline       time.Duration
	RetryPeriod         time.Duration
}

func DefaultConfig(namespace, pod string) Config {
	return Config{Namespace: namespace, ControllerNamespace: namespace, PodName: pod, LeaseDuration: 15 * time.Second, RenewDeadline: 10 * time.Second, RetryPeriod: 2 * time.Second}
}

// ValidateIdentity is also used before preflight emits its single JSON report.
func ValidateIdentity(pod, namespace, controllerNamespace string) error {
	if len(pod) == 0 || len(pod) > 253 || !dnsSubdomain.MatchString(pod) {
		return errors.New("POD_NAME is missing or invalid")
	}
	for _, part := range splitLabels(pod) {
		if len(part) > 63 || !dnsLabel.MatchString(part) {
			return errors.New("POD_NAME is missing or invalid")
		}
	}
	if len(namespace) == 0 || len(namespace) > 63 || !dnsLabel.MatchString(namespace) || namespace != controllerNamespace {
		return errors.New("POD_NAMESPACE must match controllerNamespace")
	}
	return nil
}
func splitLabels(s string) []string {
	var out []string
	start := 0
	for i := range s {
		if s[i] == '.' {
			out = append(out, s[start:i])
			start = i + 1
		}
	}
	return append(out, s[start:])
}
func (c Config) validate() error {
	if err := ValidateIdentity(c.PodName, c.Namespace, c.ControllerNamespace); err != nil {
		return err
	}
	if !c.PreflightPassed {
		return errors.New("Lease preflight did not pass")
	}
	if c.LeaseDuration < 3*time.Second || c.LeaseDuration > 60*time.Second || c.LeaseDuration%time.Second != 0 || c.RenewDeadline < time.Second || c.RenewDeadline >= c.LeaseDuration || c.RetryPeriod < 100*time.Millisecond || c.RetryPeriod >= c.RenewDeadline {
		return errors.New("invalid Lease timings: retry < renew < lease, lease 3..60 whole seconds")
	}
	return nil
}

// Fence uses the ctr-pool-status reporterEpoch and observationSeq integer domain.
type Fence struct{ Epoch, Seq int64 }
type Gate struct {
	mu         sync.Mutex
	ctx        context.Context
	cancel     context.CancelFunc
	clock      Clock
	timer      Timer
	deadline   time.Time
	epoch, seq int64
}

func (g *Gate) Context() context.Context { return g.ctx }
func (g *Gate) active() bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.clock.Now().Before(g.deadline) {
		g.cancel()
	}
	return g.ctx.Err() == nil
}
func (g *Gate) close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.cancel()
	if g.timer != nil {
		g.timer.Stop()
	}
}
func (g *Gate) extend(deadline time.Time) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.ctx.Err() != nil || !g.clock.Now().Before(deadline) {
		g.cancel()
		return false
	}
	if g.timer != nil {
		g.timer.Stop()
	}
	g.deadline = deadline
	// A delayed timer from an earlier renewal cannot cancel a later deadline.
	g.timer = g.clock.AfterFunc(deadline.Sub(g.clock.Now()), func() {
		g.mu.Lock()
		defer g.mu.Unlock()
		if !g.clock.Now().Before(g.deadline) {
			g.cancel()
		}
	})
	return true
}

// Write authorizes one fresh desired-state duty write, reserving a unique fence.
// The callback must attach BOTH fields to its write (pool-status reports use
// reporterEpoch/observationSeq) and obey ctx cancellation. In-flight writes may
// complete after closure; only receiver-side fencing makes those writes safe.
// No duty callback runs on stale/unavailable desired state or a closed gate.
func (g *Gate) Write(desiredStateFresh bool, write func(context.Context, Fence) error) error {
	g.mu.Lock()
	if !desiredStateFresh || g.ctx.Err() != nil || !g.clock.Now().Before(g.deadline) || g.seq == math.MaxInt64 {
		g.mu.Unlock()
		return ErrStandby
	}
	g.seq++
	fence := Fence{Epoch: g.epoch, Seq: g.seq}
	ctx := g.ctx
	g.mu.Unlock()
	return write(ctx, fence)
}

// PublishReport is the pool-status duty boundary. Caller-supplied fencing fields
// are always replaced. Transport/encoding and receiver authorization remain the
// publisher's responsibility; the receiver must enforce the epoch guard.
func (g *Gate) PublishReport(desiredStateFresh bool, report poolstatus.Report, publish func(context.Context, poolstatus.Report) error) error {
	return g.Write(desiredStateFresh, func(ctx context.Context, fence Fence) error {
		report.ReporterEpoch = fence.Epoch
		report.ObservationSeq = fence.Seq
		return publish(ctx, report)
	})
}

// Elector's campaign methods are serial. Gate methods are concurrency safe.
type Elector struct {
	cfg             Config
	client          Client
	clock           Clock
	holder          string // Per-process nonce prevents a restarted pod renewing an old term.
	gate            *Gate
	uid             string
	highest         int64
	observedVersion string
	observedUID     string
	observedAt      time.Time
}

func New(c Config, client Client, clock Clock) (*Elector, error) {
	if err := c.validate(); err != nil {
		return nil, err
	}
	if client == nil || clock == nil {
		return nil, errors.New("Lease client and clock are required")
	}
	var nonce [16]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, errors.New("holder identity unavailable")
	}
	// Keep the holder short even for a maximal DNS-subdomain pod name. The
	// process nonce supplies uniqueness; the prefix is only for diagnostics.
	prefix := c.PodName
	if len(prefix) > 63 {
		prefix = prefix[:63]
	}
	return &Elector{cfg: c, client: client, clock: clock, holder: prefix + "/" + hex.EncodeToString(nonce[:]), highest: -1}, nil
}
func (e *Elector) Gate() *Gate { return e.gate }
func (e *Elector) close() {
	if e.gate != nil {
		e.gate.close()
	}
}
func (e *Elector) owned(l Lease) bool {
	var identity []string
	return l.APIVersion == "coordination.k8s.io/v1" && l.Kind == "Lease" && l.Metadata.Name == LeaseName && l.Metadata.Namespace == e.cfg.Namespace && l.Metadata.UID != "" && l.Metadata.ResourceVersion != "" && l.Metadata.DeletionTimestamp == nil && l.Metadata.Labels[owner] == "pool-controller" && l.Metadata.Annotations[version] == "1" && json.Unmarshal([]byte(l.Metadata.Annotations[owner]), &identity) == nil && len(identity) == 2 && identity[0] == e.cfg.Namespace && identity[1] == "leader"
}
func epoch(l Lease) (int64, error) {
	// A restored/copied Lease cannot reuse a deleted object's epoch certificate.
	// The UID-bound floor is an operator assertion of continuity, NOT something
	// this process can invent from a 404, a clock, or its own incomplete memory.
	if l.Metadata.Annotations[EpochUIDAnnotation] != l.Metadata.UID {
		return 0, ErrHistory
	}
	text := l.Metadata.Annotations[EpochAnnotation]
	n, err := strconv.ParseInt(text, 10, 64)
	if err != nil || n < 0 || strconv.FormatInt(n, 10) != text {
		return 0, ErrHistory
	}
	return n, nil
}

// Step makes one campaign/renewal attempt. A fresh verified GET is mandatory for
// every PUT, including release. No local no-overlap timing barrier is claimed.
func (e *Elector) Step(ctx context.Context) error {
	if ctx.Err() != nil {
		e.close()
		return ctx.Err()
	}
	started := e.clock.Now()
	l, err := e.client.Get(ctx, e.cfg.Namespace, LeaseName)
	if errors.Is(err, ErrNotFound) {
		e.close()
		// Create only an inert recovery marker. No epoch means no duty gate, even
		// after repeated 404s, restart, or a delayed/ambiguous Create response.
		identity, _ := json.Marshal([]string{e.cfg.Namespace, "leader"})
		_, createErr := e.client.Create(ctx, e.cfg.Namespace, Lease{APIVersion: "coordination.k8s.io/v1", Kind: "Lease", Metadata: Metadata{Name: LeaseName, Namespace: e.cfg.Namespace, Labels: map[string]string{owner: "pool-controller"}, Annotations: map[string]string{owner: string(identity), version: "1"}}})
		if createErr != nil && !errors.Is(createErr, ErrConflict) {
			return createErr
		}
		return ErrHistory
	}
	if err != nil {
		return err
	}
	if !e.owned(l) {
		e.close()
		return ErrOwnership
	}
	floor, err := epoch(l)
	if err != nil || floor < e.highest {
		e.close()
		return ErrHistory
	}
	e.highest = floor
	if l.Metadata.ResourceVersion != e.observedVersion || l.Metadata.UID != e.observedUID {
		e.observedVersion = l.Metadata.ResourceVersion
		e.observedUID = l.Metadata.UID
		e.observedAt = e.clock.Now()
	}
	active := e.gate != nil && e.gate.active() && e.uid == l.Metadata.UID && l.Spec.HolderIdentity == e.holder && e.gate.epoch == floor
	if !active {
		e.close()
		if l.Spec.HolderIdentity != "" {
			d := time.Duration(l.Spec.LeaseDurationSeconds) * time.Second
			if l.Spec.LeaseDurationSeconds < 3 || l.Spec.LeaseDurationSeconds > 60 || e.clock.Now().Sub(e.observedAt) < d {
				return ErrStandby
			}
		}
		if floor == math.MaxInt64 {
			return ErrHistory
		}
		floor++
		l.Metadata.Annotations[EpochAnnotation] = strconv.FormatInt(floor, 10)
		l.Spec.AcquireTime = &started
	}
	e.uid = l.Metadata.UID
	l.Spec.HolderIdentity = e.holder
	l.Spec.LeaseDurationSeconds = int(e.cfg.LeaseDuration / time.Second)
	l.Spec.RenewTime = &started
	updated, err := e.client.Update(ctx, e.cfg.Namespace, l)
	if err != nil {
		return err
	}
	got, epochErr := epoch(updated)
	if !e.owned(updated) || updated.Metadata.UID != e.uid || epochErr != nil || got != floor || updated.Spec.HolderIdentity != e.holder {
		e.close()
		return ErrHistory
	}
	e.highest = floor
	if ctx.Err() != nil || !e.clock.Now().Before(started.Add(e.cfg.RenewDeadline)) {
		e.close()
		return ErrStandby
	}
	if !active {
		gateCtx, cancel := context.WithCancel(ctx)
		e.gate = &Gate{ctx: gateCtx, cancel: cancel, clock: e.clock, epoch: floor}
	}
	if !e.gate.extend(started.Add(e.cfg.RenewDeadline)) {
		return ErrStandby
	}
	return nil
}

// Release closes duty work first and clears only our currently verified term.
// Keeping the Lease (and its epoch) preserves continuity for the next leader.
func (e *Elector) Release(ctx context.Context) error {
	e.close()
	l, err := e.client.Get(ctx, e.cfg.Namespace, LeaseName)
	if err != nil {
		return err
	}
	if !e.owned(l) {
		return ErrOwnership
	}
	floor, err := epoch(l)
	if err != nil {
		return err
	}
	if e.gate == nil || l.Metadata.UID != e.uid || l.Spec.HolderIdentity != e.holder || floor != e.gate.epoch {
		return ErrStandby
	}
	l.Spec.HolderIdentity = ""
	_, err = e.client.Update(ctx, e.cfg.Namespace, l)
	return err
}

// Run maintains the gate until cancellation. Each term gets a cancellable duty
// context. Errors keep the process degraded and are logged only through observe.
func (e *Elector) Run(ctx context.Context, duty func(*Gate), observe func(error)) {
	defer func() {
		e.close()
		releaseCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := e.Release(releaseCtx); err != nil && observe != nil {
			observe(err)
		}
	}()
	var previous *Gate
	for ctx.Err() == nil {
		if err := e.Step(ctx); err != nil && observe != nil {
			observe(err)
		}
		if e.gate != nil && e.gate != previous && e.gate.Context().Err() == nil {
			previous = e.gate
			if duty != nil {
				go duty(e.gate)
			}
		}
		ready := make(chan struct{})
		timer := e.clock.AfterFunc(e.cfg.RetryPeriod, func() { close(ready) })
		select {
		case <-ctx.Done():
			timer.Stop()
		case <-ready:
		}
	}
}
