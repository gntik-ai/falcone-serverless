#!/usr/bin/env python3
"""Generate the reviewable report and GitHub issue payloads from one backlog source."""
import json
from pathlib import Path
from collections import Counter

ROOT = Path(__file__).resolve().parent
data = json.loads((ROOT / "requirements-and-stories.json").read_text())
categories = data["categories"]
requirements = []
issues = []
story_ids = {f"{c['code']}-{s[0]}" for c in categories for s in c["stories"]}
assert len(story_ids) == sum(len(c["stories"]) for c in categories)
requirement_count = sum(len(c["requirements"]) for c in categories)
req_section = ["## 2. Functional requirements checklist",
    f"All {requirement_count} statements apply to the accepted Option A design. Milestones do not establish production qualification. Unsupported existing workloads block their migration; API compatibility does not imply arbitrary Linux-runtime compatibility.",
    ""]
backlog = ["## 4. Detailed backlog — GitHub-ready",
    f"{len(categories)} epics and {len(story_ids)} implementation stories. Every story includes a title, user story, description, acceptance criteria, requirement links, dependencies, phase and effort. Option A supersedes earlier native-container assumptions; stable IDs are retained for traceability.",
    "",
    "**Sizing:** S = contained change; M = several focused changes or integrations; L = substantial work with failure/security validation. These are relative estimates, not person-day commitments. No S-sized stories are used because these initial items are integration-level slices.",
    "",
    "**Board:** Status = Backlog / Ready / In progress / Review / Validation / Done; group by parent epic; filter by area, phase and effort. M0–M3 are delivery phases, not deadlines. Parent/sub-issue hierarchy is imported natively; dependencies and requirements are included in issue bodies. The importer represents area, phase and effort as labels and does not create custom Project fields.",
    ""]

for c in categories:
    code = c["code"]
    epic_id = f"EP-{code}"
    req_section += [f"### {code}. {c['name']}", ""]
    for i, text in enumerate(c["requirements"], 1):
        rid = f"FR-{code}-{i:02}"
        statement = "The system must " + text[0].lower() + text[1:]
        requirements.append({"id": rid, "category": code, "statement": statement})
        req_section.append(f"- [ ] **{rid}:** {statement}")
    req_section.append("")
    epic_title = f"[falcone-serverless][{epic_id}] {c['name']}"
    epic_body = f"<!-- falcone-serverless:{epic_id} -->\n\n## Goal\n\n{c['goal']}\n\n## Stories\n\n"
    epic_body += "\n".join(f"- {code}-{s[0]} — {s[1]}" for s in c["stories"])
    epic_body += "\n\n## Completion\n\nAll child acceptance criteria and linked requirements pass on the agreed support matrix.\n"
    issues.append({"id":epic_id, "type":"epic", "title":epic_title, "body":epic_body,
                  "parent":None, "labels":["falcone-serverless","type:epic",f"area:{code.lower()}"]})
    backlog += [f"### {code}. {c['name']}", "", f"- **Epic {epic_id}: {c['goal']}**", ""]
    for j,s in enumerate(c["stories"]):
        num,title,role,want,benefit,description,ac,size,phase,deps = s
        sid = f"{code}-{num}"
        assert all(d in story_ids for d in deps), (sid,deps)
        refs = [f"FR-{code}-{j*2+k:02}" for k in [1,2]]
        article = "an" if role[0].lower() in "aeiou" else "a"
        user_story = f"As {article} {role}, I want to {want}, so that {benefit}."
        issue_title = f"[falcone-serverless][{sid}] {title}"
        body = f"<!-- falcone-serverless:{sid} -->\n\n## User story\n\n{user_story}\n\n## Description\n\n{description}\n\n## Acceptance criteria\n\n"
        body += "\n".join(f"- [ ] {a}" for a in ac)
        body += f"\n\n## Planning\n\n- Parent: {epic_id}\n- Phase: {phase}\n- Effort: {size}\n- Depends on: {', '.join(deps) or 'None'}\n\n## Requirements\n\n"
        body += "\n".join(f"- **{r['id']}:** {r['statement']}" for r in requirements if r["id"] in refs)
        issues.append({"id":sid,"type":"story","title":issue_title,"body":body,"parent":epic_id,
                      "requirements":refs,"dependencies":deps,"phase":phase,"effort":size,
                      "labels":["falcone-serverless","type:story",f"area:{code.lower()}",f"phase:{phase.lower()}",f"effort:{size.lower()}"]})
        backlog += [f"- **{sid} — {title}** · {size} · {phase}",
                    f"  - **Story:** {user_story}",
                    f"  - **Description:** {description}",
                    f"  - **Acceptance:** {' '.join(f'({n}) {a}' for n,a in enumerate(ac,1))}",
                    f"  - **Requirements:** {', '.join(refs)}. **Dependencies:** {', '.join(deps) or 'None'}.",
                    ""]

# Check requirement coverage and dependency cycles.
coverage = Counter(r for item in issues for r in item.get("requirements",[]))
assert len(requirements) == requirement_count and set(coverage) == {r["id"] for r in requirements}
by_id = {i["id"]:i for i in issues}
visited, visiting = set(), set()
def visit(key):
    if key in visited: return
    assert key not in visiting, f"Dependency cycle: {key}"
    visiting.add(key)
    for dep in by_id[key].get("dependencies",[]): visit(dep)
    visiting.remove(key)
    visited.add(key)
for key in by_id: visit(key)

report = "\n\n".join([
    (ROOT/"report-front.md").read_text(),
    "\n".join(req_section),
    (ROOT/"report-architecture.md").read_text(),
    "\n".join(backlog),
    (ROOT/"report-tail.md").read_text()
])
(ROOT/"falcone-serverless-design.md").write_text(report)
(ROOT/"backlog.json").write_text(json.dumps({
    "schema_version":1, "project":data["project"], "baseline_commit":data["baseline_commit"],
    "planning_revision":data["planning_revision"], "decision":data["decision"], "requirements":requirements, "issues":issues
},indent=2)+"\n")
(ROOT/"backlog.md").write_text("\n".join(backlog)+"\n")
print(json.dumps({"epics":len(categories),"stories":len(story_ids),"requirements":len(requirements),"coverage":"complete","dependency_graph":"acyclic","report_words":len(report.split())}))
