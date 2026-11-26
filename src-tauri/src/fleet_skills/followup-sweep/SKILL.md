---
name: followup-sweep
description: "Sweep the open, anchored follow-up queue into ONE bounded, non-executable PR per repo (args: --tenant <uuid> [--repo <owner/name>] [--dry-run]); never merges. Also carries the checker it gates on: prove, file by file, whether a diff is NON-EXECUTABLE -- the check the follow-up sweep gates every batched fix on. Runs exec-equivalence.py: Python through coord's own non-executable-edit proof (POST /coord/exec-equivalence/python, plus client-side doctest, shebang and tool-directive refusals), Rust / TypeScript / JavaScript / shell through pinned tree-sitter grammars with comments removed (a Rust doc edit is inert only under a positive prose whitelist; tool directives stay executable), and every other file through the repo's declared code_paths (unknown whenever Rust source may include it opaquely). Answers non_executable | executable | unknown per file -- a missing grammar, an unreadable oracle or a silent coord is unknown, never non_executable. Exit 0 every file non_executable, 4 any executable, 5 any unknown and none executable, 2 no verdict."
user-invocable: true
---

# followup-sweep

Two parts: the sweep procedure (next section) and the checker it gates on
(`exec-equivalence.py`, documented from "The non-executable proof" below).

## The sweep: `/followup-sweep --tenant <uuid> [--repo <owner/name>] [--dry-run]`

Folds open, anchored follow-ups whose whole fix is non-executable into one
bounded PR per repo. It is a skill, not a command, so it ships in the runner's
`fleet_skills/` bundle and a coord-dispatched `run_skill followup-sweep` finds
it on every device. `--dry-run` runs steps 1-2 (it skips step 0's reconcile, which
writes) and prints each repo's batch; it allocates and writes nothing. Selection and bounding are pure code:
`followup-sweep-select.py` (test: `followup-sweep-select-test.py`). Never
merge: coord's merge train lands the PR.

Credentials: the runner proxy at `http://127.0.0.1:9876/plan-library/...` needs
the session's coord-mcp proxy nonce as `Authorization: Bearer`, staged in a
file (`-H @file`), never on argv. The web routes (`GET .../followups`) take a
coord device JWT.

0. **Reconcile.** `POST http://127.0.0.1:9876/plan-library/followups/reconcile`
   as an in-session refresh; report its counts. (The scheduled reconcile is what
   closes follow-ups for every tenant; this only freshens the read.)
1. **Candidates.** For each repo, read its effective dial,
   `GET /coord/fleet-policy?domain=followup_sweep&repo=<r>`; a repo reading
   `off` is skipped, and so is one whose level cannot be read. Then read
   `GET /plan-library/followups?state=open&anchored=true`, following
   `next_cursor` until it is null and checking the count against the first page's
   `total` (a first-page read silently truncates), grouped by `anchors[].repo`; a follow-up anchored in several repos is skipped. Feed
   these to `followup-sweep-select.py`.
2. **Collisions.** For each repo gather the paths in open PRs' diffs
   (`gh pr list` + `gh pr diff --name-only`) and the paths under live file
   claims (`coord_conflict_check`) and pass them as `open_pr_paths` /
   `claimed_paths`. A follow-up touching one is skipped.
3. **Fix.** `allocate-worktree.sh --repo <repo> --intent "followup sweep"`. For
   each selected follow-up, oldest first:
   - if the issue is no longer present, record `already_fixed` with the commit
     `git log -L` names as the fix (`POST .../edges/<id>/resolution`);
   - otherwise make the fix, then run `exec-equivalence.py` on that follow-up's
     hunk alone. `non_executable` keeps it; `executable` or `unknown` reverts
     the hunk and sets `size_class` (`small` or `plan_sized`) with
     `size_class_source: sweep` through `PUT .../edges/<id>/anchors`. That PUT
     REPLACES the anchors, so read the follow-up first and send its existing
     `anchors` back with the class; an empty set would blank them.
4. **Bounds.** At most 20 follow-ups and 400 changed lines per PR (feed the verified hunks, in order, to
   `followup-sweep-select.py --bound-lines`); one PR per repo per run; the PR's files
   are a subset of the included follow-ups' anchored paths plus the derived
   artifacts step 5 regenerates. A migration revision is admissible only when its hunk is itself
   `non_executable` (the Phase 1 verdict); an executable one is reclassified.
5. **Verify, all before the PR opens.**
   - Regenerate the repo's committed derived artifacts and commit them with the
     batch (qontinui-web: `python backend/scripts/export_openapi.py`, again with
     `--base`, then `npm run generate-api-types:local`), because framework
     docstrings are published into the committed OpenAPI snapshot.
   - `exec-equivalence.py` on the whole diff (derived artifacts excepted) exits 0.
   - An independent reviewer subagent receives each note and its hunk, without
     your reasoning, and confirms each hunk addresses its note. A rejected hunk
     is reverted and its follow-up left open.
6. **Open the PR** with one `Resolves-Followup: <edge_id>` line per included
   follow-up, move each to `fixing` (`POST .../edges/<id>/resolution`), and
   read each back. `fixed` is written later by coord's land record, never by
   the sweep.
7. **Report.** Post one coord finding with the per-repo counts: examined,
   folded, already-fixed, reclassified, skipped-for-collision (and repos
   skipped for the dial).

## The non-executable proof

The checker is the sweep's verification sidecar (that plan's design decision
D7). The sweep may batch a follow-up only when its whole fix is
non-executable, and that is decided **per file by this checker, never asserted
by a model**. The checker is complete on its own.

## Run it

Against two commits (pass a merge-base when the head is a branch):

```bash
python3 <path-to-this-skill-dir>/exec-equivalence.py --repo-root . --base "$(git merge-base origin/main HEAD)" --head HEAD
```

Against the working tree, judged as each of two commits would record it, with
the worse reading kept per path. One is `git add -A`'s: staged and unstaged
edits plus untracked files that are not ignored, each read whole as git would
store it, with end-of-line conversion and working-tree-encoding applied, so a
CRLF checkout compares as a commit would store it. A file `git rm --cached`
left on disk is read that way too, but only when it is not ignored; an ignored
one is judged as the deletion alone. The other commit is the index's: what is
staged, where the working tree no longer holds it. In both modes git
attributes that change what a checkout holds are fail-closed: a changed path
whose `filter` (git-lfs, git-crypt), `text`, `eol`, `crlf`,
`working-tree-encoding` or `ident` is set on any side, or differs between
sides, reads `unknown`; a change to any `.gitattributes` makes every path
`unknown`, and so does a non-empty `$GIT_DIR/info/attributes`,
`core.attributesFile` or system `$(prefix)/etc/gitattributes` (unless
`GIT_ATTR_NOSYSTEM`; on a git before 2.42, which cannot name the system file,
every path is `unknown`), none of which a diff shows. Replace refs, `GIT_ATTR_SOURCE` and
`attr.tree` are never honoured:

```bash
python3 <path-to-this-skill-dir>/exec-equivalence.py --repo-root . --base HEAD
```

Use `python` where `python3` is the Windows Store alias. Add `--json` for one
machine-readable document, and `--code-paths '["src/**"]'` to supply the repo's
code globs instead of reading them from coord.

Each changed path prints one line, `<verdict> TAB <path> TAB <reason>`, then a
summary: `EXEC_EQUIVALENCE verdict=... exit=... files=... base=... head=...`.
Provenance -- which coord, which credential and tenant, where code_paths came
from, the grammar pack's state -- goes to stderr.

| Exit | Meaning |
|---|---|
| 0 | every changed file is `non_executable` (also a diff with no changed file: the summary says `files=0`) |
| 4 | at least one file is `executable` |
| 5 | at least one file is `unknown`, none `executable` |
| 2 | usage, git or internal error, or `--help` -- no verdict |

## What each verdict means

- `non_executable` -- proven: the change cannot alter what the repo does.
- `executable` -- NOT proven inert: coord's proof refused it, a token changed,
  a doctest, a doc-comment code block or a tool directive changed, a Rust doc
  changed outside the prose whitelist (anything but consecutive `///` / `//!`
  lines of plain prose, on a listed item, outside macros, under inert
  attributes), or a code file was added, removed or re-moded. It may still be
  harmless; it is not batched.
- `unknown` -- no proof could be attempted: coord did not answer, the grammar
  pack is missing or at another version, the repo's code_paths are
  undeclared or unreadable, some Rust source names an include it cannot
  resolve to a plain literal path (in code, or anywhere in a comment, a
  string literal or the text of a file an include names), builds doc text
  it cannot read (a non-literal `doc = ..`, a macro-built attribute, an
  include handed to a macro) (`included_by_rust_opaque:`), or names another
  compile-time file reader (`compile_time_reader:`). Those last cases
  reach EVERY file otherwise judged non_executable, source files
  included, not only non-code ones -- so a repo that includes anywhere
  through the common `concat!(env!("CARGO_MANIFEST_DIR"), ..)` pattern gets
  `unknown` for every comment edit. Treat it exactly like `executable` when
  deciding what to batch, and never report it as either.

## Requirements

- Python 3.9 or newer for the checker itself, and **3.10 or newer for the
  tree-sitter arms** (the pinned pack's wheel is `cp310-abi3`), plus `git` and `bash` (found by `lib/bash_resolve.py`,
  never Windows' WSL launcher) with `curl`, for the coord credential.
- `tree-sitter-language-pack` at the version pinned as `TREE_SITTER_PACK_PIN`
  in `exec-equivalence.py` (`python3 -m pip install tree-sitter-language-pack==0.13.0`).
  That release bundles its grammars; a later 1.x release downloads grammar
  libraries at first use, which a verifier must not do, and any other version
  reads `unknown`.
- A coord credential, staged by `lib/stage-coord-bearer.sh` through the
  tenant-proof library beside it: code_paths are read under the tenant PROVEN
  to own the repo's GitHub origin; the Python route is auth-only. Nothing is
  sent to a coord url that is neither https nor loopback.

The script header in `exec-equivalence.py` is the contract: every arm, every
refusal, the environment it reads (`EXEC_EQ_COORD_URL` is its fixture mode)
and the residuals a `non_executable` does not cover.
