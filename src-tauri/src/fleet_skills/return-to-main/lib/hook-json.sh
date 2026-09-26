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
#   HOOK_PY3_CACHE_TTL      s, default 86400
#   HOOK_PY3_NEG_TTL        s, default 3600: a DEFINITE "no working Python"
#   HOOK_PY3_TIMEOUT_TTL    s, default 300: a verdict that rests on a probe that
#                           TIMED OUT (or was killed mid-probe). A slow cold start
#                           on a loaded box is not proof that no Python exists,
#                           so it is written off for minutes, not an hour.
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
# pair, in _HP3_F. A djb2 hash in bash arithmetic: builtins only.
_hp3_cache_file() {
  local s="$1|${PYTHON:-}" h=5381 i c
  for (( i = 0; i < ${#s}; i++ )); do
    printf -v c '%d' "'${s:i:1}"
    h=$(( (h * 33 + c) & 0x7fffffff ))
  done
  _HP3_F="${HOOK_PY3_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME:-/nonexistent}/.cache}/qontinui}/hook-py3-verdict.$h"
}

# _hp3_cache_write <file> <verdict> <exe> <arg> <now>: never fails the caller.
# Written beside the target and renamed over it where `mv` exists, so a reader
# never sees half a file. Without `mv` it is written in place; a reader rejects
# any file that lacks its closing `end` line either way.
_hp3_cache_write() {
  local f="$1" t="$1.$$"
  { [ -d "${f%/*}" ] || mkdir -p "${f%/*}"; } 2>/dev/null || return 0
  if command -v mv >/dev/null 2>&1; then
    { { printf '%s\n' hp3v1 "$PATH" "${PYTHON:-}" "$2" "$3" "$4" "$5" end > "$t" &&
        mv -f "$t" "$f"; } || rm -f "$t"; } 2>/dev/null
  else
    { printf '%s\n' hp3v1 "$PATH" "${PYTHON:-}" "$2" "$3" "$4" "$5" end > "$f"; } 2>/dev/null
  fi
  return 0
}

hook_shim_python3() {
  local req="" r cand now v p py verdict exe arg kind at end f="" prc
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
    if [ "$force" = 0 ] && [ -r "$f" ] &&
       { IFS= read -r v; IFS= read -r p; IFS= read -r py; IFS= read -r verdict
         IFS= read -r exe; IFS= read -r arg; IFS= read -r at; IFS= read -r end; } < "$f" 2>/dev/null &&
       [ "$v" = hp3v1 ] && [ "$end" = end ] && [ "$p" = "$PATH" ] &&
       [ "$py" = "${PYTHON:-}" ]; then
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

# Sourcing is the whole interface — a hook adds one line and every `python3` in
# it resolves. Callers that want the verdict call the function again.
hook_shim_python3 || :
