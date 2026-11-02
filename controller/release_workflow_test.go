package main

import (
	"os"
	"os/exec"
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

func TestReleaseWorkflowSerializesEveryPublishingRun(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	// One fixed group: a v1.2.3 tag push, the Release for it and a manual 1.2.3 run (or two
	// versions of one commit, sharing its sha tag) can never race on an immutable tag.
	if !strings.Contains(text, "concurrency:\n") || !strings.Contains(text, "  group: release-images\n  cancel-in-progress: false\n") || !strings.Contains(text, "\n  queue: max\n") {
		t.Error("every publishing run must share one concurrency group")
	}
}

func TestReleaseWorkflowKeepsSinglePlatformDigests(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	for _, line := range strings.Split(text, "\n") {
		if strings.Contains(line, "imagetools create") && !strings.Contains(line, "--prefer-index=false") {
			t.Errorf("imagetools create without --prefer-index=false changes the digest: %s", strings.TrimSpace(line))
		}
	}
	for _, want := range []string{
		"bash .github/scripts/release-image-refs.sh \"docker.io/${DOCKERHUB_NAMESPACE}/${IMAGE}\" ",
		"if: steps.refs.outputs.mode == 'retag'",
		"if: steps.refs.outputs.mode == 'build'",
		"name: Verify the pushed references",
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

const (
	digestA = "sha256:" + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	digestB = "sha256:" + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
)

// fakeImagetools answers `inspect --format ... <ref>` from per-reference states: a digest,
// "absent" (manifest not found), "error" (any other registry failure) or "malformed".
const fakeImagetools = `#!/usr/bin/env bash
ref="${@: -1}"
state="$(cat "$FAKE_DIR/$(printf '%s' "$ref" | tr '/:' '__')" 2>/dev/null || echo absent)"
case "$state" in
  absent) echo "ERROR: $ref: not found" >&2; exit 1 ;;
  error) echo "ERROR: unexpected status: 429 Too Many Requests" >&2; exit 1 ;;
  token404) echo "ERROR: failed to fetch oauth token: unexpected status from GET request to https://auth.docker.io/token: 404 Not Found" >&2; exit 1 ;;
  norepo) echo "ERROR: pull access denied, repository does not exist or may require authorization: server message: insufficient_scope: authorization failed" >&2; exit 1 ;;
  otherref) echo "ERROR: docker.io/gntik/other:1.2.3: not found" >&2; exit 1 ;;
  malformed) echo '{"digest":"sha256:XYZ"}' ;;
  *) printf '{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"%s"}\n' "$state" ;;
esac
`

func runRefs(t *testing.T, push, newRepository string, states map[string]string) (map[string]string, string, error) {
	t.Helper()
	dir := t.TempDir()
	fake := dir + "/imagetools"
	if err := os.WriteFile(fake, []byte(fakeImagetools), 0o755); err != nil {
		t.Fatal(err)
	}
	for ref, state := range states {
		name := strings.NewReplacer("/", "_", ":", "_").Replace(ref)
		if err := os.WriteFile(dir+"/"+name, []byte(state), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	cmd := exec.Command("bash", "../.github/scripts/release-image-refs.sh",
		"docker.io/gntik/falcone-serverless-worker", "1.2.3", "0123456789ab", push, newRepository)
	cmd.Env = append(os.Environ(), "IMAGETOOLS="+fake, "FAKE_DIR="+dir)
	var stdout, stderr strings.Builder
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	err := cmd.Run()
	out := map[string]string{}
	for _, line := range strings.Split(strings.TrimSpace(stdout.String()), "\n") {
		if key, value, ok := strings.Cut(line, "="); ok {
			out[key] = value
		}
	}
	return out, stderr.String(), err
}

func TestReleaseImageRefsDecidesFromTheRegistryState(t *testing.T) {
	if _, err := exec.LookPath("jq"); err != nil {
		t.Skip("jq is not installed")
	}
	const version = "docker.io/gntik/falcone-serverless-worker:1.2.3"
	const sha = "docker.io/gntik/falcone-serverless-worker:sha-0123456789ab"
	cases := []struct {
		name     string
		push     string
		newRepo  string
		states   map[string]string
		mode     string
		existing string
		fails    string
	}{
		{name: "both absent: build", push: "true", states: nil, mode: "build"},
		{name: "dry run: no lookups", push: "false", states: map[string]string{version: "error", sha: "error"}, mode: "build"},
		{name: "same digest: keep", push: "true", states: map[string]string{version: digestA, sha: digestA}, mode: "existing", existing: digestA},
		{name: "commit already published: retag", push: "true", states: map[string]string{sha: digestA}, mode: "retag", existing: digestA},
		{name: "version from another commit", push: "true", states: map[string]string{version: digestA}, fails: "refusing to overwrite an immutable tag"},
		{name: "different digests", push: "true", states: map[string]string{version: digestA, sha: digestB}, fails: "refusing to overwrite an immutable tag"},
		{name: "registry error is not absence", push: "true", states: map[string]string{version: "error", sha: "error"}, fails: "refusing to guess that it is absent"},
		{name: "one lookup fails", push: "true", states: map[string]string{version: "error"}, fails: "refusing to guess that it is absent"},
		{name: "malformed digest", push: "true", states: map[string]string{version: "malformed", sha: digestA}, fails: "malformed digest"},
		{name: "a token endpoint 404 is not absence", push: "true", states: map[string]string{version: "token404", sha: "token404"}, fails: "refusing to guess that it is absent"},
		{name: "a missing repository is not absence", push: "true", states: map[string]string{version: "norepo", sha: "norepo"}, fails: "refusing to guess that it is absent"},
		{name: "not found for another reference is not absence", push: "true", states: map[string]string{version: "otherref", sha: "otherref"}, fails: "refusing to guess that it is absent"},
		{name: "a repository the first push creates: build without lookups", push: "true", newRepo: "true", states: map[string]string{version: "norepo", sha: "norepo"}, mode: "build"},
		{name: "an existing repository still decides from its tags", push: "true", newRepo: "false", states: map[string]string{version: digestA}, fails: "refusing to overwrite an immutable tag"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			newRepo := c.newRepo
			if newRepo == "" {
				newRepo = "false"
			}
			out, stderr, err := runRefs(t, c.push, newRepo, c.states)
			if c.fails != "" {
				if err == nil || !strings.Contains(stderr, c.fails) {
					t.Fatalf("expected a failure with %q, got err=%v stderr=%q out=%v", c.fails, err, stderr, out)
				}
				if _, ok := out["mode"]; ok {
					t.Fatalf("a failed decision must not print a mode: %v", out)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected failure: %v %s", err, stderr)
			}
			if out["mode"] != c.mode || out["existing_digest"] != c.existing {
				t.Fatalf("got mode=%q existing=%q, want %q %q", out["mode"], out["existing_digest"], c.mode, c.existing)
			}
			if out["version_ref"] != version || out["sha_ref"] != sha {
				t.Fatalf("references: %v", out)
			}
		})
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

// The first publish may create the Docker Hub repositories, but only from a manual run with
// create_repositories, and every push then requires its repository to be public.
func TestReleaseWorkflowCreatesRepositoriesOnlyWhenAsked(t *testing.T) {
	text := readFile(t, releaseWorkflow)
	jobs := workflowJobs(t, text)
	if !strings.Contains(jobs["meta"], `if [ "$EVENT" = workflow_dispatch ] && [ "$CREATE_REPOSITORIES" = true ] && [ "$push" = true ]; then create=true; fi`) {
		t.Error("only a manual publishing run may create repositories")
	}
	if !strings.Contains(jobs["preflight"], `if [ "$status" = 404 ] && [ "$CREATE" = true ]; then`) {
		t.Error("the preflight accepts a missing repository only in a create_repositories run")
	}
	if !strings.Contains(jobs["build"], `"$PUSH" "$new_repository" >> "$GITHUB_OUTPUT"`) ||
		!strings.Contains(jobs["build"], `if [ "$PUSH" = true ] && [ "$CREATE" = true ]; then`) {
		t.Error("the references step treats a repository as new only in a create_repositories run")
	}
	public, ok := strings.CutPrefix(jobs["build"][strings.Index(jobs["build"], "- name: The repository is public"):], "- name: The repository is public\n")
	if !ok || !strings.Contains(public, "if: env.PUSH == 'true'") || !strings.Contains(public, "is_private") {
		t.Error("every push checks that its repository is public")
	}
	if strings.Index(jobs["build"], "name: The repository is public") < strings.Index(jobs["build"], "name: Verify the pushed references") ||
		strings.Index(jobs["build"], "name: The repository is public") > strings.Index(jobs["build"], "name: Move :latest") {
		t.Error("the visibility check runs after the push is verified and before :latest moves")
	}
}
