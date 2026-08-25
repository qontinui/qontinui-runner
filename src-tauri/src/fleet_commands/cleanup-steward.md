---
description: Fleet cleanup steward — a visible, stoppable /loop session that measures the fleet's garbage (session worktrees, ad-hoc worktrees, stale worktree registrations, orphaned temp runners) against its OWN independent disk census, audits every shipped reaper for inertness (class 12), and abstains on every ambiguous signal. Report-mode by DEFAULT; destructive work is per-class opt-in and graduates only after a recorded shadow window. Consumes the shipped census/reclaim/sweep surfaces — never rebuilds them.
argument-hint: "[--mode=report|reap] [--classes=1,2,3,10,12] [--interval=15m] [--once] [--on-finding=plan|record] [--dry-run]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Grep, Glob, Monitor, Skill, ToolSearch, Agent
---

# Cleanup steward — measured, abstaining, report-first

This is `/merge-train-steward` for **garbage collection instead of PRs**, and its central
job is not deleting things. It is **verifying that the reapers which already exist actually
remove bytes** — the charter's rule 1 (*≥2 independent authoritative signals, never one
self-reported call*) applied to the cleanup subsystem itself.

**The finding this skill exists for.** Every shipped reaper on this fleet reports healthy
while reclaiming ~nothing. Between 2026-07-23 and 2026-07-27, with the reclaim pipeline
default-armed, coord reachable, the classifier taught the `qontinui-worktrees/` marker, a
freshness fix landed and declared *"output-verified"*, **6062 of 6100 session worktree dirs
were still on disk** — ≈38 removed in five days while the population grew to **6668** and
the workspace volume gained ~600 GB. Nothing alerted. **No shipped component's health signal is its own
output**; each reports on its inputs (armed? reachable? classified?), never on whether bytes
left the disk. Class 12 below is the detector for exactly that, and it is the highest-value
thing in this file.

**Do NOT build a sixth reaper.** The worktree class is architecturally complete — census,
ledger, five gates, retention pin, close disposition, executor, backstop, panel, gauges,
per-row freshness, batched ingest, a removal cap and an inertness counter. Consume it.
`cargo-sweep-all.ps1` already has the running-exe guard, the junction-unlink ordering and
the `target-pool` exclusion. Consume it. The steward adds **the cross-class verification
layer** and a handful of classes with no owning reaper.

**Coverage.** This file implements classes **1, 2, 3, 10, 12**. Classes 4, 5, 6, 7, 8, 9
and 11 are named as out-of-scope-for-now at the bottom, each with a pointer — the file is
honest about what it does not do.

## Fleet policy — the steward is a normal fleet session (applies EVERY iteration)

**The steward operates under the same fleet policy as every other session on this fleet.**
Nothing here narrows it, and where this doc and the policy disagree, **the stricter governs**.
Two sources, both authoritative:

1. **The autonomy charter** in `qontinui-claude-config/CLAUDE.md` ("Autonomous Operation").
   The clauses that bite hardest here: reads are free (rule 1) and verification needs **≥2
   independent authoritative signals**; do reversible mechanical work (2); closeout push
   authority for docs/plans (3); **exhaust the cascade before reporting blocked** (4);
   **silent-empty is UNKNOWN, not NO** (6) — this is the load-bearing rule for the entire
   file; **no silent drops** (7); escalation is a **CLOSED list** (8); consult policies
   before asking (9); **finish to zero** (10).
2. **The unified policy protocol**, served as coord prompt documents. Before substantive
   work, call `coord_list_prompt_documents`, fetch `policy/session-protocol` via
   `coord_get_prompt_document`, and follow it: classify each decision, **cite the clause you
   applied**, record a `POLICY_GAP` when none covers it, finish discovered follow-ups to
   zero, and close with a `POLICY_COMPLIANCE` footer. Read the category documents **fresh**.

Fetch the policies **once per session** (not per iteration — they are stable within a
session and re-fetching burns context) and **re-fetch on resume**.

**At any point you would ask the operator, offer instead of act, or stop short of something
you could execute — the policy documents decide it, not the operator.** Escalate only on a
hit in the closed list (Step 5), and surface it WITH a recommendation.

## Enablement gate + kill-switch (check FIRST, every iteration)

- **`QONTINUI_CLEANUP_STEWARD_ENABLED`** must be `1`/`true`. If unset or anything else, do
  nothing this iteration — print `steward disabled (QONTINUI_CLEANUP_STEWARD_ENABLED unset)`
  and stop. This is the fleet-wide off switch and it is checked **before any probe runs**,
  including read-only ones.
  **Read the DURABLE store, not just the process env — the process env is a stale
  snapshot and reading it alone made this gate lie for six consecutive iterations.**
  A tool child inherits its parent's environment *block*, captured when the parent
  started. Setting the flag after the session began — in `settings.json`, or with
  `[Environment]::SetEnvironmentVariable`, or by hand in another shell — does **not**
  reach the already-running session, so `$env:` reads empty while the durable value is
  `1`. The durable store is where the operator's intent lives; `$env:` is a cache of it
  that can be arbitrarily old.
  ```powershell
  # Durable value FIRST; the process env is only a (possibly stale) cache of it.
  $durable = [Environment]::GetEnvironmentVariable('QONTINUI_CLEANUP_STEWARD_ENABLED','User')
  $proc    = $env:QONTINUI_CLEANUP_STEWARD_ENABLED
  $en      = if (-not [string]::IsNullOrEmpty($durable)) { $durable } else { $proc }
  if ($en -ne '1' -and $en -ne 'true') {
      Write-Host "steward disabled (durable='$durable' process='$proc')"; return
  }
  # Print BOTH, always. A gate that reports only the value it acted on cannot be
  # audited for having read the wrong source -- which is exactly what happened.
  Write-Host "gate PASS (durable='$durable' process='$proc')"
  ```
  **Measured 2026-07-29 (the defect this replaces):** the flag had been written to
  `~/.claude/settings.json` while `$env:CLAUDE_CONFIG_DIR` pointed elsewhere, so the
  file Claude Code actually loads never carried it and every tool child saw empty.
  Six iterations printed `enabled=1` without that value having come from the
  fleet-wide off switch. **An off switch that cannot be read is not an off switch** —
  and a steward that would keep running after the operator flips it is the one
  failure this file cannot tolerate, because it is the failure that removes the
  operator's last resort.

  **A `$durable` that is empty while `$proc` is `1` is the reverse hazard and must
  NOT pass** — that is a flag the operator has *removed* from the durable store while
  a long-lived session still holds the stale `1`. The `if` above is written so the
  durable value wins whenever it is set to anything at all, including an explicit
  `0`; only a genuinely absent durable value falls back to the process env. Never
  reorder those two.

  Note the literal-match discipline: accept exactly `1` and `true`. Do not invent
  `on`/`yes` synonyms — coord's `RETENTION_STALE_OPEN_ENABLED` accepts only the literal
  string `"1"` (`qontinui-coord/src/retention_worker.rs:121-123`) and a mismatched synonym
  is how an arming flag silently never fires.
- **Stopping the visible session** (Ctrl-C / closing the terminal / interrupting the `/loop`)
  halts it. On stop, run the cleanup in the try/finally sense: release any coord claims this
  run holds, flush the ledger file, and leave no half-state — never a half-removed tree, never
  a `git worktree prune` interrupted between the registration read and the prune.
- **`--mode=report` is the DEFAULT** and prints what it *would* do, per class, mutating
  nothing. **`--mode=reap` without a `--classes=` allowlist is a NO-OP by design** — it
  arms nothing. And a class named in `--classes=` still only acts if the class-arming table
  below says `reap` for it. **There is no global arm flag and one must never be added:** the
  arming plan already litigated global-flag arming and lost —
  `COORD_WORKTREE_RECLAIM_ENABLED` sat OFF for seven weeks and then filled the disk. A
  single flag is right for a component with a deep guard stack; it is wrong for an LLM agent
  whose failure mode is misclassification, not misconfiguration.
- **`--on-finding`** (default **`plan`**) selects what happens when a class-12 detector
  fires. It is a **separate axis** from `--mode`: `--mode` governs whether the steward may
  touch the *fleet's* garbage, while `--on-finding` governs whether it may create *work*.
  Conflating them is why there was previously no way to measure without also authoring.
  - **`plan`** — the shipped behaviour and the one finish-to-zero wants: emit the coord
    finding, write the plan, run `Skill: vet-imp` on it (§4f).
  - **`record`** — **withholds the authoring step and nothing else.** Everything `plan` does
    up to that point still happens: the coord finding is posted, the ledger line is written,
    and a coord **gate** is registered on the already-established observable cause (§4f step 2
    record branch). It then prints the plan it *would* have written — filename, rationale,
    `gate_id` — and does not create the file or run `/vet-imp`. **Nothing is dropped** and the
    item still has a watcher (charter rule 7: no silent drops).

  **When `record` is the right choice.** Use it when a fire is *expected* and its cause is
  already known — a fix in flight, an upstream defect already planned, or a deliberate
  posture such as a reaper being intentionally unarmed. The worked example this flag was
  added for: on 2026-07-28 the worktree reaper was armed and removing nothing, which is a
  **correct** INERT verdict whose cause (`qontinui-runner` PR #895, the false-dirty
  scaffolding predicate) had already landed as `e861a2b02` and was merely not yet in the
  running binary. Under the default the steward would have authored a duplicate plan and
  burned a `/vet-imp` session re-diagnosing a solved defect. `record` lets you start the
  loop immediately — and start it *before* an intervention, which is the only way to get a
  pre-intervention lookback anchor — without that cost. **To promote a recorded finding to a plan,
  author it by hand from the printed WITHHELD line** (which carries the intended filename,
  the firing detector and the `gate_id`). Re-running under `--on-finding=plan` is **not** a
  promotion path: every iteration re-measures from scratch, so it authors a plan only if the
  detector fires *again* — and in the motivating case it deliberately will not, because the
  cause gets fixed. See §4f step 2.

- **`--dry-run`** is stricter than both, and is **not** the same thing as `record`. `record`
  withholds only the authoring; `--dry-run` additionally skips every *write* — the ledger
  line, the coord finding, and the gate — so the steward can be exercised with zero side
  effects of any kind. Because a run that suppresses the finding cannot author the plan that
  finding would justify, **`--dry-run` forces the authoring step off too**; passing
  `--dry-run --on-finding=plan` is a contradiction — refuse it at preflight with that reason
  rather than silently picking one. **A bare `--dry-run` RESOLVES `on_finding` to `record`** —
  state it that way, do not leave it implied. This matters because the record branch owns the
  print, the `$planDir` resolve and the gate: if a dry-run resolved to `plan` instead, §4f
  step 2's authoring guard would be true while authoring is forbidden, the record branch would
  never run, and a class-12 fire would produce **no plan, no print, no gate and no WITHHELD
  line** — a silent, unattributable no-op, which is exactly what this flag exists to prevent.
  Where the record print block would name a `finding_id` or a `gate_id`, a dry-run renders
  `finding SUPPRESSED (--dry-run)` / `gate SUPPRESSED (--dry-run)`; it must never invent
  either.

  **Why `--dry-run` is the wrong tool for a shadow window** — and note the naive reason is
  false. Dry-run suppresses ledger *writes*, not *reads*, and the class-12 anchor is read
  **from the ledger file**, so a dry-run against a *warm* ledger does find an anchor and does
  compute real rates (plain `INERT` carries no "sustained" qualifier and can fire in a single
  such iteration). The three real reasons are: (a) on a **cold** ledger a dry-run loop never
  accumulates an anchor, so every rate verdict is UNKNOWN forever; (b) it cannot persist the
  `PENDING-<verdict> (window k of 3)` accumulation that `INERT-BY-STARVATION` /
  `INERT-BY-RATE` require as *sustained*, so those two can never be reached across
  iterations; and (c) decisively, even when a verdict does fire it suppresses the finding and
  the gate, so nothing durable comes out. For a real shadow window use
  **`--mode=report --on-finding=record`**, not `--dry-run`.

## Class-arming table — the only thing that authorises a destructive verb

A class moves `report` → `reap` **only** after a recorded shadow window in which its
would-act set was inspected and matched expectation. Graduating a class is a **deliberate
edit to this table in a PR**, not a runtime flag. This is the `undeclared_remove_armed()`
graduation precedent applied per class.

| # | Class | Destructive verb (the ONLY one permitted) | Arming **today** | Graduation bar |
|---|---|---|---|---|
| 1 | Stale session worktree (`qontinui-worktrees/<uuid>/<repo>`) | remove the tree via the runner's WIP-safe executor | **report** (Phase 4, gated) — **streak BROKEN 2026-07-31, restart the count** | ≥7 consecutive iterations over ≥72 h where the would-remove set was **inspected** and contained **0 dirty**, **0 unproven-landed**, **0 canonical**; plus class 11 (WIP archive) live and verified. **An iteration in which the population read or the per-item state read was UNKNOWN inspected nothing and does NOT count** — a would-act set of 0 produced entirely by abstentions is not an inspected empty set. **it=166 put a canonical repo in the cleared set** (`qontinui-coord-wt-prcreate-fix`, a 26.5 GB real clone the runner's name-based classifier called a worktree), which violates the `0 canonical` conjunct outright. Under the DEFECTIVE classifier no number of further iterations could have produced a clean streak — the same clone re-entered the cohort on every rotation. The runner fix (structural `.git`-is-a-directory test + the INV-W5 refusal in `remove_worktree`) is the precondition; **the 7-iteration count restarts only once a runner carrying it is the serving build**, because evidence gathered against the old classifier does not describe the set this bar names |
| 2 | Ad-hoc worktree — `*-wt-*`, `wt-*`, `*_wt`, `<repo>/agent-worktrees/<uuid>` (see §2a for why this list, and why the OLD list was wrong) | remove the tree | **report** (Phase 4, gated) | class 1 graduated, **and** ≥7 iterations where 100 % of the would-remove set had proven provenance against a known canonical repo. **Same UNKNOWN exclusion as class 1.** Evidence gathered before 2026-08-01 does NOT count toward this bar — it was measured against the old pattern list and without the `agent-worktrees` arm, so it never covered the population the bar names |
| 3 | Stale `.git/worktrees` registration | `git worktree prune` (cannot destroy data by construction) | **reap** — GRADUATED 2026-07-29 | ≥3 consecutive iterations where the steward's own missing-`gitdir` set (computed per §2d — a `Test-Path` over each registration's recorded `gitdir`, **not** a count) **equals** `git worktree list --porcelain`'s `prunable` set, with zero unexplained members on either side; an iteration where either signal is UNKNOWN does not count. **AND** the §2d `prune --dry-run` preview check must have run green (`$previewOk` true, mapped sets equal) on those same iterations — it writes to **stderr** and keys on **admin-dir names**, so a naive implementation returns an empty set and abstains forever while looking healthy. **BAR MET — see the evidence block below this table.** |
| 10 | Orphaned temp/external runner | `POST http://localhost:9875/runners/<id>/stop` | **report** (Phase 3, gated) — **bar MET, arming DEFERRED to the operator, 2026-08-01** | an **empty-set graduation**: ≥3 iterations recording *verified safe, zero throughput* (see below). **An iteration whose supervisor `/runners` population read was UNKNOWN does NOT count** — an unreachable supervisor yields an empty list, and crediting that as a verified-safe iteration launders an UNKNOWN into graduation evidence for the one class whose verb stops processes. **The bar has been satisfied many times over** (it=165–171 and far earlier all recorded `3 entries, read OK, all excluded, verified safe, zero throughput`). It is NOT auto-graduated anyway: this class's verb stops PROCESSES, and served policy `production-and-cost` `runner-lifecycle` puts stopping a runner at **ask-first** — "only the user restarts an active runner". Arming an automated stopper is that authority granted standing, so it is an operator decision, not a steward one. Recommendation on the evidence: safe to arm (all three live entries are `protected: true` and the class abstains on `protected`, so arming is a no-op today and the guard is what makes it one — do not weaken it to gain throughput) |
| 12 | Reaper inertness (meta) | **none — finding + plan (or, under `--on-finding=record`, finding + gate) only** | **active, always** | n/a — class 12 has no destructive path and therefore never graduates |

**Class 10 is expected to be an empty set today and that is the correct outcome.** All three
live supervisor entries (`named-9879-…`, `runner-19e599d0619-1b`, `primary`) carry
`protected: true` — including the `external` one — and the class abstains on `protected:true`.
Ports 9879 and 9880 are confirmed not listening, so two of them genuinely are dead, and they
are still correctly excluded. **Record that as "verified safe, zero throughput" — but only
on an iteration whose `/runners` population read succeeded** (Step 3, Defect 0). A read that
returned UNKNOWN produces the same zero and means the opposite; it records `UNKNOWN` and
earns no graduation credit. **Do NOT fix any of this by weakening the guard.** Whether `protected` should default `true` for `external` runners
is a supervisor question, named as an unchased anomaly in the plan, not something to resolve
by relaxing a never-touch rule.

**Consequence of the table as it stands: `--mode=reap --classes=1,2,10` is a no-op; only
class 3 acts.** Classes 1, 2 and 10 remain `report`, so naming them arms nothing. Class 3
graduated 2026-07-29 on the evidence below — it is the **only** class this file can act on
today, and only when `--mode=reap --classes=3` names it explicitly.

### Class-3 graduation evidence (2026-07-29)

Recorded across `~/.qontinui/cleanup-steward/ledger.jsonl` iterations **10 → 17**, a
`--mode=report --on-finding=record` soak. **Eight consecutive** iterations satisfied the
full three-signal bar; the bar asks for three.

| Term | Every iteration it=10…17 |
|---|---|
| Signal A — own `gitdir` stat, no git process | `missing = 3` |
| Signal B — git's own `prunable` verdict | `prunable = 3` |
| Signal C — `prune --dry-run` preview | `previewOk` on **37/37** repos, `preview = 3` |
| A=B set equality (worktree-path keys) | `onlyA = 0`, `onlyB = 0` |
| A=C set equality (admin-dir keys, after mapping) | `onlyC = 0`, `onlyA-admin = 0` |
| Repos returning UNKNOWN | **0** |
| Non-vacuous | yes — a 3-member set, never an empty one |

The three registrations are the **same three paths every iteration**, all in `qontinui`:
`qontinui-worktrees/019f6165-…/qontinui`, `…/019f78f3-…/qontinui`, `…/019f7900-…/qontinui`.
Their admin-dir names are `worktrees/qontinui`, `worktrees/qontinui1`, `worktrees/qontinui2`;
the fourth admin dir (`qontinui-wt-redfix`) correctly maps to `missing=False` and never
appears in any of the three sets — so the signals agree on the exclusions as well as the
inclusions.

**Why this is safe to arm, stated in the terms the table cares about.** `git worktree prune`
removes only registrations whose recorded `gitdir` is already gone, so it **cannot destroy
data by construction** — there is no tree, no branch, no reflog behind those three entries to
lose. That is what makes class 3 the one class where an arming decision does not trade
irreversibility for throughput. Classes 1, 2 and 10 stay `report` precisely because their
verbs *can* lose something.

**What remains mandatory at act time**, unchanged by this graduation: re-read all three
signals in the acting iteration (never reuse these numbers), require the mapped sets equal
per §2d, run `prune` **per repo** rather than in a failure-swallowing loop, and ABSTAIN for
any repo whose Signal A, B or C read errored. A repo that goes UNKNOWN is skipped and
reported, never prune'd on the strength of this table.

## Step 0 — Preflight (once per session / per `/loop` spawn)

1. **Parse args.** `--mode` (default `report`), `--classes` (default `1,2,3,10,12` — the
   implemented set; naming an unimplemented class is an error, not a silent skip),
   `--interval` (default `15m`), `--once`, `--on-finding` (default `plan`; the only other
   value is `record` — anything else is an error, not a silent fallback), `--dry-run`.
   **Refuse `--dry-run --on-finding=plan`** as a contradiction, naming it: a run that
   suppresses the coord finding cannot author the plan that finding would justify. A bare
   `--dry-run` is fine and forces the authoring step off. **Echo the resolved `on_finding`
   value unconditionally in two concrete places: the `CLEANUP it=N` iteration header (beside
   `mode=`) and the JSONL ledger record** — not only when a fire is withheld. A reader must
   never have to infer from the absence of a plan whether the steward chose not to author one
   or merely failed to.
2. **Enablement gate** (above). Nothing below runs if it fails.
3. **Resolve every path at run time — hardcode nothing.** There is no absolute path
   anywhere in this file, deliberately: a hardcoded workspace root is wrong on every
   machine but one, and a hardcoded plans dir is unrecoverably destructive on this one.
   ```powershell
   # Workspace root = the parent of the MAIN checkout, worktree-safe. --show-toplevel
   # would give this worktree, not the workspace, whenever the steward runs from one.
   $common = (& git rev-parse --path-format=absolute --git-common-dir)
   $Root   = if ($env:QONTINUI_ROOT) { $env:QONTINUI_ROOT }
             elseif ($common) { Split-Path -Parent (Split-Path -Parent $common) }
             else { $null }
   if (-not $Root) { throw 'cannot resolve workspace root — abort, do not guess' }
   $WtRoot    = Join-Path $Root 'qontinui-worktrees'   # <workspace-root>/qontinui-worktrees
   $PlansDir  = $env:QONTINUI_PLANS_DIR          # HARD never-touch. See below.
   $PlansArch = $env:QONTINUI_PLANS_ARCHIVE_DIR  # optional, normally unset
   ```
   **`$QONTINUI_PLANS_DIR` is the single highest-blast-radius exclusion in this entire
   file.** As observed on the operator machine 2026-07-27 it resolves to a `plans` directory
   directly under the workspace root, holds **180 active plan `.md` files**, and **is not a
   git repo** — a deletion there is unrecoverable, with no remote and no reflog. It was
   historically mis-described as "un-owned litter"; that framing is itself now a hazard,
   because `2026-07-22-plan-discipline-user-defined-directories` SHIPPED 2026-07-23 and made
   it the *sanctioned active plans directory*. In-flight plans live there right now.
   **Resolve it from the environment at run time and never hardcode a path**; if
   `$QONTINUI_PLANS_DIR` is unset, treat *every* `plans`-named directory under `$Root` as
   never-touch rather than guessing, and say so in the report.
4. **Compute your OWN canonical-exclusion set. Do not inherit the endpoint's.**
   **A canonical checkout is a real clone — its `.git` is a DIRECTORY. A worktree's
   `.git` is a FILE.** `Test-Path` is true for both, so a bare `Test-Path` here does not
   compute the canonical set at all; it computes "every git thing under `$Root`". This is
   the same clone-vs-worktree distinction the never-touch list and the class-2 abstain
   rule already draw — it has to be applied *here*, where the set is built.
   ```powershell
   # Three-way split in ONE pass. -Force twice, for two different reasons:
   #  - Get-ChildItem -Force: enumerate hidden/system dirs under $Root.
   #  - Get-Item -Force: `.git` is HIDDEN on Windows. WITHOUT -Force Get-Item returns
   #    nothing for EVERY entry — including real clones — so `.PSIsContainer` reads
   #    falsy and the canonical checkouts silently fall OUT of the never-touch set.
   #    That is a worse fail-open than the bare-Test-Path bug. (Test-Path DOES see
   #    hidden items, which is exactly why the original bug looked like it worked.)
   $Canonical = @()   # real clones      — unconditional never-touch
   $Worktrees = @()   # .git is a file   — class 2's population
   $Unclassifiable = @()   # .git unreadable — UNKNOWN, treated as never-touch
   $ExclusionSetOk = $false
   try {
       foreach ($d in @(Get-ChildItem -LiteralPath $Root -Directory -Force -ErrorAction Stop)) {
           $dotgit = Join-Path $d.FullName '.git'
           if (-not (Test-Path -LiteralPath $dotgit)) { continue }   # not a git thing at all
           $g = Get-Item -LiteralPath $dotgit -Force -ErrorAction SilentlyContinue
           if ($null -eq $g) { $Unclassifiable += $d.FullName; continue }
           if ($g.PSIsContainer) { $Canonical += $d.FullName } else { $Worktrees += $d.FullName }
       }
       $ExclusionSetOk = ($Canonical.Count -gt 0)
   } catch {
       $ExclusionSetOk = $false
   }
   if (-not $ExclusionSetOk) {
       throw 'never-touch set is UNKNOWN (enumeration failed or zero clones found) — ABORT the iteration; do not run any class'
   }
   ```
   **The `try`/`catch` and the `$ExclusionSetOk` assertion are load-bearing, and this block
   fails LOUDER than any other in the file.** `-ErrorAction Stop` *throws*, and the
   accumulators are pre-initialised to empty — so an unwrapped failure, or an agent that
   catches the error and carries on, would leave `$Canonical` **empty**, i.e. an **empty
   never-touch set**. That is strictly worse than the `pop = UNKNOWN` case §2a guards
   against: an empty exclusion set is *permissive*, not merely unknown, and every class
   would then run with the 37 canonical checkouts unprotected. **Zero clones under `$Root`
   is never a legitimate result on this fleet** — it means the enumeration failed or
   `$Root` resolved somewhere wrong (a stale `$QONTINUI_ROOT` is the easiest way to cause
   it) — so it aborts the whole iteration rather than degrading.

   **`$Unclassifiable` fails CLOSED: it joins the never-touch set, never the reapable
   one.** An entry whose `.git` cannot be stat-ed is UNKNOWN (charter rule 6), and an
   unclassifiable dir must never become reap-eligible. The lone `-ErrorAction
   SilentlyContinue` in this file is admissible *only* because its empty result is
   routed to UNKNOWN rather than trusted as a value — and because the count is then
   surfaced. **Print `unclassifiable N` in the ledger every iteration, including when N is
   0** — an unconditional zero proves the probe ran, whereas a line that appears only when
   non-zero is indistinguishable from a probe that never executed. A systemic stat failure
   (the `-Force` bug above makes it 100 %) must be visible, not swallowed (Step 1's
   `:224` ban).

   Measured on this machine 2026-07-28: **341** dirs under `$Root` have a `.git` of some
   kind = **37 real clones** + **304 worktrees** + **0 unclassifiable**. (The worktree term
   drifts by a few between reads — the population grows ~266 dirs/day *by design*; **37** is
   the stable one. Re-read at act time, as always.) A bare `Test-Path` would put all 341 in
   `$Canonical` — a ~9× over-count with three consequences, each of which the fix resolves:
   it makes **class 2 a permanently empty set** (**280** of those worktrees match class 2's
   own `-wt-` / `_wt-` / `dn-wt-` patterns and would land in the unconditional never-touch
   set, contradicting the arming table and the plan's scope boundary — the class is not
   merely reduced by the bug, it is entirely swallowed); it makes **class 3 iterate 341
   repos instead of 37** (~20–25 min/iteration on a 15 m loop) while **double-counting
   registrations** (`qontinui-claude-config` and each of its own `ccfg-wt-*` worktrees
   all report the same 46); and it falsifies **§4d's calibration** with its own snippet.

   Then union in the unconditional never-touch set below. `canonical_excluded` on the
   reclaimable endpoint fell **30 → 1** between 2026-07-23 and 2026-07-27 (a prefix-stall
   artefact, not a degraded exclusion set) — which is exactly why the steward must compute
   its own. **A disagreement between your set and the endpoint's `canonical_excluded` is a
   class-12 finding**, reported, never reconciled away.
5. **Never-touch set — unconditional, every class, every mode:**
   - `$QONTINUI_PLANS_DIR` (and `$QONTINUI_PLANS_ARCHIVE_DIR` if set) — see 3.
   - every path in `$Canonical` (the real clones — **37** measured 2026-07-28) and every
     path in `$Unclassifiable` (UNKNOWN fails closed). **Not** `$Worktrees`: those are
     class 2's population, and folding them in here is the over-count fixed in 4.
   - `target-pool/**` in its entirety — the supervisor's build pool, already excluded whole
     by `cargo-sweep-all.ps1:179`; `lkg` holds the last-known-good binary the running
     supervisor depends on.
   - the **primary** runner and **every named** runner (`kind.type` ∈ {`primary`, `named`}),
     and anything with `protected: true`.
   - `node.exe`, `powershell.exe` — killing either terminates Claude Code sessions.
   - `supervisor.exe`'s own `target/debug`.
   - any path whose `.git` is a **directory** (a real clone, possibly with unpushed history)
     rather than a file.
   - any path containing a reparse point / junction — **unlink only, never recurse through**.
6. **Coord access cascade.** Resolve the coord base (`$COORD_HTTP_URL`, else
   `https://coord.qontinui.io`). Prefer the native `coord_*` MCP tools; enumerate
   `tools/list` at preflight and dispatch on what is actually deployed. If a `coord_*` tool
   is **absent**, run `/gate`'s transport cascade. If a tool **was visible and returned
   `"Command failed with no output"`**, that is a dead cached transport, not a masked tool —
   run `/coord-revive`, and **presume any write that returned "no output" LOST**; re-issue
   over the reported door and verify by read. Do not report blocked without naming the
   exhausted cascade (charter rule 4).
   **Coord unreachable is not "nothing to clean".** Classes 1, 2 and 12's coord arm all
   ABSTAIN, and the report says so explicitly.
7. **Open the ledger file.** `~/.qontinui/cleanup-steward/ledger.jsonl` (outside every repo,
   so a cleanup pass can never delete its own audit trail; create the directory if absent).
   One JSON line per iteration, each field carrying its own `observed_at`. This file is the
   **only** legitimate source of a *window-start* value; everything else is re-read now.
   **Every record MUST carry `mode`, `on_finding`, and each fired detector's
   `window_count`** (the k of the `PENDING-<verdict> (window k of 3)` accumulation) — the
   "sustained over ≥3 windows" bar is only meaningful if k persists across iterations, and a
   count that lives solely in printed output cannot. — a successor reading a zero-fire
   iteration cannot otherwise tell "record mode, nothing fired" from "plan mode, nothing
   fired" from "a withheld fire whose parenthetical was dropped". Those three are the same
   bytes without it.
8. **Cost budget.** Per class, per iteration: at most **3 external reads**, each hard-capped
   at **60 s** (Step 1's `Read-Bounded`). The budget counts **external** (HTTP) reads; local
   disk enumeration is governed by the depth rule instead. The steward's own disk census is
   **one level deep and never recursive**, with exactly one sanctioned exception: the husk
   pass in §2a looks one level further, at each session dir's children, and is bounded by a
   **lazy enumerator that stops at the first child** — cost O(session dirs), not O(tree),
   measured at 1.3 s over 6697 dirs. **`-Recurse` remains banned outright.** The steward's
   scan must never become the 6.7 h census walk. Never call the runner's `build_census`
   inline; that is the exact defect `f311a78fc` fixed.

## Step 1 — The bounded-read contract (read this before any probe)

**Every external read is hard-timeout-wrapped, and a timeout is `UNKNOWN`/ABSTAIN — never
`0`, never "nothing to do."** This is not polish. Measured live on 2026-07-27, three
successive calls to the same endpoint returned in **~3 s (7027 B)**, then **213 s (8555 B)**,
then **timed out at 400 s with 0 bytes**. An unwrapped read hangs the steward; a timeout
silently coerced to an empty item list makes it report *"nothing to clean"* while **6668
dirs sit on disk** — reproducing, inside the verifier, the exact self-deception it exists to
detect.

```powershell
# The ONLY sanctioned way this skill reads an HTTP surface.
function Read-Bounded {
    param([string]$Url, [int]$TimeoutSec = 60, [string]$Label = '')
    $tmp = Join-Path $env:TEMP ("cs-" + [guid]::NewGuid().ToString('N') + ".json")
    try {
        $code = & curl.exe -s -S --max-time $TimeoutSec -o $tmp -w '%{http_code}' $Url
        $rc = $LASTEXITCODE
        # A HANDLED failure must not leave a non-zero exit code behind: the PowerShell
        # tool reads $LASTEXITCODE and would mark the WHOLE call failed, turning an
        # abstention this function handled correctly into a spurious tool error.
        $global:LASTEXITCODE = 0
        if ($rc -ne 0) {
            return @{ status='UNKNOWN'; label=$Label; reason="curl exit $rc after ${TimeoutSec}s (28=timeout, 7=refused)"; body=$null }
        }
        if ("$code" -ne '200') { return @{ status='UNKNOWN'; label=$Label; reason="HTTP $code"; body=$null } }
        $len = (Get-Item -LiteralPath $tmp).Length
        if ($len -eq 0) { return @{ status='UNKNOWN'; label=$Label; reason='HTTP 200 with 0 bytes'; body=$null } }
        return @{ status='OK'; label=$Label; reason="$len bytes"; body=(Get-Content -Raw -LiteralPath $tmp) }
    } finally {
        # Always reclaim the temp file — including on every UNKNOWN return above.
        # At ~4 reads/iteration on a 15 m loop this is ~380 files/day; a cleanup
        # steward that litters its own temp dir is not one you should trust.
        # No -ErrorAction suppression: a failure to clean up must stay visible.
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
    }
}
```

`$Label` is carried into every return value so the ledger can name the probe that
produced an UNKNOWN without the caller having to remember which URL it passed.

Rules that go with it, each of which exists because its absence produced a confidently
wrong number:

- **`status='UNKNOWN'` propagates.** Every item whose class depended on that read abstains,
  and the ledger prints `UNKNOWN (<reason>)` in the cell — never a blank, never a zero.
- **Never suppress an error into a value.** No `cmd 2>$null`, no `-ErrorAction
  SilentlyContinue` on a probe whose emptiness you would trust, and never the bash idiom
  `grep -c … || echo 0` — it converts a broken probe into a confident zero and the guard
  then only ever breaks on genuinely-empty input, which looks identical to working.
- **`HTTP 200` with a body is still not a value** if the body is a JSON error envelope.
  Parse, then check the shape you expect exists; a missing field is UNKNOWN.
- **PowerShell 5.1 only** on this box: no `&&`/`||` pipeline chains, no ternary, no `??`.
  Use `; if ($?) { … }`. Do **not** redirect a native exe's stderr with `2>&1` — 5.1 wraps
  each line in an ErrorRecord and flips `$?` to `$false` on a successful exit 0.
- **Locale trap.** This machine's `netstat` is German (`ABHÖREN` for LISTENING). A grep for
  English state names silently returns nothing for a bound port and reads as "no listener".
  Use `Get-NetTCPConnection`, which is locale-independent.
- **MSYS path mangling.** Set `MSYS_NO_PATHCONV=1` for `gh`/`aws` in Bash, but **not** for
  `git -C /d/...` — it breaks those.

## Step 2 — Fleet scan: the steward's OWN census is the population

**NEVER trust `items` from `GET /agent-worktrees/reclaimable` as the population.** On
2026-07-27 the endpoint reported **13 items total and 0 under `qontinui-worktrees/`** while
disk held **6668** session dirs. The 13 were the first 14 rows of a census walk that began
21 minutes earlier and would never finish (`items` derives exclusively from the runner's
in-process `LATEST_CENSUS` cell, `on_demand.rs:602` ← `census.rs:207`, reset to `None` on
every process start and rebuilt at ~1 row/minute against a ~6900-row population).

**The endpoint is for PER-ITEM STATE ONLY. The population count comes from your own disk
walk.** A disagreement between the two is a **class-12 finding**.

### 2a. Own disk census (classes 1, 2 — population term, and class 12's independent authority)

The `try`/`catch` is **in** the snippet, not a sentence after it: `-ErrorAction Stop` makes
`Get-ChildItem` *throw*, so an unwrapped block aborts the iteration instead of emitting the
`pop = UNKNOWN` this section requires.

```powershell
# One level deep. NEVER -Recurse. -ErrorAction Stop so a failure raises instead of
# returning an empty set that would read as "population 0, nothing to clean".
$pop = 'UNKNOWN'; $popReason = ''; $husks = 'UNKNOWN'; $unreadable = 'UNKNOWN'
$older1d = 'UNKNOWN'; $older7d = 'UNKNOWN'; $older14d = 'UNKNOWN'; $walkMs = 'UNKNOWN'
try {
    $t0 = Get-Date
    $dirs = @(Get-ChildItem -LiteralPath $WtRoot -Directory -Force -ErrorAction Stop)
    $pop  = $dirs.Count
    $now  = Get-Date
    $older1d  = @($dirs | Where-Object { $_.LastWriteTime -lt $now.AddDays(-1)  }).Count
    $older7d  = @($dirs | Where-Object { $_.LastWriteTime -lt $now.AddDays(-7)  }).Count
    $older14d = @($dirs | Where-Object { $_.LastWriteTime -lt $now.AddDays(-14) }).Count

    # Husks + unreadable, in the SAME pass. This is the one depth-2 read in the file and
    # it is bounded by construction: EnumerateDirectories is lazy and MoveNext() stops at
    # the FIRST child, so cost is O(session dirs), not O(tree). Still never -Recurse.
    # Measured 2026-07-28: 6697 dirs -> 493 husks, 0 unreadable, 1.3 s.
    $husks = 0; $unreadable = 0
    foreach ($d in $dirs) {
        $hasChild = $null
        try {
            $en = [System.IO.Directory]::EnumerateDirectories($d.FullName).GetEnumerator()
            try { $hasChild = $en.MoveNext() } finally { $en.Dispose() }
        } catch { $hasChild = $null }
        if ($null -eq $hasChild) { $unreadable++ } elseif (-not $hasChild) { $husks++ }
    }
    $walkMs = [int]((Get-Date) - $t0).TotalMilliseconds
} catch {
    $popReason = $_.Exception.Message
}
```
On catch every term above stays the string `UNKNOWN` and the ledger prints
`pop = UNKNOWN (<$popReason>)`. **A failed census is UNKNOWN. It is never 0, and it never
satisfies class 12's population term** — a class-12 verdict computed against an UNKNOWN
population is itself UNKNOWN. `unreadable` is a **counted** term, not a decorative zero:
a non-zero value means the husk pass could not classify that many dirs, and those dirs are
excluded from the husk count rather than assumed non-husk.

**Empty husks** are the session dirs holding **no repo subdirectory** — the `$husks` term
above; **493** measured 2026-07-27 and re-measured 2026-07-28. They are invisible to the
runner census *by construction* — `qontinui-worktrees/` is never scanned directly
(`census.rs:1373-1399` walks only top-level `qontinui-*` dirs containing `.git`); session
worktrees are reached solely via `git worktree list`, so a dir with no repo subdir has no
registration and can never be surveyed or removed by that pipeline. **This is a permanent
leak, not a backlog** — report it as its own line and as a class-12 coverage gap, never
folded into the reclaimable backlog.

Ad-hoc worktrees (class 2) are counted from **`$Worktrees`** (Step 0.4) — the `$Root`
entries whose `.git` is a *file* — filtered to the pattern list below, plus a second
bounded pass for `<repo>/agent-worktrees/<uuid>`. They are by definition **not** in
`$Canonical`; if this count is 0 while `$Worktrees.Count` is large, the Step 0.4
discriminator has regressed and that is itself the finding.

**The pattern list, and why it is what it is (rewritten 2026-08-01 on soak evidence).**
The old list — `<repo>-wt-*`, `_wt-*`, `dn-wt-*` — was wrong in two independent ways, both
measured over iterations 165–172:

| Pattern | Matches | Why it is in the list |
|---|---|---|
| `*-wt-*` | `qontinui-runner-wt-x`, `ccfg-wt-y`, `rn-wt-860`, `runner-wt-lintfix` | The `<repo>-wt-*` reading required the prefix to be a REAL repo name, which orphaned every abbreviated nickname (`ccfg`, `cfg`, `coord`, `dev-notes`, `qcc`, `qdn`, `qweb`, `rn`, `runner`, `web`) — 57 trees, 2.4× more outside class 2 than the loose reading. **Coord settled it, not taste:** it cleared `runner-wt-lintfix` and `rn-wt-860` for removal, and no repo is named `runner` or `rn`. Coord's own pipeline treats the loose form as reclaimable, so the loose form is normative and the strict one would leave 55 trees unowned that coord is actively reaping. |
| `wt-*` | `wt-806`, `wt-coord-1108`, `wt-schemas-113` | A bare `wt-` prefix matched NOTHING in the old list. These are the **dominant arrival shape**: 9–10 of every 11–13 trees created in the last 60 min across it=169–171 (77–82 %). A class that cannot see 80 % of arrivals is not covering the population. |
| `*_wt` | `tmp_shadow_redrive` | `_wt-*` needed a TRAILING hyphen, so a `_wt` suffix matched nothing either. |
| `<repo>/agent-worktrees/<uuid>` | 45 trees across 8 repos (coord 17, runner 14, web 7, claude-config 4, +4 singletons), measured 2026-07-31 | Listed in the arming table and the §2c signal rows since the beginning, and **never once rendered** — see the mandatory second pass below. |

Anything under `$Root` that matches none of these is `unpatterned` — **list the members by
name, not just the count** (charter rule 7). Printing `unpatterned N` for 100+ iterations
without naming them is how a 13-tree bucket that no class can reach stayed invisible.
Derive `unpatterned` as `$Worktrees.Count` minus the matched count within the iteration,
never by carrying a number forward from this file.

**The `<repo>/agent-worktrees/<uuid>` arm is MANDATORY and needs its own pass.** Step 0.4
enumerates only depth-1 children of `$Root`, so a depth-3 path can never appear in
`$Worktrees` — for 170+ iterations that sub-population read as absent because it was never
looked at. Run a second bounded pass over `$Canonical`'s `agent-worktrees` dirs every
iteration and add it to the class-2 population (2026-07-31: 282 depth-1 + 45 = **327**, not
282). If the pass cannot run, print
`agent-worktrees/<uuid>: NOT MEASURED (pass failed: <reason>)`. **Never print a bare 0 for
it** — that is the silent-empty-as-NO failure this whole file rejects, and it is exactly
what a missing pass looks like from the outside.

### 2b. Per-item state (consume; never re-derive)

| Surface | Owner | Verified at | Used for |
|---|---|---|---|
| `GET http://localhost:9876/agent-worktrees/reclaimable` | **runner** | `qontinui-runner/src-tauri/src/mcp/agent_worktrees.rs:81` | per-item `reapStatus`, `is_dirty`, byte sinks, `census_*` fields, `poller.*` (executor health). **Not** the population. **Read the payload contract below before parsing it.** |
| `GET /coord/sessions/worktrees` | **coord** (not the runner) | `qontinui-coord/src/routes.rs:1532`, impl `src/session_worktrees.rs:775-809` | Signal C: session liveness / claims |
| MCP `coord_session_worktrees` | **coord MCP** (not runner MCP) | `qontinui-coord/src/mcp/tools.rs:9984` | same, via MCP |
| `GET http://localhost:9875/runners`, `POST /runners/<id>/stop` | **supervisor** | `qontinui-supervisor/src/server.rs:1004`, `routes/runners.rs:975` | class 10 |
| `coord_query_metric` | **coord MCP** | `qontinui-coord/src/mcp/tools.rs` `QUERY_METRIC_ALLOWLIST` | class 12 gauges. **Use this, not `curl /metrics`** — but it serves only ALLOWLISTED series, and a rejection reads identically to a misspelled name. Re-probe; never conclude "the series does not exist" from one rejection |
| `scripts/cargo-sweep-all.ps1` | claude-config | PR #130, merged | class 6's owner — **named only; never invoked by this file** (out of scope, no arming row) |

**Three inference traps, each of which produced a WRONG claim during the it=165–172 soak.**
They are not about any one surface — they are about reading a number as more than it is.

1. **`census_age_secs` is the age of the NEWEST merged chunk, not of the rows you are
   holding.** A snapshot carries rows from several chunks, so `read − age` is a LOWER bound
   on a cohort's start and `+900 s` an UPPER bound on its expiry. A cohort departing EARLY is
   therefore expected and refutes nothing; only a cohort arriving LATE would refute the
   ceiling, and none ever has. Two tight confirmations (13 s, 29 s) came from cohorts that
   happened to sit in the newest chunk — do not generalise the lucky case into a point
   prediction.
2. **A process count is a POINT SAMPLE and cannot attribute an INTERVAL.** `cargo`/`sccache`
   counts moved 1 → 63 within ten minutes. Sampling them once and using it to explain a
   whole interval's disk delta is not an attribution. Either bracket the interval with
   samples at both ends, or say "mechanism, not measured".
3. **A wall-time that moves is not automatically an early warning.** Class 3's slowest-repo
   time went 9.6 s → 19.9 s → 4.4 s across three iterations while registrations barely moved
   (1222 → 1227 → 1227); the 19.9 s coincided with an active build burst. §2d wants that
   number because a 1064-entry pileup once made `git worktree list` unusable — a real
   historical cause — but at ~1200 registrations the reading is dominated by concurrent
   cargo I/O. Cross-check against the registration count before reading a jump as a pileup,
   and never fire a warning on wall time alone.

**Payload contract for `/agent-worktrees/reclaimable` — every field here has produced a
false zero.** Getting one of these wrong does not error; it silently yields `0`, and a `0`
from this endpoint reads exactly like "nothing to clean". Four separate false zeros in one
soak came from this contract alone (it=169).

| Trap | Wrong | Right |
|---|---|---|
| Envelope | `json.items` on the top level → `0` against a 687 KB body | The body is `{success, data:{…}}` — read **`data.items`** |
| Item path key | `item.path` → `under_root 0` against 1330 items, and it silently made the real-clone safety check **vacuous** | The key is **`worktree_path`** |
| Grep phrases | Guessing a log phrase and reading `0 matches` as recovery | Grep a string you have SEEN in the log, then confirm the file has any lines at all |

**Rule this establishes: a `0` is only a fact once you have shown the probe can return
non-zero.** Every one of the four was caught by an adjacent number being absurd, never by
the check itself — so pair each count with a total it must be consistent with, and treat a
count that cannot be cross-checked as UNKNOWN.

**`coord_reachable` / `remove_armed` describe the REQUEST YOU JUST MADE, not the executor.**
Both read `true` throughout the 2026-07-28 → 08-01 window while the background reclaim
poller failed 100 % of its pulls for five days — because this endpoint pulls coord itself,
successfully, on every call. Nothing can be removed by that success. **Read `data.poller`
instead** (`consecutive_failures`, `last_success_unix`, `last_error` — added 2026-08-01,
`agent_worktree/reclaim.rs::poller_health`): it reports the executor's OUTPUT.
`consecutive_failures: 0` **with a recent `last_success_unix`** is the only combination
that means the reclaim pipeline is alive. On a runner built before 2026-08-01 the field is
absent — that is UNKNOWN, not healthy, and the log fallback is
`grep 'RECLAIM POLLER DOWN'` plus a count of `worktree_reclaim: GET` WARN lines against
INFO lines.

**Coord metrics: use the `coord_query_metric` MCP tool, never `curl https://coord.qontinui.io/metrics`.**
That URL returns **HTTP 403** to the steward. **ALB trap:** sending an `Authorization`
header to `/metrics` *also* 403s and mimics auth drift — you will misdiagnose a working
credential as expired. `coord_query_metric` reads the in-process registry and sidesteps the
ALB entirely.

### 2c. The ≥2-signal gate (every class, every item, re-read at ACT time)

At least two signals **from different authorities** (disk / git-remote / coord / OS-process),
each re-read at act time. **Cached census informs display only; it never authorises a
deletion.** Any probe that errors or returns silent-empty makes that item ABSTAIN.

| # | Class | Signal A (disk) | Signal B (independent) | Signal C (conjunction) | Abstain when |
|---|---|---|---|---|---|
| 1 | Stale session worktree | `git -C <wt> status --porcelain` empty, run **now** | Content on `origin/main`: `git merge-base --is-ancestor <wt HEAD> origin/main`, **or** grep the distinctive symbol on `origin/main` | No live `ClaimKind::Worktree` on the path **and** the uuid absent from the runner's live-session set **and** no OS process with a handle/cwd in the tree | dirty · not an ancestor and no content proof · any live claim · mtime < 24 h · **any** probe errored · `.git` is a directory · path is canonical |
| 2 | Ad-hoc worktree | as #1 | as #1 | **Provenance:** appears in `git -C <canonical> worktree list --porcelain`, or its `.git` **file** points into a known canonical repo | as #1, plus unregistered *and* `.git` is a directory · any reparse point on the path |
| 3 | Stale `.git/worktrees` registration | the steward's **own** `Test-Path` over each registration's recorded `gitdir` target, read from `<git-common-dir>/worktrees/*/gitdir` — **no git command involved** | `git worktree list --porcelain` reports it `prunable` — git's own verdict | **MANDATORY for the armed class:** `git worktree prune --dry-run -v`'s own preview set, mapped into Signal A's admin-dir key space per §2d, equal to A's | the directory exists (then it is class 1/2) · **any** of the three reads errored (including `$previewOk` false) · **any** of the three **sets** disagree. This is the only armed class — its Signal C is not optional corroboration, and A=B alone never authorises a prune |
| 10 | Orphaned temp/external runner | supervisor `/runners` state — **see the five defects below** | **CONJUNCTION:** a **resolvable pid that is not a live process** (`pid:null` ⇒ UNKNOWN ⇒ abstain) **AND** its **port not listening** (`Get-NetTCPConnection` — the only genuinely *independent* authority). Both must hold | — | **the `/runners` population read itself UNKNOWN ⇒ the WHOLE class abstains** · then see below; the list is long and every entry is load-bearing |
| 12 | Reaper inertness | steward's own re-counted population, this pass vs the stamped lookback-anchor ledger line | removals attributed to each reaper over the same window | reaper's self-reported arming/health (context only, never authorising) | population UNKNOWN · no ledger line ≥1 h old · the **removals counter** UNKNOWN. **Not** the reclaimable endpoint: the rate bar (§4a) does not depend on it, and abstaining when it times out would kill the detector on exactly the iterations its own flakiness makes most likely |

**Signal C in classes 1–2 is a CONJUNCTION, not a disjunction.** Zero live claims plus a
resolver miss **does not** mean a dead session — a claim lapse is expected. Verified TTLs
(`qontinui-coord/src/claims.rs:287` `default_ttl_for`): **Worktree 300 s** (`:289`),
**Symbol 300 s** (`:306`), **Session 180 s** (`:313`), **BranchName 1800 s** (`:291`).

**Lapsed coord claims are permanently report-only (class 7) and may only LOWER priority,
never authorise.** A lapse is never an input to a destructive decision.

**Two disagreeing signals are the most valuable thing on screen: re-probe both, never pick
the convenient one.**

**Never use `gh pr view` state to establish "landed".** A coord land reads `closed,
merged=false` when the rebase rewrote the sha but **`MERGED`** when it did not (both are
coord lands — `knowledge-base/qontinui-specific/coord-ff-lands.md`), and the phantom-kill bug
leaves landed proposals `OPEN`. No value of `state`/`merged` settles it in either direction.
Content on `origin/main` only — `git merge-base --is-ancestor`, or a symbol grep. Note the
ancestry probe is **one-way**: it passes on a sha-preserving land and fails on a
sha-rewriting one, so a failed probe is not evidence of "unlanded" — fall through to the
symbol grep, never to a removal.

### 2d. Class 3 — stale `.git/worktrees` registrations (own count + git's own verdict)

Two authorities, both cheap, and the whole point is that they must **agree**.

**They must also actually BE two authorities.** Deriving both terms from a single
`git worktree list --porcelain` read is precisely the defect this file diagnoses for
class 10 at "Defect 3" below — one signal wearing two names. So Signal A is computed
**without invoking git at all**: the steward reads git's on-disk registration records
itself (`<git-common-dir>/worktrees/<name>/gitdir`, each a text file holding the absolute
path of that worktree's `.git`) and stat-s the target. Signal B is git's own `prunable`
verdict. **Both sets are keyed on the worktree path, so they are directly comparable** —
which is what the graduation bar's *set equality* requires and what a pair of bare counts
could never establish.

```powershell
foreach ($repo in $Canonical) {
    $t = Get-Date

    # --- Signal A: the steward's OWN filesystem read. No git process is involved.
    # Test-Path DOES see hidden items (unlike Get-Item without -Force), so the hidden
    # `.git` targets recorded in these gitdir files stat correctly here.
    $missing = New-Object 'System.Collections.Generic.HashSet[string]'
    # Same set, keyed the OTHER way -- admin-dir names, for the Signal C comparison.
    $missingAdmin = New-Object 'System.Collections.Generic.HashSet[string]'
    $aOk = $true
    $common = (& git -C $repo rev-parse --path-format=absolute --git-common-dir)
    if ($LASTEXITCODE -ne 0 -or -not $common) { $aOk = $false }
    $global:LASTEXITCODE = 0   # handled -> do not leave the tool call marked failed
    if ($aOk) {
        $admin = Join-Path $common 'worktrees'
        if (Test-Path -LiteralPath $admin) {
            $entries = $null
            try { $entries = @(Get-ChildItem -LiteralPath $admin -Directory -Force -ErrorAction Stop) }
            catch { $aOk = $false }
            if ($aOk) {
                foreach ($e in $entries) {
                    $gf = Join-Path $e.FullName 'gitdir'
                    # An admin dir with no gitdir file is anomalous -> UNKNOWN, not "fine".
                    if (-not (Test-Path -LiteralPath $gf)) { $aOk = $false; break }
                    $gd = (Get-Content -Raw -LiteralPath $gf)
                    if ($null -ne $gd) { $gd = $gd.Trim() }
                    # An EMPTY gitdir file is corruption -> UNKNOWN. Without this guard
                    # `Test-Path -LiteralPath ''` raises a TERMINATING binding error and
                    # aborts the whole class mid-repo -- and an empty gitdir is exactly
                    # the corruption this class exists to notice.
                    if ([string]::IsNullOrWhiteSpace($gd)) { $aOk = $false; break }
                    if (-not (Test-Path -LiteralPath $gd)) {
                        [void]$missing.Add((Split-Path -Parent $gd).Replace('\','/').TrimEnd('/').ToLowerInvariant())
                        # RETAIN the admin-dir name too. Trap 2 below is a KEY-SPACE
                        # mismatch, and it cannot be fixed downstream: `prune --dry-run`
                        # emits admin-dir names, and once this loop has discarded $e.Name
                        # there is nothing left to map them onto. Signal A is the only
                        # place that holds both halves of the correspondence.
                        [void]$missingAdmin.Add('worktrees/' + $e.Name)
                    }
                }
            }
        }
    }

    # --- Signal B: git's OWN prunable verdict.
    $prunable = New-Object 'System.Collections.Generic.HashSet[string]'
    $regs = 0; $bOk = $true; $curWt = $null
    $po = & git -C $repo worktree list --porcelain
    if ($LASTEXITCODE -ne 0) { $bOk = $false }
    if ($bOk) {
        foreach ($line in $po) {
            if ($line -like 'worktree *') {
                $curWt = $line.Substring(9).Trim().Replace('\','/').TrimEnd('/').ToLowerInvariant()
                $regs++
            } elseif ($line -like 'prunable*' -and $curWt) { [void]$prunable.Add($curWt) }
        }
    }

    $secs = [math]::Round(((Get-Date) - $t).TotalSeconds, 1)
    if (-not $aOk -or -not $bOk) {
        "$repo : UNKNOWN (signalA_ok=$aOk signalB_ok=$bOk) walk=${secs}s"
    } else {
        $onlyA = @($missing  | Where-Object { -not $prunable.Contains($_) }).Count
        $onlyB = @($prunable | Where-Object { -not $missing.Contains($_)  }).Count
        $agree = (($onlyA -eq 0) -and ($onlyB -eq 0))
        "$repo : regs=$regs missing-gitdir=$($missing.Count) prunable=$($prunable.Count) agree=$agree onlyA=$onlyA onlyB=$onlyB walk=${secs}s"
    }
}
```
Verified 2026-07-28 against the corrected 37-repo `$Canonical`: `qontinui-claude-config`
`regs=46 missing-gitdir=0 prunable=0 agree=True` in 3.3 s; `qontinui-coord` `regs=2087
missing-gitdir=0 prunable=0 agree=True` in 7.5 s. Signal A's cost scales with the
registration count, which is exactly the quantity whose growth this class exists to catch.
Record the **wall time of the porcelain read per repo** in the ledger — a 1064-entry pileup
made `git worktree list` unusable once, and the wall time is the early-warning signal for
that recurring. Fleet reference points measured 2026-07-27: coord 2053 · runner 1992 ·
web 2262 · claude-config 38, each enumerating in 5–10 s. **Re-read these at act time; never
take a registration count from this file.** The loop runs over the **37** real clones, not
the 341 dirs a bare `Test-Path` would yield — at ~5–10 s per large repo the difference is
~4 min versus ~20–25 min per iteration on a 15 m loop, and the 341-dir form also counts
`qontinui-claude-config`'s registrations once per `ccfg-wt-*` worktree of it.

Class 3's destructive verb — armed since it graduated 2026-07-29 — is `git worktree prune`,
which **cannot destroy data by construction** (it removes only registrations whose `gitdir`
is already gone). Preview it with `git -C <repo> worktree prune --dry-run -v` and require
the preview's set to equal Signal A's `$missing` **set** — not merely its count — before
acting; an unexplained member on either side means ABSTAIN for that repo.
`prune` runs **per repo**, never as a loop that swallows a mid-loop failure — a repo whose
read errored is skipped and reported, never silently passed over.

**That preview comparison has three traps, and all three were live until 2026-07-29 — which
is why class 3 sat at `report` for nine iterations whose A/B bar was already met.** An
agent implementing the paragraph above literally gets a check that can never pass. Traps 1
and 3 are fixed by the block below; **trap 2 is fixed UPSTREAM, in the Signal A snippet's
`$missingAdmin` accumulator** — by the time the preview is parsed the admin-dir names have
nothing to map onto, so a Signal A that discards `$e.Name` leaves the check unfixable no
matter what the comparison does. With all three closed, Signal C ran green on 37/37 repos
across the evidence window in the arming table (it=10→17), and **that** is what graduated
the class. The traps stay documented here because a reimplementation would walk straight
back into them.

1. **`--dry-run -v` writes the preview to STDERR, not stdout.** Measured on this box via
   `Start-Process -RedirectStandardOutput/-RedirectStandardError`: **stdout 0 bytes**,
   stderr 221 bytes carrying all three `Removing worktrees/<name>: gitdir file points to
   non-existent location` lines. So `@(& git -C $repo worktree prune --dry-run -v)` yields an
   **empty** preview set, the comparison against a non-empty `$missing` reads "sets
   disagree", and the repo ABSTAINS **forever**. The gate looks like it is working — it
   prints a disagreement, which is exactly what a real conflict looks like.
2. **The two sides are in DIFFERENT KEY SPACES and are not comparable as printed.** The
   preview names git's **admin-dir names** (`worktrees/qontinui`, `worktrees/qontinui1`,
   `worktrees/qontinui2`); Signal A is keyed on **worktree paths**
   (`.../qontinui-worktrees/<uuid>/qontinui`). Their intersection is empty even when both
   describe the identical three registrations. You must map one into the other —
   admin-dir name → its `gitdir` file → `Split-Path -Parent` — before comparing. Verified
   2026-07-29 on `qontinui`: admin dirs `qontinui`/`qontinui1`/`qontinui2` map exactly onto
   the three missing worktree paths, and the fourth admin dir (`qontinui-wt-redfix`, an
   ad-hoc worktree that still exists) correctly maps to `missing=False`.
3. **You cannot capture that stderr with `2>&1`** — Step 1 bans it for native exes on
   PowerShell 5.1, because 5.1 wraps each stderr line in an ErrorRecord and flips `$?` to
   `$false` on a successful exit 0. So the obvious fix violates a different rule in this
   file. **Use a file redirect**, which yields raw text and leaves `$?` alone:
   ```powershell
   # Signal C for class 3: git's own prune PREVIEW, mapped into Signal A's key space.
   # Start-Process (not `2>&1`) so PS 5.1 never wraps the lines in ErrorRecords.
   $so = Join-Path $env:TEMP ("wtp-o-" + [guid]::NewGuid().ToString('N') + ".txt")
   $se = Join-Path $env:TEMP ("wtp-e-" + [guid]::NewGuid().ToString('N') + ".txt")
   $preview = New-Object 'System.Collections.Generic.HashSet[string]'
   $previewOk = $false
   try {
       $pp = Start-Process -FilePath 'git' -NoNewWindow -Wait -PassThru `
             -ArgumentList @('-C', $repo, 'worktree', 'prune', '--dry-run', '-v') `
             -RedirectStandardOutput $so -RedirectStandardError $se
       if ($pp.ExitCode -eq 0) {
           # Preview lines land on STDERR. Read BOTH streams: a future git that moves
           # them to stdout must not silently produce an empty preview set.
           foreach ($line in @(Get-Content -LiteralPath $se) + @(Get-Content -LiteralPath $so)) {
               if ($line -match '^Removing\s+(worktrees/\S+?):') { [void]$preview.Add($Matches[1]) }
           }
           $previewOk = $true
       }
   } finally {
       foreach ($f in @($so, $se)) { if (Test-Path -LiteralPath $f) { [System.IO.File]::Delete($f) } }
   }
   if (-not $previewOk) { "$repo : ABSTAIN (prune preview unreadable)" }
   ```
   Then compare `$preview` against the **admin-dir keys** of Signal A's missing entries
   (`"worktrees/$($adminDirName)"` for each entry whose recorded `gitdir` does not exist) —
   not against the raw worktree paths.

**The rule this episode establishes, which outlives the graduation: a bar met by the signals
that *were* implemented is not met while a mandated signal is silently returning empty.**
Class 3's A/B bar was satisfied nine times over on 2026-07-29 while Signal C had never once
executed successfully — and for those nine the class correctly stayed at `report`, because
"three of three signals agree" cannot be claimed by two of them. It graduated only
after the block above ran green (`$previewOk` true, mapped sets equal, zero UNKNOWN) across
≥3 consecutive iterations; the arming table's evidence block records which ones.

Apply the same test to any future class: count the signals that **ran**, not the signals the
bar names. A silently-empty mandated check is the one failure mode that makes a graduation
bar look satisfied while it is not.

## Step 3 — Class 10: orphaned temp/external runners (five verified defects, all encoded)

Class 10's data shape has four field-level traps and one expected-outcome trap. Every one was
verified against the live supervisor on 2026-07-27. Getting any of them wrong reproduces a
measured production defect.

**Defect 0 — the POPULATION read has its own UNKNOWN path, and without it an unreachable
supervisor launders itself into graduation evidence.** Every other class in this file gates
its population read on UNKNOWN; class 10 is the one whose destructive verb *stops processes*,
so it needs it most. The item-level abstains below are all evaluated **inside** a loop over
the runner list — over an empty list they are vacuous, and a supervisor that is down, slow,
or 500-ing yields exactly that empty list. Uncaught, the chain runs: no supervisor ⇒ zero
entries ⇒ zero would-act items ⇒ "verified safe, zero throughput" recorded ⇒ three such
iterations satisfy the empty-set graduation bar. **An UNKNOWN population read is not an
empty set, and it must never count toward graduation.**

```powershell
# Population read for class 10. UNKNOWN here ABSTAINS THE WHOLE CLASS.
$runners = $null; $runnersReason = ''
$resp = Read-Bounded -Url 'http://localhost:9875/runners' -TimeoutSec 60 -Label 'supervisor /runners'
if ($resp.status -ne 'OK') {
    $runnersReason = $resp.reason
} else {
    # HTTP 200 with a body is still not a value — check the shape (Step 1).
    # This surface serves a BARE JSON ARRAY of runner objects, not an envelope.
    $parsed = $null
    try { $parsed = $resp.body | ConvertFrom-Json } catch { $parsed = $null }
    if ($null -eq $parsed -or $parsed -isnot [array]) {
        $runnersReason = 'unexpected shape (expected a JSON array of runners)'
    } else { $runners = @($parsed) }
}
if ($null -eq $runners) {
    "class 10: UNKNOWN ($runnersReason) -- WHOLE CLASS ABSTAINS, no graduation credit"
} elseif ($runners.Count -eq 0) {
    "class 10: 0 entries (read OK, genuinely empty) -- NO graduation credit either"
} else {
    "class 10: population $($runners.Count) entries (read OK)"
}
```
**The ledger must render all three outcomes differently, and only the third earns
graduation credit.** `UNKNOWN (<reason>)` and `0 entries (read OK, genuinely empty)` are
the same number and opposite facts — but *verified safe, zero throughput* is a claim about
an **inspected, non-empty inventory** that every gate correctly excluded, so neither of
them qualifies. A supervisor that is up with an empty registry (post-restart, registry
wiped) inspects nothing, and crediting it would launder a different empty set into
graduation evidence — the same defect this section closes, one branch over.

**Defect 1 — `kind` is an OBJECT, not a string, and a string compare FAILS OPEN.**
It serialises as `{"type":"primary"}`, `{"type":"named","name":"target"}`, `{"type":"temp","id":…}`,
`{"type":"external"}` (`qontinui-supervisor/src/routes/runners.rs:434`; enum
`qontinui-schemas/rust/src/wire/runner_kind.rs:42`, `#[serde(tag = "type", rename_all = "snake_case")]`,
variants at `:46-55`). A test written `kind == "primary"` **never matches**, so every runner —
**including the primary** — reads as "not primary, not named" and becomes reap-eligible. This
is the highest-severity defect in the class. **Test `kind.type`, and treat an absent or
unrecognised `kind.type` as ABSTAIN, not as "not primary".**

**The gate must be an ALLOWLIST, not a denylist**, or the fix repeats the defect one level
in. Excluding only `primary` and `named` is a denylist: it lets every *other* string —
`secondary`, a future variant, a typo, anything the enum grows next — fall straight through
to the reap path, which is the same fail-open the prose calls this class's highest-severity
defect. Only **`temp`** and **`external`** are class-10 targets; **everything else abstains,
by default, without needing to be enumerated.** Verified 2026-07-28 on a synthetic
`kind.type='secondary'`: the denylist form reaches the reap path, the allowlist form abstains.

```powershell
foreach ($r in $runners) {
    $kt = $null
    if ($null -ne $r.kind) { $kt = $r.kind.type }
    if ([string]::IsNullOrEmpty($kt)) { "$($r.id) : ABSTAIN kind.type unreadable"; continue }
    if ($kt -eq 'primary' -or $kt -eq 'named') { "$($r.id) : EXCLUDED never-touch kind.type=$kt"; continue }
    # ALLOWLIST: only these two may ever proceed.
    if ($kt -ne 'temp' -and $kt -ne 'external') { "$($r.id) : ABSTAIN unrecognised kind.type='$kt'"; continue }
    # ABSENT `protected` is UNKNOWN, not "not protected" -- the Defect-1 fail-open one
    # field over. All three live entries are protected:true, so a serialisation change
    # that drops the field would otherwise un-protect the entire fleet at once.
    if ($null -eq $r.protected) { "$($r.id) : ABSTAIN protected field absent (UNKNOWN)"; continue }
    if ($r.protected -eq $true) { "$($r.id) : EXCLUDED protected"; continue }
    "$($r.id) : passes the kind gate -- continues to the pid + port gates below"
}
```
This is the **head** of the class-10 loop; the authoritative full form, with the remaining
gates appended in order, is the snippet after Defect 3. Note the explicit `foreach`: these
clauses are loop-body clauses, and PowerShell 5.1's `continue` outside a loop does **not**
skip to the next item — a bare fragment would fall through to the reap path it was written
to prevent.

**Defect 2 — `pid: null` means UNKNOWN and MUST abstain. It never confirms "no process".**
Live counter-example, 2026-07-27: the supervisor reported `primary` as `running:false,
api_responding:false, pid:null, derived_status:{"kind":"offline"}` — while
`Get-NetTCPConnection -LocalPort 9876` showed the port **LISTENING**, owned by **PID 219848
`qontinui-runner-primary`**, started at the supervisor's own recorded `started_at`.
`pid:null` means *the supervisor does not know the pid*, not *no process exists*. Accepting
it would have authorised a stop against a live, port-holding runner. Signal B is therefore
a **conjunction of two clauses, both of which must hold**: *(i)* a **resolvable pid that is
not a live process** — a null pid fails it — **and** *(ii)* the **port not listening**
(Defect 3). Neither clause is sufficient alone: the pid clause comes from the supervisor's
own bookkeeping, which Defect 2 shows can be wrong; the port clause is independent but
answers a different question. **Two of the three live entries report `pid: NULL` right now**
(re-verified 2026-07-28: only `primary` carries a pid, 219848), so the pid clause is the
binding one in practice and it must actually appear in the code — not only in the prose.

**Defect 3 — `running` and `api_responding` are NOT independent; one is derived from the
other.** `routes/runners.rs:369` computes
`let effectively_running = runner.running || cached.runner_responding;` and serves that as
`running` (`:436`), while `api_responding` is `cached.runner_responding` (`:439`). So
`running:false` **already entails** `api_responding:false` — the conjunction is one signal
wearing two names, and it was demonstrably stale for `primary`. **Only the OS-level
port-listen check is a genuinely independent authority**, which makes it load-bearing rather
than corroborating.

**This is the authoritative class-10 gate — the complete loop, every clause in order.**
Both process enumeration and listener enumeration are hoisted out of the loop and use the
same **enumerate-then-filter** form: passing `-LocalPort` (or `-Id`) makes the cmdlet
*throw* when nothing matches, and that message is **localised on this box (German)** —
branching on its text is the `netstat`/`ABHÖREN` trap one layer up. Filtering in PowerShell
keeps both probes locale-independent and keeps "empty" distinguishable from "failed".

```powershell
# Hoisted probes. A FAILED enumeration is UNKNOWN for every item, never "nothing found".
$procIds = $null
try { $procIds = @(Get-Process -ErrorAction Stop | ForEach-Object { $_.Id }) } catch { $procIds = $null }
$allListen = $null
try { $allListen = @(Get-NetTCPConnection -State Listen -ErrorAction Stop) } catch { $allListen = $null }

foreach ($r in $runners) {
    # --- kind gate (Defect 1): allowlist, everything unrecognised abstains.
    $kt = $null
    if ($null -ne $r.kind) { $kt = $r.kind.type }
    if ([string]::IsNullOrEmpty($kt)) { "$($r.id) : ABSTAIN kind.type unreadable"; continue }
    if ($kt -eq 'primary' -or $kt -eq 'named') { "$($r.id) : EXCLUDED never-touch kind.type=$kt"; continue }
    if ($kt -ne 'temp' -and $kt -ne 'external') { "$($r.id) : ABSTAIN unrecognised kind.type='$kt'"; continue }
    # ABSENT `protected` is UNKNOWN, not "not protected" -- the Defect-1 fail-open one
    # field over. All three live entries are protected:true, so a serialisation change
    # that drops the field would otherwise un-protect the entire fleet at once.
    if ($null -eq $r.protected) { "$($r.id) : ABSTAIN protected field absent (UNKNOWN)"; continue }
    if ($r.protected -eq $true) { "$($r.id) : EXCLUDED protected"; continue }

    # --- Signal B clause (i) (Defect 2): a RESOLVABLE pid that is NOT a live process.
    if ($null -eq $r.pid) { "$($r.id) : ABSTAIN pid null (UNKNOWN, never 'no process')"; continue }
    if ($null -eq $procIds) { "$($r.id) : ABSTAIN process enumeration failed (UNKNOWN)"; continue }
    if ($procIds -contains [int]$r.pid) { "$($r.id) : ABSTAIN pid $($r.pid) is a live process"; continue }

    # --- Signal B clause (ii) (Defect 3): port NOT listening. A missing port is UNKNOWN:
    # without this guard `$_.LocalPort -eq $null` matches nothing, reads as FREE, and a
    # port-less entry becomes reap-eligible on a probe that never actually ran.
    if ($null -eq $r.port) { "$($r.id) : ABSTAIN port absent (UNKNOWN)"; continue }
    if ($null -eq $allListen) { "$($r.id) : ABSTAIN port probe failed (UNKNOWN)"; continue }
    if (@($allListen | Where-Object { $_.LocalPort -eq $r.port }).Count -gt 0) {
        "$($r.id) : ABSTAIN port $($r.port) is LISTENING"; continue
    }

    "$($r.id) : WOULD-REAP (report mode: no action taken)"
}
```
**Class 10 MUST NOT act on any item whose port-listen probe errored or could not be run** —
nor on one whose pid is null, whose pid is live, whose port is absent, or whose process
enumeration failed. Verified 2026-07-28 against the live supervisor plus synthetic probes:
the three live entries render `EXCLUDED never-touch kind.type=named` / `EXCLUDED protected`
/ `EXCLUDED never-touch kind.type=primary`; synthetic `secondary`, null-port, null-pid and
absent-`kind` entries each abstain with their own reason; and a synthetic genuinely-dead
`temp` runner still reaches `WOULD-REAP`, confirming the gate is tight without being inert.

**Defect 4 — `POST /runners/<id>/stop` is NOT idempotent for temp runners.** It is idempotent
for a runner still in the registry (the PID kill is a documented no-op if the process already
exited, `qontinui-supervisor/src/process/manager.rs:2843-2851`, with success gated on a
port-free confirm at `:2870-2900`) — but stopping a `test-*` runner **removes it from the
registry**, so a repeat call hits `manager.rs:2728-2731` `SupervisorError::RunnerNotFound`.
**Treat a second stop's 404 as expected — "already reaped" — never as a failure to retry or
escalate.**

**Defect 5 — class 10 currently reaps NOTHING, by construction, and that is correct.** See
the class-arming table. Record an **empty-set graduation**: *verified safe, zero throughput*.
Never report it as a successful reap, and never widen the guard to manufacture throughput.

**Record it only when the population read actually succeeded** (Defect 0). *Verified safe,
zero throughput* is a claim about an inspected, non-empty inventory that every gate
correctly excluded — it is not a claim you may make about a list you never obtained. An
iteration whose `/runners` read returned UNKNOWN records `class 10: UNKNOWN (<reason>)`,
contributes **nothing** to the graduation count, and leaves that count where it was.

**Banned in this class, absolutely: `taskkill`, `Stop-Process`, and any `/T` flag.** The only
verb is the supervisor's own `POST /runners/<id>/stop`. Killing `node.exe` or `powershell.exe`
terminates Claude Code sessions.

## Step 4 — Class 12: the reaper-inertness auditor (the steward's highest-value output)

coord already ships `removals_total` and the `coord_worktree_reaper_inert` gauge (coord
`fe31b067`, with a synthetic incident-replay test). **Consume them. Do not rebuild them.**

**Why the shipped gauge did not fire on 2026-07-23 → 2026-07-27.** Its population term is
downstream of the same never-converging `LATEST_CENSUS` snapshot as the reaper it audits.
During a prefix-stall it compares *"removed 0"* against *"population 13"* and reads benign.

> **A detector whose population term shares an authority with the reaper it audits is
> structurally incapable of detecting that authority failing.**

So the steward's population term **MUST be its own bounded disk count** from Step 2a. That
single substitution is what makes this detector work where the shipped one does not.

### 4a. The bar is a RATE, not a point

The prior fix declared success on a **single `reapable = 2` observation** while ~7/day
actually left the disk. `reapable > 0` is an **input-side** signal wearing output-side
clothing: it proves the gate opened, not that bytes left the disk. **It is not admissible as
an efficacy signal, ever.**

For each class `c` with an owning reaper, over window `W` = **(the lookback anchor) → now**,
where the **lookback anchor is the most recent ledger line whose stamped `observed_at` is at
least 1 h before now** — *not* the immediately preceding line.

**Re-resolve the anchor immediately before taking the end reads, never once at the top of
the iteration.** The anchor ROLLS FORWARD while you work: at it=169 the newest line ≥1 h old
was it=165 at 09:26Z and it=166 by 09:39Z, so a full read budget was spent against a
resolution that had already expired and every read landed ~4 min before the real boundary.
An iteration that discovers this must re-take the reads and DISCLOSE the overrun, never
quietly keep the stale ones. Two ledger-hygiene rules follow, both learned by breaking them:
the class-12 block key must not drift between iterations (`CLASS12` vs
`CLASS12_VERDICT_INERT` makes older lines unresolvable), and **never write prose into a
stamp field** — one line carrying `"see endreads165.txt"` where a timestamp belonged made
that line permanently unusable as an anchor.

**This distinction is the difference between the detector working and never firing at all.**
The rate bar's precondition is `hours(W) >= 1`, and the default loop interval is 15 m. If `W`
ran from the *previous* line, every window would be 0.25 h, every verdict would be UNKNOWN
forever, and the highest-value thing in this file would be structurally dead on its own
default settings — while looking like it was running fine. A lookback anchor makes
`hours(W) >= 1` satisfied by construction on any ledger with ≥1 h of history, and it is what
the `:15 m, not 5 m` justification under "Continuous operation" and the worked ledger sample
below have both always assumed. Older lines are **not** discarded — each iteration re-anchors
against the ledger, so a 15 m loop yields a fresh ~1 h window every 15 m.

```powershell
# Anchor selection. $ledger = the ledger lines parsed from JSONL, oldest -> newest.
# PARSE observed_at TO [datetime] EXPLICITLY. ConvertFrom-Json yields it as a STRING, and
# `$string -le $datetime` coerces to the LEFT operand's type -- an ordinal STRING compare
# against the culture-formatted cutoff ('07/28/2026 00:38:10' on en-US, '28.07.2026
# 00:38:10' on this box's German locale). That compare is meaningless: measured on en-US
# it matches ZERO lines, so $anchor is null and EVERY rate verdict is UNKNOWN forever --
# silently reinstating the exact dead detector the lookback anchor exists to prevent.
# InvariantCulture + AssumeUniversal makes it locale-independent and unambiguously UTC.
$cutoff = (Get-Date).ToUniversalTime().AddHours(-1)
$styles = [Globalization.DateTimeStyles]::AdjustToUniversal -bor [Globalization.DateTimeStyles]::AssumeUniversal
$anchor = $null; $anchorTs = $null
foreach ($line in $ledger) {
    $ts = [datetime]::MinValue
    if (-not [datetime]::TryParse($line.observed_at, [Globalization.CultureInfo]::InvariantCulture, $styles, [ref]$ts)) {
        continue   # unparseable stamp is UNKNOWN, never a usable anchor
    }
    if ($ts -le $cutoff) { $anchor = $line; $anchorTs = $ts }   # keep the LAST (newest) match
}
if ($null -eq $anchor) { 'class 12: UNKNOWN (no ledger line >= 1 h old) -- all rate verdicts abstain' }
```
**A null `$anchor` must actually stop the rate computation**, not merely print that line:
every rate verdict for the iteration is UNKNOWN and is rendered with that reason.

```
pop_start(c)  = the ANCHOR ledger line's own disk count, WITH its stamp [ledger file]
pop_end(c)    = re-counted NOW, this iteration                          [Step 2a]
removals(c)   = reaper counter delta over W                             [coord_query_metric / supervisor]
arrivals(c)   = max(0, pop_end - pop_start + removals(c))               [derived]
drain_rate    = removals(c) / hours(W)
arrival_rate  = arrivals(c) / hours(W)
```

Preconditions — if any fails the verdict is **UNKNOWN**, printed as such, and no INERT/HEALTHY
claim is made:
- An anchor exists — i.e. some ledger line is ≥1 h old. This is what makes `hours(W) >= 1`
  hold; with `--once` on a cold ledger, or in the first hour of a run, there is no anchor
  and every rate verdict is UNKNOWN. The window spans ≥2 iterations.
- `pop_start` carries its own `observed_at` stamp. **In live operation the anchor is ~1 h old
  by construction, and a stamp older than 24 h is UNKNOWN** — it means the steward was off and
  the "window" silently spans its own downtime, so the arrivals term is an artefact. The one
  exception is an explicit **replay** (see "sustained" below), where a multi-day window is the
  deliberate unit of evaluation rather than an accident of downtime.
- `pop_end` is a real count, not UNKNOWN.
- the reaper counter was actually read (a **missing metric series is UNKNOWN, not 0** —
  confirm the series exists in the registry before differencing it).
- **`removals(c)` needs a reading at BOTH ENDS of `W`, not just one.** It is a **delta**, and
  one reading of a monotonic counter is not a rate. This is a *separate* precondition from
  the one above: "the series exists" is satisfied by a single point-read, while the anchor
  rule selects ledger lines **independently of when the counter first became readable**, so
  the newest line ≥1 h old can easily predate your first reading of it. Measured 2026-07-29:
  the removals series became readable at it=28; at it=29 the anchor was it=26, a line
  carrying **no** removals value, so the delta over `W` was UNKNOWN even though the counter
  itself read cleanly. **A newly-readable counter costs ~1 h of readings before the rate bar
  engages**; print that as the reason rather than an unexplained UNKNOWN.

  **The failure this prevents: do NOT read `removals = 1` and record "1 removal over W".**
  That is a cumulative **all-time** value — every row ever written to
  `coord.worktree_reclaim_events`, monotonic across deploys — not a windowed one, and using it
  as the delta manufactures a nonzero drain rate from a counter that may not have moved in
  days — an input-side signal in output-side clothing, which §4a opens by rejecting.
  (It was described here as *since-process-start*; that was wrong — see the retraction below.)

  **This binds the LIVE anchor path only. A replay is unaffected**, because a replay supplies
  `removals` (and `pop_start`/`pop_end`) *directly from recorded historical measurements*
  rather than differencing two of your own point-reads — which is exactly how the worked
  calibration's `removals ≈ 38` is admissible. Cite the source of the term, as the replay
  rules already require.

- **the counter did not RESET inside `W`** — a precondition that is **REAL for in-process
  counters and INAPPLICABLE to the removals family.** Which one you are differencing decides
  whether you owe this check at all, so establish that first.

  **RETRACTED — do not restate: "`coord_query_metric` reads in-process, so a deploy or restart
  zeroes the counter."** That inference shipped here on 2026-07-29 and is **wrong for
  `coord_worktree_reclaim_removals_total`**. The read path *is* in-process; the **value is
  not a process-lifetime accumulator.** Verified on `origin/main`:
  `worktree_reclaim_events.rs::load_removal_counts` renders it per scrape from
  `SELECT device_id, count(*)::bigint, count(*) FILTER (WHERE observed_missing_at >= $1)::bigint
  FROM coord.worktree_reclaim_events GROUP BY 1`, and `format_efficacy` says so in its own
  comment at `:523-525` — *"durable output-side counter. Rendered from count(*) over the
  append-only events table, **so it is monotonic across deploys**."* The substrate migration
  (`qontinui-web` `reclaim_ev_01_coord_worktree_reclaim_events.py`) states the same intent.
  **"The read is in-process" does not entail "the value is per-process"** — that was a
  category error, and the hedge it carried (*inferred from where the value lives, not traced
  through a restart*) should have stopped it from being written as a rule.

  A coord restart does **not change this value at all.** The series goes ABSENT only when the
  substrate table is missing or unreadable or the PG pool acquire fails — `load_removal_counts`
  returns `None` and callers *"omit their series rather than render a lie"* (`:329-330`), and
  `format_efficacy` adds *"The whole family is omitted when the substrate table is absent
  (fail-open)"*. A restart's only readability effect is a cold 60 s `cached_swr("worktree")`
  memo. Absence is handled by §4f's `present: false` ⇒ UNKNOWN rule.

  **But a restart DOES still invalidate the delta — by a different mechanism, and this is the
  one that matters most.** It destroys **attribution**, not the counter. Rows are INSERTed only
  when a key the leader holds **in memory** goes present→absent (`static TRACKER`, *"Leader-only
  memory … lost on restart by design"*, `:96-98`), and that transition **lags the actual disk
  deletion by up to the 2 h presence window** — the module doc spells the whole chain out at
  `:20-32`, ending *"A coord restart loses the in-memory tracker — up to one presence-window of
  attribution."* Once lost it is unrecoverable: *"an absent key can never be newly armed."*

  So a restart biases this counter **DOWN**, and `removals(c)` can read **0 while the reaper
  was working normally** — landing exactly on the INERT firing condition. **That is a fabricated
  INERT: the mirror image of the failure §4a exists to prevent, and it is the reason a restart
  check is still owed here even though no reset can occur.** Two consequences:

  - The window at risk is `W` **plus the ~2 h preceding it**, not just `W`.
  - The condition is a **leader change**, which is broader than a deploy — the tracker is
    leader-local, so a plain handoff to another replica loses it with no process on the box
    restarting.

  - **A DECREASE is still UNKNOWN, never "zero removals".** Differencing a smaller value
    yields a negative delta, and clamping *that* to 0 converts the anomaly into evidence of
    inertness — a fabricated verdict. **Note this is a different clamp from `arrivals`'
    `max(0, …)`**, which legitimately floors a *derived population* term; do not reuse one to
    justify the other. For this family a decrease cannot be a process reset, so it means row
    deletion or a `device_id` change — **investigate it, do not explain it away.**
  - **The restart probe, and it is ONE-SIDED.** `coord_orient(since=<start of W minus 2 h>)`:
    a non-empty `changes.leader_changes` is a **timestamped** takeover — it reads
    `coord.leader_lease WHERE acquired_at > $1` (`orient_delta.rs:262-264`) and `acquired_at`
    is bumped *only* when `holder_id` actually changes (`leader.rs`' `ON CONFLICT` CASE), so
    the cursor filter **is** the takeover filter. A `changes.flag_flips` burst sharing one `at`
    is a replica boot (every replica re-seeds the flag catalog at boot, no leader gate).
    **Non-empty ⇒ `removals(c)` is UNKNOWN for this window. Empty is NOT proof of no restart** —
    both classes are capped and a follower that re-seeded before your cursor is invisible — so
    empty licenses nothing beyond *"no restart observed"*. **Record which you got beside the
    delta**, or the literal `not-probed`; the audit trail is part of the precondition.
  - **`coord_query_release_state` cannot discharge this.** `declared_sha` identical at both ends
    is **not** proof: any same-image task replacement restarts a replica without touching the
    declared sha *(inferred from ECS task-replacement semantics — health-check recycle, spot
    interruption, scale-in/out, AZ rebalance — not observed here)*. And **`in_sync: false` /
    `drift_class: stale` is evidence FOR a possible restart, not against**: `stale` correlates
    with the firing condition for coord's own auto-recover — the watcher's `ecs-image-stale`
    alert, an ECR-latest-vs-running-digest comparison and a *different* derivation from
    `drift_class`'s commit-ordering one — gated on `ECS_AUTO_RECOVER=1` (whether that is set on
    the serving task-def is **unverified**) with `UPDATE_SERVICE_COOLDOWN_SECS = 1_800`, i.e.
    up to two forced rollouts per hour when it is on. `in_sync: false` is also coord's
    **fail-closed value for ignorance** — the unknown-observation constructor sets it with
    `coverage: 0.0` (`release_observer.rs:1398-1407`) — so reading it as safety turns an
    unreadable surface into a proof. Nor can an end-of-`W` read exclude a rollout that
    completed *earlier* in `W`. Treat a changed `declared_sha` as **UNKNOWN** and do not build
    a ladder out of `drift_class`.
  - **Terminal rule when nothing can discharge it: never difference a counter across a window
    that may contain a restart the probe cannot rule out.** The delta is UNKNOWN — use a
    durable substrate, or abstain. Do not fall back to "probably fine".

  Note also `provenance: "replica-local"` on the point-read — it lands on ONE replica via the
  ALB. That is orthogonal to resets and still applies: do not treat a single point-read delta
  as fleet-true without establishing whether the series is replica-partitioned.

Verdicts:

| Verdict | Condition | Action |
|---|---|---|
| **INERT** | reaper self-reports armed+healthy **and** `removals(c) == 0` over W **and** `pop_end >= pop_start` | fire |
| **INERT-BY-STARVATION** | `drain_rate < arrival_rate` **sustained** (see below) — draining slower than filling | fire |
| **INERT-BY-RATE** | `drain_rate * 24 < 0.01 * pop_start` (removing <1 %/day of its own backlog), **sustained** | fire |
| **HEALTHY** | `removals(c) > 0` **and** `pop_end < pop_start`, both from independently re-counted terms | record; never on a point observation |
| **UNKNOWN** | any precondition above unmet | record with the reason; **never** rendered as HEALTHY |

**"Sustained"** means either of two forms, and **both must be admissible or the detector
cannot fire on the incident it exists to catch**:

1. **Live operation: ≥3 consecutive windows.** A 15 m loop against a 1 h lookback anchor
   produces a fresh ~1 h window every 15 m, so three consecutive windows accumulate in
   **~30 min** (three iterations at 15 m spacing). This is the normal path. **Be honest
   about what it buys:** those three 1 h windows overlap by ~75 %, so the bar is closer to
   one 1.5 h observation than to three independent ones. It is still strictly more than
   the single point observation §4a exists to reject — but do not describe it, in the
   ledger or a finding, as three independent confirmations.
2. **Replay: a single window with `hours(W) >= 24`.** A long window is not weaker evidence
   than three short ones — it is strictly stronger. This is the form a *replay* takes, and
   it is not a loophole: Phase 6's own verification bar **requires** replaying this detector
   against two multi-day single windows (2026-07-19 → 07-23 and 2026-07-23 → 07-27), and
   the worked calibration below is exactly such a replay. Without this form the calibration
   — one 120 h window — could fire neither verdict, and the invariant two paragraphs down
   would be unfalsifiable decoration.
   **How a replay is entered, since the live anchor rule can never produce one:** a replay
   is an explicit, separately-initiated evaluation in which `pop_start`, `pop_end`,
   `removals` and the window bounds are supplied **directly from recorded historical
   measurements** rather than selected from the ledger by the anchor rule. It is not a mode
   of the `/loop`, and the anchor rule — which always yields `W ≈ 1 h` — is deliberately
   not used. **Label every replay verdict `REPLAY` in the ledger and cite the source of
   each term**; a replay verdict must never be mistaken for a live observation.

Until the bar is met the state is **pending**, and it prints as `PENDING-<verdict>
(window k of 3)` — never as the verdict name, which would report a not-yet-fired detector
as fired.

**Why HEALTHY requires `removals(c) > 0`.** Without it the reaper gets credit for deletions
it did not perform. Concretely: a third party — the operator, `cargo-sweep-all.ps1`, a manual
`rm` — removes 600 trees while the reaper does nothing. Then `removals = 0`,
`arrivals = max(0, 6068 − 6668 + 0) = 0`, so `drain_rate >= arrival_rate` holds as `0 >= 0`
and `pop_end < pop_start` holds — and a completely inert reaper is recorded HEALTHY on the
strength of someone else's work. **A reaper's efficacy signal must be its own output**, which
is this whole file's thesis; `removals > 0` is what enforces it here.
Note that `drain_rate >= arrival_rate` is **not** listed as a separate conjunct because it is
implied: given the `max(0, …)` clamp, `pop_end < pop_start` already forces
`arrivals <= removals`. Restating it would suggest a third independent check that is not
doing any work.

Worked calibration from the measured incident (rates shown as per-day equivalents;
`hours(W) = 120`): `pop_start = 6100`, `pop_end = 6668`, `removals ≈ 38` ⇒
`arrivals = 6668 − 6100 + 38 = 606`, `drain ≈ 7.6/day`, `arrival ≈ 121/day`, and the
1 %/day floor `0.01 × 6100 = 61/day`. `7.6 < 121.2` fires **INERT-BY-STARVATION**; `7.6 < 61`
fires **INERT-BY-RATE**. Both fire on this **single** window via sustained-form 2: it is a
**replay**, and `hours(W) = 120 >= 24`. Evaluate it as a replay, not as a live iteration —
as a live iteration its 120 h-old `pop_start` stamp would (correctly) be UNKNOWN.
The shipped gauge read benign on the same window. **If a change to this section would stop
it firing on those numbers, the change is wrong.**

### 4b. Census-never-converges detector (nothing consumes this today)

`census_build_ms` is already exposed on `GET /agent-worktrees/reclaimable` and **nothing
reads it**.

- **`census_build_ms == 0` together with non-trivial runner uptime (≥ 600 s) means no walk
  has ever completed in the current process** (`census.rs:201-203`). Fire class-12
  `census-never-converges`. **Uptime has one named source: the supervisor's own recorded
  `started_at`** for the `primary` entry in `GET /runners` — the same field cited under
  Defect 2, live-verified 2026-07-28 as `2026-07-27T21:39:14…`. Compute
  `uptime = now − started_at`; **if `started_at` is absent or unparseable, uptime is UNKNOWN
  and this detector abstains** rather than assuming the runner is young (which would
  suppress the finding) or old (which would fabricate it).
  ```powershell
  $prim = $runners | Where-Object { $_.kind -and $_.kind.type -eq 'primary' } | Select-Object -First 1
  $uptimeSecs = $null
  if ($null -ne $prim -and -not [string]::IsNullOrEmpty($prim.started_at)) {
      # Same locale/timezone discipline as the §4a anchor. A naive TryParse + .ToLocalTime()
      # is wrong for an offset-less stamp: it is read as Unspecified, then shifted by the
      # local offset, UNDERSTATING uptime by 1-2 h here -- which holds uptime under the
      # 600 s gate and SUPPRESSES this detector for the first hours of every runner's life.
      $st = [datetime]::MinValue
      $styles = [Globalization.DateTimeStyles]::AdjustToUniversal -bor [Globalization.DateTimeStyles]::AssumeUniversal
      if ([datetime]::TryParse($prim.started_at, [Globalization.CultureInfo]::InvariantCulture, $styles, [ref]$st)) {
          $uptimeSecs = [int]((Get-Date).ToUniversalTime() - $st).TotalSeconds
      }
  }
  if ($null -eq $uptimeSecs) { 'census-never-converges: UNKNOWN (no parseable started_at)' }
  ```
- **SECOND ARM — `census_build_ms` > 0 but FROZEN while `census_refreshing` stays true.**
  The `== 0` test above only catches *"no walk has **ever** completed in this process"*. It
  misses the strictly worse steady state: a walk that completed **once**, long ago, and can
  never complete again — because `build_ms` then holds a stale-but-positive value forever and
  the `== 0` guard never trips. Fire `census-never-converges` when, across **≥3 consecutive
  ledger iterations**, `census_build_ms` is **unchanged** AND `census_refreshing` is `true`
  on every read. Both terms come from the same endpoint payload, so this costs no extra probe
  — it is a comparison against the ledger you already keep.

  **Measured 2026-07-29, it=1 → it=9 (~2.1 h, nine consecutive reads):** `census_build_ms`
  frozen at **44,098,058 ms — 12.25 hours** — with `census_refreshing: true` every single
  time, `census_status: "fresh"` every single time, `census_age_secs` between 29 and 103, and
  `items` creeping 1084 → 1087 while **`under_root` sat at exactly 640 for all nine reads**.
  A walk that takes 12¼ h cannot finish inside a 15 m loop, so the snapshot is a *permanent*
  partial: it advances slightly outside `qontinui-worktrees/` and never reaches the part the
  reaper acts on. The `== 0` arm read benign throughout.

  **Why the frozen value is the more dangerous shape, not the milder one.** `build_ms: 0` is
  self-evidently "no data" — §4b already calls it the honest field. A *positive* `build_ms`
  beside `census_status: "fresh"` actively asserts a completed, recent walk, so it reads as
  healthy to anything that checks `build_ms > 0` as a liveness proxy — including this file's own
  instruction two bullets down to "require `census_build_ms > 0` before treating `fresh` as
  meaningful". That instruction is necessary but **not sufficient**, and this arm is what
  closes it: `build_ms > 0` proves a walk completed *some time*, never that one completed
  *recently*. Pair the two — `> 0` **and** moving — or the pair still lies.

  This arm is **not** redundant with `population-divergence` (§4c) even though both fired on
  the same underlying stall on 2026-07-29. Divergence answers *"is the snapshot wrong?"*;
  this answers *"why, and will it self-heal?"* — and a 12 h build time says it will not, which
  is the part that decides whether the fix is patience or a defect report. Fire both; they are
  different findings with different owners.
- **`census_status: "fresh"` is untrustworthy on its own.** `taken_at` tracks the newest
  merged *chunk*, not walk completion (`census.rs:190-200`), so a snapshot holding 14 of
  ~6900 rows reports `fresh`. **Require `census_build_ms > 0` before treating `fresh` as
  meaningful.** `census_build_ms: 0` is the honest field.
- This is a **second positive-feedback lock**, one layer above the one the freshness-lock
  plan fixed: *census too slow → never completes → snapshot is a permanent prefix → no
  removals*. Fixing the first exposed the second.
- Cross-check the matching coord ceiling in the finding.
  `COORD_WORKTREE_RECLAIM_MAX_CENSUS_AGE_SECS` defaults to **900 s**
  (`worktree_reclaim.rs:268-270`, not pinned in `deploy/taskdef.json`);
  `COORD_WORKTREE_REMOVE_GRACE_SECS` is **24 h** (`:285-287`). Both gate the same row.

  **What to record in the finding:** both thresholds, the measured `census_build_ms`, the
  population you used **and which walk produced it**, and — explicitly — the residual that
  freshness does not explain. Do not print a bare "freshness is the binding constraint".

  **This file used to add "so essentially no row is ever simultaneously fresh (<900 s) and
  past the 24 h remove grace." That is REFUTED — do not restate it.** The two thresholds read
  **independent clocks**. Freshness is `now − CensusRow.observed_at ≤ 900`
  (`census_row_is_fresh`, `:274-276`; the row field is its negation,
  `census_stale: !census_row_is_fresh(…)` at `:1541`), and `observed_at` is the DB `now()` at
  the instant the runner's census **chunk** was inserted. `grace_ok` is
  `now − reclaim_candidate_at ≥ 86400`, where for an undeclared row `reclaim_candidate_at =
  census.last_access_mtime` (`:1536-1538`) — the worktree **directory's own filesystem
  mtime**. A tree last touched three days ago and re-walked two minutes ago satisfies both.
  The old claim conflated *when we last looked at it* with *when it was last used*.

  **coord says this itself, and that citation beats any trace of ours** —
  `worktree_reclaim.rs:260-267`: the per-row gate *"DELIBERATELY replaced the old
  WHOLE-census `census_is_stale` gate: on a huge worktree population the census walk exceeds
  the ceiling, so the newest-row-age test made the whole device permanently unservable (0
  removals ever) — while the rows the walk DID refresh were perfectly fresh."* So the 900 s
  ceiling's adversary is the **census walk period**, and the whole-population form of that
  argument is the one coord already fixed.

  **Freshness bounds the rate from ABOVE; it never implies zero, and it never implies a
  floor either.** A ceiling: with `census_build_ms` at 44,098,058 ms (12.25 h),
  `900 / 44,098 = 2.04 %` of the walk's domain can be fresh at any pull. **Do not convert
  that ceiling into an expectation** — under the prefix-stall this very section documents
  (`under_root` frozen while `items` crept) the walk never reaches the session-worktree rows
  at all, so their expected fresh count is ~0, not 2.04 %. And any estimate built from it is
  an **upper bound only**, because `gate` (`:505-575`) applies **`is_dirty` (`:525`) and
  `other_live_reference` (`:543`)** before it ever reaches `landed` and `grace_ok` — both
  unmeasured from the endpoint, and the fleet's false-dirty trap (untracked
  `.claude/agents/`) can drive the first one arbitrarily high. State the bound, name the two
  omitted terms as UNKNOWN, and **never write "freshness explains a low rate, so a zero must
  have another cause"** — that inverts a ceiling into a floor.

  If you do compute a rate, take the population from **your own disk walk** (§2a), never
  from the endpoint's `items` — §2 forbids that base, and using it here would put the
  detector's own arithmetic on the authority the detector exists to distrust. Print the
  population and its stamp beside the result. The cap is
  `COORD_WORKTREE_RECLAIM_MAX_REMOVALS_PER_TICK` (`:989`), default **25** and pinned `"25"`
  in `deploy/taskdef.json` — so a cap-bound population shows ~25 reapable, not 0. (The doc
  comment at `:983-987` still says the taskdef pins `0`; it is stale, and the test
  `max_removals_per_tick_defaults_25_and_env_overrides` is the authority.)

- **`reason: not-cleared` on the reclaimable endpoint is a DEFAULT, not a diagnosis. Never
  read it as coord's verdict.** coord's `GET /coord/worktree-reclaim/:device_id` returns only
  `{rejunction_armed, remove_armed, instructions}` (`worktree_reclaim.rs:1776-1783`) — there
  is **no `blocked[]` field**, and the runner's `ReclaimPull.blocked` is `#[serde(default)]`
  with the comment *"Absent on today's coord (the route only ships cleared instructions)"*
  (`runner reclaim.rs:134-141`). So `coord_block_reason` is always `None` and every un-cleared
  row degrades to `SkipReason::NotCleared` (`on_demand.rs:255`). **A `reason` histogram from
  that endpoint therefore reflects only the RUNNER's local guards** — which is exactly why
  `dirty` appears in it and nothing coord-side ever does.

  There is no reason on the other side either: `evaluate_undeclared_reclaims` never tallies
  `deferred_by_reason` (it plain `continue`s at `:1549`/`:1564`; only the *declared* loop
  tallies at `:1407-1411`), and its shadow log at `:1580` is guarded by `shadow_count > 0`, so
  a zero-emission tick logs **nothing**. `coord_worktree_reclaim_deferred_by_reason` will not
  name a reason for the session-worktree population. **Report `not-cleared` as "coord emitted
  no Remove instruction for this row, cause not on the wire" — a location, not a cause.**

  Two further structural notes for reading the same payload:

  - **`pinned` is hardcoded `false`** for undeclared rows (`worktree_reclaim.rs:1526-1531`,
    *"there is nowhere to store one"*), so a measured `pinned = 0` over this population is
    guaranteed and is evidence of nothing. Do not cite it as "the local gates contribute
    nothing".
  - **Clearance does NOT require a declared `coord.agent_worktrees` row.**
    `qontinui-worktrees/` is matched as marker shape 4 by `path_is_agent_worktree`
    (`worktree_observer.rs:311-330`, documented *"LEDGER-LESS BY DESIGN"*), and
    `COORD_WORKTREE_UNDECLARED_REMOVE_ENABLED=true` in `deploy/taskdef.json`. The real caveat
    is narrower: the two *other* Remove producers both iterate declared rows, so session
    worktrees have exactly **one** code path and no fallback. (Whether neither of those two
    can ever emit for an undeclared path is inferred from their iteration source, not traced
    end to end — treat it as such.)

### 4c. Population-divergence detector

`endpoint_items_under_worktree_root` vs the steward's disk count from Step 2a. Fire
class-12 `census-population-divergence` when the ratio is `< 0.5`, and **always** print both
numbers side by side with their stamps. Live today this fires at `0 / 6668`.

**A steward that reports "13 worktrees" is FAILING its verification bar, not passing it.**

### 4d. Canonical-exclusion-divergence detector

Steward's own `$Canonical.Count` vs the endpoint's `canonical_excluded`. Any mismatch is a
class-12 finding. Live 2026-07-28: **37 vs 1**.

`$Canonical` here is the **real-clone** set from Step 0.4 — 37 — not the 341 dirs that have
a `.git` of any kind. Using the bare-`Test-Path` set would report `341 vs 1` and the
detector would be firing on its own measurement bug rather than on the endpoint's
prefix-stall. Also print `worktrees 304` and `unclassifiable 0` beside it: the three
numbers together are what make the 37 auditable.

### 4e. Missing-scheduler detector (the classes with no inertness coverage at all)

Classes 3, 5, 6 and 10 have **no** inertness coverage anywhere in the fleet.
`QontinuiCargoSweep` is the standing counter-example: **as of 2026-07-27, re-verified
2026-08-22, it is not a registered scheduled task on this machine at all.**

> **Corrected 2026-08-22.** This paragraph used to open "it ran green at 04:00:01 the
> morning 348 GB was found in 31 target dirs, **and** … it is not a registered scheduled
> task at all". Those cannot both be true, and the second half is the evidenced one:
> `schtasks /query` lists only `QontinuiSessionSnapshot` and `QontinuiSpecCiSoak`, and
> `git log --all --diff-filter=AD` over this repo shows no installer for the sweep was
> ever added or deleted on any branch — so nothing here could have registered it. The
> "ran green" half could not be corroborated either: this machine's
> `Microsoft-Windows-TaskScheduler/Operational` channel returns **no events at all**, so
> it is UNKNOWN rather than merely false. A detector whose own headline asserts a run
> that never happened is the defect class this file exists to catch.
> (Plan `2026-08-22-cargo-target-pools-unenumerated-and-unswept`, Phase 5.)

```powershell
# stderr is NOT redirected — a suppressed error here would become a confident
# "unregistered", which is the same class of lie the detector exists to catch.
$tasks = & schtasks /query /fo LIST
if ($LASTEXITCODE -ne 0) { 'UNKNOWN: schtasks exited ' + $LASTEXITCODE } else {
  $has = @($tasks | Select-String -SimpleMatch 'QontinuiCargoSweep').Count -gt 0
  if (-not $has) { 'CLASS-12: no scheduler behind class 6 (QontinuiCargoSweep unregistered)' }
}
```
An owner with no invoker is an inertness condition and nothing else reports it.

**Since 2026-08-22 there IS an invoker**: `qontinui-claude-config/scripts/install-cargo-sweep.ps1`
registers the task (daily 04:00, Interactive/Limited), and
`install-cargo-sweep.ps1 -Check` reports registration, argument drift, a dangling script
path, a disabled task and trigger-time drift. This detector stays exactly as it is — the
installer existing is not the task being registered on *this* machine, which is the only
thing the probe above actually establishes. When it fires, the fix line to quote is:

```
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install-cargo-sweep.ps1
```

**This file REPORTS that and stops there. It does not invoke `cargo-sweep-all.ps1`.**
That script deletes target dirs — a destructive verb — and **class 6 has no row in the
class-arming table, which is the only thing in this file that authorises a destructive
verb.** Class 6 is also listed as out-of-scope at the bottom. So "there is no scheduler" is a
class-12 *finding* about someone else's class, not a licence for this file to become the
scheduler: invoking an unarmed, out-of-scope destructive verb because its owner is inert is
precisely the reasoning the arming table exists to refuse. Report it, name the owner, and
leave invocation to whoever owns class 6. ~~Note also that the script's own default is
`-StaleDays 14` (the `$StaleDays` param default; `:91`, not the `:72` this line used to cite), not the 7 d the class-6 spec assumes — report the disagreement
rather than resolving it here~~ **Resolved 2026-08-22: the disagreement was in
`cargo-guard.sh`, which advertised 7 d against the sweeper's 14 d default. The advisory
now states 14 and names `-StaleDays`, so 14 is the single number.** If the class-6 spec
still assumes 7, it is the one left to correct — do not leave two thresholds silently
disagreeing.

### 4f. On firing

1. **Emit a coord finding** via `coord_post_finding`, carrying: class, verdict, window
   bounds with stamps, `pop_start` / `pop_end` / `removals` / `arrivals` with their sources,
   and the specific detector that fired. Never a bare "reaper looks inert".
2. **Write a plan** — **only when `on_finding == 'plan'`** (the default). Named
   `YYYY-MM-DD-<reaper>-inert-<slug>.md`, in the Symptom / Evidence-verbatim / Root cause
   file:line / Fix design + detection-gap / Recovery shape, then run `Skill: vet-imp` on it.
   Delegate the root-cause trace to a read-only `Explore` subagent; keep the main session a
   ledger.

   **Under `on_finding == 'record'`, skip THIS step (2) only — step 3 still applies in its
   `record` branch and step 4 applies unchanged — and instead do all three of:**

   a. **Resolve `$planDir` exactly as the guarded block below does** (you need its result for
      the print, and an unresolved value must never be interpolated into a path). Reuse only
      its *resolution*, not its message: that block's
      `'plan NOT written: $QONTINUI_PLANS_DIR unset…'` line would misattribute the cause here
      — under `record` the flag withheld the plan, not the unset variable — and would duplicate
      (c). Suppress it and let (c) speak.
   b. **Register a coord gate via `/gate`** whose predicate is the already-established
      observable cause, so the deferral has a watcher rather than only a record.
      - **Preferred, and the fit for the motivating case: `runner_served_sha`**
        `{device_id, repo, expected_sha}` — resolve `device_id` at run time the same way
        everything else here is resolved (never hardcode a UUID): `coord_query_identity`, else
        `$env:QONTINUI_MACHINE_ID`, else the `device_id` field of `~/.qontinui/machine.json`.
        If none yields a UUID, this predicate is unavailable — fall through to the next
        choice rather than guessing. "this device's running binary is at-or-past
        `<sha>` by ancestry". Verified present in the `coord_register_gate` predicate enum,
        and it fail-opens (never clears off a stale/offline runner or a cold mirror), which
        is the safe direction.
      - **Second choice, and genuinely usable: `metric_threshold` on
        `coord_worktree_reaper_inert`** — e.g.
        `{metric:"coord_worktree_reaper_inert", labels:{device:"<uuid>"}, op:"lte", value:0}`
        ("this device's reaper stopped being inert"). Verified on `origin/main`:
        `metric_threshold` evaluates against coord's **assembled `/metrics`** text
        (`gates.rs` → `assemble_metrics` → `sum_metric_series`), and this series IS in it
        (emitted by `worktree_reclaim_events.rs::format_efficacy` into the worktree
        `/metrics` fragment). **Do NOT infer otherwise from `coord_query_metric` rejecting
        the name** — that tool has its own, narrower agent-facing read whitelist and is an
        independent surface from the gate evaluator. Confusing the two is a live trap: it
        cost this file one wrong edit already.
      - **The removals counter is `coord_worktree_reclaim_removals_total`.** Name it
        correctly or the rate bar cannot be computed even when everything else works.
        **`coord_worktree_removals_total` — the name this file used to print — does not
        exist**, and probing it returns exactly the same "not exposable" string a
        *correctly*-named-but-unwhitelisted series returns, so the wrong name is
        indistinguishable from a permissions problem. Nine `/cleanup-steward` iterations
        on 2026-07-29 chased the non-existent name before the real one was found by
        grepping `origin/main` for `coord_worktree_`. The full family, verified on
        `origin/main` (`worktree_reclaim_events.rs:528-551`, reached from
        `worktree_metrics.rs:613` ← `worktree_metrics::render` ← `assemble_metrics`'s
        `"worktree"` leg): `coord_worktree_reclaim_removals_total`,
        `coord_worktree_reaper_inert`, `..._reclaim_candidates`,
        `..._reclaim_candidates_by_trigger`, `..._reclaim_deferred_by_reason`,
        `..._reclaim_instructions_emitted`, `..._reclaim_would_skip_building`,
        `..._orphans_total`, `..._projected_danger`, `..._repair_husks`,
        `..._unjunctioned_target_bytes`.
      - **Whether an agent can READ it is a separate question from whether a gate can.**
        `coord_query_metric` enforces its own allowlist (`qontinui-coord`
        `src/mcp/tools.rs`, `QUERY_METRIC_ALLOWLIST`), which as of 2026-07-29 held 33
        entries and **zero** worktree-family members — so every rate verdict was UNKNOWN
        on the removals term alone. The gate evaluator shares the read path
        (`query_metric_handler` calls the same `gates::read_metric_series`), which is why
        a `metric_threshold` gate worked while the point-read did not. **Re-probe before
        assuming either way**; the allowlist is a one-line edit and may already carry the
        pair by the time you read this.
      - **A `metric_threshold` gate on an absent metric fail-opens `Open` forever and never
        clears** — a rotting gate, not a loud failure. Always confirm the series is actually
        present in `/metrics` before keying a gate on it. **And when reading the pair as a
        point-read, treat `present: false` as UNKNOWN rather than "zero removals"**: they
        render on the HEAVY 60 s-capped `"worktree"` leg (`metrics.rs:148` documents that
        its per-device PG fan-out routinely exceeds the ordinary 3 s leg cap), so a timed-out
        leg and a genuinely inert reaper produce the same absence. Reading absence as zero
        manufactures a false INERT — the same silent-empty-as-NO error this whole file rejects,
        landing on the one detector that exists to catch it.
      - **If no predicate can express the cause, do not fabricate a gate.** Apply
        `planning-and-scope` `no-predicate-record-deferred`: record the item as **deferred**
        in the ledger and the exit handoff, naming (i) the triggering event that will make it
        checkable, (ii) the exact check owed then, and (iii) why no gate exists. Print
        `gate : NONE — no predicate expresses <cause>; deferred, trigger = <event>`.

      **Under `--dry-run`, do NOT register the gate** — dry-run promises zero side effects,
      and a coord gate is a durable write. Print `gate SUPPRESSED (--dry-run)` and skip (b)
      entirely; this is the one case where `record` legitimately leaves no watcher, because
      the run itself is a rehearsal.

      **Never report a gate registered without a returned `gate_id`**; if `/gate` exhausts
      every transport, say so and fall back to the deferred form above rather than claiming a
      watcher that does not exist.

      **A `gate_id` is necessary, NOT sufficient** [policy: `coordination`
      `gate-warnings-mean-not-usable`]. A non-empty `warnings[]`, or an
      `initial_verdict_reason` containing **"cannot evaluate"**, means
      REGISTERED-BUT-NOT-USABLE: the row was written and the gate can never clear. That is
      the same silent-empty-as-NO failure this file rejects everywhere else — a finding
      whose only watcher is an unevaluable gate is a DROPPED finding that reads as watched.
      Re-check with `coord_check_gate_predicate {predicate}` against a control whose answer
      you already know, re-register on a predicate coord can evaluate, withdraw the unusable
      one (`coord_withdraw_gate`), and print the NEW `gate_id`. If nothing evaluable exists,
      fall back to the `gate : NONE — deferred` form above rather than printing an id that
      will never clear. Canonical: `_gate-registration` → "Registration warnings".

      **Set `gate_class`** so coord's per-tenant `gate_clearance` matrix can resolve who may
      clear it: `ops-confirm` for the sweep/reclaim/config confirmations this steward
      defers, `routine-review` for a mechanical re-check, `security-surface` only when the
      withheld work would itself fire a `security-and-autonomy` glob or content trigger.
      **Omit when none applies** — omitting is safe and never a loophole, and a guessed
      class is worse than none. (Canonical: `_gate-registration` → "`gate_class`".)
   c. **Print:**
   ```
   class 12 <verdict> <reaper> -- finding <finding_id> posted; plan WITHHELD (--on-finding=record)
     would have written : <planDir>/<YYYY-MM-DD>-<slug>.md
                          | <fallback>/<YYYY-MM-DD>-<slug>.md  ($QONTINUI_PLANS_DIR unset --
                            this is the qontinui-dev-notes/plans fallback, not "nowhere")
     rationale          : <one line: which detector fired, and the window bounds>
     gate               : <gate_id> -- <the observable condition being watched>
                          | NONE -- no predicate expresses <cause>; deferred, trigger = <event>
     to author it       : write the plan by hand from this line; a later --on-finding=plan run
                          only re-fires if the condition still holds
   ```
   Under `--dry-run` the finding is suppressed, so render `finding SUPPRESSED (--dry-run)`
   instead of an id — **never invent or leave blank a `finding_id`**; a fabricated id is a lie
   in the audit trail, which this file bans absolutely.

   The coord finding from step 1 is still posted (except under `--dry-run`), the ledger line
   still carries the verdict, and the gate from (b) is what actually resumes the item —
   `record` withholds the *authoring*, never the *evidence* and never the *watcher*. That is
   what makes it a deferral with durable closure rather than a drop.

   **Why "author it by hand" and not "re-run under `--on-finding=plan`".** There is no
   mechanism for re-running a *past* iteration: every iteration re-measures from scratch, so a
   later `--on-finding=plan` run authors a plan only if the detector fires **again**. In the
   motivating case it deliberately will not — once the runner is rebuilt the reaper starts
   removing and the verdict correctly clears — so re-running is exactly the wrong instruction
   at the moment the withheld finding is most likely to be revisited. The gate in (b) is the
   real promotion path; hand-authoring from the printed line is the fallback.

   **Do not use `record` to duck a genuine unknown.** It is for a fire whose cause is
   already established (a landed fix not yet in the running binary, an upstream plan already
   open, a deliberate posture). A fire you cannot explain deserves a plan — that is
   finish-to-zero, and the escalation table below is explicit that an inert reaper is a
   defect, not an escalation.

   **Which surface it lands on, and the guard that decides.** `$QONTINUI_PLANS_DIR` is the
   sanctioned active plans directory and is the first choice — but it **must be resolved and
   checked before it is interpolated into a path.** An unset variable expands to the empty
   string, so `"$env:QONTINUI_PLANS_DIR/$name.md"` becomes `/<name>.md` — **the root of the
   current drive.** Never build the path unguarded:
   ```powershell
   # $slug = the <reaper>-inert-<detector> identifier for this finding, e.g.
   # 'worktree-reaper-inert-by-starvation'. Set it before this block; an empty $slug
   # would otherwise yield the meaningless filename '2026-07-28-.md'.
   if ([string]::IsNullOrWhiteSpace($slug)) { throw 'refusing to write a plan with an empty slug' }
   $planDir = $null
   if (-not [string]::IsNullOrWhiteSpace($env:QONTINUI_PLANS_DIR) -and
       (Test-Path -LiteralPath $env:QONTINUI_PLANS_DIR -PathType Container)) {
       $planDir = $env:QONTINUI_PLANS_DIR
   }
   if ($null -eq $planDir) {
       'plan NOT written: $QONTINUI_PLANS_DIR unset or not a directory -- see fallback'
   } else {
       $planPath = Join-Path $planDir ((Get-Date -Format 'yyyy-MM-dd') + '-' + $slug + '.md')
   }
   ```
   **There is no contradiction with the "write in a worktree" guardrail below, because they
   govern different surfaces.** `$QONTINUI_PLANS_DIR` is **not a git repo** (Step 0.3) — there
   is no worktree to create, no branch, no push, and nothing for a peer's WIP to collide
   with; a plain file write is the only possible form and is correct. The worktree discipline
   applies to plans and fixes landing in a **git** repo. So: **class-12 plans land in
   `$QONTINUI_PLANS_DIR` when it resolves; otherwise they land in `qontinui-dev-notes/plans`,
   authored in an isolated worktree off `origin/main` and pushed from there.** If neither
   surface resolves, **do not write** — emit the coord finding, register a gate for the
   unwritten plan, and report both; never fall back to a guessed path.

   **Exempt your own in-flight plan worktree from your own reaper.** If the fallback
   worktree is created under `$Root` with a `dn-wt-*` / `*-wt-*` name, it lands squarely in
   **class 2's population** while you are still writing in it. Add its path to the
   never-touch set for the life of the session, exactly as the ledger file is placed outside
   every repo so a cleanup pass can never delete its own audit trail (Step 0.7).
3. **An inert reaper is NOT an escalation.** Per the escalation table it is a defect, and
   under the default `on_finding == 'plan'` the disposition is: write the plan, vet it, land
   it (charter rule 10 — finish to zero). **Under `on_finding == 'record'` the disposition is
   the gate from step 2's record branch** — the classification is identical (still a defect,
   still not an escalation); only the closure mechanism differs. **Neither reading permits
   doing nothing.** Do not read this step as an unconditional order to author: step 2 owns
   whether a plan is written, and this step must not override it.
4. Also **rule out that a finding fired somewhere nothing pages** — coord's own detector may
   have written to `coord.alerts`, which pages no one
   (`reference_coord_gate_continuation_alerts_never_page_out`). "The gauge exists" is not
   "the gauge alerted".

## Step 5 — Escalation: the fleet's CLOSED list only (charter rule 8)

| Situation | Charter clause | Action |
|---|---|---|
| 6100+ worktrees / 505 GB pending deletion | **NOT an escalation.** High blast radius alone is not a trigger when a verification gate exists | The per-item ≥2-signal gate + `--mode=report` shadow window + per-class graduation **is** the gate. Proceed. |
| A `wip-archive` push fails, so ~87 GB of abandoned WIP cannot be preserved before its tree is touched | **data-loss class** | Escalate with the specific failing push. Until resolved, class 11 stays report-only and class 1 abstains on every dirty tree (which it does anyway). |
| A top-level dir with a real `.git` **directory**, unpushed commits, and an unreachable remote | **no verification gate exists** — content preservation is unprovable | **Never act.** Escalate with the path + commit list. |
| Coord read unavailable after the full documented cascade (MCP → `/coord-revive` → loopback proxy → HTTP acting-bearer → device-JWT mint → prod SQL) | **true capability floor** | Escalate **naming the exhausted cascade** (rule 4). Until then classes 1, 2 and 12's coord arm abstain. |
| Disk below critical **and** the only remaining reclaim targets are load-bearing services | **genuine priority tie** | Escalate with a recommendation, not an open question (rule 9). |
| A reaper is inert (class 12 fires) | **NOT an escalation** | It is a defect: write a plan, `/vet-imp` it, land it. Rule 10 — finish to zero. **Unless `--on-finding=record`**, which withholds only the authoring and still posts the finding — for a fire whose cause is already established. That is a deferral with a durable record, not a drop; the escalation classification is unchanged either way. |

Everything else — including deleting thousands of landed-clean-unpinned worktrees once the
class has graduated — is the steward's to decide. Surface an escalation **with a
recommendation**, and use `coord_ask_question` (then status `waiting_human`) for anything only
a human can answer.

## Guardrails (hold every iteration)

**Banned outright from this skill's posture. These are not style preferences.**

| Banned | Why |
|---|---|
| `taskkill`, `Stop-Process`, **any `/T` flag** | Killing `node.exe`/`powershell.exe` terminates Claude Code sessions. Class 10's only verb is the supervisor's `POST /runners/<id>/stop`. |
| `git branch -D` | Force-delete discards unmerged commits. Class 5 (out of scope here) uses `git branch -d` exclusively. |
| `gh pr view` as a landed-ness signal | A coord land reads `closed, merged=false` when the rebase rewrote the sha and **`MERGED`** when it did not — both are coord lands; the phantom-kill bug also leaves landed proposals `OPEN`. No `state`/`merged` value settles it either way. Use `git merge-base --is-ancestor` against `origin/main` (one-way: a fail is NOT "unlanded"), or a symbol grep. |
| Recursive delete through a junction / reparse point | **Unlink only.** The PowerShell mirror of the runner's `unlink_junction` (INV-W4) is `cargo-sweep-all.ps1:118-126`; any path with `[IO.FileAttributes]::ReparsePoint` is `Directory.Delete($p, $false)`, never `-Recurse`. |
| `curl https://coord.qontinui.io/metrics` | 403. Use `coord_query_metric`. Adding an `Authorization` header *also* 403s and mimics auth drift. |
| Writing a second census, a second reclaim gate, or a second target sweeper | The explicit non-goal. Consume `GET /agent-worktrees/reclaimable`, `GET /coord/sessions/worktrees`, `coord_session_worktrees`, supervisor `/runners`. **`cargo-sweep-all.ps1` is class 6's owner and is neither reimplemented NOR invoked here** — §4e reports its missing scheduler; class 6 has no arming-table row. |
| A single global arm flag | Per-class graduation with a recorded shadow window. See the class-arming table. |
| Fabricating a coord gate verdict | Never invent a `failed` verdict — that is a lie in the audit trail. |
| Running this headless as the primary invocation | A headless task has no coord MCP/auth, no cross-class judgement, and — decisively — no observer of its own efficacy. That is the failure mode this skill exists to catch. |

Additional standing guardrails:

- **Archive before delete, never in the same pass over the same tree.** A dirty tree is
  archived (`wip-archive/<yyyy-mm-dd>/<session>/<slug>` pushed to origin) and only re-enters
  class 1 on a **later** pass, after the ref is independently verified present on origin.
  Class 11 is out of scope in this file, so today class 1 simply abstains on every dirty tree.
- **Path name proves nothing about ownership.** `git worktree add` at an existing path
  silently reuses a foreign worktree — its branch, its WIP. Key provenance on the
  registration's `gitdir` + branch, never on the path. Two sessions that both used one path
  leave an indistinguishable history → abstain.
- **Ownership of a `qontinui-worktrees/<uuid>` dir is unprovable from coord** — these dirs
  are never declared to `coord.agent_worktrees` (`declared_count: 0`). Classes 1–2 therefore
  have two *state* signals and a *liveness* signal but **no ownership signal**. The 24 h mtime
  floor, the OS-process-handle check and the runner's live-session set are partial mitigation.
  This residual risk is why classes 1–2 stay report-only until Phase 4 graduates them, and the
  correct upstream fix is to make the runner declare session worktrees to the ledger.
- **The steward's own scan must never become the 6.7 h walk.** One-level-deep census, bounded
  act-time re-check per touched item, never `build_census` inline.
- **Write in a worktree — in a git repo.** Any plan or fix this steward authors **into a git
  repo** (`qontinui-dev-notes/plans`, `qontinui-claude-config`, any application repo) is
  written in an isolated git worktree off `origin/main` and pushed from there — never in a
  shared canonical checkout, whose WIP a peer owns. **`$QONTINUI_PLANS_DIR` is exempt because
  it is not a git repo** (Step 0.3): a plain guarded file write is the only possible form
  there. §4f states the resolution order and the unset-variable guard.

## Continuous operation

- **`/loop` (default).** `/loop 15m /cleanup-steward --mode=report` for a self-pacing
  continuous watch, or omit the interval to let the model pace itself. Each iteration runs
  Steps 0–5 once. Between iterations do nothing but wait — the loop re-invokes.
  **15 m, not 5 m:** the population moves on the order of ~266 dirs/day and the class-12 rate
  bar needs `hours(W) >= 1`, so a faster loop buys nothing and burns the endpoint's already
  unreliable latency budget. Note this holds **only because `W` is a lookback window**
  (§4a) — the anchor is the most recent ledger line ≥1 h old, so a 15 m interval still yields
  a fresh ~1 h window every 15 m. Were `W` the gap between consecutive lines, the 15 m
  default would put every window at 0.25 h and make every rate verdict UNKNOWN forever.
- **`presentation:"terminal"` continuation.** A coord `continuation_spawn` with
  `presentation:"terminal"` on the operator's device opens a visible terminal running this
  skill — the operator sees it and can interrupt.
- **`--once`** runs a single pass (assess → report → exit) for a spot-check. Note that a
  single pass **cannot** compute a class-12 rate unless a stamped ledger line **≥1 h old**
  exists to anchor the window; with `--once` on a cold ledger, every rate verdict is UNKNOWN
  and must print as such. Against a warm ledger `--once` computes real rates, because the
  anchor comes from the file rather than from this run.

## Report (each iteration, and on exit)

**Stamp every iteration `CLEANUP it=N HH:MM:SSZ`.** Every number in the ledger must come from
a command run in **this** iteration. **A carried-forward value is UNKNOWN and is printed as
`UNKNOWN (carried from it=N-1 @ HH:MM:SSZ)`** — never restated as though freshly observed.
The only sanctioned carry-forward is a class-12 *window-start* term, which must print with
its own original stamp.

```
CLEANUP it=7 03:15:02Z  mode=report  on_finding=record  classes=1,2,3,10,12  enabled=1
 pop (own disk walk, 412 ms)     class1 6681 dirs  | 1d 6560 | 7d 5104 | 14d 1620 | husks 493 | unreadable 0
 pop (own disk walk)             class2  280 ad-hoc trees ($Root, .git=file, depth 1) | unpatterned 25
 $Root classification            canonical 37 (.git=dir) | worktrees 304 (.git=file) | unclassifiable 0
 endpoint /agent-worktrees/reclaimable   UNKNOWN (curl exit 28 after 60s)  <- ABSTAIN, not 0
 registrations (class 3)         coord 2087 missing-gitdir 0 / prunable 0 agree=True (walk 7.5s)
                                 | runner 1992 agree=True | web 2262 agree=True | claude-config 46 agree=True
 runners (class 10)              population read OK, 3 entries        <- NOT "UNKNOWN"; see next line
                                 primary EXCLUDED(kind.type=primary) | named-9879 EXCLUDED(kind.type=named)
                                 | 19e599d0619-1b EXCLUDED(protected) => empty set, verified safe (counts toward graduation 1/3)
 class 12
   census-never-converges        UNKNOWN (endpoint read failed this iteration)
   population-divergence         UNKNOWN (endpoint read failed this iteration)
   canonical-exclusion           steward 37 vs endpoint UNKNOWN
   rate: worktree reaper         removals 0 over W=1.0h (anchor it=3 @02:15Z, lookback)
                                 pop 6674@02:15Z -> 6681@03:15Z | arrivals 7
                                 drain 0/h vs arrival 7/h  => PENDING-INERT-BY-STARVATION (window 2 of 3, NOT fired)
   missing-scheduler             FIRING: QontinuiCargoSweep unregistered (class 6 — reported only, never invoked here)
 would-act (mode=report)         class1 0 of 0 inspected (6681 abstained: endpoint UNKNOWN) <- NOT an inspected empty set
                                 class2 0 of 280 inspected | class3 0 of 37 | class10 0 of 3
                                 (this run is mode=report so nothing acts; only INSPECTED iterations earn credit)
 findings posted                 1 (coord finding <id>) | plans opened 0 (on_finding=record: 1 WITHHELD) | gates <gate_id>
 abstentions                     6681 class-1 items (endpoint UNKNOWN) ; reason printed per bucket
 anomalies not chased            primary /health hangs while port 9876 listens (see plan §Unchased)
```

The class-10 line is written in the two-line form above **on purpose**: `3 entries` and
`population read OK` are separate facts, and a supervisor that could not be read prints
`runners (class 10)  UNKNOWN (curl exit 7 after 60s) -- class abstains, no graduation credit`
with **no** per-entry lines and **no** "verified safe" claim beneath it.

Rules for the ledger:
- **Never print a bare number for a failed read.** `UNKNOWN (<reason>)`, always.
- **A read that failed and a population that is genuinely empty must never render the
  same.** `UNKNOWN (<reason>)` vs `0 (read OK, genuinely empty)`. Both are the number
  zero; only one of them is a fact, and conflating them is how an unreachable probe
  becomes "nothing to clean" — and, for class 10, how it becomes graduation evidence.
- **A multi-window verdict that has not met its bar prints as `PENDING-<verdict>
  (window k of 3)`**, never as the bare verdict name. A detector reported as fired when
  it has not fired is the same lie in the other direction.
- **Print both sides of every divergence** with their stamps — the disagreement is the
  product, not an inconvenience.
- **Every abstention is counted and its reason bucketed.** An item silently missing from the
  would-act list is a silent drop (charter rule 7).
- **Findings found → what you did about them.** A found deficiency with no disposition is a
  silent drop.

### Structured exit handoff (on stop / `--once` / disable)

```
CLEANUP HANDOFF  session=<id>  it=1..N  window=<first stamp>..<last stamp>  mode=<mode>  on_finding=<resolved>
 iterations run        : N          terminations: <clean|kill-switch|error>
 classes armed         : <table snapshot, READ FROM THE TABLE — today that is class 3 reap
                         (graduated 2026-07-29), classes 1/2/10 report. Never print an
                         expectation in place of the row you actually read: this template
                         carried a stale all-report expectation for a day after class 3
                         graduated, which invites distrusting a correctly-read `reap`>
 mutations performed   : <MUST be 0 in report mode; assert by `git status` across
                          $Canonical + an unchanged registration count>
 population delta      : class1 <start>@<stamp> -> <end>@<stamp>  (Δ, arrivals, removals)
 class-12 verdicts     : <detector: verdict (window count)> per detector
 findings posted       : <coord finding ids>
 plans opened          : <absolute paths> + /vet-imp status each
 plans WITHHELD        : 0  |  n/a (on_finding=plan)  |  <intended filename + firing
                         detector + gate_id each>
                         **Print this row UNCONDITIONALLY**, including `0` and the `n/a`
                         form. A row that appears only when non-zero is indistinguishable
                         from a step that never ran — the same argument as `unclassifiable
                         N`. Non-zero entries are unauthored findings a successor must
                         triage, each already carrying a gate.
 gates registered      : <gate_id> each, with the observable condition
 escalations           : <closed-list hit + YOUR RECOMMENDATION>, or none
 unchased anomalies    : <what, why not chased, where to look>
 graduation evidence   : per class — shadow-window iterations accumulated / bar
```

Close the session's final report with a **`POLICY_COMPLIANCE` footer** listing the clauses
applied and any `POLICY_GAP` recorded.

## Verification bar for this skill's own first pass

A pass is only *passing* if all of these hold:

1. Per-class counts match an **independent PowerShell measurement taken in the same window**
   (the steward's own report needs ≥2 signals too).
2. **A class-12 finding fires for the live endpoint-vs-disk divergence.** A steward that
   reports "13 worktrees" is failing this bar, not passing it.
3. **Zero mutations *in report mode*** — confirmed by `git status` across the canonical
   checkouts and an unchanged registration count, not by absence of an error message. An
   armed `--mode=reap --classes=3` pass changes the registration count **by design**, so
   there the bar is instead: the count fell by exactly the size of the pruned set, every
   `git status` is unchanged, and each pruned repo's Signal A/B/C were re-read in that
   iteration. An unqualified "zero mutations" would forbid the only action this file
   authorises.
4. Every UNKNOWN in the ledger names the probe and the reason.

## Field-tested operating lessons

- **Run as a lean COORDINATOR; delegate heavy work to subagents.** Over a long `/loop` the
  main context must stay a thin ledger. **DELEGATE:** every root-cause trace, plan authoring,
  `/vet-imp` run and pre-PR `code-reviewer` pass — one class or one defect per subagent,
  independent ones launched **in parallel in a single message**. **KEEP INLINE:** the disk
  census, the abstain decisions, the class-12 arithmetic and the ledger. Brief each subagent
  on the landmines it cannot infer (`$QONTINUI_PLANS_DIR` is never-touch; never `gh pr merge`;
  never `taskkill`; write in an isolated worktree) and require it to report what it could NOT
  do. **Consume only the compact report and spot-check its load-bearing claims** — a
  subagent's self-report is one signal, and rule 1 wants two.
- **Re-measure EVERY iteration.** In the merge steward's 2026-07-20 soak, **five of five
  incorrect reports were MEASUREMENT errors, not code errors**, and three shared one root: a
  value from an earlier iteration restated as though freshly observed. *"Unchanged since last
  tick"* is a CLAIM requiring its own fresh read.
- **An iteration is not atomic.** An hour can elapse between the first and last command of
  one scan. Re-read anything you are about to **act** on, not just anything you are about to
  report.
- **Never suppress an error into a value.** `cmd 2>$null` and `|| echo 0` convert a broken
  probe into a confident zero. Verify a probe's dependencies exist and run it with errors
  visible before trusting an empty result.
- **Do not read the working trees for a diagnosis of shipped behaviour.** `qontinui-runner`
  and `qontinui-coord` shared checkouts are routinely parked on stale feature branches that
  do not contain the code you are reasoning about — on 2026-07-27 the runner checkout was on
  `fix/runner-terminal-copy` @ `ad5c3281c`, where `on_demand.rs` **does not even exist**.
  Read via `git show origin/main:<path>`
  (`reference_stale_primary_checkout_causes_phantom_dead_code_findings`).
- **A shipped fix is not a serving fix.** Verify a coord fix is live by ECS task-def image
  tag, never by a green deploy run — coord's push-deploys debounce and silently no-op.
  The debounce reports the run **`success`** while rolling nothing, so `Deploy coord:
  success` is not evidence a change is SERVING: measured 2026-08-24 on coord `b0a6a114`,
  the run concluded `success` while containing a job literally named **`Deploy SKIPPED
  (spacing gate — no rollout)`** — also `success` — with `Build, push, and roll coord →
  skipped`, and **two landed commits were absent from the serving build**. The read that
  actually answers it:

  ```
  aws ecs describe-services --cluster qontinui-staging --services coord \
    --region us-east-1 --query 'services[0].taskDefinition' --output text
  aws ecs describe-task-definition --task-definition <that> --region us-east-1 \
    --query 'taskDefinition.containerDefinitions[*].image' --output text
  ```

  The image tag is the serving sha — compare it to `origin/main`. ⚠️ **The cluster is in
  `us-east-1`**; querying `eu-central-1` returns `ClusterNotFoundException`, which reads
  like an outage rather than a wrong region. Do not carry a region over from a neighbouring
  runbook — the fleet's SSM/Cognito reads (`.claude/commands/babysit-prs.md`,
  `.claude/commands/merge-train-steward.md`) use `eu-central-1`, and this cluster is the
  exception. Same read, stated as a rule, in
  `.claude/commands/merge-train-steward.md` → the honest-bookkeeping step. The
  serving-image surface is also discussed above under the reaper-inertness preconditions
  (`ecs-image-stale` / `coord_query_release_state` / `drift_class`) — that discussion is
  about whether a RESTART may have reset a counter; this bullet is about whether the fix
  you shipped is running at all.
- **The failure NOT to repeat, stated precisely:** a targeted fix shipped, was declared
  *"output-verified"* against a **single two-item observation**, and the population it exists
  to drain grew by 568 over the next five days. Never declare a reaper healthy from a point
  observation.

## Classes NOT covered by this file (named, with a pointer — no silent gaps)

| # | Class | Owner / pointer |
|---|---|---|
| 4 | Merged/orphaned **remote** branch | Phase 5. Registers a `branch_reapable` gate; coord's `branch_reap_worker.rs` deletes. **Blocked on a coord fix:** `GatePredicate::BranchReapable` (`gates.rs:164-171`) requires a non-optional `pr_number`, which PR-less orphan branches lack by definition — the class is a no-op over its own target population until `pr_number` becomes `Option<i64>`. Grace must also exceed the **1800 s** `BranchName` claim TTL. |
| 5 | Stale **local** branch (~6730 fleet-wide) | Phase 5. `git branch -d` **only**, never `-D`; requires `git log origin/main..<b>` empty (the unpushed-commit guard). |
| 6 | Stale `target*` dir | **`scripts/cargo-sweep-all.ps1` owns it — this file neither reimplements it NOR invokes it.** Class 6 has no class-arming-table row, and that table is the only thing here that authorises a destructive verb; the script deletes target dirs. Its default is `-StaleDays 14` (the `$StaleDays` param default; `:91`, not the `:72` this line used to cite). §4e above *reports* that no scheduler currently invokes it — a finding for the owner, not a licence to invoke it from here. |
| 7 | Lapsed coord claim | **Permanently report-only.** No destructive path exists; a lapse may only lower priority. |
| 8 | Rotted-open coord gate | coord's `orphan_reconciler` + `retention_worker`. **Out of scope here: this file reports a rotted-open gate and performs no coord write.** Attestation *is* a write to the shared audit trail, and class 8 has no class-arming-table row — the same rule that bars invoking class 6's script bars attesting here. Hand the attestation to `/gate attest` under whoever owns the class. It must never fabricate a `failed` verdict — that ban is absolute and independent of scope. |
| 9 | Stale plan-file `GATE-SWEEP` block | Hand to **`/gate-sweep`**. It must scan **both** `$QONTINUI_PLANS_DIR` **and** `qontinui-dev-notes/plans` — plans are split across the two. |
| 11 | Orphaned WIP (~1786 dirty trees, ~87 GB) | **Archive-only**, never delete: push `wip-archive/<yyyy-mm-dd>/<session>/<slug>`; the tree re-enters class 1 on a later pass only after the ref is verified on origin. Until class 11 is live, class 1 abstains on every dirty tree. |

## Rules

- **Report-mode default; destructive work is per-class opt-in.** `--mode=reap` without
  `--classes=` is a no-op by design, and a class in `--classes=` still acts only if the
  class-arming table says `reap`. No global arm flag, ever.
- **Fleet policy governs.** Autonomy charter + coord-served policy documents apply every
  iteration; the stricter wins; cite the clause applied.
- **Silent-empty is UNKNOWN, not NO.** Every external read is 60 s hard-timeout-wrapped and a
  timeout ABSTAINS. This is the rule the whole file rests on.
- **The population is your own disk count.** The endpoint supplies per-item state only, and a
  disagreement between the two is a class-12 finding — reported, never reconciled away.
- **Two independent signals per destructive decision, re-read at act time.** Cached census is
  display only. Two disagreeing signals ⇒ re-probe both, never pick the convenient one.
- **Consume, never re-derive.** No second census, no second reclaim gate, no second sweeper.
- **`$QONTINUI_PLANS_DIR` is never-touch, resolved from the environment at run time.** It is
  not a git repo; deletions there are unrecoverable.
- **Finish to zero.** Any deficiency found while doing this work — an inert reaper, a lying
  metric, a flaw in this skill — is yours: plan it, vet it, implement it (or, under
  `--on-finding=record`, gate it on its established cause — same obligation, different
  closure; never neither). Gate anything
  deferred and return a `gate_id`; name every unchased anomaly.
- **Dogfood carefully.** This steward deletes things. Re-soak in `--mode=report` after any
  change to this file; if classification quality drops, **tighten the gates** — raise the
  graduation bar, add a signal — do not remove abstentions.
