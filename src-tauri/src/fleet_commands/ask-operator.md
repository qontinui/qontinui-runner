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

## Arguments

`<the decision>` — a one-line description of what's being decided. If omitted,
infer it from the current context.

## Steps

### 1. Frame the decision

- Phrase a single clear **question**.
- Enumerate **2–4 concrete, mutually-exclusive options**, each `{label,
  description}`. `label` is a short stable id (e.g. `A`, or `postgres`).
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
