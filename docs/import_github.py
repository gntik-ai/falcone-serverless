#!/usr/bin/env python3
"""Review by default; --apply creates/reuses GitHub issues, links parents and optionally adds Project items."""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

def gh(*args, stdin=None):
    result = subprocess.run(["gh",*args], input=stdin, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "GitHub CLI failed; verify remote state before retrying.")
    return result.stdout.strip()

def api(path, method="GET", payload=None, pages=False):
    args=["api",path,"--method",method,"-H","Accept: application/vnd.github+json"]
    if pages: args += ["--paginate","--slurp"]
    if payload is not None: args += ["--input","-"]
    result=gh(*args,stdin=json.dumps(payload) if payload is not None else None)
    data=json.loads(result) if result else None
    return [row for page in data for row in page] if pages else data

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--repo",required=True,help="Existing owner/repository; all issues are created here")
    p.add_argument("--backlog",type=Path,default=Path(__file__).with_name("backlog.json"))
    p.add_argument("--apply",action="store_true",help="Perform GitHub writes after reviewing the default preview")
    p.add_argument("--project-owner")
    p.add_argument("--project-number",type=int)
    args=p.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+",args.repo):
        p.error("--repo must be owner/repository")
    if bool(args.project_owner) != bool(args.project_number):
        p.error("--project-owner and --project-number must be supplied together")
    data=json.loads(args.backlog.read_text())
    if data.get("schema_version") != 1: p.error("Unsupported backlog schema")
    items=data["issues"]
    ids=[i["id"] for i in items]
    if len(set(ids)) != len(ids): p.error("Duplicate local IDs")
    for i in items:
        if i.get("parent") and i["parent"] not in ids: p.error("Missing parent")
    if not args.apply:
        print(f"PREVIEW ONLY: {len(items)} issues for {args.repo}; no network calls or writes.")
        for i in items:
            print(f"{i['type']:5} {i['title']} | parent={i.get('parent') or '-'} | {','.join(i['labels'])}")
        print("Use --apply only after reviewing backlog.md and the target repository.")
        return

    gh("auth","status")
    repo=f"repos/{args.repo}"
    # Reads happen before mutations. Direct listing avoids search-index delay.
    api(repo)
    existing=[i for i in api(f"{repo}/issues?state=all&per_page=100",pages=True) if "pull_request" not in i]
    existing_labels={i["name"] for i in api(f"{repo}/labels?per_page=100",pages=True)}
    if args.project_number:
        gh("project","view",str(args.project_number),"--owner",args.project_owner,"--format","json")
    resolved={}
    for i in items:
        marker=f"<!-- falcone-serverless:{i['id']} -->"
        matches=[e for e in existing if marker in (e.get("body") or "")]
        if len(matches)>1:
            raise RuntimeError(f"Multiple existing issues have marker {i['id']}; reconcile manually.")
        if matches:
            resolved[i["id"]]=matches[0]
        elif any(e["title"]==i["title"] for e in existing):
            raise RuntimeError(f"Existing title without marker: {i['title']}; reconcile before importing.")

    for label in sorted({x for i in items for x in i["labels"]}-existing_labels):
        api(f"{repo}/labels","POST",{"name":label,"color":"276DA8","description":"falcone-serverless planning label"})
    # Epics first, then stories. Existing issue text/labels are never overwritten.
    for i in sorted(items,key=lambda i: 0 if i["type"]=="epic" else 1):
        if i["id"] not in resolved:
            resolved[i["id"]]=api(f"{repo}/issues","POST",{"title":i["title"],"body":i["body"],"labels":i["labels"]})
        print(f"{i['id']}: {resolved[i['id']]['html_url']}",flush=True)
    for epic in [i for i in items if i["type"]=="epic"]:
        parent=resolved[epic["id"]]
        children=api(f"{repo}/issues/{parent['number']}/sub_issues?per_page=100",pages=True)
        linked={c["id"] for c in children}
        for child in [i for i in items if i.get("parent")==epic["id"]]:
            record=resolved[child["id"]]
            if record["id"] not in linked:
                # Deliberately omit replace_parent: never move an issue from another parent.
                api(f"{repo}/issues/{parent['number']}/sub_issues","POST",{"sub_issue_id":record["id"]})
    if args.project_number:
        for i in items:
            gh("project","item-add",str(args.project_number),"--owner",args.project_owner,
               "--url",resolved[i["id"]]["html_url"],"--format","json")
    print("Import complete. Native parents linked; dependencies remain in issue bodies.")

if __name__=="__main__":
    try: main()
    except (RuntimeError, ValueError, KeyError, OSError) as exc:
        print(f"Stopped: {exc}\nPartial writes may exist. Inspect GitHub; rerun uses stable markers and preserves existing issue text.",file=sys.stderr)
        sys.exit(1)
