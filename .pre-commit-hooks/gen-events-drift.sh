#!/usr/bin/env bash
#
# gen-events-drift.sh — drift guard for the Tauri event TS bindings.
#
# WHAT IT GUARDS
#
# The typed bindings under `qontinui-schemas/ts/src/generated/` are derived
# from Rust sources in this repo (terminal payloads, AppEvent, runner-local
# Finding, the schema_export registry). A serde rename or shape change on the
# Rust side (e.g. `terminal_id` -> camelCase `terminalId`, flat -> tagged
# `{event_type, data}` envelope) silently desynchronises them. That class of
# bug is what commits ed1b45d56 / 16e6fb4ff / b0cefef89 fixed. This hook
# regenerates the bindings and tells you when your Rust change would move
# them.
#
# THE INVARIANT THIS HOOK ENFORCES ON ITSELF
#
#   *** It performs ZERO writes inside the qontinui-schemas checkout. ***
#
# `../qontinui-schemas` is not a scratch clone. On a developer box it is a
# SHARED working tree that concurrent sessions have uncommitted work in. The
# previous version of this hook ran `generate_types.sh --ts-only` with its
# default output paths, and that script starts by doing
#
#     rm -f "$TS_OUT_DIR"/*.d.ts "$TS_OUT_DIR/index.ts"
#
# directly inside that shared tree. A `git push` from this repo could
# therefore delete and rewrite another session's uncommitted work in a repo
# this hook does not own. It never had to: drift detection only needs to
# COMPARE, and comparison is read-only.
#
# The invariant is enforced three ways, deliberately overlapping:
#
#   1. Structurally — generation output is redirected into a scratch dir this
#      hook creates under $TMPDIR (QONTINUI_TS_OUT_DIR) and removes on exit.
#      The schemas path is never passed as an output.
#   2. By pre-flight assertion — if the resolved scratch dir ever lands inside
#      the schemas checkout, abort before generating anything.
#   3. By post-flight tripwire — the compared directory's CONTENT and the
#      schemas checkout's `git status` are both snapshotted before and after,
#      and any delta fails the hook loudly. (Content, not just status: git
#      reports untracked files by name only, and ts/src/generated is entirely
#      untracked, so an in-place rewrite there is invisible to `git status`.)
#      This cannot prevent a future regression, but it converts one from a
#      silent clobber into a named, diagnosable failure.
#
# It also NEVER exits silently. Every path prints why it reached its verdict
# and what to do next.
#
# RELATIONSHIP TO CI
#
# `.github/workflows/qontinui-types-drift.yml` (and its mirror,
# qontinui-schemas/.github/workflows/schema-drift.yml) run the same
# generate_types.sh against fresh sibling checkouts, cover the Python bindings
# too, and are required checks. They are the authority. This hook exists only
# to move that signal from an 8-25 minute CI roundtrip to ~a minute locally,
# so when it cannot evaluate drift it says so and gets out of the way rather
# than blocking a push over a local-environment gap. Set
# QONTINUI_GEN_EVENTS_DRIFT_STRICT=1 to turn every "cannot evaluate" into a
# hard failure instead.
#
# Bypass: `SKIP=gen-events-drift git push` (never `--no-verify`, which also
# disables the cargo fmt/clippy gate).

set -euo pipefail

STRICT="${QONTINUI_GEN_EVENTS_DRIFT_STRICT:-0}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# `cd`-and-`pwd` only works on a path that exists; resolve defensively so a
# missing sibling produces our own message rather than a bash error.
SCHEMAS_DIR_RAW="${QONTINUI_SCHEMAS_DIR:-$RUNNER_DIR/../qontinui-schemas}"
if [ -d "$SCHEMAS_DIR_RAW" ]; then
    SCHEMAS_DIR="$(cd "$SCHEMAS_DIR_RAW" && pwd)"
else
    SCHEMAS_DIR=""
fi

log()  { echo "[gen-events-drift] $*"; }
fail() { echo "[gen-events-drift] $*" >&2; }

# Exit path for "the local environment cannot answer the question". Loud by
# construction: it always states the reason, the remedy, and what coverage is
# actually left. Never a bare `exit 0`.
#
# The honest framing matters here. CI's `qontinui-types drift` check does
# `git diff --exit-code ts/src/generated src/qontinui_schemas/generated`, and
# ts/src/generated has zero tracked files in qontinui-schemas — so CI's TS arm
# is vacuous for the same reason this hook's old check was. The PYTHON arm
# (533 tracked files) is real. Do not tell the developer the TS side is
# "covered by CI" when it is not.
cannot_evaluate() {
    local reason="$1" remedy="$2"
    echo
    fail "CANNOT EVALUATE DRIFT — $reason"
    fail "  To enable the local check: $remedy"
    fail "  Coverage while this is unresolved: the Python bindings are gated by"
    fail "  CI ('qontinui-types drift'), but the TypeScript bindings are NOT —"
    fail "  CI diffs ts/src/generated with git, and that directory is untracked"
    fail "  in qontinui-schemas, so CI sees nothing there either. This local"
    fail "  check is currently the only TS drift signal that exists."
    if [ "$STRICT" = "1" ]; then
        fail "  QONTINUI_GEN_EVENTS_DRIFT_STRICT=1 — treating this as a failure."
        exit 1
    fi
    fail "  Not blocking the push — a missing local build artifact is not"
    fail "  evidence of drift. Set QONTINUI_GEN_EVENTS_DRIFT_STRICT=1 to block."
    echo
    exit 0
}

# ── Preconditions ───────────────────────────────────────────────────────────

if [ -z "$SCHEMAS_DIR" ]; then
    cannot_evaluate \
        "the qontinui-schemas checkout was not found at $SCHEMAS_DIR_RAW" \
        "clone qontinui-schemas beside qontinui-runner, or set QONTINUI_SCHEMAS_DIR"
fi

COMPILE_SCRIPT="$SCHEMAS_DIR/scripts/compile_typescript.mjs"
if [ ! -f "$COMPILE_SCRIPT" ]; then
    cannot_evaluate \
        "the codegen helper $COMPILE_SCRIPT is missing" \
        "pull qontinui-schemas, then run 'npm install' in it"
fi

# The comparison baseline: the bindings as they currently exist on disk in the
# schemas checkout. NOTE this is an on-disk content comparison, not
# `git diff`. `ts/src/generated/` carries ZERO git-tracked files (the tree was
# swept away by qontinui-schemas 34709ada, "ci: add secret-scan caller"), so
# the old `git -C "$SCHEMAS_DIR" diff -- ts/src/generated` could never report
# anything: git diff does not see untracked files. That check was vacuously
# green for its entire life while the destructive regeneration ran anyway —
# all of the risk, none of the signal. Comparing content instead restores the
# signal and is indifferent to tracking status.
BASELINE_DIR="$SCHEMAS_DIR/ts/src/generated"

# `ts/src/tauri-events/` is deliberately NOT compared. It is a hand-maintained
# re-export module; `generate_types.sh --ts-only` never writes it. Diffing it
# could only ever surface unrelated uncommitted work sitting in the shared
# schemas checkout and attribute it to the pusher.

if [ ! -d "$BASELINE_DIR" ] || ! ls "$BASELINE_DIR"/*.d.ts >/dev/null 2>&1; then
    cannot_evaluate \
        "no baseline bindings on disk at $BASELINE_DIR (they are a build artifact, untracked in qontinui-schemas)" \
        "run 'bash src-tauri/scripts/generate_types.sh --ts-only' from $RUNNER_DIR once to populate it — note that command DOES write into the shared qontinui-schemas checkout, so coordinate with any session working there first"
fi

# ── Scratch output, owned by this hook ──────────────────────────────────────

# Guard 2: containment assertion. This runs BEFORE `mktemp -d`, deliberately.
# Checking after would mean creating the scratch directory inside the schemas
# checkout and then refusing — a guard that violates the invariant it enforces.
# So resolve where mktemp would put it and vet that first.
SCRATCH_PARENT="${TMPDIR:-/tmp}"
if [ -d "$SCRATCH_PARENT" ]; then
    SCRATCH_PARENT="$(cd "$SCRATCH_PARENT" && pwd)"
fi
case "$SCRATCH_PARENT/" in
    "$SCHEMAS_DIR"/*)
        fail "REFUSING TO RUN — the temp directory this hook would generate into"
        fail "sits INSIDE the qontinui-schemas checkout:"
        fail "    TMPDIR  : $SCRATCH_PARENT"
        fail "    schemas : $SCHEMAS_DIR"
        fail "This hook must never write into a repository it does not own; that"
        fail "tree holds other sessions' uncommitted work. Point TMPDIR somewhere"
        fail "outside the schemas checkout and re-push."
        exit 1
        ;;
esac

SCRATCH_ROOT="$(mktemp -d -t gen-events-drift-XXXXXXXX)"
cleanup() { rm -rf "$SCRATCH_ROOT"; }
trap cleanup EXIT

# Belt and braces: mktemp may ignore TMPDIR (some implementations, or a -t
# fallback), so re-check what it actually handed us before writing into it.
SCRATCH_ROOT_ABS="$(cd "$SCRATCH_ROOT" && pwd)"
case "$SCRATCH_ROOT_ABS/" in
    "$SCHEMAS_DIR"/*)
        fail "REFUSING TO RUN — mktemp placed the scratch directory INSIDE the"
        fail "qontinui-schemas checkout ($SCRATCH_ROOT_ABS). Set TMPDIR to a path"
        fail "outside $SCHEMAS_DIR and re-push."
        exit 1
        ;;
esac

# Generation targets and the pre-run copy of the baseline live side by side but
# in SEPARATE directories: the copy must not sit inside the generation target,
# or it would show up as spurious drift in the final comparison.
SCRATCH_DIR="$SCRATCH_ROOT/out"
SCRATCH_PY_DIR="$SCRATCH_ROOT/out-py"
SNAP_DIR="$SCRATCH_ROOT/baseline-snapshot"
mkdir -p "$SCRATCH_DIR"

# Guard 3 (arm): capture enough state to prove afterwards that we changed
# nothing in the schemas checkout. TWO snapshots, because neither alone is
# sufficient:
#
#   a) A CONTENT copy of the compared directory. This is the one that matters.
#      `git status` reports untracked files by NAME only, so an in-place
#      rewrite of ts/src/generated/*.d.ts — which is entirely untracked in
#      qontinui-schemas — produces byte-identical status output before and
#      after. That is precisely the clobber this tripwire exists to catch, and
#      a status-only tripwire misses it. (Confirmed empirically: the sandbox
#      harness's simulated regression slipped straight past the status check.)
#
#   b) `git status --porcelain -uall` over the whole repo, to catch writes
#      OUTSIDE the compared directory. `-uall` rather than plain `--porcelain`
#      so untracked directories are expanded to individual files instead of
#      being collapsed to one `?? dir/` line.
cp -R "$BASELINE_DIR" "$SNAP_DIR"

SCHEMAS_STATUS_BEFORE=""
SCHEMAS_STATUS_AVAILABLE=0
if SCHEMAS_STATUS_BEFORE="$(git -C "$SCHEMAS_DIR" status --porcelain -uall 2>/dev/null)"; then
    SCHEMAS_STATUS_AVAILABLE=1
fi

# ── Regenerate (into the scratch dir only) ──────────────────────────────────

cd "$RUNNER_DIR"

log "regenerating Rust JSON Schemas + per-type TS bindings into a scratch dir..."
log "  scratch  : $SCRATCH_DIR"
log "  baseline : $BASELINE_DIR (read-only)"

# Run only the source-of-truth steps (schemars export + per-type d.ts
# emission). Skip the tsup bundle build because tsup's chunk filenames aren't
# deterministic across runs (e.g. `Region.d-XXXXXXXX.d.cts`), which would
# cause spurious failures here.
#
# QONTINUI_TS_OUT_DIR is what makes this non-destructive: generate_types.sh
# does `rm -f "$TS_OUT_DIR"/*.d.ts` before emitting, and that must land in the
# scratch dir, never in the shared schemas tree.
#
# QONTINUI_PY_OUT_DIR is pinned too even though `--ts-only` should never reach
# the Python branch. That branch does `rm -f "$PER_TYPE_DIR"/*.py` against a
# directory holding 533 GIT-TRACKED files — strictly worse than the bug this
# change fixes — and leaving it unpinned would rest the entire safety property
# on one argument string staying correct through future edits. Pin it.
#
# QONTINUI_SCHEMAS_DIR is forwarded so the child reads its helper scripts from
# the same checkout this hook validated and is watching with the tripwire.
# Without it the hook can vet one checkout while the child reads another.
#
# Log lives in SCRATCH_ROOT, not SCRATCH_DIR — anything inside the generation
# output directory would show up as spurious drift in the comparison below.
GEN_LOG="$SCRATCH_ROOT/.generate_types.log"
set +e
QONTINUI_TS_OUT_DIR="$SCRATCH_DIR" \
QONTINUI_PY_OUT_DIR="$SCRATCH_PY_DIR" \
QONTINUI_SCHEMAS_DIR="$SCHEMAS_DIR" \
    bash src-tauri/scripts/generate_types.sh --ts-only >"$GEN_LOG" 2>&1
GEN_RC=$?
set -e

# compile_typescript.mjs removes its own .tmp-schemas dir, but a crashed run
# can leave it behind; it is not part of the comparison either way.
rm -rf "$SCRATCH_DIR/.tmp-schemas"

# ── Guard 3 (fire): did we touch a repo we do not own? ──────────────────────

tripwire_fired() {
    echo
    fail "TRIPWIRE: the qontinui-schemas working tree changed while this hook ran."
    fail "$1"
}
tripwire_epilogue() {
    fail "Two causes are possible, and the hook cannot tell them apart:"
    fail "  1. A CONCURRENT SESSION wrote to $SCHEMAS_DIR"
    fail "     during the ~minute this hook took. On a box running several"
    fail "     sessions against one shared checkout this is the likely cause,"
    fail "     and nothing is wrong with your push — just re-run it."
    fail "  2. A REGRESSION IN THIS HOOK. It is required to be read-only with"
    fail "     respect to that checkout; if it wrote there, peer work may have"
    fail "     been overwritten and this needs fixing at the source."
    fail "Check the delta above against what you would expect from a peer."
    fail "Do NOT 'git checkout' in qontinui-schemas to tidy it up: that would"
    fail "discard whatever was clobbered instead of recovering it."
    exit 1
}

# (a) Content check on the compared directory — the load-bearing one.
#
# `git diff --no-index` exits 1 for ERRORS as well as for differences, and a
# peer deleting ts/src/generated mid-run would otherwise surface as an empty,
# unexplained tripwire. Re-check the path exists so the two are distinguishable.
if [ ! -d "$BASELINE_DIR" ]; then
    tripwire_fired "$BASELINE_DIR no longer exists — it was removed while this hook ran."
    tripwire_epilogue
fi
if ! git diff --no-index --quiet -- "$SNAP_DIR" "$BASELINE_DIR"; then
    tripwire_fired "Content delta in $BASELINE_DIR (pre-run snapshot vs. now):"
    git --no-pager diff --no-index --stat -- "$SNAP_DIR" "$BASELINE_DIR" >&2 || true
    tripwire_epilogue
fi

# (b) Repo-wide status check — catches writes outside the compared directory.
if [ "$SCHEMAS_STATUS_AVAILABLE" != "1" ]; then
    fail "NOTE: 'git status' could not be read from $SCHEMAS_DIR, so the"
    fail "repo-wide half of the write tripwire is inactive for this run. The"
    fail "content check on $BASELINE_DIR above still ran."
fi
if [ "$SCHEMAS_STATUS_AVAILABLE" = "1" ]; then
    SCHEMAS_STATUS_AFTER="$(git -C "$SCHEMAS_DIR" status --porcelain -uall 2>/dev/null || true)"
    if [ "$SCHEMAS_STATUS_BEFORE" != "$SCHEMAS_STATUS_AFTER" ]; then
        tripwire_fired "Working-tree status delta:"
        # Plain temp files rather than `diff <(...) <(...)`: process
        # substitution needs /dev/fd, which is not reliably present in every
        # shell that ends up running pre-commit hooks on Windows, and a
        # tripwire that dies with a syntax error reports nothing at all.
        printf '%s\n' "$SCHEMAS_STATUS_BEFORE" > "$SCRATCH_ROOT/.status-before"
        printf '%s\n' "$SCHEMAS_STATUS_AFTER"  > "$SCRATCH_ROOT/.status-after"
        diff "$SCRATCH_ROOT/.status-before" "$SCRATCH_ROOT/.status-after" >&2 || true
        tripwire_epilogue
    fi
fi

# ── Codegen outcome (evaluated only AFTER the tripwire) ─────────────────────
#
# Ordering is deliberate and was got wrong once: both checks below can exit,
# and if either ran before the tripwire it would mask a write into the shared
# checkout. The tripwire is the safety property; it is evaluated first,
# unconditionally, whatever codegen did.

if [ "$GEN_RC" -ne 0 ]; then
    fail "ERROR: generate_types.sh failed (exit $GEN_RC). Last 30 lines:"
    tail -n 30 "$GEN_LOG" >&2 || true
    fail "Re-run it manually to reproduce:"
    fail "    cd $RUNNER_DIR && bash src-tauri/scripts/generate_types.sh --ts-only"
    exit 1
fi

# generate_types.sh can succeed (exit 0) having generated NOTHING: with `node`
# absent it prints "WARNING: node not found. Skipping TypeScript generation."
# and carries on. Comparing a populated baseline against an empty scratch dir
# then yields a confident, wrong verdict — every baseline file reported as
# deleted, under a banner blaming the developer's Rust change, with a remedy
# that would have them regenerate over the shared checkout for no reason.
# An empty result means the question was not answered, not that the answer is
# "stale".
if [ ! -f "$SCRATCH_DIR/index.ts" ] || ! ls "$SCRATCH_DIR"/*.d.ts >/dev/null 2>&1; then
    fail "generate_types.sh reported success but produced no TypeScript output."
    fail "Last 30 lines:"
    tail -n 30 "$GEN_LOG" >&2 || true
    cannot_evaluate \
        "codegen emitted nothing into the scratch directory (usually: 'node' is not on PATH, or qontinui-schemas/node_modules is missing)" \
        "install Node 20+, then run 'npm install' in $SCHEMAS_DIR"
fi

rm -f "$GEN_LOG"

# ── Compare (read-only) ─────────────────────────────────────────────────────

# `git diff --no-index` works on plain directories outside any repository and
# exits 1 when they differ. It reports added/removed files too, so a type that
# disappeared from the Rust side is drift as much as one that changed shape.
if git diff --no-index --quiet -- "$BASELINE_DIR" "$SCRATCH_DIR"; then
    # Deliberately not "matches the checked-in output": nothing under
    # ts/src/generated is checked in. Green means it matches the build artifact
    # currently on this disk, which could itself be stale or hand-edited.
    log "OK — regenerated bindings match the ones on disk at $BASELINE_DIR."
    exit 0
fi

echo
fail "ERROR: Generated Tauri event bindings are stale."
fail "Your Rust changes would move qontinui-schemas/ts/src/generated/."
echo >&2
git --no-pager diff --no-index --stat -- "$BASELINE_DIR" "$SCRATCH_DIR" >&2 || true
echo >&2
fail "Fix it by regenerating in qontinui-schemas and committing the result there:"
fail "    cd $RUNNER_DIR && bash src-tauri/scripts/generate_types.sh --ts-only"
fail "    cd $SCHEMAS_DIR && git add ts/src/generated && git commit"
fail "(That command DOES write into qontinui-schemas — run it deliberately, when"
fail " you are ready to own the change, and coordinate first if other sessions"
fail " share that checkout. This hook itself never writes there.)"

# Attribution: if the shared schemas checkout is already dirty in the compared
# path, part of the diff above may be someone else's uncommitted work rather
# than your Rust change. Say so rather than letting the pusher assume it.
if [ "$SCHEMAS_STATUS_AVAILABLE" = "1" ] \
   && [ -n "$(git -C "$SCHEMAS_DIR" status --porcelain -uall -- ts/src/generated 2>/dev/null)" ]; then
    echo >&2
    fail "NOTE: $SCHEMAS_DIR has uncommitted changes under ts/src/generated."
    fail "Some or all of the difference above may be another session's work in"
    fail "that shared checkout, not your Rust change. Check with its owner"
    fail "before regenerating over it."
fi

echo >&2
fail "Bypass for this push only: SKIP=gen-events-drift git push"
exit 1
