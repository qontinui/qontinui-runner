# Manual-Test-Coord → Remediation Loop

Run `/manual-test-coord` iteratively with autonomous remediation of runner/config fixes. The loop auto-remediates what it can (same filter as step h: `qontinui-claude-config` / `qontinui-runner`, unambiguous, no API/schema/migration/IPC), defers coord/web items to the final report, and re-iterates — only pausing to ask the operator for genuinely unusual situations (security anomalies, regressions, iteration-cap with unresolved P0s).

**Autonomous by default.** Coord-side remediation (alembic migrations, ECS image rebuilds, Vercel deploys) is DEFERRED to the final report, never applied autonomously. Runner/config fixes that pass the step-h filter are applied without asking. The operator launched this loop expecting it to fix things — the default posture is remediate-and-continue, not stop-and-ask. The **implementation priorities** (memory: `implementation-priorities`) bind here: verified throughput (every PASS DOM-observed, not inferred), autonomy with checks, and asking the operator only for operator-resource needs, observed security anomalies, or an oversize-plan handoff.

## Role of the Main Session

The main session is a **coordinator**, not an implementer. It holds:

- The current iteration number
- A short ledger of `iter \t counts \t fingerprint \t status` lines
- One-line summaries of each iteration's outcome

The skill (`/manual-test-coord`) is invoked via the `Skill` tool — never via Bash, never inline. Remediation work (when authorized) is delegated to sub-agents the same way `/manual-test-loop` does. **If you find yourself driving UI Bridge or editing source files from the main context, you've drifted out of role.**

## Inputs

`$ARGUMENTS` — parsed for the following flag-style tokens. Anything left over is forwarded to `/manual-test-coord` as Phase 7 focus hint.

| Flag | Default | Purpose |
|---|---|---|
| `--target` | `staging` | Forwarded to `/manual-test-coord`. `staging` \| `local` \| `both`. |
| `--max-iters` | `4` | Cap on loop iterations. Coord iters touch ECS staging + RDS + temp runner build (~5–10 min each); 4 keeps total wall-clock under ~40 min. |
| `--auto-slug` | `true` | If true, the wrapper generates a fresh `--rendezvous-slug` per iteration (format: `loop-<YYYY-MM-DDTHH:MM:SS>-<iter>`). Set `--auto-slug=false` to suppress the slug entirely (single-machine runs that don't need Phase 6 cross-verification — Phase 6 will SKIP with `SETUP_GAP`). |
| `--wait-timeout` | `5` | Forwarded to `/manual-test-coord` as Phase 6's sibling-claim polling timeout (minutes). |
| `--rendezvous-window` | `5` | Seconds to pause AFTER printing the sibling-handoff prompt and BEFORE launching the iteration. Gives the operator a window to fire `/manual-test-coord --rendezvous-slug=<slug>` on the other machine. |

If `--target=local` or `--auto-slug=false`, the wrapper omits the slug (Phase 6 SETUP_GAP is expected and not a defect).

## Loop Structure

### Setup (once, in main session)

1. Parse `$ARGUMENTS`. Resolve `TARGET`, `MAX`, `AUTO_SLUG`, `WAIT_TIMEOUT`, `RENDEZVOUS_WINDOW`, plus the residual `FOCUS` string.
2. Initialize `LEDGER=""` (in-memory string, one line per iteration). Don't persist.
3. Set `ITER=1`, `CLEAN_STREAK=0`.

### Per-iteration body

Each iteration is one round-trip through the steps below. The loop runs autonomously by default; the operator gate (step g) only fires on exceptional conditions (security anomalies, regressions, iteration-cap with P0s).

#### a. Generate / accept the rendezvous slug

- If `AUTO_SLUG=true` AND `TARGET` is `staging` or `both`: build `SLUG="loop-$(date -u +%Y-%m-%dT%H:%M:%S)-iter${ITER}"`.
- Else: `SLUG=""` (no slug; Phase 6 will SETUP_GAP).

#### b. Print the sibling-handoff prompt

Emit a clearly-formatted block to the operator output (NOT `AskUserQuestion` — this is informational, not interactive):

```
========================================================================
Iteration N of MAX — rendezvous slug: loop-2026-05-21T14:30:00-iter1

If you want Phase 6 (tenant isolation) to PASS rather than SETUP_GAP,
invoke the sibling on your OTHER operator machine within the next
RENDEZVOUS_WINDOW seconds:

  /manual-test-coord --rendezvous-slug=loop-2026-05-21T14:30:00-iter1 \
                     --target=<target>

Otherwise Phase 6 will SKIP with SETUP_GAP and the rest of the run
proceeds normally.

Launching this machine's iteration in RENDEZVOUS_WINDOW seconds...
========================================================================
```

If `SLUG=""` (single-machine intent), skip this block entirely and proceed to step d.

#### c. Brief wait (RENDEZVOUS_WINDOW seconds, default 5)

Pause for the operator's window. Do NOT use `AskUserQuestion` here — the operator should be free to ignore the prompt if they don't have a second machine. Implement via a simple `Bash` `sleep` call. This is the only `sleep` allowed in the wrapper; everything else uses the skill or sub-agents.

#### d. Invoke `/manual-test-coord` via the `Skill` tool

Single call:

```
Skill: manual-test-coord
args: "--target=<TARGET> [--rendezvous-slug=<SLUG>] --wait-timeout=<WAIT_TIMEOUT> <FOCUS>"
```

Pass `--rendezvous-slug` only if `SLUG != ""`. The skill returns its compact report per the contract in `manual-test-coord.md` §"Phase 9: Report".

#### e. Parse the report

Extract the counts line: `bugs=N friction=N missing=N product_gaps=N setup_gaps=N security_anomalies=N blocked=N`.

Compute a stable fingerprint over the remediation-plan section: sort issue lines by `(repo, file_or_module, issue)`, SHA-256 the joined string, keep first 12 hex chars. If the remediation plan is empty (NO_DEFICIENCIES), fingerprint is `—`.

Append to `LEDGER`:
```
iter=N counts="bugs=… friction=… missing=… product_gaps=… setup_gaps=… security_anomalies=… blocked=…" fp=<fingerprint> status=<NO_DEFICIENCIES|HAS_DEFICIENCIES>
```

#### f. Termination checks (before involving the operator)

The canonical `NO_DEFICIENCIES` condition for THIS skill is:

```
bugs=0 friction=0 missing=0 product_gaps=0 setup_gaps=0
```

(Note: `security_anomalies` and `blocked` are NOT in the NO_DEFICIENCIES test — a SECURITY_ANOMALY is a finding that ALWAYS requires operator review; a BLOCKED phase always requires re-run or operator action. `setup_gaps` IS in the test — a single-machine run leaving Phase 6 at SETUP_GAP is a deficiency for *this skill's purpose*; if the operator wants to bypass it permanently they should pass `--target=local` so the wrapper expects the gap.)

**`NO_DEFICIENCIES` requires each phase PASS to be DOM-observed via the UI Bridge, not inferred.** A phase that "passed" only because a coord API / DB / device-registration / status / log signal implied the outcome (without observing it rendered on the dashboard) does NOT count toward the clean streak — treat it as `blocked`. Per `/manual-test-coord`'s verification principle, the operator's outcome must be seen on the page; backend evidence confirms plumbing, not the goal, and routinely disagrees with what the dashboard renders.

Apply in order:

1. If counts satisfy NO_DEFICIENCIES → increment `CLEAN_STREAK`. If `CLEAN_STREAK >= 2` → exit loop, jump to Final Report (success).
2. If `ITER >= MAX` → exit loop with reason `iteration cap`. Jump to Final Report.
3. Else → reset `CLEAN_STREAK = 0`. Auto-filter the remediation table for autonomously-fixable items (same filter as step h: repo is `qontinui-claude-config` or `qontinui-runner`, unambiguous, no API/schema/migration/IPC):
   - If there ARE autonomously-fixable items → proceed directly to step h (remediate). No operator prompt.
   - If there are ZERO autonomously-fixable items (all items are coord/web/deferred) → log deferred items in the iteration ledger, increment `ITER`, restart the loop body at step a (skip remediation, no operator prompt).
4. Before proceeding, check the operator gate conditions (step g). If ANY gate condition fires, pause for operator input before continuing. Otherwise, proceed autonomously.

#### g. Operator gate (conditional — fires only on exceptional conditions)

The operator gate fires ONLY when at least one of these conditions is true:

1. **`security_anomalies > 0`** — security findings always need human review.
2. **Regression detected** — a finding in this iteration explicitly contradicts a prior iteration's finding (i.e. an item that was PASS in a previous iteration is now FAIL, or a new deficiency appeared in a category that was previously clean).
3. **Iteration cap imminent with unresolved P0** — `ITER >= MAX - 1` AND there are unresolved `bugs` or `security_anomalies` that were not remediated.

**If none of these conditions fire, the loop proceeds autonomously** (step h if there are fixable items, or next iteration if all items are deferred). No `AskUserQuestion`.

When the gate DOES fire, surface the report's `## /manual-test-coord report` header and remediation-plan section verbatim, then prompt:

```
AskUserQuestion:
  question: "Iteration N requires operator review: <reason>. Counts: bugs=… friction=… missing=… product_gaps=… setup_gaps=… security_anomalies=…. Choose how to proceed."
  options:
    - id: "remediate"
      label: "Apply autonomously-contained fixes, then re-iterate"
      detail: "Wrapper applies ONLY fixes that touch qontinui-claude-config or qontinui-runner internals AND are unambiguous. Coord/web/RDS/ECS items are DEFERRED to the final report."
    - id: "skip"
      label: "Skip remediation, re-iterate"
      detail: "Move to iteration N+1 without applying any fixes. Useful when the deficiencies are environmental (e.g. waiting on a separate Vercel deploy)."
    - id: "stop"
      label: "Stop the loop"
      detail: "End this loop run. Final report will summarize iterations 1..N."
  default: "remediate"
```

**Default is "remediate".** The operator actively chose to run this loop and expects it to fix things. The stop option is available but not the default.

Branches:

- **remediate** → proceed to step h.
- **skip** → mark this iteration `status=SKIPPED_BY_OPERATOR`, increment `ITER`, restart the loop body at step a.
- **stop** → exit loop with reason `operator stop`. Jump to Final Report.

#### h. Autonomously-contained remediation (only on `remediate` branch)

Filter the report's remediation table to items where ALL of:

1. `repo` is `qontinui-claude-config` OR `qontinui-runner`.
2. Issue is unambiguous (the proposed fix in the table reads as a single mechanical change — file + line + replacement value).
3. The fix does NOT touch a public API surface, schema, migration, IPC contract, or anything that would require a coordinated deploy.

Items that don't pass that filter are surfaced verbatim in the iteration's outcome line ("operator action required: \<N\> items deferred — see report above").

For the items that DO pass: delegate to ONE Agent (subagent_type: `general-purpose`) per repo touched, exactly as `/manual-test-loop` does (parallel tool calls in a single message, one Agent per repo, no batching across repos). The Agent's prompt mirrors `/manual-test-loop`'s remediation-Agent contract:

> Implement these remediation items for `<repo>`. Each item lists file, issue, priority, proposed fix, verification step.
> [paste filtered table rows verbatim]
> Rules:
> - Fix the root cause, not symptoms.
> - After each item, run the verification step. Report `PASS | FAIL | DEFERRED` per item.
> - Run the repo's standard typecheck/lint at the end (Rust: `cargo check` + `cargo clippy --tests -D warnings` via cargo-guard; Markdown skill files: lint by re-reading the file end-to-end for structural sanity).
> - **Do NOT commit.** The wrapper does not commit between iterations — operator handles commits at session end.
> - Report back: changed files, per-item PASS/FAIL/DEFERRED, anything deferred and why.

Wait for the repo Agent(s) to return. Append a one-line note to the iteration's ledger entry: `remediated_repos=<list> items_passed=N items_failed=N items_deferred=N`.

**No commit step.** The wrapper deliberately does NOT auto-commit between iterations — that's the line separating it from `/manual-test-loop`. The operator commits the accumulated fixes at session end if they want them landed.

After remediation, increment `ITER`, restart the loop body at step a.

## Termination Conditions

The loop ends on:

1. **Clean run** — two consecutive iterations satisfy `NO_DEFICIENCIES` (counts all zero across `bugs`, `friction`, `missing`, `product_gaps`, `setup_gaps`). **Primary success condition.**
2. **Iteration cap** — `ITER > MAX` (default 4). Surface the unresolved remediation table.
3. **No-progress stall** — the fingerprint is identical for 2 consecutive iterations AND no autonomous remediation was applied in either of those iterations. Exit with reason `stalled` (prevents infinite loops on unfixable deficiencies that the loop cannot remediate).
4. **Hard skill failure** — `/manual-test-coord` returns with the report header missing or malformed (skill crash, supervisor unreachable, staging coord 500). Don't retry; surface the failure and exit. The operator will likely need to re-run after fixing the underlying problem.
5. **Operator stop** — operator chose "stop" at an exceptional-condition prompt (step g). Rare since the gate only fires on security anomalies, regressions, or iteration-cap with P0s.

## Final Report

After exit, in the main session, produce ONE consolidated report:

```
## manual-test-coord-loop summary

- Iterations run: N (of MAX)
- Target: <staging|local|both>
- Termination reason: <clean run | iteration cap | stalled | operator stop | hard skill failure>
- Total deficiencies across all iters: <sum>
- Per-iteration ledger:
  iter=1 counts="bugs=… …" fp=abc123… status=HAS_DEFICIENCIES   → operator chose: remediate (passed=4, deferred=2)
  iter=2 counts="…"           fp=def456… status=HAS_DEFICIENCIES   → operator chose: skip
  iter=3 counts="bugs=0 …"    fp=—       status=NO_DEFICIENCIES    → clean
  iter=4 counts="bugs=0 …"    fp=—       status=NO_DEFICIENCIES    → clean (confirmation)
- Open items requiring operator action (coord/web touch):
  • <one-line description of each deferred item, with repo + file/module>
- Repos touched by autonomous remediation: <list>
```

Do NOT regenerate per-iteration plans or transcripts. The final report is a thin ledger; the detailed remediation tables are already in the operator's transcript from the per-iteration `AskUserQuestion` displays.

## Coordination Notes

- **Coord claims.** `/manual-test-coord` itself acquires + releases its rendezvous claim per iteration. The loop wrapper does NOT acquire a separate session-level claim — the rendezvous claim is iteration-scoped, and adding a wrapper-level claim would just clutter `coord.claims`. If the wrapper exits mid-iteration (operator cancel, hard failure), the iteration-level rendezvous claim cleans itself via its 7200s TTL.
- **Two-machine timing.** The `--rendezvous-window` is operator-side; the wrapper does not poll for the sibling's claim itself (that's `/manual-test-coord`'s Phase 6 job). The wrapper only prints the slug + waits the configured window before invoking the skill. If the operator never fires the sibling, Phase 6 SETUP_GAPs and the iteration continues.
- **`--target=local` runs**: the wrapper skips the sibling-handoff prompt (no rendezvous makes sense locally) and Phase 6 SETUP_GAPs by design. NO_DEFICIENCIES on a local run requires `setup_gaps=0` even though Phase 6 SETUP_GAPped — that's why operator should pass `--target=local` when they explicitly want local-only (the skill's report counts SETUP_GAP for SKIPped Phase 6 in single-machine mode toward `setup_gaps`; if the operator wants local-only without the gap, they accept that single-machine local will never satisfy NO_DEFICIENCIES, and they should rely on `operator stop` to end the loop after one clean-modulo-phase-6 iteration). This is a known asymmetry, not a bug.
- **Sub-agents for remediation only.** All remediation work goes through sub-agents (Agent tool). The main context never edits source files directly. If the wrapper finds itself reading source from the main context, it has drifted out of role.

## Rules

- **NEVER inline `/manual-test-coord` work in the main context** — always via the `Skill` tool.
- **NEVER apply fixes touching `qontinui-coord`, `qontinui-web`, RDS, or ECS surfaces without operator OK** — those go in the deferred-items list of the iteration outcome and the final report, period. Those changes need a coordinated deploy and the wrapper cannot orchestrate that safely.
- **NEVER auto-commit between iterations.** The operator commits at session end. Differs from `/manual-test-loop`, which commits per-iteration.
- **NEVER use `--no-verify`, `core.hooksPath=/dev/null`, or any hook bypass** in the (rare) remediation sub-agents.
- **NEVER commit Claude / AI attribution** — the qontinui pre-commit hook blocks it; any sub-agent that ends up committing (rare; only if the operator-OK path explicitly authorizes it later) omits those trailers per `feedback_no_claude_attribution`.
- **NEVER restart, kill, or rebuild the primary runner.** `/manual-test-coord` already spawns temp runners via supervisor port 9875.
- **Default to autonomous remediation.** The loop auto-remediates runner/config fixes and re-iterates without asking. The operator gate fires only on exceptional conditions (security anomalies, regressions, iteration-cap with P0s); when it does fire, the default is "remediate", not "stop".
- **One `Skill` call per iteration.** Don't fan out parallel `/manual-test-coord` invocations — staging coord is rate-limited and two parallel runs would cross-contaminate the rendezvous protocol.
- **Cap iterations at 4 by default.** Coord iters are expensive (~5–10 min each). Raising the cap requires explicit `--max-iters=N` from the operator.
- **One `sleep` allowed.** The `--rendezvous-window` pause is the only `sleep` in the wrapper. Everything else is event-driven (skill return, sub-agent return, operator response).

## Focus

$ARGUMENTS
