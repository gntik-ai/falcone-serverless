// Package supplychain implements an offline, test-only chart image policy gate.
// Runtime controller code must not import it.
package supplychain

import (
	"errors"
	"regexp"
	"sort"
	"strings"
)

// Table transcribes image and repository paths from a pinned chart contract.
type Table struct {
	ContractVersion string  `json:"contractVersion"`
	OCIEnablePath   string  `json:"ociEnablePath"`
	Entries         []Entry `json:"entries"`
}

// Entry identifies a dot-separated object path and its contract role.
type Entry struct {
	Path      string `json:"path"`
	Component string `json:"component"`
	Kind      string `json:"kind"`
	OCI       bool   `json:"oci"`
}

// Policy contains normalized registry/namespace prefixes ending in a slash.
type Policy struct {
	TrustedRepositoryPrefixes []string `json:"trustedRepositoryPrefixes"`
}

// Report lists both rules in policy order, with a combined pass/fail result.
type Report struct {
	Result string       `json:"result"`
	Rules  []RuleReport `json:"rules"`
}

// RuleReport has sorted path-and-reason violations; success uses an empty array.
type RuleReport struct {
	ID         string   `json:"id"`
	Result     string   `json:"result"`
	Violations []string `json:"violations"`
}

// Evaluate checks only table paths. It does not validate the complete schema.
func Evaluate(values any, table Table, policy Policy) Report {
	report := Report{Result: "pass", Rules: []RuleReport{
		{ID: "image-digest-pinned", Result: "pass", Violations: []string{}},
		{ID: "image-trusted-origin", Result: "pass", Violations: []string{}},
	}}
	if err := table.Validate(); err != nil {
		report.Rules[0].Violations = append(report.Rules[0].Violations, err.Error())
	} else {
		enabledValue, _ := lookup(values, table.OCIEnablePath)
		enabled, _ := enabledValue.(bool)
		for _, entry := range table.Entries {
			value, present := lookup(values, entry.Path)
			ruleIndex := 0
			if entry.Kind == "repository" {
				ruleIndex = 1
			}
			if !present {
				if !entry.OCI || enabled {
					report.Rules[ruleIndex].Violations = append(report.Rules[ruleIndex].Violations, entry.Path+": missing")
				}
				continue
			}
			if entry.Kind == "image" && !pinnedImage(value) {
				report.Rules[0].Violations = append(report.Rules[0].Violations, entry.Path+": expected repository and sha256 digest without a tag")
			}
			repositoryValue := value
			if entry.Kind == "image" {
				image, _ := value.(map[string]any)
				repositoryValue = image["repository"]
			}
			repository, _ := repositoryValue.(string)
			if !trustedRepository(repository, entry.Kind == "image", policy) {
				report.Rules[1].Violations = append(report.Rules[1].Violations, entry.Path+": untrusted repository")
			}
		}
	}
	for i := range report.Rules {
		rule := &report.Rules[i]
		sort.Strings(rule.Violations)
		if len(rule.Violations) != 0 {
			rule.Result = "fail"
			report.Result = "fail"
		}
	}
	return report
}

var digestPattern = regexp.MustCompile(`^sha256:[0-9a-f]{64}$`)
var repositoryComponent = regexp.MustCompile(`^[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*$`)
var registryPattern = regexp.MustCompile(`^[a-z0-9]+(?:[.-][a-z0-9]+)*(?::[0-9]+)?$`)

func trustedRepository(repository string, image bool, policy Policy) bool {
	// Origin is independent of pinning: a trusted tag-only image fails only the
	// digest rule. Repository-kind entries have no tag/digest to strip.
	if image {
		repository, _, _ = strings.Cut(repository, "@")
		lastSlash := strings.LastIndex(repository, "/")
		if colon := strings.LastIndex(repository, ":"); colon > lastSlash {
			repository = repository[:colon]
		}
	}
	if repository == "" || len(repository) > 255 {
		return false
	}
	parts := strings.Split(repository, "/")
	registry := "docker.io"
	if strings.ContainsAny(parts[0], ".:") || parts[0] == "localhost" {
		registry, parts = parts[0], parts[1:]
		if len(parts) == 0 || !registryPattern.MatchString(registry) {
			return false
		}
	} else if len(parts) > 2 {
		return false
	}
	for _, part := range parts {
		if !repositoryComponent.MatchString(part) {
			return false
		}
	}
	if registry == "docker.io" && len(parts) == 1 {
		parts = append([]string{"library"}, parts...)
	}
	normalized := registry + "/" + strings.Join(parts, "/")
	for _, prefix := range policy.TrustedRepositoryPrefixes {
		if strings.HasSuffix(prefix, "/") && strings.HasPrefix(normalized, prefix) {
			return true
		}
	}
	return false
}

func pinnedImage(value any) bool {
	image, ok := value.(map[string]any)
	if !ok || len(image) != 2 {
		return false
	}
	repository, repoOK := image["repository"].(string)
	digest, digestOK := image["digest"].(string)
	return repoOK && repository != "" && !strings.Contains(repository, "@") &&
		!strings.Contains(repository[strings.LastIndex(repository, "/")+1:], ":") &&
		digestOK && digestPattern.MatchString(digest)
}

func lookup(values any, path string) (any, bool) {
	for _, part := range strings.Split(path, ".") {
		object, ok := values.(map[string]any)
		if !ok {
			return nil, false
		}
		values, ok = object[part]
		if !ok {
			return nil, false
		}
	}
	return values, true
}

// Validate rejects incomplete or ambiguous v1 tables.
func (t Table) Validate() error {
	if t.ContractVersion != "v1" {
		return errors.New("table: contractVersion must be v1")
	}
	if !validPath(t.OCIEnablePath) || len(t.Entries) == 0 {
		return errors.New("table: enable path and entries are required")
	}
	seen, images := map[string]bool{}, map[string]bool{}
	for _, entry := range t.Entries {
		if !validPath(entry.Path) || seen[entry.Path] {
			return errors.New("table: invalid or duplicate path")
		}
		seen[entry.Path] = true
		switch entry.Kind {
		case "image":
			images[entry.Component] = true
		case "repository":
		default:
			return errors.New("table: unknown kind")
		}
	}
	for _, component := range []string{"controller", "worker", "compiler"} {
		if !images[component] {
			return errors.New("table: missing component image")
		}
	}
	return nil
}

func validPath(path string) bool {
	for _, part := range strings.Split(path, ".") {
		if part == "" || strings.TrimSpace(part) != part {
			return false
		}
	}
	return true
}
