#!/bin/bash
# Shared helper - SOURCE this, do not execute it.
#
# ascii-only-source
#
# ONE owner, in bash, for expanding the `${NAME}` / `${NAME:-default}`
# environment references a `.mcp.json` may carry in the coord-mcp entry.
#
# -- Why this exists -----------------------------------------------------------
# Plan 2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages.
# The runner writes the in-cwd `.mcp.json` coord-mcp entry with a WORKDIR-KEYED
# env reference that ALWAYS carries a default (K = 16 uppercase hex):
#
#   http:  "Authorization": "Bearer ${QONTINUI_COORD_MCP_NONCE_<K>:-<workdir nonce>}"
#          "X-Coord-Mcp-Proxy-Key": "${QONTINUI_COORD_MCP_NONCE_<K>:-<workdir nonce>}"
#   stdio: "args": [<shim>, "--credential", "${QONTINUI_COORD_MCP_CREDENTIAL_<K>:-<file>}"]
#
# Older docs may carry the bare `${QONTINUI_COORD_MCP_NONCE:-...}`, and older
# still are literal. The expander below is GENERIC over the variable name - it
# hardcodes none of these - so every spelling reads through one code path.
#
# Claude Code expands those itself, from ITS process environment: a runner-
# hosted terminal exports its own terminal-bound nonce, every other process
# falls back to the workdir nonce. A hand-rolled door (coord-revive.sh's L1/L2
# and `call`/`tools`, pr-status.sh, the registry guard, ...) reads the SAME
# file, so it must expand the SAME way from ITS OWN environment - or it sends
# the literal text `Bearer ${QONTINUI_COORD_MCP_NONCE_<K>:-...}` as a bearer, which
# the forwarder 401s and the door then reports as a stale nonce.
#
# -- Semantics (bash `${NAME:-default}`, ALLOWLISTED names, typed refusals) ---
#   ${NAME:-default}  NAME allowlisted, EXPORTED and NON-EMPTY -> its value;
#                     else `default`.
#   ${NAME}           the same lookup; when it misses, REFUSED with
#                     `UNEXPANDED_ENV_REF <NAME>` (exit 4). Claude Code sends an
#                     unset no-default reference LITERALLY; a door here must
#                     never do that, so the refusal is typed rather than silent.
#   no `${`           passed through byte-for-byte (every legacy literal file).
#   `${` with no `}`  passed through literally - it is not a reference.
# A reference is `${`, then everything up to the FIRST `}`; the name is what
# precedes the first `:-` in that body, the default is everything after it.
# That is the same split Claude Code applies, so both readers agree on which
# bytes are a reference. Any number of references per string is expanded, and
# a reference may be embedded (`Bearer ${X:-y}` -> `Bearer <value>`).
#
# -- ALLOWLISTED names only, and why ---------------------------------------------
# Only a name matching
#     ^QONTINUI_COORD_MCP_(NONCE|CREDENTIAL)(_[0-9A-F]{16})?$
# is EVER read from the environment. Any other well-formed name reads as UNSET
# (so `${OTHER:-d}` -> `d`, and an undefaulted `${OTHER}` is refused). The
# parser stays generic; the LOOKUP does not. The reason is exfiltration: the
# sweep doors read EVERY sibling `.mcp.json` under the workspace, including one
# that arrived in a cloned repo, and send the expanded header to that file's
# URL. A generic lookup would turn `Bearer ${GH_TOKEN:-}` in such a file into
# this process's GitHub token, sent wherever the file points. The allowlist is
# exactly the set of names the runner writes, and nothing else is reachable.
#
# EXPORTED only. An unexported shell variable (BASH_VERSION, RANDOM, a caller's
# local) is not part of the environment Claude Code expands from, so it reads
# as unset here too. Detected with `${!name@a}` on bash >= 4.4 (no fork), and
# with `declare -p` on an older bash.
#
# A MALFORMED reference - a name part that is not an identifier, as in
# `${X-<value>}` or `${X:=<value>}` - is refused as
# `UNEXPANDED_ENV_REF <malformed reference>`, never echoing the body: in those
# spellings the body carries the would-be default, and the default IS a nonce.
#
# -- The URL gate: no environment value is ever sent off-box --------------------
# Every door that sends the expanded value to a URL read from the same file
# goes through mcp_expand_env_ref_for_url, which reads the environment ONLY when
# mcp_url_is_loopback accepts that URL, and resolves on the DEFAULT arm
# otherwise. The predicate is strict on purpose: the URL must START with a
# case-insensitive `http://` or `https://`; the authority is what follows, up
# to the first of `/`, `?` or `#`; userinfo (`@`), any character outside
# [A-Za-z0-9.:[]-], and a non-numeric port are all rejected; and the host must
# be exactly `localhost`, `[::1]` or a dotted-quad 127.a.b.c whose octets are
# CANONICAL decimal (`0` or no leading zero) and <= 255 - a leading-zero octet
# such as `127.08.0.1` is not an address to curl/getaddrinfo, so it goes to DNS.
#
# EVERY character class in this file is an explicit ASCII list
# ([0123456789], [ABC...Z]), never a range: under a UTF-8 locale bash's `[0-9]`
# matches U+0661 ARABIC-INDIC DIGIT ONE, and `127.0.0.<U+0661>` reaching the
# `$((10#...))` below would abort the calling shell outright. A range on
# untrusted input is a bug here, not a style choice.
# Anything else - a scheme-less `evil.example/x://127.0.0.1/`, a
# `127.0.0.1.evil.example`, a `x@127.0.0.1` - is NOT loopback. With that gate on
# every URL-bearing door (bash, python and PowerShell each own one predicate,
# all driven by the same case table), an allowlisted environment value only
# ever reaches this box's loopback. Doors with no URL (a mint's answer, the
# shim's --credential path) keep the plain expanders.
#
# -- What it never does ---------------------------------------------------------
# It never prints the value on stderr, and never the default either - the
# default IS a nonce. A refusal names the VARIABLE only. The lookup is an
# indirect expansion, not `printenv`, so it costs no fork on the hot path of an
# L2 sweep (a fork is ~1s on the Windows operator box).
#
#   mcp_env_ref_present <string>          -> 0 iff it contains `${...}`
#   mcp_env_ref_allowlisted <name>        -> 0 iff the name may be read
#   mcp_expand_env_ref_to <var> <string>  -> sets <var>; 0, or 4 + stderr
#   mcp_env_ref_default_to <var> <string> -> the same, DEFAULT arm only (a mint)
#   mcp_url_is_loopback <url>             -> 0 iff the URL is strictly loopback
#   mcp_expand_env_ref_for_url <var> <url> <string>
#                                         -> env arm for a loopback URL, else
#                                            the DEFAULT arm; 0, or 4 + stderr
#   mcp_expand_env_ref <string>           -> the expansion on stdout; 0, or 4
#
# Tested by scripts/mcp-env-ref-test.sh.

MCP_ENV_REF_UNEXPANDED_EXIT=4

# mcp_env_ref_present <string> -> 0 iff the string carries a `${...}` reference.
mcp_env_ref_present() {
  case "$1" in
    *'${'*'}'*) return 0 ;;
    *) return 1 ;;
  esac
}

# mcp_expand_env_ref_to <outvar> <string>   - the ENVIRONMENT arm (bash `:-`)
mcp_expand_env_ref_to() { __mer_expand env "$@"; }

# mcp_env_ref_default_to <outvar> <string>  - the DEFAULT arm: every reference
# resolves as though its variable were unset. For a value this process did not
# read out of its own .mcp.json but received from a MINT (the runner's
# /coord-mcp/provision-session answer): the caller asked for the freshly minted
# WORKDIR nonce, which is the reference's default, and the terminal nonce in this
# process's environment is a different credential it did not ask for. The same
# resolution the runner's own reader chokepoint applies to the workdir key.
mcp_env_ref_default_to() { __mer_expand default "$@"; }

# mcp_url_is_loopback <url> -> 0 iff the URL is strictly this box's loopback
# (see "The URL gate" above for the exact rule).
mcp_url_is_loopback() {
  local __mer_rest __mer_auth __mer_host __mer_port __mer_o
  case "$1" in
    [Hh][Tt][Tt][Pp]://*) __mer_rest="${1:7}" ;;
    [Hh][Tt][Tt][Pp][Ss]://*) __mer_rest="${1:8}" ;;
    *) return 1 ;;
  esac
  __mer_auth="${__mer_rest%%[/?#]*}"
  case "$__mer_auth" in ''|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.:\[\]-]*) return 1 ;; esac
  case "$__mer_auth" in
    '[::1]') return 0 ;;
    '[::1]:'*) __mer_port="${__mer_auth#'[::1]:'}"
               case "$__mer_port" in ''|*[!0123456789]*) return 1 ;; esac
               return 0 ;;
    *'['*|*']'*) return 1 ;;
  esac
  __mer_host="${__mer_auth%%:*}"
  if [ "$__mer_host" != "$__mer_auth" ]; then
    __mer_port="${__mer_auth#*:}"
    case "$__mer_port" in ''|*[!0123456789]*) return 1 ;; esac
  fi
  case "$__mer_host" in
    [Ll][Oo][Cc][Aa][Ll][Hh][Oo][Ss][Tt]) return 0 ;;
  esac
  [[ "$__mer_host" =~ ^127\.(0|[123456789][0123456789]{0,2})\.(0|[123456789][0123456789]{0,2})\.(0|[123456789][0123456789]{0,2})$ ]] || return 1
  # Arithmetic ONLY on octets the explicit-ASCII regex above already accepted:
  # canonical decimal (no leading zero), 1-3 of the ten ASCII digits.
  for __mer_o in "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}"; do
    [ "$((10#$__mer_o))" -le 255 ] || return 1
  done
  return 0
}

# mcp_expand_env_ref_for_url <outvar> <url> <string> - the environment arm for a
# strictly-loopback URL, the DEFAULT arm for every other one.
mcp_expand_env_ref_for_url() {
  if mcp_url_is_loopback "$2"; then
    __mer_expand env "$1" "$3"
  else
    __mer_expand default "$1" "$3"
  fi
}

# mcp_env_ref_allowlisted <name> -> 0 iff the name may be read from the
# environment (see "ALLOWLISTED names only" above).
mcp_env_ref_allowlisted() {
  local __mer_k
  case "$1" in
    QONTINUI_COORD_MCP_NONCE|QONTINUI_COORD_MCP_CREDENTIAL) return 0 ;;
    QONTINUI_COORD_MCP_NONCE_*) __mer_k="${1#QONTINUI_COORD_MCP_NONCE_}" ;;
    QONTINUI_COORD_MCP_CREDENTIAL_*) __mer_k="${1#QONTINUI_COORD_MCP_CREDENTIAL_}" ;;
    *) return 1 ;;
  esac
  [ "${#__mer_k}" -eq 16 ] || return 1
  case "$__mer_k" in *[!0123456789ABCDEF]*) return 1 ;; esac
  return 0
}

# __mer_exported <name> -> 0 iff <name> is an EXPORTED variable of this shell.
# Two implementations, both always defined so the suite can drive the fallback
# on a modern bash; __mer_exported picks by version.
__mer_exported_attr() {
  [ -n "${!1+x}" ] || return 1   # unset: and `${!1@a}` would trip `set -u`
  local __mer_attrs="${!1@a}"
  case "$__mer_attrs" in *x*) return 0 ;; esac
  return 1
}
# The FLAGS WORD is cut out explicitly - the text between `declare -` and the
# first space - and must be followed by ` <name>`, so an `x` inside the VALUE
# (`declare -- NAME="... x NAME ..."`) can never read as the export flag. (A
# glob such as `"declare -"*x*" $1"*` matched exactly that value.)
__mer_exported_declare() {
  local __mer_d __mer_flags
  __mer_d="$(declare -p "$1" 2>/dev/null)" || return 1
  case "$__mer_d" in "declare -"*) ;; *) return 1 ;; esac
  __mer_d="${__mer_d#declare -}"
  __mer_flags="${__mer_d%% *}"
  case "${__mer_d#"$__mer_flags" }" in "$1="*|"$1") ;; *) return 1 ;; esac
  case "$__mer_flags" in *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-]*) return 1 ;; *x*) return 0 ;; esac
  return 1
}
if [ "${BASH_VERSINFO[0]:-0}" -gt 4 ] || { [ "${BASH_VERSINFO[0]:-0}" -eq 4 ] && [ "${BASH_VERSINFO[1]:-0}" -ge 4 ]; }; then
  __mer_exported() { __mer_exported_attr "$1"; }
else
  __mer_exported() { __mer_exported_declare "$1"; }
fi

# __mer_expand <env|default> <outvar> <string>
__mer_expand() {
  local __mer_mode="$1" __mer_out_name="$2" __mer_s="$3" __mer_acc="" __mer_body __mer_name
  local __mer_def __mer_hasdef __mer_val
  case "$__mer_out_name" in
    ''|[0123456789]*|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_]*)
      echo "mcp-env-ref: invalid output variable name '$__mer_out_name' (caller bug)" >&2
      return 2 ;;
  esac
  while :; do
    case "$__mer_s" in
      *'${'*) ;;
      *) __mer_acc="$__mer_acc$__mer_s"; break ;;
    esac
    __mer_acc="$__mer_acc${__mer_s%%\$\{*}"
    __mer_s="${__mer_s#*\$\{}"
    case "$__mer_s" in
      *'}'*) ;;
      *) __mer_acc="$__mer_acc\${$__mer_s"; break ;;  # unterminated: literal
    esac
    __mer_body="${__mer_s%%\}*}"
    __mer_s="${__mer_s#*\}}"
    case "$__mer_body" in
      *:-*) __mer_name="${__mer_body%%:-*}"; __mer_def="${__mer_body#*:-}"; __mer_hasdef=1 ;;
      *)    __mer_name="$__mer_body"; __mer_def=""; __mer_hasdef=0 ;;
    esac
    case "$__mer_name" in
      ''|[0123456789]*|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_]*)
        # MALFORMED: never echo the body - in `${X-v}` / `${X:=v}` it holds a nonce.
        echo "UNEXPANDED_ENV_REF <malformed reference>: the .mcp.json value carries a \${...} whose name part is not an identifier - refusing to send it as a credential (the body is withheld: it may hold a nonce)" >&2
        printf -v "$__mer_out_name" '%s' ""
        return "$MCP_ENV_REF_UNEXPANDED_EXIT" ;;
    esac
    __mer_val=""
    if [ "$__mer_mode" != default ] && mcp_env_ref_allowlisted "$__mer_name" && __mer_exported "$__mer_name"; then
      __mer_val="${!__mer_name-}"
    fi
    if [ -n "$__mer_val" ]; then
      __mer_acc="$__mer_acc$__mer_val"
    elif [ "$__mer_hasdef" = 1 ]; then
      __mer_acc="$__mer_acc$__mer_def"
    else
      if [ "$__mer_mode" = default ]; then
        echo "UNEXPANDED_ENV_REF $__mer_name: the value resolves on its DEFAULT arm (a minted answer, or a door URL that is not loopback) and references \${$__mer_name} with no default - refusing to send the literal reference as a credential (UNKNOWN shape, not a coord verdict)" >&2
        printf -v "$__mer_out_name" '%s' ""
        return "$MCP_ENV_REF_UNEXPANDED_EXIT"
      fi
      echo "UNEXPANDED_ENV_REF $__mer_name: the .mcp.json value references \${$__mer_name} with no default and $__mer_name is unset, empty, unexported or not an allowlisted name in this process's environment - refusing to send the literal reference as a credential (a LOCAL fact about this environment, not a coord verdict)" >&2
      printf -v "$__mer_out_name" '%s' ""
      return "$MCP_ENV_REF_UNEXPANDED_EXIT"
    fi
  done
  printf -v "$__mer_out_name" '%s' "$__mer_acc"
  return 0
}

# mcp_expand_env_ref <string> -> the expansion on stdout (no trailing newline).
mcp_expand_env_ref() {
  local __mer_result
  mcp_expand_env_ref_to __mer_result "$1" || return $?
  printf '%s' "$__mer_result"
}
