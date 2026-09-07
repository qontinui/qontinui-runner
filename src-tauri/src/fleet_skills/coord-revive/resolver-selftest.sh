#!/usr/bin/env bash
# Self-test for the fleet-script resolver -- the block coord-revive.sh and
# pr-status.sh both carry to find their sibling helpers under the config repo's
# `scripts/` directory.
#
# WHY THIS SUITE EXISTS. The resolution rule had FIVE copies (lib/envelope.sh,
# coord-acting-bearer.sh, coord-provision-nonce.sh and lib/guard-decision-log.sh
# here; coord-acting-bearer.sh in pr-status.sh), and when the rule was fixed it
# was fixed in ONE of them. The other four went on refusing from inside an
# ordinary repo checkout -- reporting HELPER_NOT_FOUND, a LOCAL path fault
# rendered as a named CREDENTIAL verdict, in exactly the population that reaches
# for those rungs. Consolidating them onto one resolver is only half the repair;
# this suite is the half that keeps it repaired, because the failure is INVISIBLE
# from inside the config repo, which is the one layout where the original rungs
# always worked.
#
# It runs the REAL block, extracted from the sibling script by its own function
# names rather than retyped, so a suite that passes against a copy nobody ships
# is not a possible outcome. Extraction is itself asserted (case 0): an empty or
# partial lift would make every later case vacuous.
#
# EVERY POSITIVE CASE IS PAIRED WITH A NEGATIVE CONTROL, per this bundle's other
# two suites. A resolver that answered "found" for everything would pass a
# find-it corpus perfectly, and a `set -e` probe that cannot kill anything proves
# nothing about a guard that stops it. So:
#   - the layout corpus carries a case with NO config repo in reach, which must
#     resolve to nothing rather than to a plausible-looking path;
#   - the `set -e` case has a control that STRIPS the guard and asserts the
#     probe then does die -- the first harness written for this defect used an
#     inline `( set -e; ... )` subshell, which does NOT reproduce it in bash 5.2,
#     so it passed against the broken code too;
#   - each message-honesty property is asserted in BOTH of its arms, because a
#     message that always says "searched X" is as wrong as one that never does.
#
# RUNTIME, so a slow run is not mistaken for a hang. The suite is FORK-BOUND:
# each case isolates `$HERE`, `$QONTINUI_ROOT` and the CWD in a subshell, so it
# spends on the order of 250 forks. That is milliseconds of work on a normal CI
# runner and minutes on a loaded developer box -- measured 2026-09-07 on the
# operator box at 319 s wall, against a fork cost of 1098 ms measured the same
# day with 25+ agent sessions live. If this suite appears to hang, time a bare
# `x="$(:)"` loop before suspecting it: the same arithmetic explains both.
#
# HERMETIC. Every case builds its own tree under `mktemp -d`. No network, no
# coord, no runner, and no dependence on the workspace this happens to run in --
# the resolver's last rung reads `git rev-parse` from the CWD, so each case runs
# from its own fixture root to keep the ambient workspace out of the result.
set -u

HERE_SELF="$(cd "$(dirname "$0")" && pwd)"
SUBJECT="${1:-$HERE_SELF/coord-revive.sh}"
PASS=0
FAIL=0
SKIP=0

ok()   { PASS=$((PASS + 1)); printf '  PASS  %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '  FAIL  %s\n' "$1"; }
skip() { SKIP=$((SKIP + 1)); printf '  SKIP  %s  (NOT a pass)\n' "$1"; }

TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

# ---------------------------------------------------------------- case 0
# Lift the block, and prove the lift worked. Anchored on the first global and on
# the first line after the last function, both of which are load-bearing names
# rather than incidental text.
echo "case 0 -- extraction of the real block from $(basename "$SUBJECT")"
INC="$TMPROOT/resolver.inc"
S_LINE="$(grep -n '^__FLEET_SCRIPT_INIT=' "$SUBJECT" | head -1 | cut -d: -f1)"
E_LINE="$(grep -n '^ENVELOPE_READER=\|^# Dependency floor' "$SUBJECT" | head -1 | cut -d: -f1)"
if [ -n "$S_LINE" ] && [ -n "$E_LINE" ] && [ "$E_LINE" -gt "$S_LINE" ]; then
  sed -n "${S_LINE},$((E_LINE - 1))p" "$SUBJECT" > "$INC"
  ok "lifted lines $S_LINE..$((E_LINE - 1))"
else
  bad "could not locate the block in $SUBJECT (start=$S_LINE end=$E_LINE) -- every later case would be vacuous"
  echo; echo "pass=$PASS fail=$FAIL skip=$SKIP"; exit 1
fi
for fn in __fleet_script_init __rfs_try __resolve_fleet_script __fleet_script_searched; do
  if grep -q "^$fn() {" "$INC"; then ok "block defines $fn"; else bad "block does not define $fn"; fi
done

# ---------------------------------------------------------------- fixtures
# The config repo, at a depth NO fixed rung would guess, plus a sibling checkout
# carrying its own REAL .claude/ copy -- the layout the original rungs refuse.
WS="$TMPROOT/ws"
CFG="$WS/qontinui-claude-config"
mkdir -p "$CFG/scripts/lib" "$CFG/.claude/skills/coord-revive"
for f in lib/envelope.sh lib/guard-decision-log.sh coord-acting-bearer.sh coord-provision-nonce.sh; do
  mkdir -p "$(dirname "$CFG/scripts/$f")"; : > "$CFG/scripts/$f"
done
SIB="$WS/qontinui-runner"
mkdir -p "$SIB/.claude/skills/coord-revive"
mkdir -p "$WS/.claude/skills/coord-revive"          # workspace-root .claude, real dir
ORPHAN="$TMPROOT/orphan/.claude/skills/coord-revive"
mkdir -p "$ORPHAN"

# resolve <HERE> <QONTINUI_ROOT|-> <rel> -- echoes $__RFS_PATH, from a CWD with
# no git repo above it so the last rung cannot reach the ambient workspace.
resolve() {
  (
    set -u
    cd "$TMPROOT" || exit 1
    HERE="$1"
    if [ "$2" = "-" ]; then unset QONTINUI_ROOT; else QONTINUI_ROOT="$2"; export QONTINUI_ROOT; fi
    # shellcheck source=/dev/null
    . "$INC"
    __resolve_fleet_script "$3"
    printf '%s' "$__RFS_PATH"
  )
}

# Rung 1 answers with the LITERAL `$HERE/../../../scripts/...` string -- it has
# always done so -- so compare the FILES these paths name, not the spellings.
same_file() { [ -n "$1" ] && [ -n "$2" ] && [ -e "$1" ] && [ -e "$2" ] && [ "$1" -ef "$2" ]; }

layout() { # layout <name> <HERE> <ROOT|-> <expected-path-or-empty>
  local got; got="$(resolve "$2" "$3" lib/envelope.sh)"
  if { [ -z "$4" ] && [ -z "$got" ]; } || same_file "$got" "$4"; then
    ok "$1"
  else
    bad "$1 -- got [$got] want [${4:-<nothing>}]"
  fi
}

# ---------------------------------------------------------------- case 1
# Every layout the resolver claims to serve. The fixed rungs are kept FIRST, so
# the two layouts that resolved before the walk existed must still resolve --
# a regression there is the one this consolidation could plausibly cause.
echo "case 1 -- layouts (resolving scripts/lib/envelope.sh)"
layout "config repo's own .claude, ROOT unset (rung 1)" \
       "$CFG/.claude/skills/coord-revive" - "$CFG/scripts/lib/envelope.sh"
layout "sibling checkout, own real .claude, ROOT unset (the walk)" \
       "$SIB/.claude/skills/coord-revive" - "$CFG/scripts/lib/envelope.sh"
layout "sibling checkout, \$QONTINUI_ROOT set (rung 3)" \
       "$SIB/.claude/skills/coord-revive" "$WS" "$CFG/scripts/lib/envelope.sh"
layout "workspace-root .claude as a real dir, ROOT unset" \
       "$WS/.claude/skills/coord-revive" - "$CFG/scripts/lib/envelope.sh"
# THE NEGATIVE CONTROL for the whole corpus: no config repo above $HERE and no
# git checkout at $PWD. Must resolve to NOTHING -- a resolver that answered here
# would be guessing, and every PASS above would mean nothing.
layout "no config repo in reach -- must resolve to nothing" \
       "$ORPHAN" - ""

# The symlink layout: <workspace-root>/.claude symlinked into the config repo.
# Windows refuses `ln -s` without a privilege or MSYS=winsymlinks:nativestrict,
# and a SKIP is reported as not-a-pass rather than quietly counted as one.
LN="$TMPROOT/ln"
mkdir -p "$LN"
if ln -s "$CFG/.claude" "$LN/.claude" 2>/dev/null && [ -L "$LN/.claude" ]; then
  layout "workspace-root .claude SYMLINKED into the config repo" \
         "$LN/.claude/skills/coord-revive" - "$CFG/scripts/lib/envelope.sh"
else
  skip "workspace-root .claude SYMLINKED into the config repo -- this platform refused ln -s"
fi

# ---------------------------------------------------------------- case 2
# All four helpers resolve from the layout that used to refuse. This is the
# defect itself: fixing envelope.sh alone left the other three refusing.
echo "case 2 -- all four helpers from a sibling checkout, ROOT unset"
for rel in lib/envelope.sh coord-acting-bearer.sh coord-provision-nonce.sh lib/guard-decision-log.sh; do
  got="$(resolve "$SIB/.claude/skills/coord-revive" - "$rel")"
  if same_file "$got" "$CFG/scripts/$rel"; then ok "$rel"; else bad "$rel -- got [${got:-<nothing>}]"; fi
done

# ---------------------------------------------------------------- case 3
# `-f` AND `-r`: a DIRECTORY named like the helper must never be accepted, or
# the caller would `bash` a directory. The positive arm is case 2 above.
echo "case 3 -- a directory named like the helper is not a hit"
DIRTRAP="$TMPROOT/dirtrap"
mkdir -p "$DIRTRAP/.claude/skills/coord-revive" "$DIRTRAP/scripts/coord-acting-bearer.sh"
got="$(resolve "$DIRTRAP/.claude/skills/coord-revive" - coord-acting-bearer.sh)"
if [ -z "$got" ]; then ok "directory rejected, resolved to nothing"; else bad "accepted a directory: [$got]"; fi

# ---------------------------------------------------------------- case 4
# The `set -e` guard, and the control that proves the probe can kill.
# The probe MUST be a real script file run as `bash <file>`: an inline
# `( set -e; ... )` subshell does not reproduce this in bash 5.2, and the first
# harness written for this defect passed against the unguarded code because of it.
echo "case 4 -- errexit survival when \$HERE is unenterable"
seterr_probe() { # seterr_probe <inc> -> prints SURVIVED, or nothing
  local d gone drv
  d="$(mktemp -d)"
  gone="$d/vanished/skills/coord-revive"
  mkdir -p "$gone"
  drv="$d/run.sh"
  {
    echo '#!/usr/bin/env bash'
    echo 'set -euo pipefail'
    printf 'HERE=%s\n' "$gone"
    printf '. %s\n' "$1"
    echo '__fleet_script_init'
    echo 'printf SURVIVED'
  } > "$drv"
  rm -rf "$d/vanished"
  bash "$drv" 2>/dev/null
  rm -rf "$d"
}
if [ "$(seterr_probe "$INC")" = "SURVIVED" ]; then
  ok "init survives an unenterable \$HERE under set -euo pipefail"
else
  bad "errexit killed the script inside __fleet_script_init"
fi
GUARD=' || __FLEET_HERE_PHYS=""'
if [ "$(grep -c -F -- "$GUARD" "$INC")" = "1" ]; then
  # Strip the guard; the probe must now DIE, or it cannot discriminate at all.
  awk -v g="$GUARD" 'BEGIN{n=index("","")} {i=index($0,g); if(i>0 && !done){ $0=substr($0,1,i-1) substr($0,i+length(g)); done=1 } print}' \
      "$INC" > "$INC.control"
  if [ "$(seterr_probe "$INC.control")" = "SURVIVED" ]; then
    bad "CONTROL: with the guard stripped the probe still survived -- it does not discriminate, so the PASS above is meaningless"
  else
    ok "CONTROL: with the guard stripped, errexit DOES kill it"
  fi
else
  bad "CONTROL: could not find exactly one '$GUARD' to strip"
fi

# ---------------------------------------------------------------- case 5
# The refusal message names only rungs that actually emitted a candidate. Both
# arms of both conditional rungs, because a message that always claims a rung is
# exactly as dishonest as one that never does -- and this is the message a reader
# uses to tell "not found" from "did not look".
echo "case 5 -- the refusal message names what it actually searched"
searched() { # searched <cwd> <ROOT|->
  (
    set -u
    cd "$1" || exit 1
    HERE="$SIB/.claude/skills/coord-revive"
    if [ "$2" = "-" ]; then unset QONTINUI_ROOT; else QONTINUI_ROOT="$2"; export QONTINUI_ROOT; fi
    # shellcheck source=/dev/null
    . "$INC"
    __fleet_script_searched "lib/envelope.sh"
  )
}
M="$(searched "$TMPROOT" "$WS")"
case "$M" in
  *"(set"*)   bad "\$QONTINUI_ROOT SET: concatenates the word 'set' with the value" ;;
  *"$WS/qontinui-claude-config/scripts/lib/envelope.sh"*) ok "\$QONTINUI_ROOT SET: names the concrete rung path" ;;
  *)          bad "\$QONTINUI_ROOT SET: unexpected -- $M" ;;
esac
M="$(searched "$TMPROOT" -)"
case "$M" in
  *"rung emitted no candidate: it is UNSET"*) ok "\$QONTINUI_ROOT UNSET: says the rung emitted nothing" ;;
  *) bad "\$QONTINUI_ROOT UNSET: unexpected -- $M" ;;
esac
# The git rung, in a real checkout and outside one. `$TMPROOT` is not a repo;
# make one to get the positive arm, so both are exercised on any platform.
REPO="$TMPROOT/repo"
mkdir -p "$REPO"
if git -C "$REPO" init -q 2>/dev/null; then
  M="$(searched "$REPO" -)"
  case "$M" in
    *", the --git-common-dir workspace root"*) ok "in a git checkout: names the concrete root it searched" ;;
    *) bad "in a git checkout: did not name the git rung -- $M" ;;
  esac
else
  skip "git rung positive arm -- 'git init' unavailable"
fi
M="$(searched "$TMPROOT" -)"
case "$M" in
  *"NOT the --git-common-dir workspace root, which emitted no candidate"*) ok "outside a checkout: says the git rung emitted nothing" ;;
  *", the --git-common-dir workspace root"*) bad "outside a checkout: still claims the git rung was searched" ;;
  *) bad "outside a checkout: unexpected -- $M" ;;
esac

# ---------------------------------------------------------------- case 6
# The walk terminates on every path shape it can be handed. An infinite loop
# here would hang a diagnostic whose whole purpose is answering quickly when
# coord is already down, so this is asserted rather than reasoned about.
echo "case 6 -- the ancestor walk terminates"
for p in /a/b/c /a / C:/foo/bar C: //server/share/x relative/path noSlash /a/b/; do
  if [ -n "$(resolve "$p" - lib/envelope.sh; echo done)" ]; then
    ok "terminates for HERE=$p"
  else
    bad "did not terminate for HERE=$p"
  fi
done

echo
echo "pass=$PASS fail=$FAIL skip=$SKIP"
[ "$FAIL" -eq 0 ]
