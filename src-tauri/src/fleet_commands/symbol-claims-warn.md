# Symbol Claims Warning

Query coord for active `ClaimKind::Symbol` claims overlapping the current
worktree's files, and report any held by **another machine**.

Phase 4.3 of `2026-05-21-coordination-improvements`. Soft-warn surface —
helps the operator see "MSI is editing `Foo::bar` in this file right now"
**before** they start an Edit. Sibling of the PreToolUse hook (same name,
same helper) — this is the manual / on-demand version.

## Arguments

- `<file>` (optional, repeatable via `--file FOO --file BAR`) — restrict
  the scope to specific files. Paths are relative to the repo root,
  POSIX separators. Default: every file in `git diff --name-only HEAD` +
  every untracked file from `git ls-files --others --exclude-standard`.
- `--repo NAME` (optional) — override the repo name (default: basename
  of `git rev-parse --show-toplevel`). Useful when invoked from outside
  the worktree. That default is the WORKTREE's directory name in a linked
  worktree, and deliberately so: it must match how `qontinui-supervisor`'s
  symbol watcher keys the claims it writes (its `find_repo_root` stops at the
  first `.git`). Do not "correct" it to the main repo's name — that would
  match nothing.
- `--json` (optional) — emit machine-readable JSON.
- `--coord-url URL` (optional) — override `$COORD_HTTP_URL`.

`$ARGUMENTS` is passed verbatim to the Python helper, so any flag the
helper accepts works here.

## Instructions

Shell out to the canonical helper. `<workspace-root>` is the directory that
contains the repo checkouts (the parent of this repo's checkout); if
`<workspace-root>/qontinui-stack` is not checked out, report that the helper
is unavailable and stop — there is no inline fallback.

```bash
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py $ARGUMENTS
```

Display the helper's output to the operator:

- **Default (text)**: a `Symbol | File | Held by | TTL` table, or the
  literal string `No conflicting symbol claims.` when nothing's held.
- **`--json`**: a JSON object with `conflicts[]` + `errors[]`. Summarize
  count + the first few entries, offer to re-render as text on request.

If the helper exits 2, surface the stderr (e.g. `not inside a git
worktree`) and stop — don't work around it.

## Behavior notes

- **Soft warn — never blocks work.** This skill is read-only; the
  operator decides whether to wait, coordinate (Slack / dashboard /
  voice), or steal (revoke via `/coord/claims/release` — that's an
  explicit op, not part of this skill).
- **Self-suppression**: if every overlapping claim is held by the local
  machine (the `device_id` / `machine_id` from `~/.qontinui/machine.json`),
  the helper reports "no conflicting symbol claims." That's the
  symbol_watcher daemon doing its job — not a conflict.
- **Missing `machine.json`**: the helper warns once on stderr and falls
  through to "list all holders." Useful on fresh installs where the
  device hasn't been paired yet.
- **Coord unreachable**: per-file error is surfaced on stderr; other
  files still get queried. Exit code 1 only if conflicts were found.
  Network failures alone do not produce a false-positive.

## When to use

- **Pre-flight before starting a focused edit** — "am I about to step
  on someone?" Run with no args; takes <1s per file.
- **After the PreToolUse hook fires a warning** — re-run this skill to
  see the full table (the hook only emits one line per held symbol).
- **When the operator says "did MSI claim something in `Foo.rs`?"** —
  run `/symbol-claims-warn --file path/to/Foo.rs`.

## Examples

```bash
# Default: scan changed + untracked files in the current worktree
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py

# Restrict to one file
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py \
    --file src/foo.rs

# Multiple files
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py \
    --file src/foo.rs --file src/bar.rs

# JSON for tooling
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py --json

# Outside a git worktree: pass --repo + --file explicitly
python <workspace-root>/qontinui-stack/scripts/symbol-claims-by-machine.py \
    --repo qontinui-runner --file src-tauri/src/main.rs
```

## Related

- **PreToolUse hook** (`.claude/hooks/symbol-conflict-warn.sh`) — fires
  automatically before every `Edit` / `Write` / `MultiEdit`. Same helper,
  one-line stderr emit. Soft warn — never blocks.
- **Symbol watcher daemon** (qontinui-supervisor PR #49, Phase 4.1) —
  the producer of these claims. Tree-sitter-extracts Rust / TS / Python
  symbols on every save and acquires `ClaimKind::Symbol` against coord.
- **Phase 4.4 dashboard surfacing** — same data, different render
  (operator-facing "currently editing" sub-line on the activity feed).

## Environment overrides

- `COORD_HTTP_URL` — coord HTTP base (default `https://coord.qontinui.io`).
- `QONTINUI_MACHINE_JSON_PATH` — override the machine.json location
  (default `~/.qontinui/machine.json`).
