//! Capture hook for co-occurrence observations.
//!
//! Called fire-and-forget from the UI Bridge snapshot handler. Converts a
//! snapshot into a single observation row keyed by (fingerprints,
//! snapshot_metadata). Never returns an error to the caller — the snapshot
//! response must never be blocked or failed by observation capture.

use std::sync::Arc;

use tokio::sync::OnceCell;
use tracing::warn;
use uuid::Uuid;

use crate::database::pg::PgDb;
use crate::spec_api::slug::pathname_to_spec_id;

use super::fingerprint::stable_element_fingerprint;

/// Sample rate for observation capture. K=1 captures every snapshot; set
/// higher to downsample if the observations table grows too fast. See the
/// "Observation volume" risk in the design doc — partitioning and
/// downsampling are the two levers we'll reach for first.
const SAMPLE_RATE: u32 = 1;

/// Resolve the page identity that labels this observation.
///
/// This is a **selection label, not a partition key.** Derivation stays global
/// — co-occurrence clustering groups elements that appear in the same set of
/// renders, so restricting the render pool to one page would collapse that
/// page's persistent elements into a single mega-state and destroy the
/// cross-view signal the algorithm exists to find. The label is what later
/// lets authoring project the global state set `S` down to the states active
/// on one page (`S_Ξ ⊆ S`).
///
/// Precedence:
/// 1. `page.pageContext.meta.tabId` — a stable developer-supplied view id.
///    Desktop SPAs route in React state, so the URL never moves; the tab id is
///    the only thing distinguishing one view from another. (The runner's whole
///    corpus sits at `http://tauri.localhost/`.)
/// 2. top-level `activeTab` — the same identity, supplied by the SDK's own
///    `getActiveTab` provider rather than by `usePageContext`. It is already
///    present in every runner snapshot, so labelling works on runners built
///    before `meta.tabId` existed instead of waiting for a rebuild.
/// 3. `page.pageContext.name` — slugged. For apps that call `usePageContext`
///    without a tab id. Display labels drift on rename and collapse all
///    twelve `settings-*` views onto one name, so this ranks below both ids.
/// 4. `page.pathname` — slugged. Correct for real-URL apps (qontinui-web).
///
/// Blank candidates are skipped rather than accepted, so an empty `tabId`
/// falls through to the next source instead of shadowing it. Returns `None`
/// when nothing yields a usable slug; the observation is then recorded
/// unlabelled rather than dropped, since it still carries co-occurrence
/// signal for the global derivation.
fn resolve_page_label(snapshot: &serde_json::Value) -> Option<String> {
    let page = snapshot.get("page");
    let page_context = page.and_then(|p| p.get("pageContext"));

    let candidates = [
        page_context
            .and_then(|c| c.get("meta"))
            .and_then(|m| m.get("tabId")),
        snapshot.get("activeTab"),
        page_context.and_then(|c| c.get("name")),
        page.and_then(|p| p.get("pathname")),
    ];

    let raw = candidates
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|s| !s.trim().is_empty())?;

    Some(pathname_to_spec_id(raw))
}

/// Resolve the app that produced this snapshot, from the snapshot itself.
///
/// Read untyped off the top level, exactly like [`resolve_page_label`] reads
/// `activeTab` — and for the same reason. The snapshot handler dispatches by
/// `window_label` and holds no app identity of its own (the call site in
/// `mcp::ui_bridge::elements` says so outright: "the caller has no scope
/// knowledge the snapshot doesn't already carry"), so the id must arrive
/// inside the snapshot rather than as a parameter. The runner frontend stamps
/// it through its `runner-tabs` snapshot enricher from the single
/// `RUNNER_APP_ID` constant.
///
/// Blank values collapse to `None` so a whitespace-only `appId` records "app
/// unknown" rather than a key that can never match `project.apps`. The value
/// is trimmed, so incidental padding still matches the registry instead of
/// silently attributing the row to nothing.
///
/// Returning `None` is meaningful, not a failure: the column is nullable and
/// `NULL` means exactly "app unknown". Attribution is never fabricated — an
/// absent id stays absent, and an id the registry does not know is collapsed
/// to `NULL` by the INSERT itself (see [`OBSERVATION_INSERT_SQL`]).
fn resolve_app_id(snapshot: &serde_json::Value) -> Option<String> {
    let raw = snapshot.get("appId")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// The observation INSERT, hoisted to a `const` so its shape can be pinned by
/// a test.
///
/// This repo uses raw `tokio_postgres`, not sqlx: there is no compile-time
/// statement checking anywhere in the runner, so a wrong column name or arity
/// compiles clean and surfaces only as a `warn!` on a fire-and-forget path
/// nobody watches. The string-assertion test below is the only verification
/// available without a live PG — the same guard
/// `page_query_is_bounded_by_artifact_ids_not_by_time` puts on
/// `PAGE_RENDERS_SQL`.
///
/// Casts are deliberate. `$1::text::uuid` because tokio-postgres' `uuid`
/// feature is not enabled, so the id is serialized as text and PG coerces it
/// at insertion (a bare `$1::uuid` makes PG infer the parameter as uuid and
/// `String` serialization then fails). `$4::jsonb`/`$5::jsonb` land the serde
/// payloads in JSONB columns rather than text. `$6` needs **no** cast:
/// `project.apps.app_id` is `TEXT PRIMARY KEY` (so the lookup is unique and
/// index-backed) and `co_occurrence_observations.app_id` is `TEXT`.
///
/// `app_id` is validated **inside this statement** rather than with a second
/// round trip, so capture never pays an extra query per observation: an id the
/// registry does not know collapses to `NULL`, preserving "absent or unknown
/// writes NULL and logs, never fabricate".
///
/// **Two hard schema preconditions, and they are guarded.** This statement
/// names `co_occurrence_observations.app_id` (added by qontinui-web migration
/// `appid_01_co_occurrence_app_id`) and subqueries `project.apps`. If *either*
/// is absent the **whole INSERT fails** and the observation is discarded — a
/// total capture loss, strictly worse than recording the row un-attributed.
/// Neither precondition is guaranteed: `project.apps` is self-healed by the
/// runner at PG pool init (`CREATE TABLE IF NOT EXISTS`, `crate::database::pg`)
/// but the column is authored elsewhere, and a real dev database in this fleet
/// was measured with `project.apps` present and the column missing.
///
/// So this const is **not used unconditionally**. [`observation_app_id_supported`]
/// probes both preconditions once per process
/// ([`OBSERVATION_SCHEMA_PROBE_SQL`]) and `enqueue_observation` falls back to
/// [`OBSERVATION_INSERT_SQL_LEGACY`] — byte-for-byte the pre-app-scoping
/// five-column statement — when they do not hold. That is the
/// `table_exists`-guarded shape `validate_app_filter` uses in
/// `bin/qontinui_specs.rs`: keep full attribution where the schema supports it,
/// degrade to exactly the previous behaviour where it does not, never fail.
///
/// `RETURNING app_id` is what makes the reject case observable, which is why
/// the call below is `query_opt` and not `execute` — `execute` returns a row
/// count and would discard it.
const OBSERVATION_INSERT_SQL: &str = r#"INSERT INTO co_occurrence_observations
               (id, spec_id, runner_instance, fingerprints, snapshot_metadata, app_id)
               VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5::jsonb,
                       (SELECT a.app_id FROM project.apps a WHERE a.app_id = $6))
               RETURNING app_id"#;

/// Number of parameters [`OBSERVATION_INSERT_SQL`] binds.
///
/// The call site builds its params as a fixed-size array of this length, so
/// adding or dropping a bind without changing this constant is a **compile**
/// error, and `observation_insert_binds_match_their_sql` pins the constant
/// against the highest `$N` actually present in the SQL. Between them the two
/// halves cannot drift: a string-shape assertion alone never sees the params
/// array, and arity drift on a runtime-parsed statement compiles clean and
/// surfaces only as a `warn!` on a fire-and-forget path.
const OBSERVATION_INSERT_BINDS: usize = 6;

/// Number of parameters [`OBSERVATION_INSERT_SQL_LEGACY`] binds. Same
/// compile-time-array + test pinning as [`OBSERVATION_INSERT_BINDS`].
const OBSERVATION_INSERT_LEGACY_BINDS: usize = 5;

/// The pre-app-scoping observation INSERT, kept verbatim as the fallback for
/// databases that cannot satisfy [`OBSERVATION_INSERT_SQL`]'s preconditions.
///
/// This is not a degraded variant someone tuned — it is the exact statement the
/// runner shipped before app scoping (`git show 41569ea90 -- this file`),
/// character for character including the `::text::uuid` and `::jsonb` casts.
/// Keeping it identical is the whole point: on an un-migrated database this
/// path must behave *exactly* as the old code did, so the app-scoping change is
/// strictly non-regressive rather than "mostly works".
///
/// No `app_id` column and no `project.apps` subquery, so it depends on nothing
/// beyond the table that has always existed. The row lands un-attributed, which
/// is the same nullable-column "app unknown" outcome `resolve_app_id`
/// documents — attribution is missing, not fabricated, and no observation is
/// lost. Run with `execute`, not `query_opt`: there is no `RETURNING` to read.
const OBSERVATION_INSERT_SQL_LEGACY: &str = r#"INSERT INTO co_occurrence_observations
               (id, spec_id, runner_instance, fingerprints, snapshot_metadata)
               VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5::jsonb)"#;

/// One statement answering **both** of [`OBSERVATION_INSERT_SQL`]'s schema
/// preconditions: does `project.apps` exist, and does
/// `project.co_occurrence_observations` have an `app_id` column.
///
/// `pg_catalog`, not `information_schema`: `information_schema.columns` hides
/// columns the calling role holds no privilege on, so a grant quirk would read
/// as "column absent" and pin an otherwise-migrated database to the legacy path
/// for the life of the process. That is the same reason `table_exists` in
/// `bin/qontinui_specs.rs` reaches for `to_regclass`.
///
/// Both names are schema-qualified, so the answer does not depend on the pool's
/// `search_path` (`project, public`). `to_regclass` *returns NULL* rather than
/// erroring for a missing relation or a missing schema, so when the
/// observations table itself is absent the second expression compares
/// `attrelid = NULL`, matches nothing, and correctly yields `false`.
///
/// Verified against a live dev PG (`qontinui_db`) that has `project.apps` but
/// no `app_id` column: returns `(true, false)`, and on that same database the
/// six-column INSERT errors with `column "app_id" ... does not exist` while the
/// five-column one succeeds.
const OBSERVATION_SCHEMA_PROBE_SQL: &str = r#"SELECT to_regclass('project.apps') IS NOT NULL,
                      EXISTS (SELECT 1 FROM pg_attribute
                               WHERE attrelid = to_regclass('project.co_occurrence_observations')
                                 AND attname = 'app_id'
                                 AND attnum > 0
                                 AND NOT attisdropped)"#;

/// Second half of the probe: the catalog answers **"exists"**, this answers
/// **"readable"**.
///
/// `to_regclass(...) IS NOT NULL` and a `pg_attribute` lookup are both true for
/// a relation the calling role holds no `SELECT` privilege on, and column-level
/// grants can hide `app_id` specifically. Without this half, such a role caches
/// `true`, every six-column INSERT fails with `permission denied`, and the
/// legacy fallback is never selected — **total capture loss**, precisely what the
/// guard exists to prevent. It is the one case where the "the probe fails toward
/// the legacy path, so it can never make things worse" claim did not hold.
///
/// So actually read: one row (at most) from each relation, naming `app_id`
/// explicitly on the observations side so a column-level grant is exercised too.
/// A privilege failure — or a relation that vanished between the two probes —
/// surfaces as a query error, which the initializer already turns into `false`
/// and the legacy path.
///
/// Cost is bounded and paid once per process: `LIMIT 1` on each side, no join,
/// no aggregate. The values are discarded; only success or failure matters, so
/// an empty table (both scalar subqueries NULL) is a pass.
const OBSERVATION_READ_PROBE_SQL: &str = r#"SELECT (SELECT o.app_id FROM project.co_occurrence_observations o LIMIT 1),
                      (SELECT a.app_id FROM project.apps a LIMIT 1)"#;

/// How long the once-per-process probe may take before capture gives up on it.
///
/// Not a deadlock guard — there is none to guard against: the initializer
/// queries the connection its caller already holds (no pool re-entry) and
/// `tokio::sync::OnceCell` releases its permit on cancellation. It bounds a
/// **stall**. Every capture task that arrives while the initializer is running
/// waits on the `OnceCell`'s semaphore *while holding a pooled connection*, and
/// the pool is `max_size(8)` with no per-statement timeout — so a PG that
/// accepts connections but stops answering would let 8 capture tasks pin all 8
/// connections indefinitely and starve every other PG consumer in the runner.
/// Before app scoping each task held its connection only for its own INSERT.
///
/// 5s because that is the bound the pool already chose for itself
/// (`create_timeout`/`wait_timeout`/`recycle_timeout` in `database::pg`): a
/// catalog read that outlives the pool's own patience is a stalled server, not a
/// slow one, and capture is fire-and-forget — waiting longer buys nothing and
/// costs the connection. Bounding the *initializer* is what bounds the waiters,
/// since they are queued behind exactly it.
const OBSERVATION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Process-wide cache of the [`OBSERVATION_SCHEMA_PROBE_SQL`] answer.
///
/// Deliberately once per *process*, not once per observation: the reason
/// `app_id` is validated inside the INSERT at all is that capture must not pay
/// an extra round trip per snapshot, and a catalog query per observation would
/// give back exactly the cost that design avoids. Schema shape does not change
/// under a running runner without a migration, and a migration is followed by a
/// restart, which re-probes.
///
/// **Constraint, recorded rather than engineered around: this cache is keyed on
/// nothing.** It is correct in production because the runner constructs exactly
/// one `PgDb` (from `main.rs`) and has no reconnect-to-a-different-database
/// path, so "this process" and "this database" are the same thing. They are not
/// the same thing under `cargo test`: `PgDb::new_blocking_for_test()` appears in
/// ~20 test modules, so the first test to drive `enqueue_observation` against
/// one database would pin the answer for a second, differently-migrated one.
/// Nothing does that today (no test reaches this function against PG). If one
/// ever does — or if the runner gains a reconnect path that can land on another
/// database — key this per-`PgDb` instead of per-process rather than adding a
/// reset hook.
static OBSERVATION_APP_ID_SUPPORTED: OnceCell<bool> = OnceCell::const_new();

/// Resolve — once per process — whether the six-column
/// [`OBSERVATION_INSERT_SQL`] is safe on this database, warning exactly once
/// when it is not.
///
/// Two questions, both of which the six-column statement needs answered `true`:
/// does the schema have the shapes ([`OBSERVATION_SCHEMA_PROBE_SQL`], catalog),
/// and can this role actually *read* them ([`OBSERVATION_READ_PROBE_SQL`]).
/// The second is not redundant: the catalog says a relation exists even when the
/// caller holds no `SELECT` privilege on it, and caching `true` there would fail
/// every INSERT and never select the fallback. Both run under one
/// [`OBSERVATION_PROBE_TIMEOUT`] deadline.
///
/// **Fails toward the legacy path.** A probe that errors *or times out* returns
/// `false`, so a probe malfunction can only cost attribution, never capture; it
/// can never leave the runner worse off than having no probe at all. The `false` is
/// cached like any other answer, so a transient probe failure pins the legacy
/// path until restart — the safe direction to be wrong in, and it is stated in
/// the warning so nobody has to guess.
///
/// The warning lives inside the `OnceCell` initializer rather than at the call
/// site, which is what makes it fire once instead of once per snapshot on a
/// fire-and-forget path that could otherwise flood the log.
async fn observation_app_id_supported(client: &tokio_postgres::Client) -> bool {
    *OBSERVATION_APP_ID_SUPPORTED
        .get_or_init(|| async {
            // Both probes run under one shared deadline. See
            // `OBSERVATION_PROBE_TIMEOUT`: waiters queue on this initializer
            // holding pooled connections, so an unbounded probe is an unbounded
            // stall of the whole pool, not just of this task.
            let probed = tokio::time::timeout(OBSERVATION_PROBE_TIMEOUT, async {
                let row = client.query_one(OBSERVATION_SCHEMA_PROBE_SQL, &[]).await?;
                // `try_get`, not `get`: `get` panics on a type mismatch, and
                // this runs under a fire-and-forget capture task.
                let has_apps = row.try_get::<_, bool>(0).unwrap_or(false);
                let has_column = row.try_get::<_, bool>(1).unwrap_or(false);

                // "Exists" is not "readable". Only ask once the catalog says
                // both relations are there, so a genuine missing-column
                // database still gets the precise diagnosis below instead of a
                // generic query error.
                if has_apps && has_column {
                    client.query_one(OBSERVATION_READ_PROBE_SQL, &[]).await?;
                }
                Ok::<_, tokio_postgres::Error>((has_apps, has_column))
            })
            .await;

            let (has_apps, has_column) = match probed {
                Ok(Ok(answer)) => answer,
                Ok(Err(e)) => {
                    warn!(
                        "state_discovery::capture: could not probe the database for app \
                         attribution support ({}) — falling back to the pre-app-scoping \
                         five-column INSERT for the life of this process. Observations are \
                         still being recorded, but with no app_id. A `permission denied` here \
                         means this role cannot read project.apps or \
                         co_occurrence_observations.app_id, so the six-column INSERT would have \
                         failed on every observation. Remedy: fix the error above, then restart \
                         the runner to re-probe.",
                        e
                    );
                    return false;
                }
                Err(_elapsed) => {
                    warn!(
                        "state_discovery::capture: the app-attribution schema probe did not \
                         answer within {:?} — falling back to the pre-app-scoping five-column \
                         INSERT for the life of this process. Observations are still being \
                         recorded, but with no app_id. A database that accepts connections and \
                         then does not answer a catalog read is stalled, and capture tasks wait \
                         on this probe holding pooled connections, so it is bounded rather than \
                         retried. Remedy: fix the database, then restart the runner to re-probe.",
                        OBSERVATION_PROBE_TIMEOUT
                    );
                    return false;
                }
            };

            if has_apps && has_column {
                return true;
            }

            let missing = if !has_column && !has_apps {
                "column project.co_occurrence_observations.app_id is missing, AND table \
                 project.apps is missing"
            } else if !has_column {
                "column project.co_occurrence_observations.app_id is missing"
            } else {
                "table project.apps is missing"
            };
            let remedy = if !has_column {
                "apply qontinui-web migration `appid_01_co_occurrence_app_id` (it adds \
                 project.co_occurrence_observations.app_id), then restart the runner to re-probe"
            } else {
                "project.apps is created by the runner's own CREATE TABLE IF NOT EXISTS at PG \
                 pool init, so its absence means that bootstrap did not run against this \
                 database — check the pool startup logs for the failure, then restart the runner \
                 to re-probe"
            };
            warn!(
                "state_discovery::capture: app attribution DISABLED for this process — {}. \
                 Observations are still being recorded, via the pre-app-scoping five-column \
                 INSERT, so no capture is lost — the rows simply carry no app_id. Remedy: {}.",
                missing, remedy
            );
            false
        })
        .await
}

/// Pick the INSERT this database can actually run, with the number of
/// parameters it binds.
///
/// Split out as a pure function so the *selection* is testable without PG. The
/// branch it replaces was an `if !supported { …; return; }` early exit whose
/// `return` nothing verified — dropping it would have double-inserted every
/// observation, silently, on the un-migrated databases the fallback exists to
/// serve. Here the two statements are mutually exclusive by construction: one
/// expression, one value, no path that runs both.
fn observation_insert_statement(app_id_supported: bool) -> (&'static str, usize) {
    if app_id_supported {
        (OBSERVATION_INSERT_SQL, OBSERVATION_INSERT_BINDS)
    } else {
        (
            OBSERVATION_INSERT_SQL_LEGACY,
            OBSERVATION_INSERT_LEGACY_BINDS,
        )
    }
}

/// Did the INSERT's `RETURNING app_id` reject the id the snapshot claimed?
///
/// This — and only this — is why the six-column path runs `query_opt` with a
/// `RETURNING` clause instead of `execute`. We supplied a candidate and the
/// registry subquery collapsed it to `NULL`, i.e. `project.apps` does not know
/// that id: a producer/consumer key disagreement, the exact defect class this
/// subsystem has been bitten by twice.
///
/// `(None, _)` is not a rejection — no id was claimed, so `NULL` is the
/// deliberate "app unknown" the column is documented to mean. `(Some, Some)` is
/// the success case. `(None, Some)` cannot happen (nothing can invent an id) and
/// is pinned as a non-rejection so a future edit that makes it possible has to
/// decide what it means rather than inheriting a warning.
fn app_id_was_rejected(candidate: Option<&str>, stored: Option<&str>) -> bool {
    matches!((candidate, stored), (Some(_), None))
}

/// One-shot gate for the unknown-app warning: `true` exactly once per process.
///
/// The rejection is a *configuration* fact, not an event — the same snapshot
/// producer claims the same unregistered id on every read. The warning sits on a
/// path that runs once per UI Bridge snapshot with `SAMPLE_RATE == 1` and
/// nothing else throttling it, so leaving it ungated emitted one identical WARN
/// per snapshot — roughly 3600 lines an hour at 1 Hz, burying every other
/// warning in the log. Same cadence the probe warning above already has by
/// living inside the `OnceCell` initializer; this is the equivalent for a
/// warning that cannot live there.
fn should_warn_unknown_app_once() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    WARNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Enqueue a single observation derived from a UI Bridge snapshot.
///
/// Extracts a fingerprint per element, resolves the page label (see
/// [`resolve_page_label`]) and the producing app (see [`resolve_app_id`]),
/// records minimal page metadata, and inserts one row into
/// `co_occurrence_observations`. All errors downgrade to WARN logs — the
/// snapshot path must never fail because of observation capture.
///
/// Which statement runs depends on what this database can support: the
/// six-column [`OBSERVATION_INSERT_SQL`] when
/// [`observation_app_id_supported`] says both preconditions hold, otherwise the
/// verbatim pre-app-scoping [`OBSERVATION_INSERT_SQL_LEGACY`]. Recording the
/// row un-attributed always beats losing it.
///
/// Takes no `app_id` parameter on purpose: the id is read off the snapshot,
/// which keeps the call site's invariant that the caller carries no scope
/// knowledge the snapshot doesn't, and avoids threading a value through the
/// fire-and-forget `tokio::spawn` that invokes this.
pub async fn enqueue_observation(
    pg_db: Arc<PgDb>,
    snapshot: &serde_json::Value,
    runner_instance: String,
) {
    // Sample-rate gate. SAMPLE_RATE == 1 means every call goes through;
    // K > 1 is the planned lever for volume management.
    if SAMPLE_RATE > 1 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // SAMPLE_RATE > 1 is guaranteed above, so modulo is well-defined.
        if !n.is_multiple_of(SAMPLE_RATE) {
            return;
        }
    }

    // Extract fingerprints from elements[].
    let elements = match snapshot.get("elements").and_then(|e| e.as_array()) {
        Some(arr) => arr,
        None => {
            // No elements to fingerprint — nothing to record. Not an error;
            // e.g. native-capture fallback snapshots have empty element
            // arrays and contain no co-occurrence signal.
            return;
        }
    };

    if elements.is_empty() {
        return;
    }

    let fingerprints: Vec<String> = elements
        .iter()
        .map(stable_element_fingerprint)
        .collect::<std::collections::BTreeSet<_>>() // dedup + deterministic order
        .into_iter()
        .collect();

    if fingerprints.is_empty() {
        return;
    }

    let element_count = elements.len() as i64;

    // Build snapshot_metadata from shallow page fields.
    let page = snapshot.get("page");
    let pathname = page
        .and_then(|p| p.get("pathname"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let url = page
        .and_then(|p| p.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let viewport = page.and_then(|p| p.get("viewport")).cloned();
    // Keep the raw developer context alongside the derived label so a
    // mislabelled corpus is diagnosable without replaying snapshots.
    let page_context = page.and_then(|p| p.get("pageContext")).cloned();

    let spec_id = resolve_page_label(snapshot);
    let app_id = resolve_app_id(snapshot);

    let snapshot_metadata = serde_json::json!({
        "pathname": pathname,
        "url": url,
        "viewport": viewport,
        "element_count": element_count,
        "page_context": page_context,
    });

    let fingerprints_json = serde_json::Value::Array(
        fingerprints
            .into_iter()
            .map(serde_json::Value::String)
            .collect(),
    );

    let id = Uuid::new_v4().to_string();

    // Insert. Soft-fail on any error — observation capture must never
    // compromise the snapshot response.
    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            warn!("state_discovery::capture: PG pool error: {}", e);
            return;
        }
    };

    // Which statement this database can actually run. Cached after the first
    // call, so this is a bool read per observation, not a catalog query.
    let app_id_supported = observation_app_id_supported(&conn).await;
    let (sql, binds) = observation_insert_statement(app_id_supported);

    // One expression, one statement, no early return: the two paths are
    // mutually exclusive by construction rather than by a `return` nothing
    // checks. `Ok(Some(row))` = the six-column statement ran and its
    // `RETURNING app_id` is readable; `Ok(None)` = the legacy statement ran and
    // there is no attribution to inspect (the probe already said why, once).
    //
    // See `OBSERVATION_INSERT_SQL` for why each cast is there and why the
    // `project.apps` subquery is safe when the probe says so. `query_opt`
    // rather than `execute` on that path because `execute` returns a row count,
    // which would discard the `RETURNING app_id` that makes an unknown id
    // observable; the legacy statement has no `RETURNING`, so it uses `execute`.
    //
    // The params arrays are sized by the bind constants, so SQL/param arity can
    // only drift past a compile error plus
    // `observation_insert_binds_match_their_sql`.
    let inserted = if app_id_supported {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); OBSERVATION_INSERT_BINDS] = [
            &id,
            &spec_id,
            &runner_instance,
            &fingerprints_json,
            &snapshot_metadata,
            &app_id,
        ];
        conn.query_opt(sql, &params).await.map(Some)
    } else {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); OBSERVATION_INSERT_LEGACY_BINDS] = [
            &id,
            &spec_id,
            &runner_instance,
            &fingerprints_json,
            &snapshot_metadata,
        ];
        conn.execute(sql, &params).await.map(|_| None)
    };

    match inserted {
        Err(e) => {
            // The bind count names which statement ran (6 = app-scoped,
            // 5 = the pre-app-scoping fallback) without dumping the SQL.
            warn!(
                "state_discovery::capture: failed to insert observation ({} elements, \
                 {}-bind statement): {}",
                element_count, binds, e
            );
        }
        // Legacy path: un-attributed by design, nothing to inspect. The reason
        // was already logged once by the probe, so stay quiet here.
        Ok(None) => {}
        Ok(Some(row)) => {
            // The stamped id came back NULL while we supplied one: the app is
            // not registered in `project.apps`. The row is kept and recorded
            // un-attributed rather than dropped — it still carries
            // co-occurrence signal — but this is a producer/consumer key
            // disagreement, so say so loudly enough to diagnose.
            // `try_get`, not `get`: `get` panics on a type mismatch, and this
            // runs inside a fire-and-forget task whose whole contract is that
            // it cannot disturb the snapshot response.
            let stored: Option<String> = row
                .and_then(|r| r.try_get::<_, Option<String>>(0).ok())
                .flatten();
            if app_id_was_rejected(app_id.as_deref(), stored.as_deref())
                && should_warn_unknown_app_once()
            {
                warn!(
                    "state_discovery::capture: snapshot claimed appId {:?}, which is not \
                     registered in project.apps — observation recorded with app_id NULL \
                     (un-attributed). Register the app or fix the producer's id. This warning \
                     is emitted ONCE per process: the condition repeats on every snapshot, so \
                     it is a configuration fact, not a stream of events.",
                    app_id.as_deref().unwrap_or_default()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tab_id_wins_over_name_and_pathname() {
        let snap = json!({
            "page": {
                "pathname": "/",
                "pageContext": {
                    "name": "DAG Workflow Editor",
                    "meta": { "tabId": "dag-workflow-editor" }
                }
            }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("dag-workflow-editor")
        );
    }

    #[test]
    fn falls_back_to_top_level_active_tab() {
        // Runners built before `meta.tabId` existed still emit the SDK's own
        // top-level `activeTab`, so labelling must not depend on a rebuild.
        let snap = json!({
            "activeTab": "config-log-sources",
            "page": { "pathname": "/", "pageContext": { "name": "Settings" } }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("config-log-sources"),
            "activeTab must outrank the display name, which collapses every settings-* view"
        );
    }

    #[test]
    fn blank_candidate_falls_through_instead_of_shadowing() {
        let snap = json!({
            "activeTab": "capture",
            "page": { "pageContext": { "meta": { "tabId": "   " } } }
        });
        assert_eq!(resolve_page_label(&snap).as_deref(), Some("capture"));
    }

    #[test]
    fn falls_back_to_slugged_context_name() {
        let snap = json!({
            "page": { "pathname": "/", "pageContext": { "name": "Active Dashboard" } }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("active-dashboard")
        );
    }

    #[test]
    fn falls_back_to_pathname_for_real_url_apps() {
        // qontinui-web (Next.js) has no pageContext but a meaningful pathname.
        let snap = json!({ "page": { "pathname": "/account/billing" } });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("account-billing")
        );
    }

    #[test]
    fn desktop_spa_root_pathname_is_the_degenerate_case() {
        // The failure this whole change exists to fix: a Tauri SPA reports
        // `/` for every one of its views, so pathname alone cannot tell them
        // apart. Without a pageContext there is nothing better to key on.
        let snap = json!({ "page": { "pathname": "/", "url": "http://tauri.localhost/" } });
        assert_eq!(resolve_page_label(&snap).as_deref(), Some("root"));
    }

    #[test]
    fn missing_page_or_blank_label_yields_none() {
        assert_eq!(resolve_page_label(&json!({})), None);
        assert_eq!(resolve_page_label(&json!({ "page": {} })), None);
        assert_eq!(
            resolve_page_label(&json!({ "page": { "pathname": "   " } })),
            None
        );
    }

    #[test]
    fn label_is_idempotent_under_reslugging() {
        // The label is written to `spec_id` and later compared against spec
        // ids on disk; re-slugging an already-canonical label must not move it.
        let snap = json!({
            "page": { "pageContext": { "meta": { "tabId": "config-log-sources" } } }
        });
        let once = resolve_page_label(&snap).unwrap();
        assert_eq!(pathname_to_spec_id(&once), once);
    }

    // ---- app attribution -------------------------------------------------

    #[test]
    fn app_id_is_read_from_the_snapshot_top_level() {
        // The runner frontend's `runner-tabs` enricher stamps this from
        // `RUNNER_APP_ID`; the handler never passes it in.
        let snap = json!({ "appId": "qontinui-runner", "page": { "pathname": "/" } });
        assert_eq!(resolve_app_id(&snap).as_deref(), Some("qontinui-runner"));
    }

    #[test]
    fn absent_app_id_is_none_not_a_fabricated_default() {
        // A snapshot from a producer that does not stamp an id records
        // "app unknown". Defaulting to `qontinui-runner` here would be the
        // confidently-wrong inference this whole change exists to avoid.
        assert_eq!(resolve_app_id(&json!({})), None);
        assert_eq!(
            resolve_app_id(&json!({ "page": { "pathname": "/" } })),
            None
        );
    }

    #[test]
    fn blank_app_id_is_none() {
        // Mirrors `resolve_page_label`'s blank handling: an empty or
        // whitespace-only id is not a key, so it must not be sent to the
        // registry lookup as if it were one.
        assert_eq!(resolve_app_id(&json!({ "appId": "" })), None);
        assert_eq!(resolve_app_id(&json!({ "appId": "   " })), None);
    }

    #[test]
    fn non_string_app_id_is_none() {
        // The field is untyped on the wire; anything that is not a string
        // cannot be an app id.
        assert_eq!(resolve_app_id(&json!({ "appId": 42 })), None);
        assert_eq!(resolve_app_id(&json!({ "appId": null })), None);
        assert_eq!(
            resolve_app_id(&json!({ "appId": ["qontinui-runner"] })),
            None
        );
    }

    #[test]
    fn app_id_is_trimmed_so_padding_still_matches_the_registry() {
        // `project.apps.app_id` is matched by exact equality, so an untrimmed
        // value would collapse to NULL for a reason nobody could see.
        assert_eq!(
            resolve_app_id(&json!({ "appId": "  qontinui-runner  " })).as_deref(),
            Some("qontinui-runner")
        );
    }

    #[test]
    fn observation_insert_stamps_app_id_and_validates_it_in_one_statement() {
        // Shape guard, in the spirit of
        // `page_query_is_bounded_by_artifact_ids_not_by_time` in
        // `workflow_generation::spec_authoring`. There is no sqlx in this repo
        // — every statement is a runtime-parsed string, so a wrong column name
        // or arity compiles clean and fails only as a `warn!` on a
        // fire-and-forget path. This test is the only pre-PG verification of
        // this statement that exists.
        assert!(
            OBSERVATION_INSERT_SQL.contains(
                "(id, spec_id, runner_instance, fingerprints, snapshot_metadata, app_id)"
            ),
            "the six-column list must stay in sync with the six binds. Got: {OBSERVATION_INSERT_SQL}"
        );
        assert!(
            OBSERVATION_INSERT_SQL
                .contains("(SELECT a.app_id FROM project.apps a WHERE a.app_id = $6)"),
            "app_id must be validated against the registry inside this statement, so an \
             unknown id collapses to NULL instead of being written as fact. Got: \
             {OBSERVATION_INSERT_SQL}"
        );
        assert!(
            OBSERVATION_INSERT_SQL.contains("RETURNING app_id"),
            "without RETURNING, the collapsed-to-NULL case is invisible and the `query_opt` \
             call below has nothing to read"
        );
        // The pre-existing casts are load-bearing; see the const's doc comment.
        assert!(
            OBSERVATION_INSERT_SQL.contains("$1::text::uuid"),
            "tokio-postgres' uuid feature is not enabled — the id must round-trip as text"
        );
        assert!(
            OBSERVATION_INSERT_SQL.contains("$4::jsonb")
                && OBSERVATION_INSERT_SQL.contains("$5::jsonb"),
            "the serde payloads must land in JSONB columns, not text"
        );
        // $6 must NOT be cast: both sides of the lookup are already TEXT, and a
        // cast here would only obscure that.
        assert!(
            !OBSERVATION_INSERT_SQL.contains("$6::"),
            "project.apps.app_id and co_occurrence_observations.app_id are both TEXT — $6 \
             needs no cast"
        );
    }

    #[test]
    fn legacy_observation_insert_depends_on_nothing_the_migration_adds() {
        // The fallback's entire value is that it runs on a database that cannot
        // run `OBSERVATION_INSERT_SQL`. If either precondition leaks back into
        // it, the fallback fails on exactly the databases it exists to serve
        // and capture is lost there — the regression this guard prevents.
        assert!(
            OBSERVATION_INSERT_SQL_LEGACY
                .contains("(id, spec_id, runner_instance, fingerprints, snapshot_metadata)"),
            "the legacy statement must list exactly five columns, matching its five binds. \
             Got: {OBSERVATION_INSERT_SQL_LEGACY}"
        );
        assert!(
            !OBSERVATION_INSERT_SQL_LEGACY.contains("app_id"),
            "the legacy statement must not name app_id anywhere — the column does not exist on \
             databases that have not applied qontinui-web migration \
             `appid_01_co_occurrence_app_id`. Got: {OBSERVATION_INSERT_SQL_LEGACY}"
        );
        assert!(
            !OBSERVATION_INSERT_SQL_LEGACY.contains("project.apps"),
            "the legacy statement must not reference the project.apps registry — a missing \
             relation there fails the whole INSERT. Got: {OBSERVATION_INSERT_SQL_LEGACY}"
        );
        assert!(
            !OBSERVATION_INSERT_SQL_LEGACY.contains("RETURNING"),
            "nothing to return without app_id, which is why the legacy path runs `execute` \
             rather than `query_opt`"
        );
        assert!(
            !OBSERVATION_INSERT_SQL_LEGACY.contains("$6"),
            "a sixth bind would not match the five parameters the legacy call site passes"
        );
    }

    #[test]
    fn the_two_observation_inserts_agree_on_their_shared_five_columns() {
        // Same row, two schemas — the five shared columns and their casts must
        // stay identical, or an un-migrated database starts writing subtly
        // different rows from a migrated one and the corpus stops being
        // comparable across environments. Pins them as one unit so a future
        // edit to either cannot silently drift.
        for shared in [
            "INSERT INTO co_occurrence_observations",
            "id, spec_id, runner_instance, fingerprints, snapshot_metadata",
            "VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5::jsonb",
        ] {
            assert!(
                OBSERVATION_INSERT_SQL.contains(shared)
                    && OBSERVATION_INSERT_SQL_LEGACY.contains(shared),
                "both statements must contain {shared:?}; six-column: \
                 {OBSERVATION_INSERT_SQL}; legacy: {OBSERVATION_INSERT_SQL_LEGACY}"
            );
        }
        // And the six-column form must be the legacy form *extended*, not a
        // reworded one. Compute the actual common prefix rather than asserting
        // a hand-copied literal, so whitespace can't make this pass or fail for
        // the wrong reason: the two must stay byte-identical right through the
        // fifth column name, diverging only at `, app_id)` vs `)`.
        let common_len = OBSERVATION_INSERT_SQL
            .as_bytes()
            .iter()
            .zip(OBSERVATION_INSERT_SQL_LEGACY.as_bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let common = OBSERVATION_INSERT_SQL.get(..common_len).unwrap_or("");
        assert!(
            common.ends_with("snapshot_metadata"),
            "the two statements must be byte-identical through the fifth column and diverge only \
             there; they instead share only {common:?}"
        );
    }

    #[test]
    fn schema_probe_answers_both_preconditions_without_information_schema() {
        // The probe is the only thing standing between an un-migrated database
        // and total capture loss, so pin what it actually asks.
        assert!(
            OBSERVATION_SCHEMA_PROBE_SQL.contains("to_regclass('project.apps')"),
            "precondition 1 (the registry table the subquery reads) must be probed. Got: \
             {OBSERVATION_SCHEMA_PROBE_SQL}"
        );
        assert!(
            OBSERVATION_SCHEMA_PROBE_SQL.contains("'app_id'")
                && OBSERVATION_SCHEMA_PROBE_SQL
                    .contains("to_regclass('project.co_occurrence_observations')"),
            "precondition 2 (the column migration `appid_01_co_occurrence_app_id` adds) must be \
             probed. Got: {OBSERVATION_SCHEMA_PROBE_SQL}"
        );
        assert!(
            !OBSERVATION_SCHEMA_PROBE_SQL.contains("information_schema"),
            "information_schema.columns hides columns the role lacks privilege on, so a grant \
             quirk would read as `column absent` and pin a migrated database to the legacy path. \
             Use pg_catalog, as `table_exists` in bin/qontinui_specs.rs does"
        );
        assert!(
            OBSERVATION_SCHEMA_PROBE_SQL.contains("NOT attisdropped"),
            "a dropped column still has a pg_attribute row; it must not count as present"
        );
        // Both relation names are schema-qualified so the answer cannot depend
        // on the pool's `SET search_path TO project, public`.
        assert!(
            !OBSERVATION_SCHEMA_PROBE_SQL.contains("to_regclass('apps')"),
            "probe names must stay schema-qualified, independent of search_path"
        );
    }

    /// Highest `$N` placeholder in a statement, and the set of numbers used.
    ///
    /// Deliberately derived from the SQL rather than hand-copied: the point of
    /// the bind tests is that nobody has to keep two lists in their head.
    fn placeholders(sql: &str) -> (usize, std::collections::BTreeSet<usize>) {
        let mut used = std::collections::BTreeSet::new();
        let bytes = sql.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    if let Ok(n) = sql[start..end].parse::<usize>() {
                        used.insert(n);
                    }
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        (used.iter().copied().max().unwrap_or(0), used)
    }

    #[test]
    fn observation_insert_binds_match_their_sql() {
        // THE gap the string-shape assertions above do not close: they inspect
        // the SQL only, never the `&[…]` params array, so SQL/param arity drift
        // compiles clean and fails at runtime on a `warn!`-only fire-and-forget
        // path. Tying each statement's highest placeholder to the constant that
        // *sizes the call site's params array* makes the two inseparable — the
        // array is `[&(dyn ToSql + Sync); OBSERVATION_INSERT_BINDS]`, so
        // changing the constant to satisfy this test breaks compilation until
        // the params list is changed to match, and vice versa.
        for (label, sql, declared) in [
            (
                "OBSERVATION_INSERT_SQL",
                OBSERVATION_INSERT_SQL,
                OBSERVATION_INSERT_BINDS,
            ),
            (
                "OBSERVATION_INSERT_SQL_LEGACY",
                OBSERVATION_INSERT_SQL_LEGACY,
                OBSERVATION_INSERT_LEGACY_BINDS,
            ),
        ] {
            let (max, used) = placeholders(sql);
            assert_eq!(
                max, declared,
                "{label} binds $1..=${max} but its bind constant says {declared}; the call \
                 site passes exactly {declared} parameters, so this is a runtime failure \
                 waiting on a fire-and-forget path. SQL: {sql}"
            );
            let expected: std::collections::BTreeSet<usize> = (1..=declared).collect();
            assert_eq!(
                used, expected,
                "{label} must use every placeholder from $1 to ${declared} exactly once — a \
                 gap means a parameter is passed and silently ignored. SQL: {sql}"
            );
        }
    }

    #[test]
    fn probe_answer_selects_the_statement_and_its_bind_count() {
        // The plan's load-bearing claim — "the probe fails toward the legacy
        // path, so it can never make things worse" — was verified by nothing:
        // only the probe SQL's *text* was asserted. This pins the behaviour the
        // claim is about, and pins it to the bind constants so the pairing
        // cannot drift either.
        assert_eq!(
            observation_insert_statement(false),
            (
                OBSERVATION_INSERT_SQL_LEGACY,
                OBSERVATION_INSERT_LEGACY_BINDS
            ),
            "a false probe answer (missing column, missing table, probe error, or probe \
             timeout) MUST select the pre-app-scoping statement — that is the whole \
             fails-toward-legacy guarantee"
        );
        assert_eq!(
            observation_insert_statement(true),
            (OBSERVATION_INSERT_SQL, OBSERVATION_INSERT_BINDS),
            "a true probe answer must select the app-scoped statement, or the feature is a \
             silent no-op on a perfectly capable database"
        );
    }

    #[test]
    fn the_two_statements_are_mutually_exclusive() {
        // The branch this replaces was `if !supported { …; return; }`. An edit
        // dropping that `return` would have run BOTH inserts and duplicated
        // every observation, silently. The selector returns one statement, so
        // the only way to run both is to call it twice — but pin that the two
        // arms are genuinely different statements, so a copy-paste that returns
        // the same one from both arms (silently losing the fallback, or
        // silently losing attribution) fails here.
        let (legacy, _) = observation_insert_statement(false);
        let (scoped, _) = observation_insert_statement(true);
        assert_ne!(
            legacy, scoped,
            "the two arms must be different statements; identical arms mean one of the two \
             behaviours has been silently deleted"
        );
    }

    #[test]
    fn a_claimed_but_unregistered_app_id_is_a_rejection() {
        // The reject path is the entire reason the six-column INSERT uses
        // `query_opt` + `RETURNING` rather than `execute`: we supplied an id and
        // the registry subquery collapsed it to NULL.
        assert!(
            app_id_was_rejected(Some("qontinui-web"), None),
            "a claimed id that came back NULL is a producer/consumer key disagreement and must \
             be reported"
        );
    }

    #[test]
    fn a_stored_or_absent_app_id_is_not_a_rejection() {
        assert!(
            !app_id_was_rejected(Some("qontinui-runner"), Some("qontinui-runner")),
            "the id was accepted by the registry — nothing to report"
        );
        assert!(
            !app_id_was_rejected(None, None),
            "no id was claimed, so NULL is the deliberate `app unknown` the column means — \
             warning here would fire on every snapshot from a producer that does not stamp"
        );
        assert!(
            !app_id_was_rejected(None, Some("qontinui-runner")),
            "cannot happen (the INSERT cannot invent an id); pinned so a future edit that makes \
             it possible has to decide what it means"
        );
    }

    #[test]
    fn the_unknown_app_warning_fires_once_per_process() {
        // Cadence, not text. With SAMPLE_RATE == 1 and nothing else throttling,
        // an ungated warning on this path emits one identical line per UI Bridge
        // snapshot (~3600/hour at 1 Hz) and buries every other warning.
        //
        // NOTE: the gate is process-global, so this test consumes it. It is the
        // only test that touches it, and it must stay that way — a second
        // consumer would make both order-dependent.
        assert!(
            should_warn_unknown_app_once(),
            "the first rejection in a process must be reported"
        );
        for _ in 0..1000 {
            assert!(
                !should_warn_unknown_app_once(),
                "every subsequent rejection must be silent — the condition is a configuration \
                 fact that repeats on every snapshot, not a stream of events"
            );
        }
    }

    #[test]
    fn schema_probe_also_proves_readability_not_just_existence() {
        // `to_regclass(...) IS NOT NULL` is true for a relation the calling role
        // cannot SELECT from, and a column-level grant can hide `app_id`
        // specifically. Caching `true` there fails every six-column INSERT and
        // never selects the fallback: total capture loss, the one hole in
        // "the probe can never make things worse".
        assert!(
            OBSERVATION_READ_PROBE_SQL.contains("FROM project.co_occurrence_observations")
                && OBSERVATION_READ_PROBE_SQL.contains("o.app_id"),
            "the read probe must actually read the target column, so a column-level privilege \
             failure surfaces as a probe error. Got: {OBSERVATION_READ_PROBE_SQL}"
        );
        assert!(
            OBSERVATION_READ_PROBE_SQL.contains("FROM project.apps"),
            "the read probe must also read the registry the INSERT subqueries. Got: \
             {OBSERVATION_READ_PROBE_SQL}"
        );
        assert_eq!(
            OBSERVATION_READ_PROBE_SQL.matches("LIMIT 1").count(),
            2,
            "each side must stay bounded to one row — the probe proves readability, it does not \
             scan. Got: {OBSERVATION_READ_PROBE_SQL}"
        );
        assert_eq!(
            placeholders(OBSERVATION_READ_PROBE_SQL).0,
            0,
            "the read probe takes no parameters; it is run with an empty bind slice"
        );
        assert_eq!(
            placeholders(OBSERVATION_SCHEMA_PROBE_SQL).0,
            0,
            "the catalog probe takes no parameters; it is run with an empty bind slice"
        );
    }

    #[test]
    fn the_probe_timeout_is_bounded_and_matches_the_pool() {
        // Not a deadlock guard — a stall bound. Capture tasks queue on the
        // OnceCell initializer *holding pooled connections* (max_size 8, no
        // per-statement timeout), so an unbounded probe can pin the whole pool
        // and starve every other PG consumer in the runner.
        assert!(
            OBSERVATION_PROBE_TIMEOUT > std::time::Duration::ZERO,
            "a zero timeout would make the probe unable to ever answer true"
        );
        assert_eq!(
            OBSERVATION_PROBE_TIMEOUT,
            std::time::Duration::from_secs(5),
            "the bound tracks the pool's own create/wait/recycle timeout in database::pg — a \
             catalog read that outlives the pool's patience is a stalled server, not a slow one"
        );
    }
}
