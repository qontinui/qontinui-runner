#!/usr/bin/env bash
#
# gen-events-path-deps-check.sh — is every in-repo path dependency of the
# export build an attribution input?
#
# `lib/gen-events-attribution.sh` keeps a hand-written list of the paths whose
# content can move `schemas.json`. A crate the export build depends on by
# `path =` but that the list does not name is invisible to attribution: an
# edit to it clears a guilty pusher. Two such crates went unnoticed until
# 2026-09-30. The walk itself lives in the library
# (`gen_events_uncovered_path_deps`), so this hook and the attribution
# self-test share one implementation; this file only runs it on this repo and
# turns a non-empty answer into a failure.
#
# Toolchain-free and sub-second: it reads manifests with awk and never runs
# cargo, so it can afford to run on every Cargo.toml edit.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/gen-events-attribution.sh
. "$SCRIPT_DIR/lib/gen-events-attribution.sh"

UNCOVERED="$(gen_events_uncovered_path_deps "$REPO_ROOT")"
if [ -z "$UNCOVERED" ]; then
    echo "[gen-events-path-deps] OK — every in-repo path dependency is an attribution input."
    exit 0
fi
echo "[gen-events-path-deps] ERROR: in-repo path dependencies missing from GEN_EVENTS_ATTRIBUTION_PATHS:" >&2
printf '%s\n' "$UNCOVERED" | sed 's/^/[gen-events-path-deps]     /' >&2
echo "[gen-events-path-deps] Add each as a directory entry (trailing /) in" >&2
echo "[gen-events-path-deps] .pre-commit-hooks/lib/gen-events-attribution.sh, or an edit" >&2
echo "[gen-events-path-deps] to that crate will never be blamed for the drift it causes." >&2
exit 1
