package main

import (
	"regexp"
	"strings"
	"testing"
)

// Static gates for ../.github/workflows/release-images.yml (#250): the official images are
// published to docker.io/gntik only from strict vX.Y.Z versions and SHA-pinned actions,
// never overwriting an immutable tag, with write access limited to the Release asset job.

const releaseWorkflow = "../.github/workflows/release-images.yml"

// workflowJobs splits the jobs section into its top-level jobs, in file order.
func workflowJobs(t *testing.T, text string) map[string]string {
	t.Helper()
	_, body, ok := strings.Cut(text, "\njobs:\n")
	if !ok {
		t.Fatal("no jobs section")
	}
	jobs := map[string]string{}
	current := ""
	for _, line := range strings.Split(body, "\n") {
		if strings.HasPrefix(line, "  ") && !strings.HasPrefix(line, "   ") && strings.HasSuffix(strings.TrimSpace(line), ":") {
			current = strings.TrimSuffix(strings.TrimSpace(line), ":")
			jobs[current] = ""
			continue
		}
		if current != "" {
			jobs[current] += line + "\n"
		}
	}
	return jobs
}

func TestReleaseWorkflowTriggersOnlyOnStrictVersions(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	for _, want := range []string{
		"  push:\n    tags:\n      - 'v[0-9]+.[0-9]+.[0-9]+'\n",
		"  release:\n    types: [published]\n",
		"  workflow_dispatch:\n",
		`'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'`,
	} {
		if !strings.Contains(text, want) {
			t.Errorf("missing %q", want)
		}
	}
	if strings.Contains(text, "pull_request") {
		t.Error("a pull request must never publish")
	}
}

func TestReleaseWorkflowPinsEveryActionToACommit(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	uses := regexp.MustCompile(`(?m)^\s*(?:- )?uses: (\S+)`).FindAllStringSubmatch(text, -1)
	if len(uses) < 6 {
		t.Fatalf("expected the checkout, buildx, login, build, upload and download actions, got %d", len(uses))
	}
	pinned := regexp.MustCompile(`^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}$`)
	for _, match := range uses {
		if !pinned.MatchString(match[1]) {
			t.Errorf("%s is not pinned to a full commit SHA", match[1])
		}
	}
}

func TestReleaseWorkflowPublishesTheWorkerAndTheController(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	if !strings.Contains(text, "  DOCKERHUB_NAMESPACE: gntik\n") {
		t.Error("images must be published under docker.io/gntik")
	}
	for image, file := range map[string]string{
		"falcone-serverless-worker":     "worker/Containerfile",
		"falcone-serverless-controller": "controller/Containerfile",
	} {
		entry := "          - image: " + image + "\n            containerfile: " + file + "\n"
		if !strings.Contains(text, entry) {
			t.Errorf("%s missing from the build matrix", image)
		}
		readFile(t, "../"+file) // the Containerfile exists
	}
}

func TestReleaseWorkflowWritesOnlyInTheReleaseAssetJob(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	top, _, _ := strings.Cut(text, "\njobs:\n")
	if !strings.Contains(top, "permissions:\n  contents: read\n") || strings.Contains(top, ": write") {
		t.Error("the workflow-level token must be read-only")
	}
	jobs := workflowJobs(t, text)
	for _, name := range []string{"meta", "preflight", "build", "summary", "release-asset"} {
		if _, ok := jobs[name]; !ok {
			t.Fatalf("job %s missing", name)
		}
	}
	for name, job := range jobs {
		writes := strings.Count(job, ": write")
		if name == "release-asset" {
			if writes != 1 || !strings.Contains(job, "contents: write") || !strings.Contains(job, "if: github.event_name == 'release'") {
				t.Errorf("release-asset must hold the only contents: write, for Release events only")
			}
		} else if writes != 0 {
			t.Errorf("job %s must not have write permissions", name)
		}
	}
}

func TestReleaseWorkflowNeverOverwritesImmutableTags(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	for _, want := range []string{
		"refusing to overwrite an immutable tag",
		"mode=existing",
		"${{ steps.refs.outputs.version_ref }}\n            ${{ steps.refs.outputs.sha_ref }}",
		"name: Verify the pushed references",
		// :latest moves only for tag pushes, in its own guarded step.
		"if [ \"$EVENT\" = push ]; then latest=true; fi",
		"if: env.PUSH == 'true' && env.LATEST == 'true'",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("missing %q", want)
		}
	}
	for _, line := range strings.Split(text, "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.Contains(line, ":latest") && !strings.HasPrefix(trimmed, "#") &&
			!strings.Contains(line, "Move :latest") && !strings.Contains(line, "${REPO}:latest") {
			t.Errorf(":latest outside its guarded step: %s", trimmed)
		}
	}
}

func TestReleaseWorkflowDryRunPushesNothing(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	if !strings.Contains(text, "if [ \"$EVENT\" = workflow_dispatch ] && [ \"$DRY_RUN\" = true ]; then push=false; fi") {
		t.Error("a dry run must not push")
	}
	jobs := workflowJobs(t, text)
	if !strings.Contains(jobs["preflight"], "if: needs.meta.outputs.push == 'true'") || !strings.Contains(jobs["preflight"], "is_private") {
		t.Error("the preflight checks secrets and public repositories, and only for runs that push")
	}
	if !strings.Contains(jobs["build"], "push: ${{ needs.meta.outputs.push == 'true' }}") {
		t.Error("the build pushes only when the run publishes")
	}
}
