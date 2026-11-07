package main

import (
	"os"
	"regexp"
	"strings"
	"testing"
)

// sections returns the text under each "###" heading of the degraded-behavior doc.
func degradedSections(t *testing.T) map[string]string {
	t.Helper()
	data, err := os.ReadFile("../docs/operations/degraded-behavior.md")
	if err != nil {
		t.Fatal(err)
	}
	out := map[string]string{}
	parts := regexp.MustCompile(`(?m)^#{2,3} (.+)$`).Split(string(data), -1)
	titles := regexp.MustCompile(`(?m)^#{2,3} (.+)$`).FindAllStringSubmatch(string(data), -1)
	for i, m := range titles {
		out[m[1]] = parts[i+1]
	}
	return out
}

func TestDegradedBehaviorDocumentsEveryState(t *testing.T) {
	sections := degradedSections(t)
	for _, name := range []string{"Leader loss", "API server unavailable", "Stale or unavailable desired state", "Failed preflight"} {
		body, ok := sections[name]
		if !ok || strings.TrimSpace(body) == "" {
			t.Errorf("docs/operations/degraded-behavior.md has no non-empty %q section", name)
		}
	}
	for _, name := range []string{"Recovery barrier", "Shutdown"} {
		if _, ok := sections[name]; !ok {
			t.Errorf("section %q is missing", name)
		}
	}
	degraded := sections["Degraded states"]
	for _, want := range []string{"changes no replica count on stale data", "deletes nothing"} {
		if !strings.Contains(degraded, want) {
			t.Errorf("the Degraded states introduction must say %q", want)
		}
	}
}
