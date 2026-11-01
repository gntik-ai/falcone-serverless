package main

import (
	"encoding/json"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
)

// Static gates for the trusted controller image inputs. They read the build files
// and cannot authenticate registry contents or run a build. Each check returns one
// rule id per violation, independent of the other checks, so a single edit to a
// valid input reports exactly its own rule.

const (
	builderRef = "docker.io/library/golang:1.26.10-trixie@sha256:a050dcceee6eb9bf110151662dfac7662f3bec40927c7dee48731e3d92083d42"
	runtimeRef = "gcr.io/distroless/static-debian13:nonroot@sha256:e2e927ec666bae08560abb3c55d0659eceabb657f56b6782ab500a9fc7f555e3"
)

var refFormat = regexp.MustCompile(`^[a-z0-9./_-]+:[A-Za-z0-9._-]+@sha256:[0-9a-f]{64}$`)

func readFile(t *testing.T, name string) string {
	t.Helper()
	data, err := os.ReadFile(name)
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

type instruction struct {
	op   string
	args []string
	raw  string
}

// instructions joins backslash continuations and drops blanks and comments.
func instructions(containerfile string) []instruction {
	var out []instruction
	var pending string
	for _, line := range strings.Split(containerfile, "\n") {
		line = strings.TrimSpace(line)
		if pending == "" && (line == "" || strings.HasPrefix(line, "#")) {
			continue
		}
		if strings.HasSuffix(line, "\\") {
			pending += strings.TrimSuffix(line, "\\") + " "
			continue
		}
		line = pending + line
		pending = ""
		words := strings.Fields(line)
		if len(words) == 0 {
			continue
		}
		out = append(out, instruction{strings.ToUpper(words[0]), words[1:], strings.TrimSpace(strings.TrimPrefix(line, words[0]))})
	}
	return out
}

// externalImages returns the FROM references that are not earlier stage aliases.
func externalImages(ins []instruction) []string {
	var aliases, images []string
	for _, in := range ins {
		if in.op != "FROM" || len(in.args) == 0 {
			continue
		}
		if !slices.Contains(aliases, strings.ToLower(in.args[0])) {
			images = append(images, in.args[0])
		}
		if len(in.args) == 3 && strings.EqualFold(in.args[1], "AS") {
			aliases = append(aliases, strings.ToLower(in.args[2]))
		}
	}
	return images
}

// globRegexp translates a BuildKit/COPY glob; "*" and "?" stay within one path element.
func globRegexp(pattern string) *regexp.Regexp {
	var b strings.Builder
	b.WriteString("^")
	for i := 0; i < len(pattern); i++ {
		switch c := pattern[i]; {
		case c == '*' && i+1 < len(pattern) && pattern[i+1] == '*':
			b.WriteString(".*")
			i++
		case c == '*':
			b.WriteString("[^/]*")
		case c == '?':
			b.WriteString("[^/]")
		case c == '[':
			end := strings.IndexByte(pattern[i:], ']')
			if end < 0 {
				b.WriteString(`\[`)
				break
			}
			b.WriteString(pattern[i : i+end+1])
			i += end
		default:
			b.WriteString(regexp.QuoteMeta(string(c)))
		}
	}
	b.WriteString("$")
	return regexp.MustCompile(b.String())
}

// matchesTree reports whether the pattern names p or any parent directory of p.
func matchesTree(pattern, p string) bool {
	re := globRegexp(strings.TrimSuffix(strings.TrimPrefix(path.Clean(pattern), "/"), "/"))
	for prefix := p; prefix != "." && prefix != "/"; prefix = path.Dir(prefix) {
		if re.MatchString(prefix) {
			return true
		}
	}
	return false
}

// contextSources lists the COPY sources read from the build context.
func contextSources(ins []instruction) []string {
	var sources []string
	for _, in := range ins {
		if in.op != "COPY" {
			continue
		}
		var words []string
		fromStage := false
		for _, arg := range in.args {
			if strings.HasPrefix(arg, "--from=") {
				fromStage = true
			} else if !strings.HasPrefix(arg, "--") {
				words = append(words, arg)
			}
		}
		if !fromStage && len(words) > 1 {
			sources = append(sources, words[:len(words)-1]...)
		}
	}
	return sources
}

func goDirective(goMod string) string {
	for _, line := range strings.Split(goMod, "\n") {
		if words := strings.Fields(line); len(words) == 2 && words[0] == "go" {
			return majorMinor(words[1])
		}
	}
	return ""
}

func majorMinor(version string) string {
	parts := strings.SplitN(version, ".", 3)
	if len(parts) < 2 {
		return version
	}
	return parts[0] + "." + strings.TrimFunc(parts[1], func(r rune) bool { return r < '0' || r > '9' })
}

// checkContainerfile applies the controller image rules. repoFiles are repository
// relative slash paths of existing files, used to detect copied test sources.
func checkContainerfile(containerfile, goMod string, repoFiles []string) []string {
	var violations []string
	ins := instructions(containerfile)
	images := externalImages(ins)
	if len(images) != 2 {
		violations = append(violations, "from-count")
	}
	for i, want := range []string{builderRef, runtimeRef} {
		if len(images) != 2 {
			break
		}
		if !refFormat.MatchString(images[i]) {
			violations = append(violations, "digest")
		} else if images[i] != want {
			violations = append(violations, "base-ref")
		}
	}
	if len(images) > 0 {
		tag := strings.TrimPrefix(strings.SplitN(images[0], "@", 2)[0], "docker.io/library/golang:")
		if majorMinor(tag) != goDirective(goMod) {
			violations = append(violations, "go-version")
		}
	}

	lastFrom := -1
	var aliases []string
	for i, in := range ins {
		if in.op == "FROM" {
			lastFrom = i
		}
	}
	finalUser, entrypointOK, binary := "", false, ""
	sawEntrypoint := false
	for i, in := range ins {
		switch in.op {
		case "FROM":
			if len(in.args) == 3 && strings.EqualFold(in.args[1], "AS") {
				aliases = append(aliases, strings.ToLower(in.args[2]))
			}
		case "ADD":
			violations = append(violations, "add")
		case "RUN":
			if i > lastFrom {
				violations = append(violations, "runtime-run")
			}
		case "COPY":
			for _, arg := range in.args {
				if src, ok := strings.CutPrefix(arg, "--from="); ok && !slices.Contains(aliases, strings.ToLower(src)) {
					violations = append(violations, "copy-from")
				}
				if strings.HasPrefix(arg, "--from=") && i > lastFrom {
					binary = in.args[len(in.args)-1]
				}
			}
		case "USER":
			if i > lastFrom {
				finalUser = strings.Join(in.args, " ")
			}
		case "ENTRYPOINT":
			sawEntrypoint = true
			var exec []string
			if json.Unmarshal([]byte(in.raw), &exec) != nil {
				violations = append(violations, "entrypoint-exec")
			} else {
				entrypointOK = len(exec) == 1 && exec[0] == binary && binary != ""
			}
		}
	}
	if finalUser != "65532:65532" {
		violations = append(violations, "user")
	}
	if sawEntrypoint && !slices.Contains(violations, "entrypoint-exec") && !entrypointOK {
		violations = append(violations, "entrypoint-binary")
	}
	if !sawEntrypoint {
		violations = append(violations, "entrypoint-exec")
	}

	build := ""
	for _, in := range ins {
		if in.op == "RUN" && strings.Contains(in.raw, "go build") {
			build = in.raw
		}
	}
	if !slices.Contains(strings.Fields(build), "CGO_ENABLED=0") {
		violations = append(violations, "cgo")
	}
	if !slices.Contains(strings.Fields(build), "-trimpath") {
		violations = append(violations, "trimpath")
	}
	if !strings.Contains(build, "-ldflags '-s -w'") {
		violations = append(violations, "ldflags")
	}

	for _, source := range contextSources(ins) {
		for _, file := range repoFiles {
			if strings.HasSuffix(file, "_test.go") && matchesTree(source, file) {
				violations = append(violations, "test-copy")
				break
			}
		}
	}
	return violations
}

// checkDockerignore requires the three directory entries and that no COPY source is
// excluded; later entries win and "!" entries re-include, as in BuildKit.
func checkDockerignore(dockerignore string, sources []string) []string {
	var violations []string
	entries := map[string]bool{}
	var patterns []string
	for _, line := range strings.Split(dockerignore, "\n") {
		line = strings.TrimSpace(line)
		if line != "" && !strings.HasPrefix(line, "#") {
			entries[line] = true
			patterns = append(patterns, line)
		}
	}
	for _, want := range []string{"docs/", "target/", ".git"} {
		if !entries[want] {
			violations = append(violations, "dockerignore-entry")
		}
	}
	for _, source := range sources {
		excluded := false
		for _, pattern := range patterns {
			if negated, ok := strings.CutPrefix(pattern, "!"); ok {
				if matchesTree(negated, path.Clean(source)) {
					excluded = false
				}
			} else if matchesTree(pattern, path.Clean(source)) {
				excluded = true
			}
		}
		if excluded {
			violations = append(violations, "excluded-copy-source")
		}
	}
	return violations
}

func checkDocs(doc string) []string {
	var violations []string
	for _, want := range []struct{ rule, token string }{
		{"doc-ref", builderRef},
		{"doc-ref", runtimeRef},
		{"doc-build-command", "docker build -f controller/Containerfile ."},
		{"doc-build-command", "CGO_ENABLED=0 go build -trimpath -ldflags '-s -w'"},
		{"doc-user", "65532:65532"},
		{"doc-dockerignore", ".dockerignore"},
		{"doc-behaviour", "ignores arguments"},
		{"doc-behaviour", "falcone-serverless-controller 0.1.0"},
		{"doc-age-exception", "2026-10-28"},
		{"doc-age-exception", "1.26.9"},
		{"doc-age-exception", "2026-09-13"},
		{"doc-age-exception", "nonroot-02a141105c8d1b1987df7993c1a635ec72ac9c65"},
	} {
		if !strings.Contains(doc, want.token) {
			violations = append(violations, want.rule)
		}
	}
	return violations
}

func repoFiles(t *testing.T) []string {
	t.Helper()
	var files []string
	err := filepath.WalkDir("..", func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() && (d.Name() == ".git" || d.Name() == "target") {
			return filepath.SkipDir
		}
		if !d.IsDir() {
			files = append(files, filepath.ToSlash(strings.TrimPrefix(p, "../")))
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func mutate(t *testing.T, text, before, after string) string {
	t.Helper()
	if !strings.Contains(text, before) {
		t.Fatalf("mutation must change the input: %q", before)
	}
	return strings.Replace(text, before, after, 1)
}

func TestCommittedControllerInputsSatisfyEveryRule(t *testing.T) {
	files := repoFiles(t)
	containerfile := readFile(t, "Containerfile")
	if got := checkContainerfile(containerfile, readFile(t, "go.mod"), files); len(got) != 0 {
		t.Fatalf("Containerfile violations: %v", got)
	}
	sources := append(contextSources(instructions(containerfile)), contextSources(instructions(readFile(t, "../worker/Containerfile")))...)
	if len(sources) == 0 {
		t.Fatal("expected COPY sources from the build context")
	}
	if got := checkDockerignore(readFile(t, "../.dockerignore"), sources); len(got) != 0 {
		t.Fatalf(".dockerignore violations: %v", got)
	}
	if got := checkDocs(readFile(t, "../docs/supply-chain/trusted-images.md")); len(got) != 0 {
		t.Fatalf("trusted-images.md violations: %v", got)
	}
	for _, name := range []string{"Containerfile.dockerignore", "../worker/Containerfile.dockerignore"} {
		if _, err := os.Stat(name); err == nil {
			t.Fatalf("%s would take precedence over the root .dockerignore", name)
		}
	}
}

func TestContainerfileSingleEditsReportExactlyTheirRule(t *testing.T) {
	files := repoFiles(t)
	containerfile := readFile(t, "Containerfile")
	goMod := readFile(t, "go.mod")
	builderDigest := builderRef[strings.Index(builderRef, "@"):]
	runtimeDigest := runtimeRef[strings.Index(runtimeRef, "@")+len("@sha256:"):]
	for _, c := range []struct {
		name, before, after, rule string
	}{
		{"digest dropped", builderDigest, "", "digest"},
		{"digest 63 chars", runtimeDigest, runtimeDigest[1:], "digest"},
		{"digest uppercase", runtimeDigest, strings.ToUpper(runtimeDigest), "digest"},
		{"different digest", runtimeDigest[1:], strings.Repeat("a", 63), "base-ref"},
		{"static swapped for cc", "static-debian13", "cc-debian13", "base-ref"},
		{"extra external stage", "\nFROM gcr.io/distroless/static", "\nFROM docker.io/library/alpine:3@sha256:" + strings.Repeat("a", 64) + "\nFROM gcr.io/distroless/static", "from-count"},
		{"user root", "USER 65532:65532", "USER 0", "user"},
		{"user by name", "USER 65532:65532", "USER nonroot", "user"},
		{"shell-form entrypoint", `["/usr/local/bin/falcone-serverless-controller"]`, "/usr/local/bin/falcone-serverless-controller", "entrypoint-exec"},
		{"entrypoint is not the binary", `ENTRYPOINT ["/usr/local/bin/falcone-serverless-controller"]`, `ENTRYPOINT ["/bin/sh"]`, "entrypoint-binary"},
		{"remote ADD", "USER 65532:65532", "ADD https://example.invalid/x /x\nUSER 65532:65532", "add"},
		{"copy from external image", "COPY --from=build", "COPY --from=docker.io/library/alpine:3", "copy-from"},
		{"RUN in runtime stage", "USER 65532:65532", "RUN true\nUSER 65532:65532", "runtime-run"},
		{"cgo enabled", "CGO_ENABLED=0 ", "", "cgo"},
		{"no trimpath", " -trimpath", "", "trimpath"},
		{"no strip flags", " -ldflags '-s -w'", "", "ldflags"},
		{"glob matches tests", "COPY controller/go.mod controller/main.go ./", "COPY controller/go.mod controller/*.go ./", "test-copy"},
		{"test file copied", "controller/main.go ./", "controller/main.go controller/main_test.go ./", "test-copy"},
		{"directory copied", "controller/go.mod controller/main.go ./", "controller ./", "test-copy"},
	} {
		t.Run(c.name, func(t *testing.T) {
			got := checkContainerfile(mutate(t, containerfile, c.before, c.after), goMod, files)
			if !slices.Equal(got, []string{c.rule}) {
				t.Fatalf("violations = %v, want [%s]", got, c.rule)
			}
		})
	}
}

func TestBuilderTagMustMatchGoDirective(t *testing.T) {
	files := repoFiles(t)
	containerfile := readFile(t, "Containerfile")
	for _, goMod := range []string{"module m\n\ngo 1.25\n", "module m\n\ngo 1.27.1\n", "module m\n"} {
		if got := checkContainerfile(containerfile, goMod, files); !slices.Equal(got, []string{"go-version"}) {
			t.Fatalf("%q: violations = %v, want [go-version]", goMod, got)
		}
	}
	if got := checkContainerfile(containerfile, "module m\n\ngo 1.26.0\n", files); len(got) != 0 {
		t.Fatalf("patch-level go directive: %v", got)
	}
	older := mutate(t, containerfile, "golang:1.26.10-trixie", "golang:1.25.0-trixie")
	if got := checkContainerfile(older, readFile(t, "go.mod"), files); !slices.Equal(got, []string{"base-ref", "go-version"}) {
		t.Fatalf("1.25 tag: violations = %v, want [base-ref go-version]", got)
	}
}

func TestDockerignoreSingleEditsReportExactlyTheirRule(t *testing.T) {
	dockerignore := readFile(t, "../.dockerignore")
	sources := append(
		contextSources(instructions(readFile(t, "Containerfile"))),
		contextSources(instructions(readFile(t, "../worker/Containerfile")))...)
	for _, c := range []struct {
		name, before, after, rule string
	}{
		{"docs removed", "docs/\n", "", "dockerignore-entry"},
		{"target removed", "target/\n", "", "dockerignore-entry"},
		{".git removed", ".git\n", "", "dockerignore-entry"},
		{"worker source excluded", ".git\n", ".git\nworker/src\n", "excluded-copy-source"},
		{"controller source excluded", ".git\n", ".git\ncontroller/main.go\n", "excluded-copy-source"},
		{"glob excludes sources", ".git\n", ".git\n**/*.toml\n", "excluded-copy-source"},
		{"parent directory excluded", ".git\n", ".git\ncompiler\n", "excluded-copy-source"},
	} {
		t.Run(c.name, func(t *testing.T) {
			got := checkDockerignore(mutate(t, dockerignore, c.before, c.after), sources)
			if len(got) == 0 || got[0] != c.rule {
				t.Fatalf("violations = %v, want first %s", got, c.rule)
			}
			for _, rule := range got {
				if rule != c.rule {
					t.Fatalf("violations = %v, want only %s", got, c.rule)
				}
			}
		})
	}
	if got := checkDockerignore(dockerignore+"worker/src\n!worker/src\n", sources); len(got) != 0 {
		t.Fatalf("negated entry re-includes the source: %v", got)
	}
}

func TestDocsSingleRemovalsReportTheirRule(t *testing.T) {
	doc := readFile(t, "../docs/supply-chain/trusted-images.md")
	for _, c := range []struct{ token, rule string }{
		{builderRef, "doc-ref"},
		{runtimeRef, "doc-ref"},
		{"docker build -f controller/Containerfile .", "doc-build-command"},
		{"65532:65532", "doc-user"},
		{"ignores arguments", "doc-behaviour"},
		{"2026-10-28", "doc-age-exception"},
		{"1.26.9", "doc-age-exception"},
		{"nonroot-02a141105c8d1b1987df7993c1a635ec72ac9c65", "doc-age-exception"},
	} {
		got := checkDocs(strings.ReplaceAll(doc, c.token, "REMOVED"))
		if len(got) == 0 || got[0] != c.rule {
			t.Fatalf("removing %q: violations = %v, want %s", c.token, got, c.rule)
		}
	}
}
