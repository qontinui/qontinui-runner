#!/usr/bin/env bash
# capability-evidence.sh -- the EMITTED-evidence ledger writer for the
# capability-negative publication gate. EXECUTE this; it refuses to be sourced.
#
# Plan: 2026-09-30-a-capability-negative-is-checked-where-it-is-published,
# Phase 1, design decision D2 (dossier:coord-door-declared-dead-while-live).
#
# Usage (from a PRODUCER, never by hand):
#   bash scripts/lib/capability-evidence.sh --producer <p> -- '<line>'
#     <p> is one of the CLOSED set: coord-route-census | coord-revive | refusal-remedy
#
# Wired producers: scripts/coord-route-census.sh (its `census:` trailer) and
# .claude/skills/coord-revive/coord-revive.sh `floor_claim_block` (the
# `FLOOR-CLAIM:` line). NOT YET WIRED: `refusal-remedy` is reserved for
# scripts/refusal-remedy.sh, which Phase 3 of the plan ships -- it must print its
# `refusal-read:` line and pass that exact line here through its own internal
# `capneg_emit`, or no refusal-read line is ever evidence.
#
# Appends ONE line to $CAPNEG_STATE_DIR/emitted.jsonl (default
# ~/.qontinui/capability-negative-gate/emitted.jsonl):
#   {"sha256":"<hex of the line's exact bytes, no newline>","at":"<ISO Z>","producer":"<p>"}
# The directory is created 0700 and the file 0600. Nothing is printed on
# stdout, ever: a producer's visible output must not change because it
# ledgered a line. Exit 0 written, 1 not written (unwritable dir, no hasher),
# 2 usage (unknown producer, no line). A producer ignores every non-zero exit.
#
# WHY IT EXISTS. The gate accepts a census trailer, a `FLOOR-CLAIM:` line or a
# `refusal-read:` line beside a capability negative only when that EXACT line's
# digest is in this ledger and younger than 2 h
# (scripts/detector_reach/capability_negative.py `accepted_evidence`). A line an
# author TYPED is well-formed and not in the ledger, so it is not evidence --
# accepting any well-formed stamp would rebuild direction 3 (unearned
# confidence from a stamp that confers nothing).
#
# ⚠️ WHAT THIS DOES NOT BUY. The ledger is a box-local file the author's own
# shell can write, so this is a control against a session SKIPPING the probe,
# not against one FORGING it. Two mitigations, both D2 (i):
#   * this is an EXECUTABLE with a mandatory `--producer` from a closed set,
#     not a sourced public function -- each producer wraps it in its own
#     internal `capneg_emit`, and scripts/capability-negative-selftest.sh
#     asserts by `git grep` that nothing else in the tree invokes either;
#   * the producer is recorded per row, and `capability-negative-gate.sh
#     --report` breaks `stamped` down by producer, so a stamp whose producer
#     never ran in the decision window is visible.
# Describing this as making a probe unforgeable restates direction 3.

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  echo "capability-evidence.sh: execute this, do not source it (D2 (i): no public emit function)" >&2
  return 2 2>/dev/null || exit 2
fi

set -u

producer=""
line=""
have_line=0
while [ $# -gt 0 ]; do
  case "$1" in
    --producer) producer="${2-}"; shift 2 || { echo "capability-evidence.sh: --producer needs a value" >&2; exit 2; } ;;
    --producer=*) producer="${1#--producer=}"; shift ;;
    --) shift; line="${1-}"; have_line=1; [ $# -gt 0 ] && shift; break ;;
    *) echo "capability-evidence.sh: unknown argument '$1'" >&2; exit 2 ;;
  esac
done

case "$producer" in
  coord-route-census|coord-revive|refusal-remedy) ;;
  *) echo "capability-evidence.sh: --producer must be one of coord-route-census|coord-revive|refusal-remedy (got '$producer')" >&2; exit 2 ;;
esac
if [ "$have_line" -ne 1 ] || [ -z "$line" ]; then
  echo "capability-evidence.sh: no line given (usage: --producer <p> -- '<line>')" >&2
  exit 2
fi
# A trailing CR (a CRLF producer on Windows) is not part of the line the
# consumer re-hashes after splitting on \n.
line="${line%$'\r'}"

digest=""
if command -v sha256sum >/dev/null 2>&1; then
  digest="$(printf '%s' "$line" | sha256sum 2>/dev/null)"
elif command -v shasum >/dev/null 2>&1; then
  digest="$(printf '%s' "$line" | shasum -a 256 2>/dev/null)"
fi
digest="${digest%% *}"
case "$digest" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f]*) ;;
  *) exit 1 ;;
esac
[ "${#digest}" -eq 64 ] || exit 1

at="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"
[ -n "$at" ] || exit 1

dir="${CAPNEG_STATE_DIR:-${HOME:-}/.qontinui/capability-negative-gate}"
[ -n "$dir" ] || exit 1
umask 077
mkdir -p "$dir" 2>/dev/null || exit 1
file="$dir/emitted.jsonl"
# One printf of one line under O_APPEND: concurrent producers interleave whole
# lines, never fragments (a single write well under PIPE_BUF).
printf '{"sha256":"%s","at":"%s","producer":"%s"}\n' "$digest" "$at" "$producer" >>"$file" 2>/dev/null || exit 1
chmod 600 "$file" 2>/dev/null || :
exit 0
