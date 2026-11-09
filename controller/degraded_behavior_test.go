package main

import (
	"strings"
	"testing"

	"github.com/gntik-ai/falcone-serverless/controller/internal/leader"
)

const degradedBehaviorPath = "../docs/operations/degraded-behavior.md"

// The operations document must cover every degraded state of OPS-01.t3, in its own words,
// and name every failure reason the election can report.
func TestDegradedBehaviorDocumentCoversEveryState(t *testing.T) {
	doc := strings.Join(strings.Fields(readFile(t, degradedBehaviorPath)), " ")
	for topic, phrases := range map[string][]string{
		"no replica changes, no deletions": {"No replica changes.", "No deletions."},
		"leader loss":                      {"Leader lost", "Closes the gate, cancels duty work"},
		"API server unavailable":           {"API server unavailable or slow", "An ambiguous Create or Update never makes a replica lead"},
		"stale desired state":              {"Falcone desired state stale or unavailable", "make no replica change and no deletion"},
		"failed preflight":                 {"Failed preflight", "never campaigns"},
		"token rotation":                   {"reloaded on every Lease call", "fails closed"},
		"release namespace only":           {"lives only in the release namespace", "no Lease is ever created or read in a pool namespace", "`POD_NAMESPACE` must equal `controllerNamespace`"},
		"ownership":                        {"`falcone.ai/ownership-version`", `["<release namespace>","leader"]`, "never modified"},
		"epoch":                            {"never comes from a wall clock", "optimistic concurrency", "epoch 1"},
		"lineage fence":                    {"`reporterLineage` (the Lease `metadata.uid`)", "never accepted after a newer epoch has published"},
		"overlap window":                   {"The overlap window", "Two replicas may both believe they lead", "paused", "clock runs slow"},
		"Lease deletion":                   {"Lease deletion and operator recovery", "refuses every report of the new lineage", "no automatic recovery for a lost epoch or lineage"},
		"operator recovery":                {"Resets the receiver's recorded lineage and epoch for this singleton", "Restarts the controller pods", "binding it to the current Lease's `metadata.uid`", "still refused"},
		"SIGTERM":                          {"SIGTERM", "stops its duties first", "releases the Lease"},
		"kill switch":                      {"set the controller replica count to 1"},
	} {
		for _, p := range phrases {
			if !strings.Contains(doc, p) {
				t.Errorf("degraded-behavior.md (%s): missing %q", topic, p)
			}
		}
	}
	for _, reason := range []string{
		leader.ReasonStandby, leader.ReasonLeaseUnavailable, leader.ReasonOwnershipMismatch,
		leader.ReasonEpochInvalid, leader.ReasonLeadershipLost, leader.ReasonFenceRejected,
	} {
		if !strings.Contains(doc, "`"+reason+"`") {
			t.Errorf("degraded-behavior.md does not document the status reason %q", reason)
		}
	}
	for _, p := range []string{"`falcone-pool-controller-leader`", "`leaseDurationSeconds` 15, `renewDeadlineSeconds` 10 and `retryPeriodSeconds` 2"} {
		if !strings.Contains(doc, p) {
			t.Errorf("degraded-behavior.md does not state %q", p)
		}
	}
}
