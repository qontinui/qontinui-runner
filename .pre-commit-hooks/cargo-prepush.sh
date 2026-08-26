#!/usr/bin/env bash
# Pre-push guard: mirrors CI's Rust gate so a correctness failure is surfaced
# in 60-90s of local cycle time instead of 8-15 min of CI roundtrip per push.
# Tiered per plan 2026-07-05-lint-tiers-and-diff-scoped-gates: formatting
# auto-applies (never blocks), and clippy runs plain (no `-D warnings`) so only
# deny-tier (correctness/suspicious) lints from Cargo.toml [lints.clippy] block.
#
# The two halves are DECOUPLED, and the skip switch governs only the expensive
# one. `cargo fmt` takes seconds and can only help you; `cargo clippy` builds
# the whole workspace and is the thing people actually need to escape. Wiring
# both to a single switch meant anyone who skipped the slow build also silently
# lost the free formatting fix — and CI runs `cargo fmt -- --check` BEFORE
# `cargo test` inside the required `test (ubuntu-22.04)` / `test
# (windows-latest)` contexts, so the entire Rust suite never even executed.
# That stranded runner#812 and runner#806 on pure whitespace in 2026-07.
#
#   QONTINUI_PREPUSH_SKIP=1      skip clippy; cargo fmt STILL auto-applies
#   QONTINUI_PREPUSH_SKIP_ALL=1  skip both — the explicit total escape hatch
#   QONTINUI_PREPUSH_STRICT=1    turn the "cannot load this workspace" DECLINE
#                                below into a hard failure. Off by default; see
#                                the preflight's own comment for why declining
#                                is the right default there.
#   git push --no-verify         skips both, because git never invokes the hook
#                                at all. Nothing in this file can preserve fmt
#                                for you in that case; prefer
#                                QONTINUI_PREPUSH_SKIP=1.
#
# ⚠️ THE DIFF-SCOPE CHECK MUST SURVIVE A MISSING UPSTREAM. Do not "simplify"
# the `push_base_ref` cascade below back to `git rev-parse @{u}`. An allocated
# agent worktree reserves a fresh branch, so its branch has NO upstream until
# the first push completes — and every push from such a worktree is a first
# push. Keying the check on `@{u}` alone (as this hook did until
# 2026-08-26) makes the `if` false on every worktree, drops the hook into its
# conservative "run the gate" fallback, and charges a TS-only PR the full Rust
# gate for a diff containing no Rust. The intended edge case was the only path
# those pushes ever took. Plan:
# 2026-08-26-runner-prepush-gate-unrunnable-in-worktrees.
set -euo pipefail

if [[ "${QONTINUI_PREPUSH_SKIP_ALL:-}" == "1" ]]; then
  echo "[pre-push] QONTINUI_PREPUSH_SKIP_ALL=1 — skipping cargo fmt + clippy"
  exit 0
fi

# Backwards compatible: the long-standing name keeps working, but now scopes to
# the expensive half only. Other sessions and docs reference it, so it must not
# change meaning to "runs nothing" — it means "don't make me pay for clippy".
SKIP_CLIPPY=0
if [[ "${QONTINUI_PREPUSH_SKIP:-}" == "1" ]]; then
  SKIP_CLIPPY=1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(git rev-parse --show-toplevel)"

# If nothing under src-tauri/ changed in the pushed range, skip the cargo gate.
#
# The base ref comes from the shared cascade in `lib/push-range.sh`
# (`@{upstream}` -> `origin/HEAD` -> `origin/main`, each validated), NOT from a
# bare `@{u}` — see the ⚠️ in this file's header for why that distinction is
# load-bearing rather than cosmetic.
#
# Conservative fallback in BOTH directions: if the cascade resolves nothing, or
# the library itself is missing, RUN the gate. This hook fails OPEN where
# gen-events attribution fails CLOSED, which is exactly why `push_base_ref`
# takes no position on the fallback.
PUSH_RANGE_LIB="$SCRIPT_DIR/lib/push-range.sh"
if [[ -f "$PUSH_RANGE_LIB" ]]; then
  # shellcheck source=lib/push-range.sh
  . "$PUSH_RANGE_LIB"
  if base_ref="$(push_base_ref "$ROOT")"; then
    if base=$(git -C "$ROOT" merge-base "$base_ref" HEAD 2>/dev/null); then
      if ! git -C "$ROOT" diff --name-only "$base"..HEAD -- src-tauri/ | grep -q .; then
        echo "[pre-push] no src-tauri/ changes since $base_ref — skipping cargo gate"
        exit 0
      fi
    fi
  else
    echo "[pre-push] no upstream, origin/HEAD or origin/main to scope the diff against — running the full gate"
  fi
else
  echo "[pre-push] NOTE — missing $PUSH_RANGE_LIB, so the pushed range cannot be"
  echo "          scoped. Running the full gate rather than skipping it."
fi

# ── Preflight: can cargo load this workspace at all? ─────────────────────────
#
# `src-tauri/Cargo.toml` path-deps three crates in a FILESYSTEM SIBLING repo
# (`../../qontinui-schemas/{rust,code-graph,rust-vision-core}`). Path deps are
# literal manifest paths, so from an allocated worktree at
# `<root>/qontinui-worktrees/<uuid>/qontinui-runner` they resolve INSIDE the
# bundle — where, on virtually every worktree on this fleet, no
# `qontinui-schemas` checkout exists.
#
# Without this preflight the failure lands in `cargo fmt` below (the script's
# first bare cargo call) and `set -euo pipefail` kills the push on a raw cargo
# dump: no `[pre-push]` framing, no cause, no remedy, and no mention of the
# skip variables — the exact class of misleading failure the clippy classifier
# further down was built to prevent. It is a MANIFEST-LOAD failure, so nothing
# in the developer's own diff explains it.
#
# Probe with `cargo metadata`, not a hardcoded sibling-path list: a hardcoded
# list covers today's three deps and silently stops covering a fourth added
# later. Asking cargo the question cargo will actually answer cannot drift from
# the manifest.
#
# Probed at $ROOT because the REPO root is the `[workspace]` root (`src-tauri`
# is a member), which is also how the failure presents ("failed to load
# manifest for workspace member .../src-tauri").
#
# ⚠️ `--no-deps` is load-bearing and must not be dropped here. It skips
# dependency resolution entirely, which is why the probe costs ~19ms warm and
# leaves `Cargo.lock` BYTE-IDENTICAL. That matters because this preflight sits
# ABOVE the Cargo.lock snapshot/restore trap below — a probe that rewrote the
# lock would break the hook's non-mutating invariant from outside the trap that
# protects it. If `--no-deps` is ever dropped, MOVE the probe below that trap.
STRICT="${QONTINUI_PREPUSH_STRICT:-0}"

# Typed decline for "the local environment cannot run this gate". Modelled on
# `cannot_evaluate()` in gen-events-drift.sh: always states the reason, the
# remedy, and the residual coverage. Never a bare `exit 0`.
#
# ⚠️ THE COVERAGE SENTENCE IS THE OPPOSITE OF ITS SIBLING'S, AND THAT IS
# DELIBERATE. gen-events-drift.sh truthfully tells you CI does NOT cover its TS
# domain. Here the reverse is true and verified: ci.yml runs
# `cargo fmt -- --check` and `cargo clippy` inside the REQUIRED
# `test (ubuntu-22.04)` context, plus a parallel `clippy-windows` job. Copying
# the sibling's wording would tell developers they are unprotected when they
# are not.
cannot_evaluate() {
  local reason="$1"
  echo
  echo "[pre-push] CANNOT RUN THE CARGO GATE — $reason" >&2
  echo "[pre-push]   This is a MANIFEST-LOAD failure, not a lint failure. Nothing in" >&2
  echo "[pre-push]   your diff caused it: src-tauri/Cargo.toml path-deps three crates" >&2
  echo "[pre-push]   in a sibling qontinui-schemas checkout, and cargo resolves those" >&2
  echo "[pre-push]   paths relative to the manifest — so from a worktree bundle they" >&2
  echo "[pre-push]   point inside the bundle." >&2
  echo "[pre-push]   To enable the local gate: materialize a sibling qontinui-schemas" >&2
  echo "[pre-push]   checkout in this same worktree bundle, allocated with the SAME" >&2
  echo "[pre-push]   work_unit_id (see coordination-tiers.md, 'The sibling" >&2
  echo "[pre-push]   build-dependency checkout'). No environment variable can redirect" >&2
  echo "[pre-push]   a cargo path dependency; materializing the checkout is the only" >&2
  echo "[pre-push]   remedy." >&2
  echo "[pre-push]   Coverage while this is unresolved: CI STILL GATES BOTH HALVES." >&2
  echo "[pre-push]   ci.yml runs 'cargo fmt -- --check' and 'cargo clippy' inside the" >&2
  echo "[pre-push]   required 'test (ubuntu-22.04)' context, plus a parallel" >&2
  echo "[pre-push]   'clippy-windows' job. Declining here costs LATENCY, NOT COVERAGE:" >&2
  echo "[pre-push]   you lose the 60-90s local signal, not the gate itself." >&2
  if [[ "$STRICT" == "1" ]]; then
    echo "[pre-push]   QONTINUI_PREPUSH_STRICT=1 — treating this as a failure." >&2
    exit 1
  fi
  echo "[pre-push]   Not blocking the push — a missing sibling checkout is a property" >&2
  echo "[pre-push]   of this environment, not evidence of a defect in the pushed code." >&2
  echo "[pre-push]   Set QONTINUI_PREPUSH_STRICT=1 to block instead." >&2
  echo
  exit 0
}

METADATA_LOG="$(mktemp)"
cleanup_metadata_log() { rm -f "$METADATA_LOG"; }
trap cleanup_metadata_log EXIT

# `|| metadata_rc=$?` rather than `if ! cargo …; then rc=$?`: inside the body of
# an `if !`, `$?` is the status of the NEGATION (always 0), not cargo's.
metadata_rc=0
(cd "$ROOT" && cargo metadata --no-deps --format-version 1) >/dev/null 2>"$METADATA_LOG" || metadata_rc=$?

if [[ "$metadata_rc" -ne 0 ]]; then
  # Show what cargo actually said, then classify against the captured log —
  # the same grep-the-log-then-branch shape the clippy classifier below uses.
  # One hook, one reporting idiom.
  cat "$METADATA_LOG" >&2
  if grep -qiE 'failed to (read|load manifest)' "$METADATA_LOG" \
     && grep -qiE 'Cargo\.toml|No such file or directory' "$METADATA_LOG"; then
    cannot_evaluate "cargo cannot load this workspace: a path dependency's manifest is missing (exit $metadata_rc)"
  elif grep -qiE 'memory allocation of|handle_alloc_error|out of memory|STATUS_STACK_BUFFER_OVERRUN|0xc0000409|SIGKILL|\bKilled\b' "$METADATA_LOG"; then
    echo "[pre-push] FAIL — the workspace probe ran out of memory. This is NOT a lint failure."
    echo "          Nothing is wrong with your code. Free memory (close peer builds;"
    echo "          check vmmemWSL) and retry, or QONTINUI_PREPUSH_SKIP_ALL=1 git push"
    echo "          to let CI gate."
    exit 1
  elif grep -qE 'E0786|invalid metadata files|failed to mmap' "$METADATA_LOG"; then
    echo "[pre-push] FAIL — corrupt build cache. This is NOT a lint failure."
    echo "          A .rmeta is truncated, usually from a previously killed build."
    echo "          Delete the named artifact AND its debug/.fingerprint/<crate>-*"
    echo "          dir, then retry."
    exit 1
  elif [[ "$metadata_rc" -eq 127 ]] || grep -qiE 'command not found|could not execute' "$METADATA_LOG"; then
    echo "[pre-push] FAIL — cargo could not run at all (exit $metadata_rc). This is NOT a lint failure."
    echo "          Check the toolchain: install Rust via rustup."
    exit 1
  else
    # Honest terminal: non-zero, but nothing in the log identifies why. Say that
    # rather than guessing — a wrong specific cause is worse than an admitted
    # unknown, and this arm must NOT silently decline: an unclassified workspace
    # failure is not evidence that the environment is merely incomplete.
    echo "[pre-push] FAIL — 'cargo metadata' exited $metadata_rc, but the output identifies"
    echo "          no specific cause. Full output is above. Do NOT assume a lint:"
    echo "          re-run 'cargo metadata --no-deps' at the repo root before"
    echo "          changing any code."
    exit 1
  fi
fi
rm -f "$METADATA_LOG"
trap - EXIT

cd "$ROOT/src-tauri"

# A path dependency on a sibling repo (e.g. qontinui-schemas/rust) that sits
# ahead of the version pinned in the committed Cargo.lock makes every cargo
# invocation rewrite Cargo.lock. That churn is environment-local — it depends
# on which ref the sibling checkout is parked at (a feature branch cut before
# a sibling version bump, a sibling that's been pulled ahead, etc.) — and must
# NOT be committed. But pre-commit reports the hook as Failed whenever it
# leaves a tracked file modified, even when the gate itself passed. Snapshot
# Cargo.lock and restore it on exit so the gate is non-mutating regardless of
# local skew. A Cargo.lock change the developer actually intends rides in their
# own staged diff (which pre-commit hands us as the baseline), so this never
# reverts an intended update.
LOCK="$ROOT/Cargo.lock"
LOCK_BAK=""
if [[ -f "$LOCK" ]]; then
  LOCK_BAK="$(mktemp)"
  cp -p "$LOCK" "$LOCK_BAK"
fi
restore_lock() {
  if [[ -n "$LOCK_BAK" ]]; then
    cp -p "$LOCK_BAK" "$LOCK"
    rm -f "$LOCK_BAK"
  fi
}
trap restore_lock EXIT

# T2 (formatting) — auto-apply, never block, and never governed by the skip
# switches above. Run `cargo fmt` (writes) instead of `cargo fmt -- --check`
# (fails). Formatting is mechanical; there is no value in bouncing a push over
# whitespace, and CI is where `--check` does the enforcing.
echo "[pre-push] cargo fmt (auto-apply)"

# Fingerprint the src-tauri worktree against HEAD so we can tell whether fmt
# actually rewrote anything. A content hash rather than a filename list: a file
# that was already dirty before fmt and then got reformatted too would not show
# up as a change in the name set. Falls back to a constant when the fingerprint
# can't be taken, so a diagnostic can never be the thing that blocks a push.
fmt_fingerprint() {
  git -C "$ROOT" diff HEAD -- src-tauri | git hash-object --stdin || echo "unavailable"
}
fmt_before="$(fmt_fingerprint)"
cargo fmt
fmt_after="$(fmt_fingerprint)"

# Re-stage any src-tauri files fmt rewrote. `git add -u` on the src-tauri tree
# picks up tracked-file modifications only (no new/untracked files). Best-effort:
# if nothing changed this is a no-op.
git -C "$ROOT" add -u -- src-tauri || true

if [[ "$fmt_before" != "$fmt_after" ]]; then
  echo
  echo "[pre-push] NOTE — cargo fmt rewrote files, and they are now STAGED."
  echo "          A pre-push hook cannot alter commits that are already being"
  echo "          pushed, so THIS push still carries the unformatted tree and"
  echo "          CI's required 'Format Rust code check' will red it. Commit"
  echo "          (or amend) the staged fmt fix and push again."
  echo
fi

if [[ "$SKIP_CLIPPY" == "1" ]]; then
  echo "[pre-push] QONTINUI_PREPUSH_SKIP=1 — skipping clippy (fmt above still ran)."
  echo "[pre-push] cargo gate passed (fmt only; CI will gate clippy)."
  exit 0
fi

# T1 (correctness) — block. Plain `cargo clippy`: NO `--all-targets` (that
# compiles #[cfg(test)] modules this PR didn't touch — stricter than CI and a
# frequent source of unrelated-lint push blocks) and NO `-D warnings` (that
# would promote every warning to an error and neuter the Cargo.toml tiering).
# Lint levels come solely from `[lints.clippy]` in src-tauri/Cargo.toml: a
# `deny`-tier (correctness/suspicious) lint still exits non-zero and blocks,
# while `warn`-tier (style/complexity/perf) lints are emitted but do not fail
# the push. See plan 2026-07-05-lint-tiers-and-diff-scoped-gates.
echo "[pre-push] cargo clippy (levels from Cargo.toml [lints.clippy]; deny-tier blocks)"

# Capture clippy's output as well as showing it, so the failure message below can
# name the ACTUAL cause instead of asserting one.
#
# This block used to be `if ! cargo clippy; then echo "a deny-tier lint fired"`,
# which mapped EVERY non-zero exit onto one specific cause. That is false-safe in
# the worst direction: it accuses you of writing a correctness bug when the
# toolchain, the build cache, or the machine is at fault, and the message is
# specific enough to be believed. Three distinct non-lint failures were observed
# on one box on 2026-07-29 alone — a truncated `libserde-*.rmeta` (`E0786`) left
# by an OOM-killed build, `tauri-runtime-wry` aborting with
# STATUS_STACK_BUFFER_OVERRUN, and a cold build heading for the bin crate that
# OOMs — every one of them reported as "a deny-tier clippy lint fired".
#
# `set -o pipefail` is already on (line 23), so the pipeline's status is cargo's
# whenever cargo is the failing stage.
CLIPPY_LOG="$(mktemp)"
cleanup_clippy_log() { rm -f "$CLIPPY_LOG"; }
trap 'restore_lock; cleanup_clippy_log' EXIT

if ! cargo clippy 2>&1 | tee "$CLIPPY_LOG"; then
  rc=$?
  echo
  # Classify by what the log actually contains. Order matters: resource and
  # toolchain failures can also emit an `error:` line, so they are tested first.
  if grep -qiE 'memory allocation of|handle_alloc_error|out of memory|STATUS_STACK_BUFFER_OVERRUN|0xc0000409|SIGKILL|\bKilled\b' "$CLIPPY_LOG"; then
    echo "[pre-push] FAIL — the build ran out of memory. This is NOT a lint failure."
    echo "          rustc aborted on an allocation. Nothing is wrong with your code."
    echo "          Free memory (close peer builds; check vmmemWSL) and retry, or"
    echo "          QONTINUI_PREPUSH_SKIP=1 git push to let CI gate."
  elif grep -qE 'E0786|invalid metadata files|failed to mmap' "$CLIPPY_LOG"; then
    echo "[pre-push] FAIL — corrupt build cache. This is NOT a lint failure."
    echo "          A .rmeta is truncated, usually from a previously killed build."
    echo "          Delete the named artifact AND its debug/.fingerprint/<crate>-*"
    echo "          dir, then retry. (A bare 'cargo clean' is blocked on shared"
    echo "          target dirs, so remove just the one crate's files.)"
  elif [[ "$rc" -eq 127 ]] || grep -qiE 'command not found|could not execute|no such file or directory' "$CLIPPY_LOG"; then
    echo "[pre-push] FAIL — clippy could not run at all (exit $rc). This is NOT a lint failure."
    echo "          Check the toolchain: rustup component add clippy."
  elif grep -qE '^error(\[|:)' "$CLIPPY_LOG"; then
    echo "[pre-push] FAIL — a deny-tier (correctness/suspicious) clippy lint fired."
    echo "          Fix the lint, or QONTINUI_PREPUSH_SKIP=1 git push to let CI gate."
  else
    # Honest terminal: non-zero, but nothing in the log identifies why. Say that
    # rather than guessing — a wrong specific cause is worse than an admitted
    # unknown.
    echo "[pre-push] FAIL — cargo clippy exited $rc, but the output identifies no"
    echo "          specific cause. Full output is above. Do NOT assume a lint:"
    echo "          re-run 'cargo clippy' directly before changing any code."
  fi
  exit 1
fi

echo "[pre-push] cargo gate passed."
