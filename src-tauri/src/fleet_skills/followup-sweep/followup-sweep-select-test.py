#!/usr/bin/env python3
"""Run: python3 -I .claude/skills/followup-sweep/followup-sweep-select-test.py"""
import importlib.util
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("sel", Path(__file__).with_name("followup-sweep-select.py"))
sel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sel)
cases = 0


def check(c, m):
    global cases
    cases += 1
    if not c:
        print("FAIL:", m)
        sys.exit(1)


def fu(i, repo="o/a", path="p.py", ts="2026-01-01"):
    return {"edge_id": i, "created_at": ts, "anchors": [{"repo": repo, "path": path}]}


ACT = {"o/a": "act", "o/b": "act"}

# 20 follow-ups per PR
r = sel.select([fu(f"e{i:02d}", path=f"f{i}.py", ts=f"2026-01-{i + 1:02d}") for i in range(25)], ACT, {}, {})
check(len(r["repos"]["o/a"]["batch"]) == 20, "20-follow-up bound")
check(len(r["repos"]["o/a"]["skipped"]) == 5, "overflow stays open")

# oldest first
r = sel.select([fu("new", ts="2026-03-01"), fu("old", path="q.py", ts="2026-01-01")], ACT, {}, {})
check(r["repos"]["o/a"]["batch"] == ["old", "new"], "oldest first")

# multi-repo anchored follow-up is skipped
m = {"edge_id": "m", "anchors": [{"repo": "o/a", "path": "x"}, {"repo": "o/b", "path": "y"}]}
r = sel.select([m, fu("ok")], ACT, {}, {})
check(r["multi_repo"] == ["m"] and r["repos"]["o/a"]["batch"] == ["ok"], "multi-repo skip")

# collision with an open PR or a claim
r = sel.select([fu("a", path="hot.py"), fu("b", path="mine.py"), fu("c", path="free.py")], ACT,
               {"o/a": ["hot.py"]}, {"o/a": ["mine.py"]})
check(r["repos"]["o/a"]["batch"] == ["c"] and set(r["repos"]["o/a"]["skipped"]) == {"a", "b"}, "collision skip")

# dial off and dial unreadable skip the repo
r = sel.select([fu("a"), fu("b", repo="o/b"), fu("c", repo="o/c")], {"o/a": "off", "o/b": None, "o/c": "act"}, {}, {})
check(r["repo_skips"] == {"o/a": "dial off", "o/b": "dial unreadable"} and list(r["repos"]) == ["o/c"], "dial skips")
check(sel.select([fu("z", repo="o/zz")], {}, {}, {})["repo_skips"] == {"o/zz": "dial unreadable"}, "absent level is unreadable")

# one PR per repo, and the --repo filter
r = sel.select([fu("a"), fu("b", repo="o/b")], ACT, {}, {})
check(set(r["repos"]) == {"o/a", "o/b"}, "one entry per repo")
check(set(sel.select([fu("a"), fu("b", repo="o/b")], ACT, {}, {}, only_repo="o/b")["repos"]) == {"o/b"}, "repo filter")

# 400-line bound
kept, dropped = sel.bound_lines([("a", 300), ("b", 100), ("c", 1)])
check(kept == ["a", "b"] and dropped == ["c"], "400-line bound, exact fit kept")
kept, dropped = sel.bound_lines([("a", 300), ("b", 200), ("c", 1)])
check(kept == ["a"] and dropped == ["b", "c"], "no skipping past an overflow")

# directory and glob claims collide; missing timestamps sort last; --repo filter scopes multi_repo
r = sel.select([fu("d", path="src/x/y.py"), fu("g", path="docs/a.md"), fu("k", path="ok.py")], ACT,
               {"o/a": ["src/x/"]}, {"o/a": ["docs/*.md"]})
check(r["repos"]["o/a"]["batch"] == ["k"], "dir/glob collision")
r = sel.select([{"edge_id": "nots", "anchors": [{"repo": "o/a", "path": "n.py"}]}, fu("dated", ts="2030-01-01")], ACT, {}, {})
check(r["repos"]["o/a"]["batch"] == ["dated", "nots"], "no-timestamp last")
m2 = {"edge_id": "m2", "anchors": [{"repo": "o/b", "path": "x"}, {"repo": "o/c", "path": "y"}]}
check(sel.select([m2], ACT, {}, {}, only_repo="o/a")["multi_repo"] == [], "multi_repo respects --repo")

print(f"ok: {cases} cases")
