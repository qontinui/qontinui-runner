#!/bin/bash
# Shared helper - SOURCE this, do not execute it.
#
# The bash twin of scripts/lib/envelope.py: typed envelope reads for every
# coord / qontinui-web / runner response a shell script parses.
#
#   envelope_require       <door> <dotted.path>          <file|->   -> the value as JSON
#   envelope_first_present <door> <path,path,...>        <file|->   -> the first NON-EMPTY STRING among the paths, bare
#   envelope_collection    <door> <collection.key>       <file|->   -> the rows as a JSON array, iff sibling `count` == length
#   envelope_mcp_body      <door>                        <file|->   -> an MCP tools/call result's BODY (content[0].text decoded)
#
# envelope_mcp_body is the one that reads `coord-revive.sh call` stdout, whose
# body is a JSON STRING one decode below the result. It composes:
#   envelope_mcp_body coord_memory_search "$R" | envelope_require coord_memory_search hits -
# (`hits` with envelope_require: coord_memory_search carries no `count`, so
# envelope_collection would answer UNKNOWN on every call - envelope.py's table.)
#
# Stdout carries a value or NOTHING. An absent key, a `count` that disagrees
# with the rows, a body that is not JSON: `UNKNOWN: <door>: ...` on stderr,
# exit 3 (ENVELOPE_UNKNOWN_EXIT), nothing on stdout - so a caller that
# ignores the status still cannot capture an empty string as a value. The
# key-name table (which door answers which shape) lives in envelope.py's
# docstring; this file carries none of it.
#
# ── Two arms, one reader picked up front ─────────────────────────────────────
# `jq` is ABSENT on the Windows operator box (coord-revive.sh, "jq is NOT
# guaranteed to exist"), and `command -v python` proves a NAME resolves, not
# that it WORKS (the App Execution Alias stubs). So the reader is chosen once,
# with the same smoke test coord-revive.sh runs, into ENVELOPE_READER:
#   jq          - the jq arm below
#   python[3]   - the python arm: `python envelope.py require ...`
#   ""          - neither: every call prints a LOCAL fault on stderr and exits
#                 127. Never a coord verdict, never an empty value.
# A caller that already chose a reader (coord-revive.sh's JSON_READER) may
# export ENVELOPE_READER before sourcing and it is honoured unchanged.
#
# ── The path boundary, honoured rather than re-solved ────────────────────────
# Bodies are fed on STDIN: bash opens the file (`< "$file"`), so no POSIX path
# ever crosses to a NATIVE jq.exe / python.exe (coord-revive.sh read_cfg, the
# MSYS_NO_PATHCONV=1 note). The one path that MUST cross - envelope.py's own
# location, for the python arm - goes through native_path_m, the boundary's
# single owner (scripts/lib/native-path.sh).

ENVELOPE_UNKNOWN_EXIT=3

# shellcheck disable=SC2034  # read by callers that want the script location
ENVELOPE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENVELOPE_PY_POSIX="$ENVELOPE_LIB_DIR/envelope.py"

if [ -r "$ENVELOPE_LIB_DIR/native-path.sh" ]; then
  # shellcheck source=native-path.sh
  . "$ENVELOPE_LIB_DIR/native-path.sh"
  ENVELOPE_PY="$(native_path_m "$ENVELOPE_PY_POSIX")"
else
  ENVELOPE_PY="$ENVELOPE_PY_POSIX"
fi

if [ -z "${ENVELOPE_READER:-}" ]; then
  ENVELOPE_READER=""
  if command -v jq >/dev/null 2>&1; then
    ENVELOPE_READER=jq
  else
    for __env_c in python python3; do
      if command -v "$__env_c" >/dev/null 2>&1 \
         && [ "$("$__env_c" -c 'import json;print(1)' </dev/null 2>/dev/null | tr -d '\r\n')" = "1" ]; then
        ENVELOPE_READER="$__env_c"; break
      fi
    done
    unset __env_c
  fi
fi

# _envelope_no_reader: the LOCAL fault, typed and loud. Exit 127, not 3: a
# missing binary is not an absent key, and the two have different fixes.
_envelope_no_reader() {
  echo "UNKNOWN: $1: neither jq nor a working python can read JSON (LOCAL fault, not a door verdict)" >&2
  return 127
}

# _envelope_python_missing: the python arm was chosen but envelope.py is gone.
_envelope_python_missing() {
  echo "UNKNOWN: $1: scripts/lib/envelope.py not found beside envelope.sh at $ENVELOPE_PY_POSIX (LOCAL fault)" >&2
  return 127
}

# _envelope_feed <file|-> <cmd...>: run the reader with the body on stdin.
# `-` (or an empty file argument) means the caller's own stdin. Bash opens the
# file, so the reader never sees a path.
_envelope_feed() {
  local file="$1"; shift
  if [ -z "$file" ] || [ "$file" = "-" ]; then
    "$@"
  else
    "$@" < "$file"
  fi
}

# _envelope_jq <door> <file|-> <jq args...>: the jq arm. jq's own exit codes
# are folded onto the contract: 3 (our halt_error) passes through, 0 passes
# through, and anything else - a body that is not JSON is exit 2 or 5 - is
# ALSO an UNKNOWN, typed on stderr, exit 3. Never a bare parse error the
# caller reads as "no value".
#
# An EMPTY body is the one input jq accepts silently - no value, exit 0 - and
# it is exactly what the right-hand side of a pipe sees when the left-hand
# reader failed (`envelope_mcp_body ... | envelope_collection ... -`). Every
# success here prints at least one character (`null`, `[]`, a non-empty
# string), so empty output with exit 0 can only mean no input: UNKNOWN, exit 3,
# the same answer the python arm gives.
#
# The capture keeps the value BYTE-EXACT: jq always ends its output with a
# newline, and `$(...)` strips every trailing newline - a raw string value that
# itself ends in one would lose it. A sentinel after jq's output survives the
# strip and carries the exit status; jq's final newline is then the only one
# removed, and put back on print.
#
# `-s` with a one-document guard: jq would otherwise read `{}{}` as TWO values
# and answer for each, where the python arm (json.loads) refuses the body. So
# both arms now say the same thing: exactly one JSON document, else UNKNOWN -
# and an empty body (zero documents) is caught here too.
_envelope_jq() {
  local door="$1" file="$2" rc out filter; shift 2
  filter="${*: -1}"; set -- "${@:1:$#-1}"
  filter='if length != 1 then ("UNKNOWN: \($door): body is \(length) JSON document(s), not one; source=\($source)\n") | halt_error(3) else .[0] | ('"$filter"') end'
  out="$(_envelope_feed "$file" jq -s "$@" "$filter"; printf 'x%s' "$?")"
  rc="${out##*x}"; out="${out%x*}"
  case "$rc" in
    0)
      if [ -z "$out" ]; then
        echo "UNKNOWN: $door: empty body (no JSON value on input); source=${file:--}; nothing on stdout" >&2
        return 3
      fi
      printf '%s' "$out"; return 0 ;;
    3) return 3 ;;
    *) echo "UNKNOWN: $door: body is not JSON (jq exit $rc); source=${file:--}; nothing on stdout" >&2; return 3 ;;
  esac
}

# jq: walk $path with `has()` so a key holding null is PRESENT (only absence
# is UNKNOWN), naming the keys present at the level the walk stopped.
_ENVELOPE_JQ_WALK='
def walk_path($door; $path):
  if $path == "." or $path == "" then . else
  ($path | split(".")) as $segs
  | . as $root
  | reduce range(0; $segs | length) as $i ({cur: $root, at: []};
      ($segs[$i]) as $seg
      | if (.cur | type) == "object" and (.cur | has($seg)) then
          {cur: .cur[$seg], at: (.at + [$seg])}
        elif (.cur | type) == "array" and ($seg | test("^[0-9]+$")) and (($seg | tonumber) < (.cur | length)) then
          {cur: .cur[$seg | tonumber], at: (.at + [$seg])}
        else
          ("UNKNOWN: \($door): key `\($path)` absent; present: ["
            + (if (.cur | type) == "object" then (.cur | keys | join(", "))
               elif (.cur | type) == "array" then "<list of \(.cur | length)>"
               else "<\(.cur | type)>" end)
            + "]; source=\($source) at \(now | todate)"
            + (if (.at | length) > 0 then "; stopped at `\(.at | join("."))`" else "" end)
            + "\n") | halt_error(3)
        end)
  | .cur end;
'

envelope_require() {
  local door="$1" path="$2" file="${3:--}"
  case "$ENVELOPE_READER" in
    jq)
      _envelope_jq "$door" "$file" -c --arg door "$door" --arg path "$path" --arg source "${file:--}" \
        "$_ENVELOPE_JQ_WALK"'walk_path($door; $path)' ;;
    "") _envelope_no_reader "$door" ;;
    *)
      [ -r "$ENVELOPE_PY_POSIX" ] || { _envelope_python_missing "$door"; return $?; }
      _envelope_feed "$file" "$ENVELOPE_READER" "$ENVELOPE_PY" require --door "$door" --key "$path" --source "${file:--}" ;;
  esac
}

# envelope_first_present: the first path that is present AND a non-empty
# string, printed BARE (jq -r). The mint read: the paths are
# `data.value,data.result.value,data` - live eval shape first, boxed second,
# the invoke door's bare-string `data` last - and the non-empty-string
# predicate is what stops the always-present `data` OBJECT from winning.
envelope_first_present() {
  local door="$1" paths="$2" file="${3:--}"
  case "$ENVELOPE_READER" in
    jq)
      _envelope_jq "$door" "$file" -r --arg door "$door" --arg paths "$paths" --arg source "${file:--}" '
        ($paths | split(",")) as $ps
        | . as $root
        | [ $ps[] | . as $p
            # Same segment rule as walk_path (and envelope.py `_walk`): `.` or
            # "" is the root; an object key wins over an array index.
            | (if $p == "." or $p == "" then $root
               else reduce ($p | split("."))[] as $seg ($root;
                 if . == null then null
                 elif type == "object" then (if has($seg) then .[$seg] else null end)
                 elif type == "array" and ($seg | test("^[0-9]+$")) then .[$seg | tonumber]
                 else null end)
               end)
            | select(type == "string" and test("\\S")) ]
        | if length > 0 then .[0]
          else ("UNKNOWN: \($door): key `\($ps | join(" | "))` absent; present: ["
                + (if ($root | type) == "object" then ($root | keys | join(", ")) else "<\($root | type)>" end)
                + "]; source=\($source) at \(now | todate); tried: "
                + ($ps | map("`" + . + "` absent or rejected") | join("; ")) + "\n") | halt_error(3)
          end' ;;
    "") _envelope_no_reader "$door" ;;
    *)
      [ -r "$ENVELOPE_PY_POSIX" ] || { _envelope_python_missing "$door"; return $?; }
      _envelope_feed "$file" "$ENVELOPE_READER" "$ENVELOPE_PY" require --door "$door" --first-of "$paths" \
        --accept non-empty-str --raw --source "${file:--}" ;;
  esac
}

# envelope_collection: the rows under <key> iff the sibling `count` is present
# and equals their length. Absent `count` is UNKNOWN (a backend predating the
# phase that added it), a mismatch is UNKNOWN naming both numbers.
envelope_collection() {
  local door="$1" key="$2" file="${3:--}"
  case "$ENVELOPE_READER" in
    jq)
      _envelope_jq "$door" "$file" -c --arg door "$door" --arg key "$key" --arg source "${file:--}" \
        "$_ENVELOPE_JQ_WALK"'
        . as $root
        | ($key | split(".")) as $kp
        | (if ($kp | length) > 1 then ($kp[:-1] | join(".") + ".count") else "count" end) as $cpath
        | ($root | walk_path($door; $key)) as $rows
        | ($root | walk_path($door; $cpath)) as $count
        | if ($rows | type) != "array" then
            ("UNKNOWN: \($door): key `\($key)` absent; present: [<\($rows | type)>]; source=\($source) at \(now | todate); `\($key)` is not a list\n") | halt_error(3)
          elif ($count | type) != "number" or ($count | floor) != $count then
            ("UNKNOWN: \($door): key `\($cpath)` absent; present: [<\($count | type)>]; source=\($source) at \(now | todate); `\($cpath)` is not an int\n") | halt_error(3)
          elif $count != ($rows | length) then
            ("UNKNOWN: \($door): key `\($cpath)` absent; present: [" + ($root | keys | join(", ")) + "]; source=\($source) at \(now | todate); `\($cpath)`=\($count) but `\($key)` holds \($rows | length) row(s) - the page is not self-consistent\n") | halt_error(3)
          else $rows end' ;;
    "") _envelope_no_reader "$door" ;;
    *)
      [ -r "$ENVELOPE_PY_POSIX" ] || { _envelope_python_missing "$door"; return $?; }
      _envelope_feed "$file" "$ENVELOPE_READER" "$ENVELOPE_PY" require --door "$door" --collection "$key" --source "${file:--}" ;;
  esac
}

# envelope_mcp_body: the tool's own body out of an MCP `tools/call` result -
# what `coord-revive.sh call` prints. A full JSON-RPC envelope descends into
# `result` (an `error` is UNKNOWN); `isError: true` is UNKNOWN naming the
# tool's error text; `content[0].text` must be a string holding JSON; an object
# with no `content` falls through unchanged. Same contract as the rest: the
# body JSON on stdout, or NOTHING + exit 3 + `UNKNOWN:` on stderr.
envelope_mcp_body() {
  local door="$1" file="${2:--}"
  case "$ENVELOPE_READER" in
    jq)
      _envelope_jq "$door" "$file" -c --arg door "$door" --arg source "${file:--}" '
        def fail($m): ("UNKNOWN: \($door): " + $m + "; source=\($source) at \(now | todate)\n") | halt_error(3);
        (if type == "object" and has("jsonrpc") then
           (if has("error") then fail("key `error` present; the JSON-RPC call failed: \(.error | tojson | .[0:300])")
            elif has("result") then .result
            else fail("key `result` absent; present: [" + (keys | join(", ")) + "]") end)
         else . end)
        | if type != "object" then .
          elif has("isError") and (.isError as $e | [false, null, 0, "", "false", "False"] | any(. == $e) | not) then
            fail("key `isError` is true; present: [" + (keys | join(", ")) + "]; the tool answered an error, not a body: "
                 + ((if (.content | type) == "array" and (.content | length) > 0 and (.content[0] | type) == "object"
                     then (.content[0].text // "") else "" end) | tostring | .[0:300]))
          elif has("content") | not then .
          elif (.content | type) == "array" and (.content | length) > 1 then
            fail("key `content` holds more than one item; present: [" + (keys | join(", ")) + "]; an MCP result carrying \(.content | length) content items; exactly one is read")
          elif (.content | type) == "array" and (.content | length) > 0 and (.content[0] | type) == "object"
               and (.content[0].text | type) == "string" then
            (.content[0].text as $t | try ($t | fromjson)
               catch fail("key `content.0.text` not JSON; first 120 chars: \($t | .[0:120] | tojson)"))
          else fail("key `content.0.text` absent or not a string; present: [" + (keys | join(", ")) + "]") end' ;;
    "") _envelope_no_reader "$door" ;;
    *)
      [ -r "$ENVELOPE_PY_POSIX" ] || { _envelope_python_missing "$door"; return $?; }
      _envelope_feed "$file" "$ENVELOPE_READER" "$ENVELOPE_PY" require --door "$door" --mcp-text --key . --source "${file:--}" ;;
  esac
}
