#!/bin/bash
# Shared helper — SOURCE this, do not execute it.
#
# Resolves a Claude Code session id to the name Claude Code itself shows in the
# session window and in `/resume`, read from Claude Code's own per-process
# registry:
#
#   <config-dir>/sessions/<pid>.json
#
# Real shape (verbatim, this fleet, Claude Code 2.1.220):
#
#   {"pid":198780,"sessionId":"6c71bd20-…","cwd":"<the session cwd>",  (a captured sample, cwd elided)
#    "startedAt":1785189526987,"version":"2.1.220","peerProtocol":1,
#    "kind":"interactive","entrypoint":"sdk-cli","name":"qontinui-coord-c7",
#    "nameSource":"derived"}
#
# ── The `nameSource` gate (mandatory — do not remove) ─────────────────────────
# `nameSource:"derived"` means Claude Code auto-derived a `<dir>-<2hex>` slug
# from the cwd (`qontinui-root-ec`, `qontinui-coord-88`). That is STRICTLY WORSE
# than the plan-title and coord-slug tiers the callers already have, so a
# derived name is rejected outright — it never competes. An ABSENT `nameSource`
# key means the name is an operator-set `/rename` value (`worktree prune`,
# `merge-shepherd`) and IS accepted. Ungated, this helper would regress every
# plan-titled session into a cwd slug — the opposite of what it is for.
#
# ── Resolution order: NEWEST ROW FIRST, then gate ─────────────────────────────
# A registry file is per-PID, but a `sessionId` outlives its PID — `/resume`
# (and crash-then-restart) reuses the same id under a new PID and leaves the old
# `<old-pid>.json` behind. So multiple rows can match one id with DIFFERENT
# names and different `nameSource`.
#
# We therefore pick the row with the greatest `startedAt` FIRST, and only then
# apply the derived gate to that single row. Order matters, and the reverse is a
# real bug: gate-then-sort lets a stale non-derived row outrank the live derived
# row, and since tier 0 is the only caller tier allowed to overwrite, that
# resurrected name would be written over the current one — exactly the staleness
# this helper exists to remove.
#
# `startedAt` is preferred over a live-PID filter (what the runner's Rust reader
# `claude_session_registry.rs::read_live_sessions` does) because it needs no
# process enumeration and behaves identically on Windows and POSIX.
#
# Callers (keep this the single implementation — do not re-write the scan):
#   scripts/populate-session-name.sh          — SessionStart, tier 0
#   scripts/sync-session-name-from-rename.sh  — Stop, /rename propagation
#
# Plan: 2026-07-24-propagate-claude-code-window-name-to-all-session-surfaces
#       (P1 tier-0 read + P2 rename sync share this lookup).

# Emit every candidate Claude Code session-registry directory, one per line.
# Nonexistent candidates are dropped here so callers never glob a missing dir.
claude_registry_dirs() {
  local dir seen="" candidates=()

  # 1. The config dir THIS session was launched with, when Claude Code exports
  #    it. On Windows it arrives with backslashes (`C:\claude\.claude-tiohorst`);
  #    normalise separators and strip a trailing slash so it dedupes against the
  #    glob in step 2, which finds the same directory.
  if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
    dir="${CLAUDE_CONFIG_DIR//\\//}"
    dir="${dir%/}"
    candidates+=("$dir/sessions")
  fi

  # 2. Windows multi-account layout on this fleet: C:/claude/.claude-<account>/
  candidates+=(C:/claude/.claude*/sessions)

  # 3. POSIX / single-account layout: ~/.claude/sessions (plus any
  #    ~/.claude-<account>/ siblings a non-Windows box may use).
  candidates+=("${HOME:-}"/.claude*/sessions)

  # Unmatched globs stay literal; `-d` drops them along with stale entries.
  # Dedupe with a plain string, not an associative array — this must also run
  # under macOS's system bash 3.2, where `local -A` is a syntax error. Runs on
  # every Stop hook, so it spawns nothing.
  for dir in "${candidates[@]}"; do
    [ -d "$dir" ] || continue
    case "$seen" in
      *"|$dir|"*) continue ;;
    esac
    seen="$seen|$dir|"
    printf '%s\n' "$dir"
  done
}

# Emit every registry JSON file across all candidate dirs, one per line.
claude_registry_files() {
  local dir file
  while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    for file in "$dir"/*.json; do
      [ -f "$file" ] && printf '%s\n' "$file"
    done
  done < <(claude_registry_dirs)
}

# claude_registry_name <session-id>
#
#   Prints the operator-meaningful Claude Code name for <session-id> and
#   returns 0. Prints nothing and returns 1 when no registry row carries that
#   id, when the newest matching row carries no name, or when that row is
#   Claude Code's own `nameSource:"derived"` cwd slug.
#
#   The returned name is normalised the same way the plan-title tier is (first
#   line only, written WHOLE -- never length-capped, since a cut trailer name
#   addresses nothing) and additionally stripped of ALL control characters:
#   registry names are operator-typed free text (spaces are common —
#   `"worktree prune"`), and the value flows into `set-terminal-title.sh`, which
#   embeds it raw inside an `ESC ] 0 ; <title> BEL` sequence. An ESC or BEL in
#   the name would be terminal-escape injection.
claude_registry_name() {
  local session_id="${1:-}"
  [ -n "$session_id" ] || return 1

  local -a files=()
  local file
  while IFS= read -r file; do
    [ -n "$file" ] && files+=("$file")
  done < <(claude_registry_files)
  [ ${#files[@]} -gt 0 ] || return 1

  # ── mtime gate: skip the `jq` scan when no registry row has changed ─────────
  #
  # The scan below is ONE jq process, but `sync-session-name-from-rename.sh`
  # runs it on EVERY assistant turn to observe a value that changes at most a
  # handful of times per session — usually zero. On this fleet that is a
  # 0.2-2.3s process start per turn over 53 JSON files in 5 config dirs.
  #
  # `test -nt` is a bash BUILTIN, so N of them cost N stat syscalls and no
  # process. Two sidecars, because one file cannot be both the clock and the
  # payload:
  #   .<sid>.scan-stamp  — empty; its MTIME is "as of when we last scanned"
  #   .<sid>.scan-cache  — the scan's RESULT (line 1 `hit`/`miss`, line 2 name)
  #
  # The stamp is written BEFORE the scan reads the files, never after. A rename
  # landing mid-scan then has an mtime >= the stamp and is picked up next turn;
  # writing the stamp afterwards would hide it. Coarse filesystem timestamps
  # therefore cost an extra scan, never a missed rename — the safe direction,
  # since a gate that delays a /rename by a turn defeats the hook this serves.
  #
  # Plan: 2026-08-06-stop-hook-per-turn-latency (P2).
  # `${HOME:-}` — callers run under `set -u`, where a bare unset HOME aborts.
  local cache_dir="${QONTINUI_SESSION_NAMES_DIR:-${HOME:-}/.qontinui/session-names}"
  local stamp="" cache=""
  if [ -d "$cache_dir" ]; then
    stamp="$cache_dir/.$session_id.scan-stamp"
    cache="$cache_dir/.$session_id.scan-cache"
  fi

  if [ -n "$stamp" ] && [ -e "$stamp" ] && [ -r "$cache" ]; then
    local changed=0
    for file in "${files[@]}"; do
      # A NEW <pid>.json has a fresh mtime, so this catches additions too. A
      # DELETION moves no mtime — but it can only remove a candidate row, never
      # introduce a newer name, so missing that rescan is safe.
      if [ "$file" -nt "$stamp" ]; then changed=1; break; fi
    done
    if [ "$changed" -eq 0 ]; then
      local cached_status="" cached_name=""
      {
        IFS= read -r cached_status || true
        IFS= read -r cached_name || true
      } < "$cache"
      # A cached name of EXACTLY 80 chars is read as a MISS and rescanned: it is
      # most likely a name the old `${name:0:80}` cut wrote here, and serving it
      # would keep stamping the truncated trailer until a registry row moved.
      # A genuine 80-char name only pays one rescan per turn for it.
      if [ "$cached_status" = "hit" ] && [ -n "$cached_name" ] && [ "${#cached_name}" -ne 80 ]; then
        printf '%s\n' "$cached_name"
        return 0
      elif [ "$cached_status" = "miss" ]; then
        return 1
      fi
      # Any other content is a corrupt/partial sidecar — fall through and rescan.
    fi
  fi

  # Mark the scan's "as of" instant BEFORE reading a single registry file.
  [ -n "$stamp" ] && { : > "$stamp"; } 2>/dev/null

  # Fast path: ONE jq process for the whole scan.
  #
  # `-s` slurps every row into a single array so the newest can be picked.
  # `last` of an empty array is `null`, and the `if` maps that to no output.
  # `startedAt` is coerced to 0 unless it is genuinely a number, so this agrees
  # with the python3 path on a malformed row (jq otherwise sorts strings ABOVE
  # numbers, which would let a junk row win).
  local name="" jq_ok=0
  if command -v jq >/dev/null 2>&1; then
    if name=$(jq -s -r --arg sid "$session_id" '
      map(select(type == "object" and .sessionId == $sid))
      | sort_by(if (.startedAt | type) == "number" then .startedAt else 0 end)
      | last
      | if . == null then empty
        elif ((.nameSource // "") == "derived") then empty
        else (.name // empty)
        end
    ' "${files[@]}" 2>/dev/null); then
      jq_ok=1
    else
      name=""
    fi
  fi

  # Fall back to python3 ONLY when jq could not COMPLETE — i.e. it is absent, or
  # it aborted (exit 5) on a half-written file. jq exiting 0 with empty output
  # is a CLEAN MISS and must be trusted: an unrenamed session is the common
  # case, and re-scanning it in python3 would double the cost of every turn.
  if [ "$jq_ok" -eq 0 ] && command -v python3 >/dev/null 2>&1; then
    name=$(python3 -c '
import json
import sys

session_id = sys.argv[1]
best_started_at = None
best_row = None
for path in sys.argv[2:]:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            row = json.load(handle)
    except Exception:
        # One half-written or corrupt file must not poison the whole scan —
        # this is why jq (all-or-nothing) falls back to here.
        continue
    if not isinstance(row, dict) or row.get("sessionId") != session_id:
        continue
    started_at = row.get("startedAt")
    # bool is a subclass of int; exclude it so jq and python agree on junk.
    if isinstance(started_at, bool) or not isinstance(started_at, (int, float)):
        started_at = 0
    # Newest row wins. `>=` mirrors jq sort_by + last (stable, later input wins
    # a tie) — both iterate the file list in the same order.
    if best_started_at is None or started_at >= best_started_at:
        best_started_at = started_at
        best_row = row

# Gate AFTER the recency pick, never before — see the header.
if best_row is not None and best_row.get("nameSource") != "derived":
    name = best_row.get("name") or ""
    if name:
        print(name)
' "$session_id" "${files[@]}" 2>/dev/null || true)
  fi

  # Normalise in pure bash so the hot path spawns nothing extra: first line,
  # then strip every control character. NO length cap: the name is stamped into
  # the immutable `Session-Name` trailer, and a cut name (this was `${name:0:80}`
  # until 2026-10-01) matches no roster entry exactly, so the commit's author
  # became unreachable by name (plan
  # 2026-10-01-a-commit-author-session-is-unreachable-because-every-session-roster-is-per-account).
  name="${name%%$'\n'*}"
  name="${name//[[:cntrl:]]/}"

  # Record the scan's result for the mtime gate above. Cache the MISS as well
  # as the hit — an unrenamed session is the common case, and caching only hits
  # would leave exactly that case paying `jq` on every turn, which is the cost
  # this gate exists to remove. Written AFTER the scan (the stamp's mtime, set
  # before it, is what the gate compares against — this file's mtime is unused).
  if [ -n "$cache" ]; then
    if [ -n "$name" ]; then
      { printf 'hit\n%s\n' "$name" > "$cache"; } 2>/dev/null || true
    else
      { printf 'miss\n\n' > "$cache"; } 2>/dev/null || true
    fi
  fi

  [ -n "$name" ] || return 1

  printf '%s\n' "$name"
  return 0
}
