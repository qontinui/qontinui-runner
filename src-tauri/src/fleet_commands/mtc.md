# /mtc — Manual Test Coord (easy launcher)

A thin, memorable launcher for `/manual-test-coord` against **staging**. It removes the two things you'd otherwise have to remember and keep in sync across machines: the **rendezvous key** and **which `--operator` role this machine is**. Just run `/mtc` on each machine — they self-pair.

**Run `/mtc` on both machines within the same ~5-minute window.** No arguments needed.

## What it does (and why you don't need a shared key)

1. **Auto-detects this machine's role** from `~/.qontinui/machine.json`:
   - **MSI** (`84c02292-32cb-4983-be85-d00f868b7003`) → `--operator=secondary` (logs in as the distinct-tenant operator-2 = `tester2`, via `QONTINUI_OPERATOR2_*`). **MSI is always secondary.**
   - **anything else** (spaceship `c79a07d5-7e40-49b4-87fa-554c749f9644`, or unknown) → `--operator=primary` (josh).
2. **Auto-derives the rendezvous slug from the wall clock** — a 5-minute **UTC** time-bucket — so both machines compute the **same** slug independently. You never type or copy a key. (You only ever run one test per machine at a time, so any *concurrent* sibling claim is the pair; the rotating time-bucket just prevents a stale ≤2 h-old claim from matching a fresh run.)
3. **Auto-starts at the next 5-minute boundary** (`:00 / :05 / :10 / …`) by default, so both machines fire aligned on the same bucket. `--now` skips the wait and uses the *current* 5-min floor as the slug.
4. Invokes `/manual-test-coord --target=staging --rendezvous-slug=<slug> --operator=<role>` via the `Skill` tool. That skill does the rest (role-based credentials, pairing, Phase 6 cross-tenant isolation, cleanup).

## Args (all optional)

| Flag | Effect |
|---|---|
| _(none)_ | Wait to the next 5-min boundary, auto-role, auto-slug, run. |
| `--now` | Don't wait — start immediately using `floor(now, 5min)` as the slug. Use when both machines are already launched in the same 5-min bucket. |
| `--solo` | Single-machine: pass **no** rendezvous slug to the skill → Phase 6 SETUP_GAPs by design (no sibling expected). |
| _other tokens_ | Forwarded verbatim to `/manual-test-coord` (e.g. a Phase 7 focus hint). |

Recurring runs (e.g. every 5 min unattended) are out of scope here — use `/loop` or `/schedule` to wrap `/mtc` if you want that.

## Steps (execute these, then hand off to the skill)

### 1. Resolve role from machine identity
```bash
MACHINE_ID="${QONTINUI_MACHINE_ID:-$(python -c "
import json, os
try:
    d = json.load(open(os.path.expanduser('~/.qontinui/machine.json')))
    print(d.get('machine_id') or d.get('device_id') or '', end='')
except Exception:
    print('', end='')
")}"
MSI_ID="84c02292-32cb-4983-be85-d00f868b7003"
if [ "$MACHINE_ID" = "$MSI_ID" ]; then ROLE="secondary"; else ROLE="primary"; fi
echo "machine_id=${MACHINE_ID:-<unknown>} → --operator=$ROLE"
```
If `MACHINE_ID` is empty, default `ROLE=primary` and note it in the report (don't abort — a missing machine.json shouldn't block a primary run).

### 2. Derive the rendezvous slug (UTC 5-min bucket) and optionally wait
```bash
# WAIT=1 by default; WAIT=0 if --now was passed. SOLO=1 if --solo.
NOW=$(date -u +%s)
if [ "${WAIT:-1}" = "1" ]; then
  BUCKET=$(( (NOW/300 + 1) * 300 ))   # next 5-min boundary
else
  BUCKET=$(( (NOW/300) * 300 ))       # current 5-min floor
fi
# Format the bucket as a UTC ISO minute (identical on both machines).
SLUG=$(date -u -d "@$BUCKET" +%Y-%m-%dT%H:%M 2>/dev/null || date -u -r "$BUCKET" +%Y-%m-%dT%H:%M)
echo "rendezvous slug=$SLUG (UTC bucket) | local now=$(date +%H:%M:%S)"
if [ "${WAIT:-1}" = "1" ]; then
  SLEEP=$(( BUCKET - $(date -u +%s) ))
  if [ "$SLEEP" -gt 0 ]; then echo "waiting ${SLEEP}s for the $SLUG boundary…"; sleep "$SLEEP"; fi
fi
```
Note: the slug is **UTC-derived** so the two machines match even if their local timezones or clocks differ slightly. The boundary wait + launching both within the same window keeps them in the same bucket; near a boundary, a few seconds of clock skew is absorbed because the skill's Phase 6 polls coord for up to 5 min and the claim TTL is 2 h.

### 3. Hand off to the real skill
Invoke via the `Skill` tool (never inline):
```
Skill: manual-test-coord
args: "--target=staging --rendezvous-slug=<SLUG> --operator=<ROLE> <forwarded-tokens>"
```
- If `--solo` was passed, **omit** `--rendezvous-slug` entirely (single-machine; Phase 6 SETUP_GAP is expected).
- Surface the skill's Phase 9 report as-is.

## Notes / related
- This is just a launcher; all test logic + remediation guidance lives in `/manual-test-coord`. For the iterate-and-fix loop use `/manual-test-coord-loop` (or wrap `/mtc` in `/loop`).
- `/mtc` delegates entirely to `/manual-test-coord` and needs no behavior change for injected transport. **Pre-auth / bare-login-page flows** (a login page shipping zero UI Bridge code) are out of scope here — drive those via `/manual-test --transport=injected --target-url=<page>` (injects the engine bundle + registers against a local temp runner relay). `/mtc`'s dashboard `/login` is already instrumented, so it uses the normal headless-tab path.
- Credentials, machine-id allow-list, and the rendezvous/topic mechanics are documented in `manual-test-coord.md`. Operator-2 (`tester2`) creds are stored in AWS SSM `/qontinui/operator2/*` (eu-central-1) and hydrated to `QONTINUI_OPERATOR2_*` on each machine — see `reference_secrets_via_aws_ssm`.
