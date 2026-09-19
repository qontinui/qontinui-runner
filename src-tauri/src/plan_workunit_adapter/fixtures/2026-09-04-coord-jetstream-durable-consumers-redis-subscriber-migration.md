# Coord JetStream Durable Consumers — Redis Subscriber Migration (Row 9 Phase 6)

> **Status: IN PROGRESS 2026-09-18 — Phases 0, 2 and 3 DELIVERED; Phases 4 and
> 5 REMAIN; Phase 1 dropped at vet.** Deliberately **not** SHIPPED: this plan
> has outstanding phases, and a terminal stamp here is exactly what makes a
> multi-phase plan stop being dispatched.
>
> **Phase 0** — qontinui-coord #1982, landed (CLOSED / `mergedAt: null` is
> coord's fast-forward land; verified by CONTENT on `origin/main`, and coord's
> own `pr_merged` gate `4e2055c7` reads `cleared`).
> **Phases 2–3** — qontinui-coord **#2225**, commit `c5e936d0`: the
> `nats_consumers` seam, durable JetStream consumers for
> `claims_alert_watcher` and `expectation_supervisor` under two DISTINCT
> durable names (`coord-claims-alert-watcher-claim-expired` /
> `coord-expectation-supervisor-claim-expired`), and the `ws.rs` ephemeral arm.
> **Additive dual-read only: Redis remains authoritative and no `psubscribe`
> was deleted.** Full coord suite VERIFIED on that exact tree — 13381 tests, 0
> failures — with 11 new inline tests that run in BOTH CI lanes (they need no
> NATS server, so unlike the Phase 0 probe they cannot go dormant-green).
>
> **Phase 4 is HELD, not forgotten**: a concurrent session owns
> `build_events.rs`, `state.rs` `PublishMetrics` and the `routes.rs` `/health`
> `publish_metrics` block. Phases 2–3 touch none of those three and add no
> counters. **Phase 5** (`pr_merge/engine.rs` intake) stays last, behind a full
> dual-read cycle and behind Phase 4.
>
> Redis retirement remains gated: `0a32f042` (`failmodes-phase6-redis-retire`)
> is untouched and unattested — its condition is genuinely unmet. See §8 for a
> corrected sibling predicate and §7 for the five vet defects (D9–D13).
>
> History: DRAFT 2026-09-04 → VETTED 2026-09-05 (qontinui-dev-notes#945, D1–D8,
> Phase 1 dropped) → IN PROGRESS 2026-09-06 (Phase 0, #1982) → VETTED
> 2026-09-17 against qontinui-coord `origin/main` `b826916e` (D9–D13; corrected
> the stale "#1982, OPEN" stamp) → IN PROGRESS 2026-09-18 (Phases 2–3, #2225).
> Depends-On: 2026-05-14-failure-modes-at-scale-design — PROVENANCE (this plan
> IS that parent's Phase 6 split), MET.

**Repo(s):** qontinui-coord

## 1. Goal

Give every event subject that is **durable-class** — covered by a stream in
`nats_streams::stream_specs()`, which is what `subject_has_covering_stream()`
(`crates/coord/src/nats_streams.rs:351`) tests and what
`build_events::dual_publish_body` itself consults — a JetStream consumer, and
retire the Redis `psubscribe` for those subjects, so a coord subscriber that
disconnects replays what it missed instead of losing it.

Redis pub/sub stays for the intentionally-Redis-only class
(cache-invalidation fanouts, live dashboard signals, point-to-point command
channels whose durable contract is a DB row or a TTL). **`stream_specs()`,
not this plan, decides class** — and not the test constants either; see §2A.

## 2. Substrate (re-verified 2026-09-17, qontinui-coord `origin/main` `b826916e`)

- **Publish side is done.** `build_events::dual_publish_body`
  (`crates/coord/src/build_events.rs:897`) dual-publishes to Redis and
  JetStream; `nats_streams::ensure` (`nats_streams.rs:145`) declares the
  streams from `stream_specs()` (`:162-330`).
- **Consume side is absent.** Zero `pull_subscribe` / `create_consumer` /
  `jetstream::consumer` references under `crates/coord/src` (grepped against
  `origin/main` `b826916e`, 2026-09-17). The only JetStream-adjacent reader is the
  **core-NATS** `subscribe("events.auth.revocation")` at `auth.rs:1189` —
  see D2, which is why that is *not* the Phase 1 it looks like.
- **Streams declared** (`stream_specs()`), with the retention each consumer
  will replay from on first creation:

  | Stream | Subjects | `max_age` |
  |---|---|---|
  | `github` | `events.github.*` | 30 d |
  | `merge` | `events.merge.>` | 7 d |
  | `coord-events` | `events.coord.>` | 7 d |
  | `ci` | `events.ci.*` | 7 d |
  | `agent-lifecycle` | `events.agent.lifecycle.*` | 7 d |
  | `agent-inboxes` | `events.agent.*.inbox` | 7 d |
  | `fleet` | `events.fleet.>` | 24 h |
  | `agent-intent` | `events.agent.intent.*` | 5 min |
  | `worktree-dirty` | `events.worktree.dirty.*` | 5 min |
  | `sessions` | `qontinui.sessions.>` | 7 d |

- **Redis `psubscribe` call sites — 11 total, 9 non-test** (count verified
  exact; all line numbers re-pinned):

  | Site | Subject | Class | Migrate? |
  |---|---|---|---|
  | `pr_merge/engine.rs:284` | `events.github.*` | durable (`github`, 30 d) | **yes** — merge train intake |
  | `pr_merge/engine.rs:12396` | `events.merge.landed.*` | durable (`merge`, 7 d) | **yes** |
  | `pr_merge/engine.rs:13901` | `INNER_STATE_PATTERN` = `events.coord.merge.*` (`:13892`) | durable (`coord-events`, 7 d) | **yes** — decided, see D4 |
  | `claims_alert_watcher.rs:285` | `events.coord.claim.expired.*` | durable (`coord-events`, 7 d) | **yes** |
  | `expectation_supervisor.rs:1144` | `events.coord.claim.expired.*` — **the same subject** | durable (`coord-events`, 7 d) | **yes** — decided, see D4; and see D3 |
  | `ws.rs:73` | client-supplied pattern (dashboard WS bridge) | mixed | ephemeral ordered consumer for covered subjects; Redis for the rest |
  | `strategy_presence.rs:365` | `PRESENCE_SUB_PATTERN` = `events.strategy.presence.*.*` (`:77`) | Redis-only — uncovered, live signal | no |
  | `policies/resolver.rs:505` | `INVALIDATION_CHANNEL_PATTERN` = `events.coord.tenant.policies.*` (`:426`) | **bypass** — raw-redis published, JetStream never sees it | no — impossible |
  | `pr_merge/settings.rs:2096` | `INVALIDATION_CHANNEL_PATTERN` = `events.coord.tenant.settings.*` (`:2003`) | **durable-class subject**, cache-invalidation reader | no — deliberate carve-out, see D7 |
  | `dirty_state.rs:937`, `:1089` | tests | — | no |

## 2A. What the "durability registry" actually is — and is not

The draft treated `PUBLISHED_SUBJECTS` / `INTENTIONALLY_REDIS_ONLY` /
`REDIS_BYPASS_SUBJECTS` as a registry code could consult. **They live inside
`#[cfg(test)] mod tests` (`nats_streams.rs:378`) and do not exist in a release
build.** They are a *review-time* audit fixture — the parent design's own §1A
says so ("classified **at review time**"), and the module doc says the guard
exists to force "an explicit durability decision at review time".

Two consequences this plan depends on:

- **The runtime authority is `stream_specs()`**, reached through the `pub fn
  subject_has_covering_stream(subject: &str) -> bool` predicate
  (`nats_streams.rs:351`), whose own doc comment states: *"`stream_specs` is the
  single source of truth for which subjects are durable."* `dual_publish_body`
  already calls it. **Every runtime coverage decision in this plan — Phase 3's
  `ws.rs` split above all — MUST call that function.** A test const is
  unreachable from a release binary; a design that "asks the registry" at
  runtime does not compile.
- **The test consts remain the audit half and must be kept in step.** Any
  subject that gains a consumer, or a stream, changes what
  `every_published_subject_is_classified` and
  `redis_bypass_subjects_are_covered_but_ungated` assert. Those guards are the
  regression backstop for this whole migration — expect to update them in the
  same commit as any classification change, never afterwards.

## 3. Design

- **One helper, not nine:** `nats_consumers::durable_pull(js, stream, name,
  filter_subject)` returning a `Stream` of `(subject, payload, acker)`, with
  durable name `coord-<module>-<subject-slug>`, `AckPolicy::Explicit`,
  `DeliverPolicy::All` on first creation, `ack_wait` 30 s,
  `InactiveThreshold` unset (a coord restart must find its consumer). Mirrors
  the `publish_event` single-seam idiom on the consume side. (async-nats 0.48.)
- **The durable NAME is per-reader, never per-subject.**
  `claims_alert_watcher` and `expectation_supervisor` subscribe to the
  *identical* subject `events.coord.claim.expired.*`. Redis pub/sub fans out —
  both see every message. A JetStream durable is a **queue group**: two readers
  sharing one durable name would have the stream *split* between them, each
  silently seeing roughly half its input, with no error anywhere. The
  `coord-<module>-...` prefix is what prevents this, so it is load-bearing
  rather than cosmetic — two readers of one subject get two durables.
- **Idempotency is the load-bearing change, not the transport.** Each migrated
  reader today assumes at-most-once. Before switching, each handler gets a
  replay guard keyed on the event's own id (GitHub `delivery_id` for
  `events.github.*`; `(repo, pr_number, landed_sha)` for
  `events.merge.landed.*`; the claim audit row id for claim events), so a
  redelivered message is a no-op. This is what makes ack-on-completion safe.
- **`DeliverPolicy::All` means the FIRST START is itself a replay** of up to
  the stream's `max_age` — 30 days for `events.github.*`, 7 days for `merge`
  and `coord-events`. So the idempotency guard must be live **before the
  consumer is first created**, not merely before authority flips: creating the
  consumer is the replay event. (Alternative, if a cold replay is judged
  unacceptable for the merge engine: create that one consumer with
  `DeliverPolicy::New` and accept that it starts with no history. Decide per
  subscriber in its own phase; do not make it a global default.)
- **Cutover per subscriber:** dual-read (Redis + JetStream, JetStream path
  shadow-logged, Redis path authoritative) for one deploy cycle → flip
  authority → delete the Redis `psubscribe`. Never both authoritative.
- **Poison-message handling is not optional.** `max_deliver` unbounded plus a
  bounded `max_ack_pending` (§5) means one message a handler always fails on
  stalls that consumer permanently. Pair them: bounded `max_deliver` with a
  terminal `ack_term()` + an error-counter metric, or an explicit park subject.
  Choose one in Phase 4 and apply it to every consumer.
- The three test registries in `nats_streams.rs` are updated in the same commit
  as each classification change (§2A).

## 4. Phases

**Phase 0 is a gate, not a step.** Nothing below it is built until it passes.

0. **Falsification probe (first).** Stand up `durable_pull` against the
   `github` stream with a synthetic restart: publish 3, consume+ack 1, drop
   the consumer handle, reconnect to the *same durable name*, assert the
   remaining 2 are delivered. Also assert the negative that D3 turns on: two
   handles on the **same** durable name split the messages, while two
   *different* durable names each receive all of them. If JetStream replay
   does not behave as §3.1 of the parent design asserts, **stop here.**

   **Phase 0 has a prerequisite the draft did not budget for: coord has no
   NATS test lane at all (D8).** Writing the test is not enough — it would
   never execute. Phase 0 therefore ships three things together:

   a. `test_nats_url()` in `crates/coord/src/test_fixtures.rs`, alongside the
      existing `test_pg_dsn()` (`:138`) and `test_redis_url()` (`:166`),
      reading `COORD_TEST_NATS_URL`. Follow the crate's stated doctrine —
      **self-skip on a missing URL, never `#[ignore]`**
      (`merge_scheduler.rs:75832`: *"Self-skips without a DSN rather than
      carrying `#[ignore]`"*).
   b. A JetStream NATS container in the `coord-db-tests` CI job, beside the
      hand-rolled `docker run` pair at `.github/workflows/ci.yml:1422-1426`,
      exporting `COORD_TEST_NATS_URL` in the env block at `:1502-1510`.
      **Without (b) the test is permanently dormant-green** — the exact rot
      `ci.yml:646-651` legislates against, and that `COORD_REQUIRE_DB_TESTS`
      (`test_fixtures.rs:115-126`) exists to convert into a failure. A
      self-skipping test with no lane that ever satisfies its precondition is
      worse than no test: it reports success forever.
   c. The probe itself, placed **inline in `src/nats_streams.rs`**, not under
      `crates/coord/tests/`. `coord-db-tests` runs `--lib --bins` and
      deliberately excludes the integration-test targets
      (`ci.yml:1587-1590`), while the only job that does build `tests/` is
      `rust-ci` on the self-hosted box, which provisions no services at all —
      so a probe under `tests/` lands in the one lane that can never serve it.
      Inline placement is the only spelling that reaches the provisioned job.
1. ~~`events.auth.revocation` → durable consumer.~~ **DROPPED — the premise
   is false. See D2.** The subject is deliberately Redis-only, has no
   covering stream, and its durable contract is the `coord.revoked_tokens`
   PG table, not the message.
2. `claims_alert_watcher` + `expectation_supervisor` — both on
   `events.coord.claim.expired.*` (durable, decided). **Two distinct durable
   names** (D3). Smallest genuine migration, two readers, one subject: the
   right place to prove the helper and the split-vs-fanout property in prod.

   Concretely, as vetted 2026-09-17:

   - Durable names `coord-claims-alert-watcher-claim-expired` and
     `coord-expectation-supervisor-claim-expired`. They differ in the
     `<module>` slot, which is what D3 makes load-bearing.
   - Stream `coord-events` (`events.coord.>`, 7 d) — resolved through the
     new `covering_stream_for()` (D10), never hardcoded.
   - **Leader-gated (D13).** Both subscriber tasks run on EVERY coord
     instance today; a durable name shared across INSTANCES is the same
     queue-group split D3 describes, one level up. Only the leader consumes.
   - **Dual-read, shadow only.** Redis stays authoritative; the JetStream arm
     is `tracing`-logged and feeds nothing. **No counters** — the
     `PublishMetrics` / `/health` surface is Phase 4's and is owned by a
     concurrent session; Phase 2 must not touch `state.rs`,
     `build_events.rs` or the `routes.rs` `/health` `publish_metrics` block.
   - Neither subscriber closure captures `state` today
     (`claims_alert_watcher.rs` captures `obs`; `expectation_supervisor.rs`
     captures `buffer`); both must clone it in to reach `state.jetstream`.
3. `ws.rs` dashboard bridge: ordered ephemeral JetStream consumer for
   subjects `subject_has_covering_stream()` returns true for, Redis fallback
   for the rest. Moved ahead of the merge engine deliberately — it is
   read-only fanout to dashboards, so a replay bug shows up as a duplicate
   dashboard row rather than a double-landed PR.

   Three corrections from the 2026-09-17 vet, without which this phase is
   not implementable as written:

   - **The `pattern` is a REDIS GLOB, not a NATS subject (D11).** It is
     client-supplied (`WsParams::pattern`, default `events.*`) and the two
     wildcard languages are NOT the same: Redis `*` matches across dots,
     NATS `*` matches exactly one token. So `subject_has_covering_stream()`
     — which takes a concrete subject and uses NATS token semantics
     (`nats_streams.rs:360`) — **cannot be applied to the pattern**. The
     split is decided per pattern by a NATS-side translation, and any
     pattern that does not translate cleanly stays wholly on Redis.
   - **`DeliverPolicy::New`, not `All` (D12).** This consumer is ephemeral
     and per-connection, so "first creation" is EVERY dashboard connect;
     `All` would replay up to the stream's `max_age` (30 d on `github`) into
     a live dashboard each time.
   - Redis remains authoritative for the whole bridge in this phase; the
     JetStream arm is additive and shadow-logged.
4. Observability: `/health` gains `jetstream_consumer_lag_secs{consumer}` and
   `jetstream_redelivered_total{consumer}`; the parent §3.6 "consumer lag
   > 5 min" alert threshold becomes real; the poison-message policy from §3
   is implemented here. **Must precede Phase 5** — the merge engine does not
   go onto JetStream without lag and redelivery visibility already live.
5. `pr_merge/engine.rs`, last and behind a full dual-read cycle:
   `events.coord.merge.*`, then `events.merge.landed.*`, then
   `events.github.*` with the `delivery_id` replay guard. This is the live
   merge train's intake — highest blast radius in the repo.

**Success:** every durable-class subject with a coord reader that needs
at-least-once has a named durable consumer; the only durable-class reader
still on Redis `psubscribe` is the `pr_merge/settings.rs` cache-invalidation
subscriber, by the recorded carve-out in D7; the test registries in
`nats_streams.rs` still pass their guards; and a coord restart mid-burst
loses zero `events.github.*` events (Phase 0 harness, run against prod-shape
streams).

> The success criterion is deliberately **not** "no durable-class reader still
> `psubscribe`s Redis". That phrasing, which the draft used, would mark the
> settings carve-out as a defect. Durability is a property of the
> **subject**; whether a reader needs it is a property of the **reader**.

## 5. Risks

- Replay into the merge engine without the idempotency guard double-lands or
  double-freshens a PR. Mitigation: guard first, dual-read, engine last —
  and note the first-creation replay in §3, which is when the risk actually
  fires.
- Durable consumers accumulate unacked backlog if a handler panics.
  Mitigation: bounded `max_ack_pending` + the Phase 4 lag alert + the
  poison-message policy in §3.
- Two readers of one subject sharing a durable name silently halve each
  other's input (D3). Mitigation: the per-module durable name, and the Phase 0
  negative assertion that proves it.

## 6. Vet findings (2026-09-05)

- **D1 — the durability registry is test-only.** `PUBLISHED_SUBJECTS`,
  `INTENTIONALLY_REDIS_ONLY` and `REDIS_BYPASS_SUBJECTS` are inside
  `#[cfg(test)] mod tests` (`nats_streams.rs:378`). The draft's "the registry,
  not this plan, decides class" implied a runtime authority that does not
  exist in a release build. **Corrected:** §1 and §2A now name
  `stream_specs()` / `subject_has_covering_stream()` (`:351`) as the runtime
  authority and keep the consts as the review-time audit half. The parent
  design's §1A had this right ("classified at review time"); the split lost it.
- **D2 — Phase 1's premise is falsified; Phase 1 dropped.** The draft called
  `events.auth.revocation` "non-durable — a restart during a revocation loses
  it … the highest-value subject". It is not lost. `events.auth.revocation` is
  on `INTENTIONALLY_REDIS_ONLY` (`nats_streams.rs:573`) under the documented
  rationale "live UI / cache-invalidation / command signals with no durable
  reader" (`:562-564`), and therefore has **no covering stream** — so there is
  no stream to put a durable consumer on. `auth.rs:1181-1185` states the
  design directly: *"no JetStream durability needed — missed revocations are
  caught by the boot snapshot from PG"*. Corroborated in code, not just in the
  comment: `snapshot_revocations` (`auth.rs:81`) loads the live revoked set
  from `coord.revoked_tokens` at boot, and the module doc (`auth.rs:9-13`)
  records that a hot-path cache miss *"falls through to a PG SELECT to defend
  against stale bootcache"*. The durable record is the PG row; the message is
  only a cache-warming signal. Implementing Phase 1 would have added a stream,
  a consumer and a reclassification of a deliberately-curated subject for zero
  durability gain.
- **D3 — two readers, one subject, one durable name would silently split it.**
  `claims_alert_watcher.rs:285` and `expectation_supervisor.rs:1141`
  `psubscribe` the *identical* pattern `events.coord.claim.expired.*` and both
  rely on Redis fanout. A JetStream durable is a queue group. The draft never
  stated the constraint. **Corrected:** §3 makes the per-module durable name
  load-bearing, §4 Phase 0 adds the negative assertion, §5 carries the risk.
- **D4 — both "decide in Phase 0" rows are now decided.** `INNER_STATE_PATTERN`
  is `events.coord.merge.*` (`engine.rs:12045`) and the supervisor pattern is
  `events.coord.claim.expired.*` (`expectation_supervisor.rs:1140`). Both are
  in `PUBLISHED_SUBJECTS` and both are covered by the `coord-events` stream
  (`events.coord.>`, 7 d) → **durable, migrate**. No Phase 0 investigation
  needed for either.
- **D5 — nine citations had drifted** across 168 commits (`927361ff` →
  `f3942732`), three by ~1,000 lines. Re-pinned: `dual_publish_body`
  850-1000 → `:894`; `engine.rs` 275 → `:284`, 9760 → `:10745`,
  11054 → `:12054`; `claims_alert_watcher` 283 → `:285`;
  `expectation_supervisor` 1139 → `:1141`; `policies/resolver.rs` 618 →
  `:433`; `pr_merge/settings.rs` 2107 → `:1402`; `auth.rs` 1140-1160 →
  `:1189` (the `subscribe` call; the fn doc is at `:1156`);
  `REDIS_BYPASS_SUBJECTS` doc 641-643 → `:635-666`. Exact and unchanged:
  `ws.rs:73`, `strategy_presence.rs:356`, `dirty_state.rs:901`/`:1053`.
  The draft's psubscribe **count** — 11 total, 9 non-test — verified exact.
- **D6 — `DeliverPolicy::All` makes consumer creation a replay event**, of up
  to 30 days on `events.github.*`. The draft ordered the idempotency guard
  "before switching [authority]"; it must be before the consumer is *created*.
  **Corrected** in §3 and §5. Also reconciled the draft's contradictory
  `max_deliver` unbounded (§3) against bounded `max_ack_pending` (§5): §3 now
  requires an explicit poison-message policy, implemented in Phase 4.
- **D7 — the two "Redis-only invalidation" rows are not the same case, and one
  of them is durable-class.** The draft labelled `policies/resolver.rs` and
  `pr_merge/settings.rs` identically as `INTENTIONALLY_REDIS_ONLY`. Neither is.
  `policies/resolver.rs:433` reads `events.coord.tenant.policies.*` (`:357`),
  which is in **`REDIS_BYPASS_SUBJECTS`** (`nats_streams.rs:671`) — published
  only through a raw `redis::cmd("PUBLISH")`, so JetStream genuinely never
  carries it and migration is *impossible*, not merely unwanted.
  `pr_merge/settings.rs:1402` reads `events.coord.tenant.settings.*` (`:1330`),
  which is in **`PUBLISHED_SUBJECTS`** (`nats_streams.rs:493`), gate-published,
  and covered by `coord-events` — the `REDIS_BYPASS_SUBJECTS` doc calls it out
  by name as *"genuinely durable, not a bypass-only subject"* (`:660-663`). So
  it is a durable-class subject whose reader deliberately does not want
  durability: replaying 7 days of cache invalidations at boot achieves nothing
  the boot-time load does not already do. **Corrected:** the §2 table now
  distinguishes the two, and the success criterion above no longer counts the
  settings reader as a violation.
- **D8 — Phase 0, the plan's own gate, was not implementable as written.**
  coord has **no NATS test lane of any kind**. No test in the repo connects to
  NATS; `nats_streams::connect()` (`:64`) and `ensure()` (`:145`) are exercised
  by nothing, and their only callers are the production boot path
  (`state.rs:260-262`). There is no `test_nats_url` fixture beside
  `test_pg_dsn` / `test_redis_url` in `test_fixtures.rs`, no NATS analogue of
  the `spawn_stub_redis` stub (`:349`, and a `+OK` handshake stub could not
  serve replay semantics anyway), no `testcontainers` dependency, and **no CI
  job anywhere starts a NATS server** — the string does not occur in any of
  the 8 workflows outside a post-deploy `/health` assertion. The two Rust jobs
  are `rust-ci` (self-hosted, builds `tests/`, provisions *nothing*) and
  `coord-db-tests` (provisions PG + Redis by hand at `ci.yml:1422-1426`, but
  runs `--lib --bins` and explicitly excludes `tests/` at `:1587-1590`). So a
  Phase 0 probe written the obvious way either hard-fails every PR or is
  dormant-green forever. **Corrected:** Phase 0 now ships the fixture, the CI
  container and the inline probe together, and §4 records why each placement
  choice is forced. This is the single largest omission the vet found — the
  plan's stop-gate could not have run.
- **Phase order changed** as a consequence: old Phase 1 dropped; the two claim
  watchers become the first migration; `ws.rs` moves ahead of the merge engine
  (read-only fanout fails visibly and harmlessly); observability moves ahead of
  the merge engine rather than being noted as a precondition of it.

## 7. Vet findings (2026-09-17, `origin/main` `b826916e`)

Five defects, all corrected above. This pass was run ahead of a **scoped
Phases 2–3 implementation**; Phases 4 and 5 were re-read but not re-derived.

- **D9 — the §2 citations had drifted AGAIN**, across the commits from
  `f3942732` to `b826916e`. Six of the eleven `psubscribe` rows moved, two by
  well over 1,000 lines: `engine.rs` `10745 → 12396` and `12054 → 13901`
  (its `INNER_STATE_PATTERN` const `12045 → 13892`); `expectation_supervisor`
  `1141 → 1144`; `policies/resolver.rs` `433 → 505` (const `357 → 426`);
  `pr_merge/settings.rs` `1402 → 2096` (const `1330 → 2003`);
  `strategy_presence.rs` `356 → 365`; `dirty_state.rs` `901/1053 → 937/1089`;
  `dual_publish_body` `894 → 897`. **Exact and unchanged:**
  `claims_alert_watcher.rs:285`, `ws.rs:73`, `engine.rs:284`,
  `nats_streams.rs` `:64` / `:145` / `:162` / `:351` / `:378`. The
  psubscribe COUNT — 11 total, 9 non-test — is still exact. Re-pinned in §2.
- **D10 — Phase 3 needs subject → stream NAME resolution, and no such API
  exists.** `subject_has_covering_stream()` (`nats_streams.rs:351`) returns
  `bool`; `stream_specs()` (`:162`) is **private**; and the cached
  `COVERING_SUBJECT_FILTERS` flattens every spec's subjects and **discards the
  stream name**. But `stream.create_consumer()` needs a stream to attach to,
  so both Phase 2 and Phase 3 need the name, not the boolean. **Corrected:**
  Phase 2/3 add `pub fn covering_stream_for(subject: &str) -> Option<&'static str>`
  beside the existing predicate, sharing one cache, and
  `subject_has_covering_stream` becomes `covering_stream_for(..).is_some()` so
  the two can never disagree.
- **D11 — `ws.rs`'s pattern is a Redis glob, so Phase 3's own instruction was
  a category error.** Phase 3 said to take "subjects `subject_has_covering_stream()`
  returns true for", but `ws.rs` never has a subject: it has a
  **client-supplied Redis PSUBSCRIBE pattern** (`WsParams::pattern`, default
  `events.*`). Redis `*` matches across dots; NATS `*` matches exactly one
  token (`subject_matches_filter`, `nats_streams.rs:360`). Under Redis the
  default `events.*` matches `events.coord.claim.expired.x`; under NATS it
  matches only two-token subjects. Handing the pattern straight to the
  predicate would silently change which events a dashboard receives.
  **Corrected** in §4 Phase 3: translate Redis-glob → NATS filter explicitly,
  and fall back wholly to Redis for any pattern that does not translate.
- **D12 — `DeliverPolicy::All` is wrong for the `ws.rs` consumer.** §3
  mandates `All` "on first creation", which is right for a durable whose
  cursor survives restarts. The `ws.rs` consumer is **ephemeral and
  per-connection**, so first creation is every dashboard connect, and `All`
  would replay up to `max_age` — 30 days on `github` — into a live dashboard
  every time. **Corrected** to `DeliverPolicy::New` in §4 Phase 3. (Robustness:
  the phase was placed early precisely because its failures are supposed to be
  visible and harmless; a 30-day replay per connect is neither.)
- **D13 — a durable name shared across coord INSTANCES splits the stream, and
  D3 only caught the two-MODULE case.** Both claim watchers' subscriber tasks
  run on every coord instance (only their tick loops are leader-gated —
  `state.leader.is_leader()`). Redis pub/sub fans out, so every instance sees
  every message today and the non-leaders simply discard their buffers. A
  JetStream durable is a queue group **keyed on the durable name**, so N
  instances on one durable name would each see ~1/N of the subject — D3's
  exact failure mode, one level up, and equally silent. **Corrected:** the
  JetStream consumer task is leader-gated (§4 Phase 2). This is also strictly
  better than today — the leader consumes and acks, and a failover re-attaches
  to the same durable and replays the unacked remainder, which is the
  durability win the plan exists for. (Decided on **robustness**: per-instance
  durable names would be the alternative, and they defeat cross-restart
  durability outright, since instance identity does not survive a restart.)

### Not a defect, but a trap the next reader will hit

coord's derived delivery for this plan reads `shipped: true`,
`evidence_complete: true`, `phases_remaining: []` while **four phases are
outstanding**. The cause is visible in the same response:
`phases_declared_indices: [1]` (coord parsed exactly one phase, index 1 — the
phase this plan DROPPED) and `phases_delivered: []`. The only citation, PR
#1982, carries no `phases:` scope, so nothing could be attributed and
`phases_remaining` computed empty. Recorded in the status block; the
remedy is a scoped citation (`Plan: <stem> phases: 2,3`) on every later PR.

### Related gate

The parent work unit's Redis-retire gate
`0a32f042-1d28-441f-a363-91e788dac4ca` (`failmodes-phase6-redis-retire`)
carries a **mis-specified** CHECK: `abs(redis_publish_ok - jetstream_publish_ok)
< 10` can never pass, because `dual_publish_body` deliberately skips the
JetStream arm for uncovered subjects, so the delta accumulates permanently in
`jetstream_publish_skipped_uncovered`. A corrected gate was registered
2026-09-17 as `73139ecd-268e-41a4-8aa0-80d7cf1a90d1`
(`failmodes-phase6-redis-retire-burnin-corrected`, same work unit), using the
exact residual identity `redis_ok - js_ok - skipped_uncovered == 0` plus a
volume floor, a JetStream-configured precondition, and an explicit statement
that these process-local counters cannot evidence burn-in DURATION at all.
The old gate is deliberately left open and unattested — its condition is
genuinely unmet, and withdrawing it would permanently block the parent unit
from deriving `ready`.

## 8. Implementation review findings (2026-09-17, Phases 2–3)

Found by an independent `code-reviewer` pass on the Phases 2–3 diff, recorded
here because three of them are **durable facts the remaining phases need**, not
just defects in one patch.

- **The Redis and NATS spellings of one subscription MUST be bound in code.**
  The first draft of Phase 2 passed the Redis glob `events.coord.claim.expired.*`
  straight through as the JetStream `filter_subject` — the exact D11 conflation
  this plan documents, committed by the phase that documents it. It matters
  because `claims_expiry_watcher::claim_expired_subject` builds
  `events.coord.claim.expired.{resource_key}` **with no sanitisation**, and real
  claim resource keys contain dots: a `FileGlob` key is `src/**/*.rs`, a
  `Symbol` key is `qontinui-coord:src/claims.rs:default_ttl_for`. Those are six
  NATS tokens. A one-token `*` filter matches five, so **every `FileGlob` and
  `Symbol` claim expiry would have been invisible to both shadow consumers**
  while the Redis arm delivered all of them — and because both watchers would
  miss the same events, the dual-read would have reported success. Fixed with
  bound consts (`CLAIM_EXPIRED_REDIS_PATTERN` / `CLAIM_EXPIRED_NATS_FILTER`)
  and a test asserting the translator maps one to the other.
  **Scope check for Phase 5, done: its three subjects are NOT exposed to this.**
  `events.github.{event}` interpolates a GitHub webhook event name
  (`pull_request`, `push`, …) — always one token; `events.merge.landed.<repo>`
  uses a hyphenated repo slug (`owner-repo`). Verify before relying on it, but
  the hazard is specific to claim resource keys.
- **`AckPolicy::None` is not valid on a PULL consumer** — async-nats states
  `Explicit` is the only valid policy for pull-based consumers, and nats-server
  carries a matching refusal. Phase 3's ephemeral consumer originally used
  `None`; had the server refused it, `ws.rs` would have degraded to Redis-only
  on every connect and **Phase 3 would have shipped as a permanent silent
  no-op** that no test on a box without NATS could catch. Now `Explicit` with
  bounded acking.
- **Bound every ack.** Both ack sites are wrapped in a 500 ms timeout, matching
  the bound `build_events::dual_publish_body` already puts on the publish-side
  ack. In `ws.rs` this is load-bearing for the plan's own rule that Redis stays
  unaffected: the shadow ack sits in the same `select!` loop that forwards the
  authoritative Redis frames, so an unbounded ack against a sick NATS would
  have turned a JetStream problem into a dashboard outage.
- **Leader-gating needs the fencing token, not just `is_leader()`.**
  `is_leader()` is a snapshot, so check-then-act leaves a window in which an old
  and a new leader are both attached to one durable — D13's split, narrowed to a
  failover but not eliminated. `LeaderHandle::fenced_token()` already existed
  and is now captured with the leadership check.
- **A distinctness test over two literals typed into the test is a tautology.**
  The original D3 test hardcoded both module strings in its own body, so either
  call site could have been changed to the other's value with the test still
  green — while the two watchers queue-group-split. It now asserts over the
  consts the call sites actually use.

**Recorded decision, for the Phase 4 owner — stated as a BOUND, not a mood.**
`/ws` is an unauthenticated route (`routes.rs`, the public group beside
`/livez`). The concrete unbounded quantity Phase 3 introduces is:

> **an anonymous client can create one server-side JetStream consumer per
> `/ws` connection, on any declared stream whose subject filter contains its
> translated pattern; nothing caps this but the connection count.**

What already bounds it: an explicit 30 s `inactive_threshold` caps a consumer's
life after a connection is abandoned; `max_ack_pending` bounds in-flight
messages per consumer; `max_deliver: 1` stops an unackable message redelivering
for the life of the connection; and the DEFAULT pattern `events.*` never takes
the JetStream arm at all, because `events.>` straddles several streams. What is
NOT bounded is N connections → N consumers.

This is a deliberate non-decision, not an oversight: adding a semaphore silently
would hide a new amplification on an anonymous route behind a mechanism nobody
reviewed. Settle it explicitly before authority flips. A related instance of the
same family: a pattern that translates to a syntactically INVALID NATS filter
(e.g. `events.a b`, which contains no Redis metacharacter and so translates)
still reaches `create_consumer` and costs one `warn!` per connection.

### Two premises Phase 4 / Phase 5 must re-check before flipping authority

Both are correct **today** and become wrong the moment the `ws.rs` JetStream arm
stops being shadow-only. They are recorded here because the next reader meets
the settings long after the premise that justified them.

1. **`max_deliver: 1` makes the `ws.rs` arm explicitly AT-MOST-ONCE.** It is
   right while nothing replays that consumer and no client frame depends on it
   — it is what stops an unackable message re-logging for the life of the
   connection, and it is what releases the `max_ack_pending` slot within
   `ack_wait` instead of pinning it. But if a later phase makes this arm
   authoritative for frames the dashboard receives, `max_deliver: 1` becomes a
   **silent drop on a slow ack**. Revisit it in the same change that flips
   authority, never after.
2. **The `/ws` consumer amplification** recorded above — one JetStream consumer
   per anonymous connection, capped only by the connection count — is bounded
   today by `inactive_threshold: 30s`, `max_ack_pending`, `max_deliver: 1` and
   by the default `events.*` never taking the arm. Flipping authority changes
   the cost of every one of those bounds.

