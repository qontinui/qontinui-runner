#!/usr/bin/env bash
# The behavioural axis of the published-build parity check: diff the pass/fail
# SETS of two contract-smoke logs (development vs published) and write the
# result to $GITHUB_STEP_SUMMARY. One implementation for both platform legs of
# .github/workflows/published-parity.yml, so the windows and linux numbers are
# computed the same way.
#
# Usage: scripts/behavioural-diff.sh <dev-log> <published-log> [label]
# Writes dev-routes.txt, pub-routes.txt and joined.txt into the current
# directory (the workflow uploads the first two), and the axis verdict for the
# provenance block to parity-report/behavioural-axis.txt, which the workflow's
# stamp step reads (Update-ParityReportBehaviouralAxis, lib/parity-diff.ps1). A leg with no probe lines is
# reported as UNKNOWN and exits 0: a missing measurement is never a clean one.
# Gates nothing; exits non-zero only on a usage error.
set -u
if [ $# -lt 2 ]; then
  echo "usage: $0 <dev-log> <published-log> [label]" >&2
  exit 2
fi
DEV_LOG="$1"
PUB_LOG="$2"
LABEL="${3:+ ($3)}"
: "${GITHUB_STEP_SUMMARY:=/dev/stdout}"

# contract-smoke's Record emits "STATUS  METHOD  PATH  DETAIL" and only
# ever PASS / FAIL / SKIP. A handful of synthetic rows reuse a path
# that a real route also probes (e.g. "/control/elements (probe)"), so
# collapse to ONE row per METHOD+PATH keeping the worst status --
# FAIL > SKIP > PASS. Keeping duplicates would make the join below a
# cross product and invent differences that are not there.
extract() {
  grep -aoE '^(PASS|FAIL|SKIP)[[:space:]]+[A-Z]+[[:space:]]+[^[:space:]]+' "$1" \
    | awk '{
        key = $2 " " $3
        rank = ($1 == "FAIL" ? 3 : ($1 == "SKIP" ? 2 : 1))
        if (!(key in best) || rank > bestrank[key]) { best[key] = $1; bestrank[key] = rank }
      }
      END { for (k in best) print k " " best[k] }' \
    | sort
}

{
  echo ""
  echo "### Behavioural axis — contract smoke, development vs published${LABEL}"
  echo ""
} >> "$GITHUB_STEP_SUMMARY"

dev_ok=1; pub_ok=1
[ -s "$DEV_LOG" ] || dev_ok=0
[ -s "$PUB_LOG" ] || pub_ok=0
extract "$DEV_LOG" > dev-routes.txt 2>/dev/null || true
extract "$PUB_LOG" > pub-routes.txt 2>/dev/null || true
[ -s dev-routes.txt ] || dev_ok=0
[ -s pub-routes.txt ] || pub_ok=0

# The axis verdict for the provenance block. Written on every path that
# reaches an answer; a run that never gets here leaves no file, which the stamp
# reads as unknown(behavioural_step_did_not_report) -- never as observed.
mkdir -p parity-report
if [ "$dev_ok" = "0" ] || [ "$pub_ok" = "0" ]; then
  echo "unknown(no_probe_lines: dev_parsed=$dev_ok published_parsed=$pub_ok)" > parity-report/behavioural-axis.txt
  echo "::warning::Behavioural axis UNKNOWN — one or both contract-smoke legs produced no probe lines (dev=$dev_ok published=$pub_ok). This is NOT a statement that the two builds behave the same; the measurement did not happen."
  {
    echo "**UNKNOWN** — one or both legs produced no probe lines (dev parsed: \`$dev_ok\`, published parsed: \`$pub_ok\`)."
    echo ""
    echo "This is not evidence of agreement. A leg that was never measured is unknown, never clean."
  } >> "$GITHUB_STEP_SUMMARY"
  exit 0
fi

join -j 1 -o 0,1.2,2.2 \
  <(awk '{print $1"|"$2" "$3}' dev-routes.txt | sort) \
  <(awk '{print $1"|"$2" "$3}' pub-routes.txt | sort) > joined.txt || true

differing=$(awk '$2 != $3 {print}' joined.txt | wc -l | tr -d ' ')
only_dev=$(comm -23 <(cut -d' ' -f1,2 dev-routes.txt | sort -u) <(cut -d' ' -f1,2 pub-routes.txt | sort -u) | wc -l | tr -d ' ')
only_pub=$(comm -13 <(cut -d' ' -f1,2 dev-routes.txt | sort -u) <(cut -d' ' -f1,2 pub-routes.txt | sort -u) | wc -l | tr -d ' ')

{
  echo "Routes whose outcome differs between the two builds: **${differing}**"
  echo ""
  echo "Probed only on the development leg: **${only_dev}** · only on the published leg: **${only_pub}**"
  echo ""
} >> "$GITHUB_STEP_SUMMARY"

if [ "$differing" != "0" ]; then
  {
    echo "| Route | Development build | Published build |"
    echo "|---|---|---|"
    awk '$2 != $3 {gsub(/\|/, " ", $1); print "| `" $1 "` | " $2 " | " $3 " |"}' joined.txt
    echo ""
  } >> "$GITHUB_STEP_SUMMARY"
  awk '$2 != $3 {print "::warning::Behavioural parity difference - " $1 ": development build " $2 ", published build " $3}' joined.txt
fi

{
  echo "Where this axis and the capability-manifest axis disagree, the disagreement is a **finding about the roster** — the manifest can only see capabilities somebody enumerated in \`CAPABILITY_SPECS\`, and this axis is what notices the ones nobody did. It is not reconciled toward either side."
  echo ""
  echo "_This report gates nothing._"
} >> "$GITHUB_STEP_SUMMARY"

# LAST, deliberately: `observed` is a claim that the comparison above
# completed. Written any earlier, a failure between it and here would leave
# `observed` over a diff that never finished.
echo "observed" > parity-report/behavioural-axis.txt
