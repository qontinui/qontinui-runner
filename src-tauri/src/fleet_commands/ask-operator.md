# Ask Operator (policy-recorded escalation)

Surface a decision to the operator **through coord** instead of the bare
`AskUserQuestion` tool, so the decision is recorded, priority-policy
shadow-scored, and answerable from the dashboard. This is the **narrow opt-in
pilot** for plan `2026-06-08-policy-driven-question-resolution-and-authoring-ui`:
every decision routed here produces a `(question, your self-scores, operator
answer)` triple that feeds the shadow-agreement measurement (does the policy
resolver reproduce the operator's choice?).

Use it for a genuine decision you would otherwise escalate AND that the
operator's priority sets could plausibly govern (an engineering trade-off, a UX
choice, a deploy/infra call). Don't use it for trivia or pure information.

If what you would ask is permission for an action you believe is closed to
agents (merging a PR, a runner restart, an operator-gated write), that is not a
decision between options. Read the served catalog first, with
`/policy get policy agent-restricted-actions`. A clause that matches names the
sanctioned hand-off for that one step. Take it, then continue everything else.
If no clause matches, find the policy clause that would forbid the step before
you ask. Without one, the step is most likely within your autonomy.

## Arguments

`<the decision>` — a one-line description of what's being decided. If omitted,
infer it from the current context.

## Steps

### 0. Find the clause that reserves it, and record what you found

Before framing anything, write down two things, and keep them for step 3:

- **The step or decision** you would hand to the operator, in one line.
- **The served clause that reserves it to the operator.** Read it fresh this
  run (`/policy get policy <name>`) and quote it as `<doc>@<current_version>
  <clause id>: "<the line>"`. The only reserving sources are the
  `agent-restricted-actions` catalog (an act) and the `escalation-bar` closed
  list (a decision).

Then branch:

- **No clause reserves it.** Do not ask. The step is within your autonomy:
  decide it under the decision policy (engineering, then UX, then
  implementation priorities), record a POLICY_GAP if no clause covers the
  case, and do the work.
- **The clause is closed-list item 2** ("a resource you cannot obtain
  yourself"), **or your premise is that no agent door exists** ("only an
  operator can clear / run / approve this", a refusal such as
  `no_agent_clearable_hits`, an SSO-only route). Run the door test before
  asking, and keep each step's output:
  1. Read `operating-rules` fresh and quote
     `operator-asks-are-decisions-never-api-calls` with its `current_version`.
     It says a missing door for a mechanical action is NOT an item-2 hit,
     because building the door is agent work; item 2 still covers a
     credential or access only a human can grant (an account, a billing
     change, a third-party approval).
  2. Search for a door already in flight: open pull requests across the
     tenant's repositories, and the plan corpus, using a few distinctive words
     from the refusal (its code, the route, the subsystem). Read the
     candidates and keep only the ones that really are the missing door. A
     search that failed, was not reviewed, or showed fewer results than it
     matched (a capped page) proves nothing.
  3. Decide. A door in flight means land it, or adopt it when its author is
     not live. No door means build it. Either way this is not an operator
     question [policy: operator-asks-are-decisions-never-api-calls]: do the
     work and do not ask. What remains is only a credential, account, billing
     change or third-party approval that only a human can grant, with the
     reason no door can exist written down.
  4. Before asking on that remainder, give a fresh-context subagent the
     blocker, the two quoted clauses and your search results (not your
     reasoning) and ask it to argue that this is agent work. If it names a
     door you can verify, do the work instead. Put its verdict in step 0's
     record. Only a remainder that survives this continues to step 1.

  Where the fleet's door-test tool is installed, `/next-move` C0 ("Item 2 is a
  hit only after the door test") runs these same steps through it.
- **Any other clause** (a catalog hit, closed-list items 1, 3 or 4). Continue
  to step 1.

On 2026-10-04 a session asked the operator to clear a classifier-rejected
migration's escalate marker, framed as "operator clears it" vs "rework the
migration". The agent door was already open as a PR, and `operating-rules` had
not been read that run. The shadow resolver agreed with option A, but only
because the right answer was not among the options, so its agreement was
circular.

### 1. Frame the decision

- Phrase a single clear **question**.
- Enumerate **2–4 concrete, mutually-exclusive options**, each `{label,
  description}`. `label` is a short stable id (e.g. `A`, or `postgres`).
- **When the question's premise is that no agent door exists, one option MUST
  be "an agent builds or lands the missing door"**, naming any door step 0
  confirmed. An option set without it can only offer the operator two wrong
  framings, and the shadow verdict over it means nothing.
- **Open the question text with step 0's record:** the reserving clause, quoted
  with its version, and, when the door test ran, the doors it confirmed, why
  no door can exist, and the adversary's verdict. That line is what makes the question auditable: the question row is
  coord's durable copy of why it was asked.
- Pick the **surface** that selects the priority composition:
  - `user_facing` — a user-facing UX decision (ux-led).
  - `system_internal` — an implementation/code decision (engineering-led). **Default.**
  - `infra` — a deploy/infra/operational decision (engineering-led + implementation tiebreaker).

### 2. Self-score the options (you are the judge)

coord has no LLM — **you** score. For each option, assign a `0.0–1.0` score on
each priority of the surface's lead set, judged honestly from the priority
definitions (NOT to justify a pre-picked answer):

- **engineering** (`system_internal`/`infra`): `powerful`, `scalable`, `robust`, `clean`.
- **ux** (`user_facing`): `predictability`, `discoverability`, `no_surprise_reversibility`, `honesty`.
- **implementation** (`infra` tiebreaker): `verified_throughput`, `early_risk_retirement`, `autonomy_with_checks`, `momentum_through_replanning`.

These are the EXACT `coord.priority_sets.ordering` slugs for this tenant — scoring on
any other key (e.g. `powerful_features`) binds to nothing and the resolver returns
`undecided/ResidualTie`. If unsure, fetch them first via the priorities UI / coord.

Score an option on a priority only where the options genuinely differ on it;
where they're equal, give equal scores (the resolver is lexicographic — it falls
through to the next priority). Build the matrix:
`{ "<label>": { "<priority>": 0.0-1.0, ... }, ... }`.

### 3. Record + surface via coord

Call the MCP tool (this requires a coord-connected session — runner-hosted or
otherwise MCP-authed):

```
coord_ask_question(
  question:   "<the question>",
  options:    [{label, description}, ...],
  surface:    "<surface>",
  plan_phase: "<optional context label>",
  scores:     { "<label>": { "<priority>": 0.0-1.0 } }
)
```

The response carries `question_id`, `status`, and — when escalated — a `shadow`
verdict (`{status: resolved|undecided, option?, margin?}`): **what the policy
resolver WOULD have answered**. This is recorded but NOT acted on (shadow). Show
the shadow verdict in your output so the operator sees the policy's pick
alongside their own.

**If `coord_ask_question` is unavailable** (not in this session's MCP allow-set
/ no coord connection): fall back to the built-in `AskUserQuestion`, and note in
your output that the decision was **not recorded** (no coord connection). Never
block the task on coord availability.

### 4. Get the operator's answer

If `status` was `answered` (a policy auto-answered it), use that response.
Otherwise poll:

```
coord_get_answer(question_id, wait_seconds: 120)
```

The operator answers from the dashboard (the question is in their inbox). If it
returns `pending`, tell the operator the question is waiting in the dashboard
inbox and re-poll (or proceed only if you can safely defer). Once `answered`,
proceed with their choice.

### 5. Proceed

Act on the operator's answer. The `(question, your scores, their answer)` triple
is now persisted (`coord.policy_rule_resolutions`, `source:
question_resolve_shadow`, joined to the answer by `question_id`) — it will be
used to measure whether the policy resolver agrees with the operator before any
auto-answer goes live (Phase 5).

## Honesty

- Score from the priorities, not from the answer you'd prefer — a self-serving
  judge poisons the agreement measurement.
- Never claim a decision was recorded without a returned `question_id`.
- Never fabricate the operator's answer; if it never arrives, say so.
