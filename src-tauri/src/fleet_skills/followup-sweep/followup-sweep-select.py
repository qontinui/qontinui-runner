#!/usr/bin/env python3
"""Pure selection and bounding logic for the follow-up sweep (SKILL.md steps 1, 2, 4).

No network, no git: the caller reads the follow-ups, the dial levels, the open-PR
paths and the claimed paths, and this decides what each repo's one PR may carry.

    followup-sweep-select.py < input.json     # prints the plan as JSON

With ``--bound-lines`` it instead reads {"hunks": [[edge_id, changed_lines], ...]}
(ordered, verified) and prints the kept / dropped edge ids under the 400-line bound.

Candidates: a ``size_class`` of ``plan_sized``, or ``small`` with
``size_class_source == "sweep"``, is skipped (reason in ``skipped``); declared
``trivial`` rows are taken first, then the rest (unclassified, or ``small``
declared by anything but a sweep), oldest first within each.

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
    return any(path == b or fnmatch.fnmatchcase(path, b) or (b.endswith("/") and path.startswith(b))
               for b in busy)


def size_exclusion(f):
    """Why a follow-up is not a sweep candidate on its size class, or None.

    ``plan_sized`` (whoever declared it) is a plan, not a sweep fix; ``small``
    set by a sweep (``size_class_source == "sweep"``) is a follow-up an earlier
    sweep already tried and reverted. Unclassified rows stay in the pool: the
    sweep classifies on attempt."""
    cls = f.get("size_class")
    if cls == "plan_sized":
        return "size_class plan_sized"
    if cls == "small" and f.get("size_class_source") == "sweep":
        return "size_class small (reclassified by a sweep)"
    return None


def _order_key(f):
    """Declared ``trivial`` first, then everything else; oldest first within each
    group; rows without a timestamp last in their group, in input order."""
    return (f.get("size_class") != "trivial", not f.get("created_at"), f.get("created_at") or "")


def select(followups, levels, open_pr_paths, claimed_paths, only_repo=None,
           max_followups=MAX_FOLLOWUPS):
    """Return ``{"repos": {repo: {"batch": [edge_id], "skipped": {edge_id: reason}}},
    "repo_skips": {repo: reason}, "multi_repo": [edge_id]}``. One entry per repo = one PR."""
    out = {"repos": {}, "repo_skips": {}, "multi_repo": []}
    by_repo = {}
    for f in followups:
        repos = {a["repo"] for a in (f.get("anchors") or [])}
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
        # trivial first, then the rest; oldest first within each (sort is stable)
        for f in sorted(by_repo[repo], key=_order_key):
            excluded = size_exclusion(f)
            hit = sorted({a["path"] for a in (f.get("anchors") or []) if _busy(a["path"], busy)})
            if excluded:
                skipped[f["edge_id"]] = excluded
            elif hit:
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
    if sys.argv[1:] not in ([], ["--bound-lines"]):
        print("usage: followup-sweep-select.py [--bound-lines] < input.json", file=sys.stderr)
        sys.exit(2)
    d = json.load(sys.stdin)
    if sys.argv[1:] == ["--bound-lines"]:  # stdin: {"hunks": [[edge_id, changed_lines], ...]}
        kept, dropped = bound_lines([tuple(h) for h in d["hunks"]])
        json.dump({"kept": kept, "dropped": dropped}, sys.stdout)
        print()
        sys.exit(0)
    json.dump(select(d["followups"], d.get("levels", {}), d.get("open_pr_paths", {}),
                     d.get("claimed_paths", {}), d.get("repo")), sys.stdout, indent=1)
    print()
