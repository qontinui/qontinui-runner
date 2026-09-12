# Manual-Test → Remediation Loop

Run `/manual-test` and implement the resulting remediation plan iteratively until manual testing surfaces no remaining deficiencies. Coordinate with subagents to keep the main context lean across a long-running session.

**Fully autonomous.** No "ready to commit?" gating. No "should I keep going?" check-ins. The loop terminates on its own conditions (see "Termination" below). User stays out of the cycle.

## Role of the Main Session

The main session is a **coordinator**, not an implementer. It holds only:

- The current iteration number
- A short ledger of deficiency-set fingerprints (to detect no-progress stalls)
- One-line summaries of each iteration's outcome

Heavy work — UI Bridge probing, file reads, code edits, build/restart cycles — happens inside subagents and returns to the main session as compact structured reports. **If you find yourself reading source files or running curl probes from the main context, you've drifted out of role.**

## Inputs

`$ARGUMENTS` — optional. Forwarded to every `/manual-test` invocation as its test focus. If empty, manual-test exercises both Runner UI and Web frontend per its own defaults.

Optional flag-style tokens parsed from `$ARGUMENTS`:
- `--target=runner|web|mobile|both` — restrict scope; forwarded to `/manual-test`
- `--no-commit` — implement but don't commit between iterations (default each iteration: branch-first → commit → push branch → open PR; never a direct push to the default branch, and never merge — coord lands it)

- `--max-rounds=N` — override the hard-ceiling backstop (default 12; see below)

This loop's PRIMARY stops are organic: clean run and no-progress stall (the two natural signals that it's done), plus repeated hook failure and operator interrupt. Those remain the real termination signals — see "Termination Conditions". There is now also a **hard ceiling of 12 iterations as a BACKSTOP** (arg-overridable via `--max-rounds=N`): if the loop somehow runs 12 iterations without a clean run or a detected stall, stop and emit a structured handoff rather than looping indefinitely. The ceiling is generous on purpose — it should almost never fire (the stall/clean-run stops normally trigger first); it exists only so an unexpected non-converging loop can't burn tokens forever. This is the shared loop-control rubric (`_loop-control.md`): this skill's existing fingerprint-stall logic and per-iteration ledger ARE the rubric's stall-detection and per-round ledger; the 12 ceiling + the structured escalation handoff are the rubric additions.

## Loop Structure

### Setup (once, in main session)

1. Create a TaskCreate checklist with the first few iteration tasks ("Iter 1: test + remediate", "Iter 2: test + remediate", "Iter 3: test + remediate"). Add tasks for "final clean run" and "summary report" at the end. Add more iter tasks as you go — don't try to predict how many you'll need. Mark each complete the moment it lands — don't batch.
2. Initialize `LEDGER=""` (in-memory string holding `iter\tdeficiency_count\tfingerprint\tstatus\tending` lines — this IS the rubric's per-round ledger; `ending` is the rubric's Element 5 column, see "Turn-ending classification" under Termination Conditions). Don't write it to a file; it lives in your scratch text.
3. Set `ITER=1`.
4. Set `MAX_ROUNDS=12` (the hard-ceiling backstop). If `$ARGUMENTS` contains `--max-rounds=N`, use N instead.

### Per-iteration body

Each iteration is a single round-trip through three subagent calls. Do NOT inline any of these in the main context.

#### 1. Test-and-plan subagent

Launch one Agent (subagent_type: `general-purpose`) with this contract:

**Prompt template:**
> Run the `/manual-test` skill end-to-end with focus: `$ARGUMENTS` (or "general health check on both Runner UI and Web frontend" if empty). Follow every phase of the skill including Phase 6's Remediation Plan.
>
> **Verify the goal on the actual page, never by inference.** If the focus names a user-visible outcome (e.g. "a session appears in Live Sessions on the production website"), you MUST confirm it by driving the UI Bridge to that page (`control/page/navigate` → `discover`/`snapshot`) and observing the rendered content. A coord/API/DB/registration/log signal is NOT acceptable evidence — those confirm plumbing and routinely disagree with the page (a coord API returning 3 sessions while the page shows "0 sessions" is a FAIL, not a PASS). If you cannot reach the surface (relay down, no connected tab, auth wall), the goal is a `blocked_task`, NOT satisfied — and `NO_DEFICIENCIES` is then forbidden.
>
> When done, report back ONLY the following — do NOT include the Phase 1–4 transcript:
>
> 1. **Deficiency count** by category: `bugs=N, friction=N, missing=N, blocked_tasks=N`.
> 2. **Per-repo remediation table** with one row per item — columns: `repo | file_or_module | issue (≤80 char) | priority (P0/P1/P2) | proposed_fix (≤120 char) | verification (≤80 char)`.
> 3. **Execution order** — numbered list of items in the order an implementer should tackle them.
> 4. **Fingerprint** — a stable hash you compute by sorting the issues by `(repo, file_or_module, issue)` and SHA-256-ing the joined string. Report the first 12 hex chars.
> 5. **Cleanup status** — confirm any temp runner spawned was stopped (`POST /runners/<id>/stop`).
> 6. **Goal-observed evidence** — for each user-visible goal in the focus, the page URL you navigated to and the exact rendered text/element that confirms it (or `UNVERIFIED: <reason>`). No element observed = not verified.
>
> If Phase 5 surfaced no deficiencies **and every focus goal was observed on its page per item 6**, report exactly: `NO_DEFICIENCIES` and stop. If any goal is UNVERIFIED, do NOT report `NO_DEFICIENCIES` — surface it as a `blocked_task`. Do not invent work.
>
> Be terse. The coordinator only needs the report — not narration.

When this subagent returns, append a line to `LEDGER`:
```
iter=$ITER  defs=<count>  fp=<fingerprint>  status=<NO_DEFICIENCIES | HAS_DEFICIENCIES>  ending=<pending>
```

`ending` is deliberately `<pending>` here: this row is written when the
test-and-plan subagent returns, which is *before* remediation and commit run, so
the iteration's final paragraph does not exist yet. **Fill it in once the
iteration's work is finished — after step 3, as the last thing you do before
looping** — since that is the only point at which there is a final paragraph to
read. (The canonical skeleton classifies it just before its termination checks
because there the append and the checks are both at the end of the round body;
this skill splits them, so the classification follows the work rather than the
checks below.) A row still reading `<pending>` when the loop exits is itself
worth noticing: it means the iteration ended somewhere other than its own end —
which is exactly what Element 5 is for.

**Termination checks (run before implementation, PRIMARY stops first, BACKSTOP last):**

- If status is `NO_DEFICIENCIES` → exit loop, go to "Clean run confirmation" below.
- If the remaining deficiencies cannot clear until an **observable** condition does (a deploy going green, a migration reaching head, a rebuilt runner becoming the serving build) → that is a BLOCK, not a stall. Run `/blocked` to register the typed coord gate FIRST, then exit and report naming the `gate_id`. Checked before the stall rule because a block presents as one. An unmerged PR is **not** a blocker here — the next iteration tests the branch.
- If `fingerprint` matches the **previous** iteration's fingerprint → no-progress stall. Surface the stalled plan, exit loop, report.
- If `ITER > MAX_ROUNDS` (default 12, arg-overridable via `--max-rounds=N`) → **hard-ceiling backstop**. This should almost never fire — if it does, a stall normally would have caught it first. Exit loop and emit the structured escalation handoff (see "Hard-ceiling handoff" under Termination Conditions). Don't keep looping.
- If this iteration is about to end on a `bailout`- or ungated-`user_deflection`-shaped final paragraph (see "Turn-ending classification") → **you are not done**. Run the next iteration, or register the typed coord gate via `/blocked` first. Never stop here silently.

Otherwise, proceed to step 2.

#### 2. Remediation subagent(s)

For the items in this iteration's remediation table, group by **repo** (qontinui-runner, qontinui-web, qontinui-mobile, ui-bridge, etc.). Each repo-group becomes one Agent (subagent_type: `general-purpose`) launched **in a single message** as parallel tool calls. Serialize only when one group truly depends on another (e.g. ui-bridge endpoint added before runner can call it — pre-flight that explicit dep and split into two waves).

**Worktree isolation — bound rule.** Before launching a remediation Agent against repo `R`, the coordinator allocates an isolated git worktree for `R` **through coord** and passes that path as the Agent's working directory. (The former HTTP face, `POST /agents/allocate-local`, was removed as dead code in runner #443.)

**Allocate the worktree THROUGH coord — never a bare `git worktree add`**
*(plan `2026-08-18-undeclared-worktree-exposure-and-classification`, Phase 3)*.
A hand-rolled worktree has no `coord.agent_worktrees` row, which means it cannot
be attributed to a session, cannot hold a retention pin, and cannot be counted or
drained by policy — it just accumulates. (It is no longer *deleted* underneath
you: Phase 2 of that plan withdrew removal authority from the `undeclared`
trigger. The cost of skipping this is now a permanent leak rather than data loss,
which is a trade made deliberately — not a reason to skip it.)

```
POST $COORD_HTTP_URL/agents/allocate
{
  "device_id":  "<this machine's device_id>",
  "repos":      [{"repo": "<repo>"}],
  "intent":     "<what this worktree is for>",
  "work_unit_id": "<the plan's work_unit_id UUID, WHEN THERE IS ONE>"
}
```

`work_unit_id` is **optional** — a declaration without a plan is still a
declaration, and that is exactly why the field is nullable. Omit it rather than
inventing one.

Three response shapes you must handle, or the call is worse than useless:

1. **`worktrees[].worktree_path` is RELATIVE** (`agent-worktrees/<agent_id>/<repo>`)
   — re-root it under the workspace root and create the checkout with the branch
   coord reserved:
   `git -C <repo> worktree add -b <worktrees[].branch> <absolute-path> <worktrees[].parent_sha>`.
2. **`isolation.mode` may be `wait` or `shared_branch`.** On `wait`, coord is out
   of disk/build-slot budget — report `reason` / `blocking` and retry or
   serialize; do not force a worktree. On `shared_branch`, the canonical checkout
   can carry the branch.
3. **HTTP 409 `repo_not_registered`** — the repo is not in
   `coord.canonical_repos`, so coord cannot decide a parent SHA. Supply
   `parent_sha` explicitly, or fall back to a plain `git worktree add` and say in
   your report that the worktree is undeclared and why. An unregistered repo is
   the one legitimate reason to skip the declaration.
 The remediation Agent treats that path as its repo root for the duration of the task — every `Edit` / `Write` / `git checkout -b` lands there, never in the operator's primary checkout. Remove the worktree after the remediation ships. **Why:** the proximate cause of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md` was two concurrent remediation Agents from this very skill editing `operations.py` in the same primary checkout simultaneously.

**Prompt template for each repo Agent:**
> Implement these remediation items for `<repo>`. Your working directory is `<allocated worktree path or primary checkout fallback>` — treat it as the repo root; do NOT `cd ..` out of it or touch the primary checkout. Each item lists file, issue, priority, proposed fix, verification step.
>
> [paste the table rows for this repo verbatim]
>
> Rules:
> - Fix the root cause, not symptoms. Don't add scaffolding the items don't ask for.
> - After each item, run the verification step from the table. Report `PASS | FAIL | DEFERRED` per item with a one-line note.
> - Run the repo's standard typecheck/lint after all items in your group (`cargo check` + `cargo clippy -D warnings` for Rust, `npx tsc --noEmit` for TS, `ruff check` + `mypy` for Python). Fix any new warnings introduced by your changes.
> - If a verification step requires a temp runner, spawn one via supervisor port 9875 with LKG-first per `/manual-test` Phase 0, then stop it when done. Never touch the primary runner.
> - **Do NOT commit.** The coordinator commits per-iteration after all repo Agents return.
> - Report back: changed files (with line counts), per-item PASS/FAIL/DEFERRED, any items deferred and why, any verification step that needed adjustment.

Wait for all repo Agents to return.

#### 3. Commit subagent (unless `--no-commit`)

Launch one Agent (subagent_type: `general-purpose`) with this contract:

**Prompt template:**
> For each repo touched in this iteration, create one commit per repo containing only the files changed by the remediation Agents above. Commit subject format:
>
> `<repo-prefix>: manual-test-loop iter <N> — <one-line summary>`
>
> Body: bullet list of items addressed in this commit (issue → fix), one per line. **Do not include Claude or AI attribution** (the qontinui pre-commit hook blocks `Co-Authored-By: Claude` lines anyway).
>
> **Branch-first — NEVER commit on the default branch.** For each touched repo, before committing run `git -C <repo> symbolic-ref --short HEAD`. If it equals that repo's default branch (`main`), first create a session branch `loop/manual-test-loop-iter<N>-<short-session>` and switch to it — **keep the `<short-session>` discriminator; a bare iteration-numbered name is unusable.** `qontinui-merge-orchestrator[bot]` reaps the head branch of a merged PR on sight, so any name a previous loop run already landed under is permanently burned: the push prints `* [new branch]` and exits 0, the ref is deleted ~2s later, and `gh pr create` then reports "No commits between main and <branch>". Observed 2026-08-05 with `loop/mtl-iter2-runner`, which runner#568 had merged in June. **Verify every push landed** (`git ls-remote --heads origin <branch>`) — a successful-looking push is not proof the ref exists; otherwise commit on the current (already non-default) branch. Then commit there (Session-Id + Session-Name trailers are added by the repo's PER-CLONE `prepare-commit-msg` hook — `/tag-session` only supplies the NAME that hook reads, it injects nothing itself; a clone the installer never ran against emits neither trailer, so fix that with `qontinui-dev-notes/scripts/install-session-id-hook.sh` rather than re-running `/tag-session`), `git push` the BRANCH (never push the default branch directly), `gh pr create` with a title matching the commit subject and a body naming the loop + iteration, and then **STOP — do not merge.** Coord is the sole merge authority for `qontinui/*` repos; agents never run `gh pr merge` or `--admin` (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). Opening the PR IS shipping — coord's merge train lands it. The loop does not wait for the merge: the next iteration's test-and-plan subagent tests the BRANCH build, so an unmerged PR never blocks progress (pass the branch/worktree path to it explicitly, or it will re-find the already-fixed defect on `main`, report an identical fingerprint, and trip the no-progress stall detector with a false positive). **Never push to the default branch directly** ([[feedback_no_direct_pushes_to_main_loops_use_branches]]): a loop on a checkout sitting on `main` that committed + pushed there is exactly what caused the 2026-06-07 fleet-wide fmt-red incident (untrailered, PR-less commit reached `main` via the operator's admin bypass).
>
> If a pre-commit hook fails, surface the failure with its full output — do not retry with `--no-verify` or any other bypass without explicit operator instruction. If a hook failure looks like an environment issue (path-dep stash, sccache wedge, etc.), diagnose its root cause before bypassing. Report failures back with the hook output verbatim.
>
> Report back: per-repo branch name, commit SHAs, PR URLs, merge status, any hook failures.

After commit subagent returns, mark this iteration's TaskCreate task complete, increment `ITER`, loop.

### Clean run confirmation

When the test-and-plan subagent reports `NO_DEFICIENCIES`, run ONE MORE iteration to confirm — manual testing has nondeterminism, and a single clean pass can be a fluke. If the confirmation also reports `NO_DEFICIENCIES`, exit successfully.

**A `NO_DEFICIENCIES` report is only valid if its item-6 "Goal-observed evidence" shows every focus goal was observed on its page via the UI Bridge.** If the loop reaches a "clean run" but no focus goal was ever visually confirmed (the subagent only checked backend/API state), that is NOT a clean run — reject it, and re-run with an explicit instruction to navigate to the goal's page and observe the rendered outcome. The loop's success condition is "the user's goal is visible on the page," not "no code defects found."

If the confirmation surfaces new deficiencies, treat it as a normal iteration (implement, commit, then re-confirm). Don't loop the confirmation step forever; if confirmation fails 3 times in a row, surface "non-deterministic test surface" and exit with the partial result.

### Session-bucket scenarios (Terminal StatusStrip) — use the test seam

Shipped 2026-06-05 (runner #420, plan `2026-06-04-inject-session-seam-statusstrip-coverage-plan`): on any debug/temp runner, every StatusStrip bucket is deterministically drivable on-page — no real PTYs, no 60s staleness waits.

1. `POST :<port>/ui-bridge/test/seed-terminal-scenario` with `{"working":1,"idle":2,"needs_input":0}` (atomic clear-then-seed; also `error`/`completed`).
2. Read the strip via DOM, not OCR: `POST :<port>/ui-bridge/control/page/read-value {"selector":"[data-page-element=status-strip]"}` — ground truth, immune to the vision cache/occlusion. (If you must OCR, pass `{"force":true}` to `vision/extract` — its cache only invalidates on control actions, not UI re-renders.)
3. Poll ≤5s after seeding (the 2s staleness sweep must tick once for `idle`); the count pills only render when `sessionCount > 1`.
4. Teardown: `POST /ui-bridge/test/clear-injected` (10-min TTL backstops leaks).

Full contract: `qontinui-runner/src-tauri/src/mcp/test_fixtures.rs` module docs.

## Termination Conditions

The loop ends on (1–4 and 6 are PRIMARY; the hard ceiling is a BACKSTOP):

1. **Clean run** — two consecutive `NO_DEFICIENCIES` reports. **Primary success condition.**
2. **No-progress stall** — two consecutive iterations with identical remediation fingerprints. Report the persistent set and stop. (Often means a fix didn't take; the next session will need to diagnose.)
3. **Repeated hook failure** — same pre-commit hook failure surfaced twice with no obvious env fix. Surface the failure verbatim and exit; do not power through.
4. **Operator interrupt** — never block on operator input, but if the operator cancels the run, release any coord claims acquired and exit cleanly.
5. **Hard-ceiling backstop** — `ITER` exceeds `MAX_ROUNDS` (default 12, arg-overridable). Should almost never fire; if it does, the loop is not converging and a stall normally would have caught it. Exit and emit the **Hard-ceiling handoff** below. This is `stop_and_report` — do NOT turn it into an `AskUserQuestion` unless it hits the autonomous-default carve-outs (operator-resource need / observed security anomaly / oversize-plan handoff), per the `implementation-priorities` memory — which supersedes the coord-deploy-or-migration carve-out in `feedback_mtc_loop_autonomous_default`: deploys and migrations proceed autonomously when their documented checks pass.
6. **Blocked on an observable condition** — the remaining deficiencies cannot clear until a deploy goes green, a migration reaches head, or a rebuilt runner becomes the serving build. Register the typed coord gate via `/blocked` FIRST, then exit and emit the handoff naming the `gate_id`. Evaluated before conditions 2 and 5, because a block presents as either a stall or a non-converging ceiling. **An unmerged PR is not this** — the next iteration tests the branch, so the merge train is never a reason to stop.

### Turn-ending classification

None of conditions 1–6 reads what an iteration SAID — they watch deficiency counts, fingerprints,
iteration counts, hook output and the operator. This one watches the loop's **prose**, because the
failure none of them can see is the iteration that quietly gives up. The stall rule compares iteration N against N+1, and a bail ends the loop
before N+1 exists, so the last ledger row reads `HAS_DEFICIENCIES` forever and the run looks merely
unfinished rather than abandoned.

Judge the ending from the **last non-empty paragraph** of the iteration's final text, matched at its
**start**. The anchoring is the whole trick: an iteration that *discusses* stopping mid-paragraph
and then keeps remediating is `complete`.

| Ending | Shape |
|---|---|
| `complete` | Does not start with a stop pattern. The overwhelming majority. |
| `waiting_on_signal` | Stops on an **observable** signal with a bounded wait — "resume once the branch build finishes". Legitimate. |
| `user_deflection` | Stops on a **person** — "retry when you approve", "let me know how to proceed". Not a verdict on its own. |
| `bailout` | Stops with neither a signal nor a person to wait on — "I'll stop here", "I am unable to proceed". |
| `unknown` | The iteration's final text could not be read. **Never fold this into `complete`** — count it separately. |

**`user_deflection` is only a bailout when the work is UNGATED.** Policy `planning-and-scope`
`dependency-wait-and-resume` prescribes stopping on a human decision — *provided* the gate and
continuation were registered first ("never end a session with a blocked item that has no registered
gate"). So join the text with gate state: deflection **+ a registered gate** is the prescribed
`stop with status waiting`; deflection **+ no gate** is a bailout. Collapsing that distinction flags
every correctly-closed blocked session, which is how a control this cheap gets switched off. Note
this loop already has a legitimate `waiting_on_signal` shape built in: an unmerged PR does not block
the next iteration (the next test-and-plan subagent tests the BRANCH), so "waiting for the merge
train" is never a reason to stop.

**What to do with it — nothing automatic.** Record it in the `ending` column and name it in the
handoff. A `bailout` or ungated `user_deflection` is the `finish-to-zero` clause telling you the run
is not done: either run the next iteration, or register a typed coord gate via `/blocked` when the
blocker is an observable condition. Do not implement a re-prompt loop off this verdict; acting on it
automatically is gated behind the runner detector's shadow-corpus review.

**Emit-on-block — a stall or the hard ceiling is sometimes a BLOCK, not an absence of progress.**
Before you emit the handoff, ask what the loop is actually waiting on. If the remaining deficiencies
cannot clear until some **observable** condition changes — a deploy going green, a migration
reaching head, a rebuilt runner becoming the serving build, an upstream fix landing — then this is
not "no progress", it is *blocked on an observable condition*, and you **MUST** invoke `/blocked` to
register the typed coord gate **BEFORE** you stop. Then emit the handoff as usual, naming the
registered `gate_id`. If the blocker has no observable trigger, say so — that case is NOT a gate
(see `/blocked`).

**The one condition that is never a blocker here is the one you will reach for first: an unmerged
PR.** The next test-and-plan subagent tests the BRANCH, so waiting on the merge train is the
built-in legitimate `waiting_on_signal` described above, not a gate — run the next iteration. Gate
only what actually stops the branch from being testable.

Emit-on-block is **in addition to** the stall/ceiling handoff, not a replacement, and it is a
**separate trigger from the `bailout` arm above**. That arm cannot cover it: an iteration that stops
on a real signal classifies as `waiting_on_signal`, which is *legitimate* by construction, so the
bailout check waves it through. Ungated, it is still an unwatched blocked item.

### Hard-ceiling handoff

When the loop exits on the hard-ceiling backstop, emit this structured handoff (assembled mechanically from `LEDGER` — do not re-derive it):

```
## manual-test-loop escalation — <hard-ceiling backstop (non-converging) | blocked on an observable condition>

- Iterations run: <N> / <MAX_ROUNDS>
- Registered gate: <gate_id from /blocked, or "none — blocker has no observable trigger">
- Current failing signal: <the persistent deficiency set / fingerprint still present>
- Per-iteration ledger:
  iter=1 defs=12 fp=abc123… status=HAS_DEFICIENCIES ending=complete
  …
- What was tried each iteration: <one line per iter — what was remediated and the outcome>
- Decision needed: <the specific blocker, or "diagnosis handoff: next session should investigate <X>">
```

## Final Report

After exit, in the main session, produce ONE consolidated report:

```
## manual-test-loop summary

- Iterations run: N
- Termination reason: <clean run | no-progress stall | hook failure | hard ceiling>
- Endings seen: <tally per `ending` value; any `bailout`, ungated `user_deflection` or `unknown` named explicitly>
- Total deficiencies fixed: <sum of per-iteration deltas>
- Per-iteration ledger:
  iter=1 defs=12 fp=abc123… status=HAS_DEFICIENCIES ending=complete → fixed 12, commits a1b2c3 (runner), d4e5f6 (web)
  iter=2 defs=4  fp=def456… status=HAS_DEFICIENCIES ending=complete → fixed 4,  commit  789abc (runner)
  iter=3 defs=0  fp=—       status=NO_DEFICIENCIES  ending=complete → clean
  iter=4 defs=0  fp=—       status=NO_DEFICIENCIES  ending=complete → clean (confirmation)
- Repos touched: <list>
- Open items (if any): <list with reasons>
```

Do NOT regenerate per-iteration plans, transcripts, or evaluations — those lived in subagents and were summarized at the time. The final report is a thin ledger.

## Coordination Notes

- **Coord claims (per `/implement-plan` Step 0.6):** This skill operates on no specific plan file, so claim keys use `resource_key=manual-test-loop:session:<UTC-iso8601-start>`. Acquire once at loop entry, heartbeat from the main session every 30 min, release on exit. If `held` is returned, surface the holder and ask abort/wait/steal via `AskUserQuestion`. Skip-and-warn for non-coord environments (no `QONTINUI_MACHINE_ID`).
- **Memory pressure:** Each repo Agent should be launched in its own subagent — do NOT batch multiple repos into one Agent prompt. UI Bridge data per element is heavy; one repo per agent keeps each subagent's working set bounded.
- **Don't restart primary runners.** All test runners are temp runners via supervisor port 9875 with LKG-first. Per memory, "spawn temp runners — never block on primary."
- **Mobile target needs an active transport** — if `$ARGUMENTS` mentions mobile and no transport probe (USB/LAN/cloud) succeeds, manual-test will fall back to AAB artifact verification per its own gotcha doc. The loop trusts that and proceeds.
- **Cross-repo dependencies surface as DEFERRED items.** If a repo Agent reports `DEFERRED: blocked on <other-repo> change` for an item, the next iteration's test-and-plan subagent will rediscover the deficiency once the blocker lands — don't try to chain dependencies manually in the loop.

## Rules

- **NEVER call the loop "clean" on inferred verification** — the user's goal must be observed rendered on its page via the UI Bridge. Backend/API/DB/registration/coord/log evidence confirms plumbing, not the goal, and routinely disagrees with the page. A goal that can't be observed on the surface is a `blocked_task`, not a pass.
- **NEVER inline `/manual-test` or remediation work in the main context** — always via Agent. The main context is a thin coordinator.
- **NEVER ask the operator to confirm a fix, restart a service, or look at a log** — the loop is autonomous end-to-end.
- **NEVER use `--no-verify`, `core.hooksPath=/dev/null`, or any hook bypass** without explicit operator instruction. Surface hook failures verbatim and stop.
- **NEVER commit Claude / AI attribution** — qontinui's pre-commit hook blocks it; the commit subagent must omit those trailers.
- **NEVER restart, kill, or rebuild the primary runner** — always spawn temp runners via supervisor port 9875.
- **Edit work runs in an allocated worktree, never the primary checkout** — see "Worktree isolation — bound rule" under the Remediation subagent section. Sibling to the temp-runner rule: same shape ("never touch the shared primary"), different substrate (git worktree vs supervisor temp runner). Allocate it through `POST $COORD_HTTP_URL/agents/allocate` — see the bound rule for the response shapes and the one legitimate fallback. A plain `git worktree add` produces an undeclared worktree coord cannot attribute, pin, or drain (the HTTP allocate-local endpoint was removed in runner #443).
- **NEVER let one iteration's transcript leak into the next** — the test-and-plan subagent returns a fingerprint + table, not narration. If a subagent returns a multi-page transcript, summarize it down to the contract above in the main session before continuing.
- **Always ship after committing, but NEVER to the default branch directly** — per the autonomous-commit-ship feedback there's no "ready to push?" gating, but shipping means branch-first → push the branch → open the PR — and stop there. Coord is the sole merge authority for `qontinui/*` repos; the loop never merges its own PRs ([[feedback_no_direct_pushes_to_main_loops_use_branches]]). See the commit subagent's branch-first contract above.
- **Never end an iteration on a `bailout`- or ungated-`user_deflection`-shaped final paragraph** — record the `ending` in the ledger, then either run the next iteration or register a typed coord gate via `/blocked`. A loop that stops on a person nobody asked is an ungated blocked item, which policy forbids outright.
- **Never stop on an observable blocker without registering a gate** — if the deficiencies can't clear until a deploy goes green or a migration reaches head, run `/blocked` BEFORE stopping and name the `gate_id` in the handoff. The `bailout` check will not catch this one: waiting on a real signal is `waiting_on_signal`, which is legitimate — what makes it a defect is stopping there ungated. An unmerged PR is not such a blocker; the next iteration tests the branch.
- **Always release coord claims on exit** — try/finally semantics, even on abort.

## Focus

$ARGUMENTS
