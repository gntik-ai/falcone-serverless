# falcone-serverless starter package

Updated 27 October 2026. Option A is selected: Wasmtime/WASI inside restricted pods. [ADR 0010](adr/0010-oci-execution-mode-and-base-catalog.md) adds a superadmin-authorized OCI execution mode and a global base catalog. This is a revised implementation specification, not a runtime implementation.

## Contents

- `falcone-serverless-design.md`: complete six-section report, including 100 requirements and 58 stories under 16 epics.
- `contracts/falcone-contract-inventory.md`: frozen Falcone contract inventory v1.
- `backlog.md`: standalone human-readable backlog.
- `backlog.json`: issue payloads, parent IDs, dependencies, labels and requirement traceability.
- `import_github.py`: Python 3 importer using your authenticated GitHub CLI.
- `requirements-and-stories.json`: structured authoring source.
- `build_package.py` and report fragments: regenerate the report and backlog after edits.

## Review and import

This package is published in `gntik-ai/falcone-serverless/docs`. See the repository issues for the imported backlog.

1. Read the report and backlog. Decide the destination repository; the importer does not create repositories or Projects.
2. Install Python 3 and a current GitHub CLI, authenticate for the chosen repository, and ensure issue/label and optional Project write access.
3. Preview locally:

```bash
python3 import_github.py --repo gntik-ai/falcone-serverless
```

4. To create or reuse the 74 issues (stable markers prevent duplicate creation):

```bash
python3 import_github.py --repo gntik-ai/falcone-serverless --apply
```

5. To also add all issues to an existing Project, provide its real owner and number:

```bash
python3 import_github.py --repo gntik-ai/falcone-serverless --apply --project-owner gntik-ai --project-number YOUR_PROJECT_NUMBER
```

Replace YOUR_PROJECT_NUMBER with an integer. This example does not claim that a particular Project exists.

The importer uses native GitHub sub-issues for epic/story hierarchy. It creates area/type/phase/effort labels, not native issue types or custom Project fields. Set up a board view grouped by parent epic and your preferred Status field. Dependency IDs are in issue bodies and JSON; they are not imported as native blocked-by relations.

The default preview makes no network calls. Apply mode lists existing open and closed issues, deduplicates by stable body markers and refuses ambiguous matches or exact unmarked title collisions. It preserves existing issue text and labels and never moves an issue from a different parent. Existing titles/body content are not synchronized on rerun.

On an API error or rate limit, the importer stops. It does not automatically retry uncertain writes. Inspect remote state, then rerun; markers allow reuse of already-created issues. Do not run two importers concurrently. Project authorization errors can leave a partial import; no rollback/delete is performed.

All issues are created in the supplied repository. If implementation is split among falcone, falcone-serverless and falcone-charts, assign ownership before importing or use reviewed issue transfers later.

## Validation performed

The generated backlog is checked for unique IDs, complete 100-requirement coverage and an acyclic dependency graph. Python compilation, local preview and a mocked import/rerun (56 issues, 42 parent links, 56 Project items, before the ADR 0010 revision) are checked. Live authenticated GitHub import is not exercised.

## Rebuild

```bash
python3 build_package.py
```

The report distinguishes repository evidence, architectural recommendations and provisional benchmark targets. Verify the actual deployed contract and current licenses before implementation.

## Option A revision

Raw tenant OCI execution, mandatory CRDs and administrator-installed sandbox runtimes have been removed from the selected design. WAS and JOB epics add feasibility, capability security and checkpoint/resume qualification. Existing issue IDs remain stable. The importer preserves previously imported bodies; apply revised scope to those issues through a separately reviewed update. No live cluster or security qualification has been performed.

## Repository publication

The GitHub connector creates issues and links epics/stories in their Markdown bodies. Native sub-issue relationships and Project membership are separate operations; run the importer with authenticated GitHub CLI to establish native parent links. Run these commands from this `docs/` directory. No Project board is created automatically.
