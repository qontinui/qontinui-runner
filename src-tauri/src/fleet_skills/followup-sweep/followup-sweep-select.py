#!/usr/bin/env python3
"""Pure selection and bounding logic for the follow-up sweep (SKILL.md steps 1, 2, 4).

No network, no git: the caller reads the follow-ups, the dial levels, the open-PR
paths and the claimed paths, and this decides what each repo's one PR may carry.

    followup-sweep-select.py < input.json     # prints the plan as JSON

With ``--bound-lines`` it instead reads {"hunks": [[edge_id, changed_lines], ...]}
(ordered, verified) and prints the kept / dropped edge ids under the 400-line bound.

input: {"followups": [...open, anchored rows...], "levels": {repo: "off"|"act"|null},
        "open_pr_paths": {repo: [path]}, "claimed_paths": {repo: [path]},
        "repo": optional "owner/name" filter}
A level of null (the dial could not be read) skips the repo, like "off".
"""
import fnmatch
import json
import sys

MAX_FOLLOWUPS = 20
MAX_LINES = 400


def _busy(path, busy):
    """A claim or open-PR entry may be a file, a directory (trailing ``/``) or a glob."""
    return any(path == b or fnmatch.fnmatch(path, b) or (b.endswith("/") and path.startswith(b))
               for b in busy)


def select(followups, levels, open_pr_paths, claimed_paths, only_repo=None,
           max_followups=MAX_FOLLOWUPS):
    """Return ``{"repos": {repo: {"batch": [edge_id], "skipped": {edge_id: reason}}},
    "repo_skips": {repo: reason}, "multi_repo": [edge_id]}``. One entry per repo = one PR."""
    out = {"repos": {}, "repo_skips": {}, "multi_repo": []}
    by_repo = {}
    for f in followups:
        repos = {a["repo"] for a in f.get("anchors", [])}
        if not repos:
            continue
        if len(repos) > 1:
            if not only_repo or only_repo in repos:
                out["multi_repo"].append(f["edge_id"])  # not batchable
            continue
        repo = next(iter(repos))
        if only_repo and repo != only_repo:
            continue
        by_repo.setdefault(repo, []).append(f)
    for repo in sorted(by_repo):
        level = levels.get(repo)
        if level != "act":
            out["repo_skips"][repo] = "dial unreadable" if level is None else f"dial {level}"
            continue
        busy = sorted(set(open_pr_paths.get(repo, [])) | set(claimed_paths.get(repo, [])))
        batch, skipped = [], {}
        # oldest first; rows without a timestamp go last, in input order (sort is stable)
        for f in sorted(by_repo[repo], key=lambda r: (not r.get("created_at"), r.get("created_at") or "")):
            hit = sorted({a["path"] for a in f["anchors"] if _busy(a["path"], busy)})
            if hit:
                skipped[f["edge_id"]] = "collision: " + ", ".join(hit)
            elif len(batch) >= max_followups:
                skipped[f["edge_id"]] = "over the per-PR follow-up bound"
            else:
                batch.append(f["edge_id"])
        out["repos"][repo] = {"batch": batch, "skipped": skipped}
    return out


def bound_lines(hunks, max_lines=MAX_LINES):
    """``hunks``: ordered ``[(edge_id, changed_lines)]`` of verified fixes. Keep the
    longest prefix whose total stays within ``max_lines``; the rest stay open.
    Returns ``(kept_edge_ids, dropped_edge_ids)``."""
    kept, dropped, total = [], [], 0
    for edge_id, n in hunks:
        if not dropped and total + n <= max_lines:
            kept.append(edge_id)
            total += n
        else:
            dropped.append(edge_id)
    return kept, dropped


if __name__ == "__main__":
    d = json.load(sys.stdin)
    if sys.argv[1:] == ["--bound-lines"]:  # stdin: {"hunks": [[edge_id, changed_lines], ...]}
        kept, dropped = bound_lines([tuple(h) for h in d["hunks"]])
        json.dump({"kept": kept, "dropped": dropped}, sys.stdout)
        print()
        sys.exit(0)
    json.dump(select(d["followups"], d.get("levels", {}), d.get("open_pr_paths", {}),
                     d.get("claimed_paths", {}), d.get("repo")), sys.stdout, indent=1)
    print()
