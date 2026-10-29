#!/bin/bash
# Shared helper — SOURCE this, do not execute it.
#
# Makes `python3` resolvable for the PreToolUse hooks on a machine that has a
# Python 3 under a different name.
#
# ── Why this exists ──────────────────────────────────────────────────────────
# Every guard parses its JSON payload with `jq`, falling back to `python3`. On
# the operator's Windows box (measured 2026-08-29) NEITHER is on the Git Bash
# PATH: there is no `jq` at all, and the Python 3.12 install is spelled
# `python`. Both rungs missed, the parse returned empty, and each guard took its
# no-command branch and exited 0.
#
# The guards were not degraded — they were INERT. Feeding `git-guard.sh` its own
# flagship trigger on stdin:
#
#   {"tool_name":"Bash","tool_input":{"command":"git reset --hard origin/main"}}
#   -> exit 0, no output, no block
#
# and confirmed the hard way in the same session: a `git reset --hard` ran
# against a worktree holding 19 uncommitted files and nothing stopped it. The
# hook was installed, registered, executable, and blocking nothing.
#
# That is the state `git-guard.sh`'s own comment calls "the one state this
# component keeps mistaking for a working guard". Its prediction for the
# both-parsers-missing case — "it fires on every Bash call, and that flood is
# the alarm working" — does not hold: the branch exits 0 in silence.
#
# ── Why a shim rather than an edit at each call site ─────────────────────────
# Six hooks carry twelve `python3` invocations between them, several of them
# multi-line. Rewriting each is twelve chances to typo a quoting rule inside
# security tooling, and it is the paste-into-six-readers divergence
# `lint-jwt-cascade-parity.py` exists one directory over to prevent. Defining
# the missing name once fixes every site, present and future, and leaves a
# machine that HAS `python3` byte-for-byte unchanged — including CI, which is
# ubuntu-latest and never enters this branch.
#
# ── Why the version probe ────────────────────────────────────────────────────
# `python` is a Python 2 on some machines. The hooks' extractor one-liners are
# valid Python 2 syntax and would RUN there, so accepting a 2.x silently would
# swap one inert guard for a subtly wrong one. A candidate must prove
# `sys.version_info[0] == 3` before it is accepted.
#
# ── Why a WindowsApps `python3` is not trusted (plan
#    2026-09-13-heartbeat-stop-python3-stub-ide-cargo-quiesce, Phase 1) ───────
# On Windows, `%LOCALAPPDATA%\Microsoft\WindowsApps\python3.exe` is an App
# Execution Alias. `command -v python3` resolves it, so this shim used to return
# at once and do nothing. What the alias actually RUNS depends on the box:
#
#   - the Microsoft Store stub: prints nothing and exits 49 (measured on
#     `nomad` 2026-09-13);
#   - the Python Install Manager (PEP 773, Store package
#     PythonSoftwareFoundation.PythonManager): HANGS with no output, or prints
#     "Permission denied" (measured on `spaceship` 2026-09-18/19:
#     `timeout 20 python3 -c ...` -> rc=124, twice);
#   - a genuine Store-installed Python 3, which works (measured on `spaceship`
#     2026-08-30).
#
# The first two turned data reads into false verdicts downstream:
# allocate-worktree.sh reported "box is not paired" and sent coord an empty
# body, and agent-checkpoint.sh reported "--label must be UTF-8" (coord finding
# 4ee60c2f). So an alias is identified by its RESOLVED PATH, WITHOUT executing
# it, and a real interpreter found elsewhere on PATH is preferred. An alias is
# executed only when nothing else exists, and then only under `timeout`. With no
# `timeout` on PATH it is never executed at all.
#
# Stated residual: a broken `python3` OUTSIDE WindowsApps is still trusted on the
# fast path. That class has not been observed, and probing it would put an
# interpreter spawn on every hook call on every box.
#
# ── Why a verdict cache ──────────────────────────────────────────────────────
# Several guards source this on every Bash tool call. Where `python3` resolves
# outside WindowsApps the answer costs zero processes (builtins only). Where it
# does not, proving a candidate costs an interpreter spawn. So the verdict is
# kept in $HOOK_PY3_CACHE_DIR (default ${XDG_CACHE_HOME:-$HOME/.cache}/qontinui)
# and re-validated with builtins only. A hit needs the same PATH, the same
# $PYTHON, the interpreter still executable and not modified since the verdict
# was written, and the verdict younger than its TTL. Any doubt is a miss and a
# re-probe. A negative verdict ("nothing works") is cached for a shorter TTL, so
# a box whose only Python is a hanging alias does not pay the probe bound on
# every hook call. A failed cache write never fails the source.
#
# ── One verdict PER PATH (plan 2026-10-19-hook-spawn-rate-and-orphaned-processes,
#    Phase 3) ─────────────────────────────────────────────────────────────────
# Until then the file held ONE verdict per (require, $PYTHON) and a hit needed
# the stored PATH to equal the caller's. Every concurrent session on a box has
# its own PATH (a runner session's `/tmp/qontinui-identity-<hash>` entry, a
# nested shell's re-prepended `/mingw64/bin:/usr/bin`, a scratchpad tools dir),
# so sessions overwrote each other's verdict and nearly every hook call missed
# and re-probed under `timeout` -- measured 2026-10-19 on the operator box: every hook
# that sourced this file in one Bash tool call paid timeout.exe x2, python.exe
# and mv.exe. The file (`hook-py3-verdict2.<h>`, a new name so the hp3v1
# generation still running elsewhere on the box keeps its own file) now holds
# up to $_HP3_SLOTS records, one per PATH, most recent first, so N concurrent
# PATHs cost N probes per TTL instead of one per alternation. Keyed by the PATH STRING, compared exactly -- not a hash of it
# (a bash djb2 over a ~1.7 KB PATH measured 25 ms per call, on every hook) and
# not a PATH with the volatile identity entry removed (the measured PATHs of
# concurrent sessions differ in a dozen other entries too, so that comparison
# still missed on every alternation).
#
# The litter: a writer killed between writing its temp file and renaming it
# (the harness `taskkill`s a hook that blows its timeout) leaves
# `hook-py3-verdict.<h>.<pid>` behind -- 1,057 of them on the operator box
# 2026-10-19. No in-process cleanup can catch a kill, so the WRITE path sweeps
# temp files older than an hour (one `find`, at most once per shell, and never
# on a cache hit).
#
# ── Contract ─────────────────────────────────────────────────────────────────
#   hook_shim_python3                   return 0 iff a working Python 3 is
#                                       callable as `python3` (real or shimmed)
#   hook_shim_python3 --require <code>  the same, but the interpreter must also
#                                       run <code> cleanly. A zoneinfo consumer
#                                       passes
#                                       'import zoneinfo; zoneinfo.ZoneInfo("UTC")',
#                                       which skips a Python with no tz database
#                                       (Windows has no system tzdb, so zoneinfo
#                                       needs the PyPI `tzdata` package)
#   hook_shim_python3 --force           for a caller that has MEASURED the
#                                       resolvable `python3` dead (it exited
#                                       126/127: not executable, or a loader
#                                       failure). Skips the fast path, the
#                                       in-shell negative memo and the cache
#                                       READ, and probes every candidate, so a
#                                       dead `python3` that merely RESOLVES no
#                                       longer shadows a working `python`. The
#                                       cost is paid only on the broken box
#                                       (ccfg#897, git-guard.sh's 126/127 arm).
#                                       Combines with --require.
# $PYTHON, when set, is the ONLY candidate. It must pass the probe, or the call
# returns 1 rather than silently using another interpreter.
#
# Callers that need a verdict use this return value, never `command -v python3`,
# which an alias satisfies. After a shim the re-call is the zero-process path.
# On success HOOK_PY3_EXE (and HOOK_PY3_ARG, `-3` for the launcher) name the
# interpreter, for a caller that must hand a PATH to another process: a shell
# function does not cross a process boundary.
#
# Tunables (a non-numeric value falls back to its default):
#   HOOK_PY3_PROBE_TIMEOUT  s, default 8: the bound on probing a real interpreter
#   HOOK_PY3_ALIAS_TIMEOUT  s, default 3: the bound on probing an alias. After one
#                           alias times out, the remaining aliases are skipped.
#                           Several hooks that source this run under a 5-10 s
#                           harness timeout.
#   HOOK_PY3_CACHE_DIR, HOOK_PY3_NO_CACHE=1
#   HOOK_PY3_CACHE_ONLY=1   never probe: answer from the fast path or the cache,
#                           else return 1 at once. For a caller whose time is
#                           already spent (the hook dispatcher holding a deny).
#   HOOK_PY3_CACHE_TTL      s, default 86400
#   HOOK_PY3_NEG_TTL        s, default 3600: a DEFINITE "no working Python"
#   HOOK_PY3_TIMEOUT_TTL    s, default 300: a verdict that rests on a probe that
#                           TIMED OUT (or was killed mid-probe). A slow cold start
#                           on a loaded box is not proof that no Python exists,
#                           so it is written off for minutes, not an hour.
#
# ── The jq launcher shim (same plan, Phase 3) ────────────────────────────────
#   hook_shim_jq            run at source time, like hook_shim_python3. When the
#                           `jq` on PATH is a scoop launcher (`shims/jq.exe` +
#                           `jq.shim`), it defines `jq` to run the binary the
#                           .shim names, so every `jq` call in a hook is one
#                           process instead of two (the launcher, then jq). A
#                           real jq, no jq, a shim that adds `args`, or a shim
#                           whose target is gone is left exactly as PATH has it
#                           -- the last of those must keep MISBEHAVING the way
#                           git-guard.sh's section-9 tests stage, not vanish.
#                           HOOK_JQ_EXE names what `jq` runs (empty: no jq).
#
# Needs bash >= 4.4 (`${x,,}`, `printf '%(%s)T'`, empty arrays under `set -u`).

# _hp3_int <var> <default> [nonzero]: $<var> as a non-negative integer, else
# <default>. With `nonzero`, zero is refused too: `timeout 0` means NO bound.
_hp3_int() {
  local v="${!1:-}"
  case "$v" in ''|*[!0-9]*) v="$2" ;; esac
  [ "${3:-}" = nonzero ] && [ "$((10#$v))" = 0 ] && v="$2"
  printf -v "_HP3_$1" '%s' "$v"
}

# True when <path> lies under a WindowsApps directory (App Execution Aliases).
# Builtins only: this runs on the hot path.
_hp3_is_alias() {
  local l="${1,,}"
  l="${l//\\//}"
  case "$l" in */windowsapps/*) return 0 ;; esac
  return 1
}

# True for an absolute path, POSIX or Windows spelling.
_hp3_is_abs() { case "$1" in /*|[A-Za-z]:*|\\\\*) return 0 ;; esac; return 1; }

# _hp3_lookup <name> [skip-glob]: the first executable <name> (or <name>.exe)
# in an ABSOLUTE PATH entry, in _HP3_W. Builtins only. A PATH walk rather than
# `hash`, because `hash` silently does nothing while a FUNCTION of that name
# exists, which is exactly the state after this lib shims `python3`. Entries
# whose lowercased path matches <skip-glob> are passed over.
_hp3_lookup() {
  _HP3_W=""
  local rest="${PATH}:" d f l
  while [ -n "$rest" ]; do
    d="${rest%%:*}"; rest="${rest#*:}"
    _hp3_is_abs "$d" || continue
    for f in "$d/$1" "$d/$1.exe"; do
      [ -x "$f" ] && [ ! -d "$f" ] || continue
      if [ -n "${2:-}" ]; then
        l="${f,,}"; l="${l//\\//}"
        # shellcheck disable=SC2254  # the glob is the point
        case "$l" in $2) continue ;; esac
      fi
      _HP3_W="$f"
      return 0
    done
  done
  return 0
}

# _hp3_path <name-or-path>: an explicit interpreter ($PYTHON) in _HP3_W.
_hp3_path() {
  _HP3_W=""
  local p="$1"
  case "$p" in
    */*|*\\*|[A-Za-z]:*)
      _hp3_is_abs "$p" || p="$PWD/$p"
      if [ -x "$p" ] && [ ! -d "$p" ]; then _HP3_W="$p"
      elif [ -x "$p.exe" ] && [ ! -d "$p.exe" ]; then _HP3_W="$p.exe"; fi ;;
    *) _hp3_lookup "$p" ;;
  esac
  return 0
}

# The `timeout` to bound probes with, resolved once per shell. Windows'
# System32\timeout.exe (and its SysWOW64 twin) is a different program (it waits
# for a key press), so nothing under a `windows` directory counts.
_hp3_timeout() {
  if [ -z "${_HP3_TO_SET:-}" ]; then
    _hp3_lookup timeout '*/windows/*'
    _HP3_TO="$_HP3_W"; _HP3_TO_SET=1
  fi
}

# _hp3_probe <require-code> <exe> [arg]: the candidate runs <require-code> and
# prints "3". Judged by STDOUT, not exit status: a stub resolves, prints nothing
# and may exit anything. Returns 0 pass, 1 fail, 2 timed out, 3 not executed (an
# alias with no `timeout` to bound it).
_hp3_probe() {
  local req="$1" exe="$2" arg="${3:-}" out code rc bound
  local -a cmd=("$exe")
  [ -n "$arg" ] && cmd+=("$arg")
  code="import sys
$req
sys.stdout.write(str(sys.version_info[0]))"
  _hp3_timeout
  if [ -n "$_HP3_TO" ]; then
    if _hp3_is_alias "$exe"; then bound="$_HP3_HOOK_PY3_ALIAS_TIMEOUT"; else bound="$_HP3_HOOK_PY3_PROBE_TIMEOUT"; fi
    out="$("$_HP3_TO" -k 2 "$bound" "${cmd[@]}" -c "$code" 2>/dev/null </dev/null)"
    rc=$?
    case "$rc" in 124|137) return 2 ;; esac
  elif _hp3_is_alias "$exe"; then
    return 3
  else
    out="$("${cmd[@]}" -c "$code" 2>/dev/null </dev/null)"
  fi
  [ "$out" = 3 ] && return 0
  return 1
}

# Fill _HP3_C with candidates in preference order, one "exe<TAB>arg<TAB>kind"
# each: every non-alias python3/python in PATH order, then a non-alias `py -3`,
# then the aliases in the same order (the last resort, always bounded). Relative
# PATH entries are skipped: a relative interpreter baked into a function would
# change meaning with the caller's cwd.
_hp3_candidates() {
  _HP3_C=()
  local rest="${PATH}:" d n f arg seen=$'\n'
  local -a real=() launch=() alias=()
  while [ -n "$rest" ]; do
    d="${rest%%:*}"; rest="${rest#*:}"
    _hp3_is_abs "$d" || continue
    for n in python3 python py; do
      f="$d/$n"
      if [ -x "$f" ] && [ ! -d "$f" ]; then :
      elif [ -x "$f.exe" ] && [ ! -d "$f.exe" ]; then f="$f.exe"
      else continue; fi
      case "$seen" in *$'\n'"$f"$'\n'*) continue ;; esac
      seen="$seen$f"$'\n'
      arg=""; [ "$n" = py ] && arg="-3"
      if _hp3_is_alias "$f"; then alias+=("$f"$'\t'"$arg"$'\t'alias)
      elif [ "$n" = py ]; then launch+=("$f"$'\t'"$arg"$'\t'real)
      else real+=("$f"$'\t'"$arg"$'\t'real); fi
    done
  done
  _HP3_C=("${real[@]}" "${launch[@]}" "${alias[@]}")
}

# _hp3_define <exe> <arg> <python3-resolution>: make `python3` run <exe> [arg].
# When <exe> is what the name already resolves to, drop our function instead.
# The path is baked in with %q (printf -v, so no fork on the hot path), and
# nothing later can re-point the function.
_hp3_define() {
  local exe="$1" arg="${2:-}" r="${3:-}" q
  HOOK_PY3_EXE="$exe"; HOOK_PY3_ARG="$arg"
  if [ "$exe" = "$r" ] && [ -z "$arg" ]; then
    [ -n "${_HP3_OURS:-}" ] && unset -f python3
    _HP3_OURS=""
    return 0
  fi
  if [ -n "$arg" ]; then printf -v q '%q %q' "$exe" "$arg"; else printf -v q '%q' "$exe"; fi
  eval "python3() { $q \"\$@\"; }"
  _HP3_OURS=1
}

# _hp3_cache_file <require-code>: the verdict file for this (require, $PYTHON)
# pair, in _HP3_F. A djb2 hash in bash arithmetic: builtins only. The PATH is
# deliberately NOT in the hash (see "One verdict PER PATH" above): it selects a
# RECORD inside the file instead.
#
# The name is `hook-py3-verdict2.<h>`, NOT the hp3v1 file's `hook-py3-verdict.<h>`.
# A box runs both generations side by side for as long as any checkout or
# runner bundle predates this change (the operator box's primary checkout was 66
# commits behind on 2026-10-19), and two generations sharing ONE file would
# each read the other's write as a miss -- the very thrash this format exists
# to end, rebuilt between versions. Separate names let each keep its own.
_hp3_cache_file() {
  local s="$1|${PYTHON:-}" h=5381 i c
  for (( i = 0; i < ${#s}; i++ )); do
    printf -v c '%d' "'${s:i:1}"
    h=$(( (h * 33 + c) & 0x7fffffff ))
  done
  _HP3_F="${HOOK_PY3_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME:-/nonexistent}/.cache}/qontinui}/hook-py3-verdict2.$h"
}

# ── File format hp3v2 ────────────────────────────────────────────────────────
# Line 1 `hp3v2`; then one RECORD PER LINE, most recent first, at most
# $_HP3_SLOTS of them:
#   PATH <US> PYTHON <US> verdict <US> exe <US> arg <US> at <US> end
# where <US> is the ASCII unit separator (0x1f), which no PATH, interpreter path
# or verdict contains. A record is accepted only with all seven fields and the
# literal `end`, so a record cut short by a writer that died mid-`printf` (the
# in-place arm below, which has no rename) is never trusted. Read with ONE
# `mapfile` -- a builtin, a C loop -- and matched with a prefix test, so a hit
# costs the same few syscalls whatever the slot count. The `hp3v1` file (one
# verdict, the pre-Phase-3 format) has a different NAME (see _hp3_cache_file)
# and is never read here; a file under this name without the `hp3v2` header
# reads as a miss and is rewritten.
_HP3_SLOTS=32
_HP3_US=$'\x1f'

# _hp3_cache_load <file>: the file's records in _HP3_RECS (empty on failure);
# 1 when the file is absent, unreadable or not hp3v2.
_hp3_cache_load() {
  _HP3_RECS=()
  [ -r "$1" ] || return 1
  local -a lines=()
  mapfile -t lines < "$1" 2>/dev/null || return 1
  [ "${#lines[@]}" -gt 0 ] && [ "${lines[0]%$'\r'}" = hp3v2 ] || return 1
  _HP3_RECS=("${lines[@]:1}")
  return 0
}

# _hp3_split <record>: its fields in _HP3_M=(PATH PYTHON verdict exe arg at end);
# 1 unless it is well-formed (seven fields, the last the literal `end`).
_hp3_split() {
  local rest="$1"
  _HP3_M=()
  while [[ "$rest" == *"$_HP3_US"* ]]; do
    _HP3_M+=("${rest%%"$_HP3_US"*}"); rest="${rest#*"$_HP3_US"}"
  done
  _HP3_M+=("$rest")
  [ "${#_HP3_M[@]}" -eq 7 ] && [ "${_HP3_M[6]}" = end ]
}

# _hp3_cache_match: the record in _HP3_RECS for THIS $PATH and $PYTHON, split
# into _HP3_M. 1 when there is none or the matching record is malformed.
_hp3_cache_match() {
  local rec
  for rec in "${_HP3_RECS[@]}"; do
    [[ "$rec" == "$PATH$_HP3_US"* ]] || continue
    _hp3_split "$rec" && [ "${_HP3_M[1]}" = "${PYTHON:-}" ] && return 0
    _HP3_M=()
    return 1
  done
  _HP3_M=()
  return 1
}

# _hp3_sweep <dir>: remove temp files older than an hour that killed writers
# left behind (see "One verdict PER PATH"). WRITE PATH ONLY, at most once per
# shell, bounded to <dir> itself, and only the temp-file shape of BOTH
# generations, `hook-py3-verdict.<h>.<pid>` and `hook-py3-verdict2.<h>.<pid>`
# -- the verdict files themselves, `hook-py3-verdict.<h>` and
# `hook-py3-verdict2.<h>`, have one dot after the prefix and do not match. One
# `find` (not Windows' find.exe, a different program); a box without one keeps
# its litter. Never fails the caller.
_hp3_sweep() {
  [ -z "${_HP3_SWEPT:-}" ] || return 0
  _HP3_SWEPT=1
  _hp3_lookup find '*/windows/*'
  [ -n "$_HP3_W" ] || return 0
  "$_HP3_W" "$1" -maxdepth 1 -type f -name 'hook-py3-verdict*.*.[0-9]*' -mmin +60 -delete \
    </dev/null >/dev/null 2>&1 || :
  return 0
}

# _hp3_cache_write <file> <verdict> <exe> <arg> <now>: never fails the caller.
# Puts THIS PATH's record first and keeps every other PATH's well-formed record
# that is younger than the longest TTL, up to $_HP3_SLOTS.
#
# A kept record whose interpreter is NEWER than the file being replaced is
# dropped, and that is what keeps the reader's `exe -nt file` test meaning what
# it meant with one record per file ("not modified since the verdict was
# written"): every rewrite moves the file's mtime forward, so a modification
# between two writes is caught by the next writer, and one after the last write
# by the reader.
#
# Written beside the target and renamed over it where `mv` exists, so a reader
# never sees half a file. Without `mv` it is written in place; a reader rejects
# a record that lacks its closing `end` field either way. Two writers racing can
# drop each other's newest record; the loser's PATH then misses once more and
# writes it back.
_hp3_cache_write() {
  # BASHPID, not $$: the hook dispatcher runs block guards concurrently as
  # subshells of one shell, where $$ is shared and two writers would share t.
  local f="$1" t="$1.${BASHPID:-$$}" rec at max="$_HP3_HOOK_PY3_CACHE_TTL" n=1
  local -a keep=()
  { [ -d "${f%/*}" ] || mkdir -p "${f%/*}"; } 2>/dev/null || return 0
  [ "$_HP3_HOOK_PY3_NEG_TTL" -gt "$max" ] && max="$_HP3_HOOK_PY3_NEG_TTL"
  [ "$_HP3_HOOK_PY3_TIMEOUT_TTL" -gt "$max" ] && max="$_HP3_HOOK_PY3_TIMEOUT_TTL"
  _hp3_cache_load "$f" || _HP3_RECS=()
  for rec in "${_HP3_RECS[@]}"; do
    [ "$n" -lt "$_HP3_SLOTS" ] || break
    _hp3_split "$rec" || continue
    [ "${_HP3_M[0]}" = "$PATH" ] && continue
    at="${_HP3_M[5]}"
    case "$at" in ''|*[!0-9]*) continue ;; esac
    [ $(( $5 - at )) -lt "$max" ] || continue
    [ -n "${_HP3_M[3]}" ] && [ "${_HP3_M[3]}" -nt "$f" ] && continue
    keep+=("$rec"); n=$(( n + 1 ))
  done
  rec="$PATH$_HP3_US${PYTHON:-}$_HP3_US$2$_HP3_US$3$_HP3_US$4$_HP3_US$5${_HP3_US}end"
  if command -v mv >/dev/null 2>&1; then
    { { printf '%s\n' hp3v2 "$rec" "${keep[@]}" > "$t" &&
        mv -f "$t" "$f"; } || rm -f "$t"; } 2>/dev/null
  else
    { printf '%s\n' hp3v2 "$rec" "${keep[@]}" > "$f"; } 2>/dev/null
  fi
  _hp3_sweep "${f%/*}"
  return 0
}

hook_shim_python3() {
  local req="" r cand now verdict exe arg kind at f="" prc
  local real_timed_out=0 alias_timed_out=0 provisional=0 force=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --require) req="${2:-}"; [ $# -ge 2 ] && shift; shift ;;
      --force) force=1; shift ;;
      *) shift ;;
    esac
  done
  _hp3_int HOOK_PY3_PROBE_TIMEOUT 8 nonzero
  _hp3_int HOOK_PY3_ALIAS_TIMEOUT 3 nonzero
  _hp3_int HOOK_PY3_CACHE_TTL 86400
  _hp3_int HOOK_PY3_NEG_TTL 3600
  _hp3_int HOOK_PY3_TIMEOUT_TTL 300

  _hp3_lookup python3; r="$_HP3_W"

  if [ "$force" = 0 ] && [ -z "$req" ] && [ -z "${PYTHON:-}" ]; then
    # Fast path, zero processes: a python3 outside WindowsApps, or one this lib
    # already shimmed in this shell.
    [ -n "${_HP3_OURS:-}" ] && declare -F python3 >/dev/null 2>&1 && return 0
    if [ -n "$r" ] && ! _hp3_is_alias "$r"; then
      HOOK_PY3_EXE="$r"; HOOK_PY3_ARG=""
      return 0
    fi
  fi

  # A negative verdict already reached in THIS shell for the same question is
  # answered again for free, so a caller that re-calls for the verdict after
  # sourcing never pays the probe bound twice (with the disk cache off or
  # unwritable, a hanging alias would otherwise cost it once per call).
  [ "$force" = 0 ] && [ "${_HP3_NEG:-}" = "$req"$'\x1f'"$PATH"$'\x1f'"${PYTHON:-}" ] && return 1

  # The cache, validated with builtins and never by executing anything.
  printf -v now '%(%s)T' -1
  # A relative PATH-shaped $PYTHON resolves against $PWD, which the key does not
  # carry, so it is never cached. A bare name resolves through PATH, which the
  # key does carry, so it is.
  local nocache=0
  case "${PYTHON:-}" in */*|*\\*) _hp3_is_abs "$PYTHON" || nocache=1 ;; esac
  if [ "${HOOK_PY3_NO_CACHE:-0}" != 1 ] && [ "$nocache" = 0 ]; then
    _hp3_cache_file "$req"; f="$_HP3_F"
    # --force never TRUSTS a cached verdict: it exists because a verdict was
    # just measured wrong. It still WRITES the fresh one.
    if [ "$force" = 0 ] && _hp3_cache_load "$f" && _hp3_cache_match; then
      verdict="${_HP3_M[2]}"; exe="${_HP3_M[3]}"; arg="${_HP3_M[4]}"; at="${_HP3_M[5]}"
      case "$at" in ''|*[!0-9]*) at=0 ;; esac
      if [ "$verdict" = OK ] && [ -x "$exe" ] && [ ! -d "$exe" ] && [ ! "$exe" -nt "$f" ] &&
         [ $(( now - at )) -lt "$_HP3_HOOK_PY3_CACHE_TTL" ]; then
        _hp3_define "$exe" "$arg" "$r"
        return 0
      fi
      if [ "$verdict" = NONE ] && [ $(( now - at )) -lt "$_HP3_HOOK_PY3_NEG_TTL" ]; then
        return 1
      fi
      if [ "$verdict" = TIMEOUT ] && [ $(( now - at )) -lt "$_HP3_HOOK_PY3_TIMEOUT_TTL" ]; then
        return 1
      fi
    fi
  fi

  # Cache-only: a miss is answered now, with no probe and no verdict recorded.
  [ "${HOOK_PY3_CACHE_ONLY:-0}" = 1 ] && return 1

  if [ -n "${PYTHON:-}" ]; then
    _hp3_path "$PYTHON"
    _HP3_C=()
    if [ -n "$_HP3_W" ]; then
      kind=real; _hp3_is_alias "$_HP3_W" && kind=alias
      _HP3_C=("$_HP3_W"$'\t'$'\t'"$kind")
    fi
  else
    _hp3_candidates
  fi

  verdict=NONE; exe=""; arg=""
  for cand in "${_HP3_C[@]}"; do
    kind="${cand##*$'\t'}"; cand="${cand%$'\t'*}"
    if [ "$kind" = alias ]; then
      # One hung alias predicts the rest (the Install Manager installs python3,
      # python and py together), and the harness may kill this hook mid-probe:
      # record TIMEOUT first, so a killed probe still leaves a verdict behind.
      [ "$alias_timed_out" = 1 ] && continue
      if [ -n "$f" ] && [ "$provisional" = 0 ] && [ "$real_timed_out" = 0 ]; then
        _hp3_cache_write "$f" TIMEOUT "" "" "$now"; provisional=1
      fi
    fi
    _hp3_probe "$req" "${cand%%$'\t'*}" "${cand#*$'\t'}"; prc=$?
    if [ "$prc" = 0 ]; then
      verdict=OK; exe="${cand%%$'\t'*}"; arg="${cand#*$'\t'}"
      break
    fi
    if [ "$prc" = 2 ]; then
      if [ "$kind" = alias ]; then alias_timed_out=1; else real_timed_out=1; fi
    fi
  done

  # A REAL interpreter that timed out (a cold start on a loaded box) is not
  # evidence that no Python exists: that NONE is kept for this shell only. A
  # NONE that rests on an ALIAS timing out is cached, but as TIMEOUT, whose TTL
  # is minutes rather than an hour.
  if [ -n "$f" ]; then
    if [ "$verdict" = OK ]; then
      _hp3_cache_write "$f" OK "$exe" "$arg" "$now"
    elif [ "$real_timed_out" = 0 ]; then
      if [ "$alias_timed_out" = 1 ]; then verdict=TIMEOUT; fi
      _hp3_cache_write "$f" "$verdict" "" "" "$now"
    fi
  fi

  if [ "$verdict" != OK ]; then
    _HP3_NEG="$req"$'\x1f'"$PATH"$'\x1f'"${PYTHON:-}"
    return 1
  fi
  _hp3_define "$exe" "$arg" "$r"
  return 0
}

# ── jq: bypass a launcher shim ───────────────────────────────────────────────
# A scoop install puts `shims/jq.exe` on PATH: a small launcher that reads
# `shims/jq.shim` (`path = "<real jq.exe>"`, optionally `args = …`) and starts
# the real binary as a CHILD. So every `$(jq …)` in a hook was two processes on
# top of the subshell -- measured 2026-10-19 on the operator box, `x=$(jq -n 1)` is 6
# processes through the shim and 5 through a function naming the target -- and
# every one is a CreateProcess on a box that leaks a kernel Token object for
# about half of them. Defining `jq` as a function that runs the target directly
# removes the launcher and changes nothing else: same binary, same arguments,
# same stdin/stdout, same exit code.
#
# Resolved ONCE PER PATH and memoised in the shell, NOT in a disk cache like the
# python verdict: proving a python costs an interpreter spawn, so its verdict is
# worth a file; this resolution is a builtin PATH walk plus one builtin `read`
# of a ~60-byte .shim file, which is what reading a cache file would cost too.
#
# NOT bypassed, so PATH's `jq` runs exactly as before:
#   - a `jq` with no `<name>.shim` beside it (a real binary, a Linux box, a
#     test's stub -- git-guard-payload-read-bound-test.sh section 9 stages a
#     MISBEHAVING `jq` and must keep getting it);
#   - a .shim that carries `args`: the launcher prepends them, so running the
#     target bare would change the call;
#   - a .shim whose target is gone or not executable: a broken scoop install
#     must keep presenting as the broken launcher it is (rc 1, the measured
#     broken-shim shape git-guard.sh's re-ask exists for), not vanish.
#
# _hp3_unshim <path>: the launcher's target in _HP3_W; 1 if <path> is not a
# bypassable launcher (see above). Builtins only.
_hp3_unshim() {
  local d="${1%/*}" b="${1##*/}" line key val="" have=0
  b="${b%.exe}"; b="${b%.EXE}"
  _HP3_W=""
  [ -r "$d/$b.shim" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    key="${line%%=*}"; key="${key//[[:space:]]/}"
    case "$key" in
      path) val="${line#*=}"; have=1 ;;
      args) return 1 ;;
    esac
  done < "$d/$b.shim"
  [ "$have" = 1 ] || return 1
  val="${val#"${val%%[![:space:]]*}"}"; val="${val%"${val##*[![:space:]]}"}"
  val="${val#\"}"; val="${val%\"}"
  val="${val//\\//}"
  [ -n "$val" ] && [ -x "$val" ] && [ ! -d "$val" ] || return 1
  _HP3_W="$val"
}

# HOOK_JQ_EXE is an OUTPUT, read by callers.
# shellcheck disable=SC2034
hook_shim_jq() {
  [ "${_HJQ_SET:-}" = 1 ] && [ "${_HJQ_PATH:-}" = "$PATH" ] && return 0
  _HJQ_SET=1; _HJQ_PATH="$PATH"
  if [ -n "${_HJQ_OURS:-}" ]; then unset -f jq; _HJQ_OURS=""; fi
  HOOK_JQ_EXE=""
  _hp3_lookup jq
  [ -n "$_HP3_W" ] || return 0
  HOOK_JQ_EXE="$_HP3_W"
  _hp3_unshim "$_HP3_W" || return 0
  local q
  printf -v q '%q' "$_HP3_W"
  eval "jq() { $q \"\$@\"; }"
  HOOK_JQ_EXE="$_HP3_W"; _HJQ_OURS=1
  return 0
}

# Sourcing is the whole interface — a hook adds one line and every `python3` in
# it resolves. Callers that want the verdict call the function again.
hook_shim_python3 || :
hook_shim_jq || :
