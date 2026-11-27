//! The runner's database health, reported OUTWARD to coord as the
//! `runner_database` key of `coord.device_status.details`.
//!
//! Plan `2026-11-27-devops-runner-database-degraded-is-invisible-to-coord`
//! Phase 1 (D1, D2). A primary runner booted without its database on
//! 2026-11-24 and stayed that way for 59 hours. Its scheduler was dead the
//! whole time, while every coord surface called the device `healthy`. The
//! runner knew: `/health` `data.database` said `arm: degraded`,
//! `breaker.state: open`. Nothing carried that off the box. The coord
//! heartbeat has no database field, and the status bag held only
//! `coord_credential`.
//!
//! # What is published
//!
//! [`runner_database_bag`] is the `/health` database block, rendered by the
//! SAME serializer (`database::pg::health::database_health_json_with`), so the
//! two cannot drift. Three changes are applied to its output, and nothing else:
//!
//! * **Keys are snake_case** (`causeClass` -> `cause_class`, `failedTrials` ->
//!   `failed_trials`). This is a mechanical projection of the serializer's own
//!   key names, matching the status bag's existing keys. A field added to
//!   `/health` therefore reaches the bag with no second list to update.
//! * **Every string is bounded.** Every string leaf of the bag is redacted
//!   FIRST and then cut to [`RUNNER_TEXT_MAX_CHARS`] characters on a char
//!   boundary ([`bounded_runner_text`]). The free text today is
//!   `breaker.last_error` and `schema_detail`, and a connect error can echo a
//!   DSN. Bounding EVERY leaf, not a list of known free-text fields, is what
//!   keeps the projection above safe: a free-text field added to `/health`
//!   later is redacted with no list to update. The arm and breaker tokens,
//!   the timestamps and `build_id` contain nothing the redactors match and
//!   never reach the cut, so they pass through byte-identical. A path is
//!   redacted and cut like any other string leaf: it is unchanged unless a
//!   redactor matches it (a DSN, or what `redact_secrets` rewrites: credential
//!   pairs, bearer tokens, JWT shapes, URL userinfo) or it runs past
//!   [`RUNNER_TEXT_MAX_CHARS`] characters. Cutting
//!   before redacting could split a DSN before its `@` and leave the password
//!   where no redactor recognises it any more.
//! * **Three fields are added**, none of them on `/health`: `build_id`
//!   (`/health`'s `buildId`), `reported_at` (the wall-clock instant this bag
//!   was built) and `breaker.open_for_ms`, how long the breaker has been open
//!   on the MONOTONIC clock ([`open_for_ms`]). Coord's sustain would
//!   otherwise be `reported_at - breaker.since`, two wall-clock readings, and
//!   a breaker that trips at boot can trip before NTP corrects a clock that
//!   was behind, which inflates the sustain. The monotonic clock is
//!   `std::time::Instant`, which on Linux is `CLOCK_MONOTONIC`: it stops
//!   while the machine is suspended, so across a suspend `open_for_ms`
//!   UNDERSTATES how long the breaker has been open. That is the safe
//!   direction for a sustain threshold: a suspend can delay an alert, never
//!   invent one.
//!
//! When the bag cannot be built, the sibling key [`RUNNER_DATABASE_ERROR_KEY`]
//! is sent INSTEAD, carrying the reason. It is never an empty report. Coord
//! ranks a key against its `*_error` sibling by receipt time
//! (`runner_reports::effective`), so neither post has to clear the other.
//!
//! # How it is published
//!
//! [`spawn_database_report_publisher`] runs its own task on a 60 s cadence
//! ([`PUBLISH_EVERY`]). It does not ride the device-JWT refresher, which
//! ticks every 300 s and can block on its idle arm. Each tick:
//!
//! * runs on the PRIMARY only. The spawn and every tick consult
//!   [`super::machine_state_publish_allowed`], and so does the post helper.
//!   A temp runner shares the primary's `device_id`, so its report would
//!   raise (or clear) a database alert on the primary's device. A secondary
//!   OMITS the key entirely. It never sends `null`, which coord's per-key
//!   merge would read as "delete the primary's report";
//! * reads only the in-memory snapshot and does no database I/O, because its
//!   whole purpose is to work when the database is gone;
//! * posts through [`super::status_report::post_runner_report`]: a connected
//!   coord only (`connected_coord_base`, where `None` is a no-op), carrying the
//!   paired device bearer that coord requires before it writes a runner-report
//!   key.
//!
//! Detection lives in this process and is reported outward. Nothing here
//! touches the dev supervisor. A running runner never gains this publisher:
//! it starts with the next runner start onto a build that contains it.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use crate::database::pg::health::{database_health_json_with, PgHealthSnapshot};
use crate::embedded_pg::DbArm;
use qontinui_runner_lib::circuit::{CircuitSnapshot, CircuitState};

/// The `details` key the report is published under.
pub(crate) const RUNNER_DATABASE_KEY: &str = "runner_database";

/// The sibling sent INSTEAD of [`RUNNER_DATABASE_KEY`] when the runner could
/// not build the report: a reason for absence, never an empty report.
pub(crate) const RUNNER_DATABASE_ERROR_KEY: &str = "runner_database_error";

/// The publish cadence. Coord's shared runner-report stale bound (900 s) is 15
/// of these, so one skipped tick never ages a report out.
pub(crate) const PUBLISH_EVERY: Duration = Duration::from_secs(60);

/// The longest free-text field the report carries, in characters.
pub(crate) const RUNNER_TEXT_MAX_CHARS: usize = 300;

/// The `runner_database` bag. Pure: the arm, port and data root are process
/// globals (`embedded_pg`), so they are arguments, and `now` (an `Instant`)
/// drives `reachable` / `stale` / `age_ms` exactly as it does for `/health`.
/// `wall` is when the bag was built (`reported_at`). `observed_at` stays the
/// probe's own instant, which is what `/health` serves. Every string leaf,
/// the added ones included, goes through [`bounded_runner_text`].
pub(crate) fn runner_database_bag(
    snapshot: &PgHealthSnapshot,
    arm: DbArm,
    embedded_port: Option<u16>,
    embedded_data_root: Option<String>,
    build_id: &str,
    now: Instant,
    wall: DateTime<Utc>,
) -> Value {
    let shared = database_health_json_with(snapshot, now, arm, embedded_port, embedded_data_root);
    let mut bag = snake_case_keys(shared);
    if let Some(obj) = bag.as_object_mut() {
        // Only where `/health` serves a breaker object; a null breaker stays
        // null.
        if let Some(breaker) = obj.get_mut("breaker").and_then(Value::as_object_mut) {
            breaker.insert(
                "open_for_ms".to_string(),
                serde_json::json!(open_for_ms(snapshot.breaker.as_ref(), now)),
            );
        }
        obj.insert("build_id".to_string(), Value::String(build_id.to_string()));
        obj.insert(
            "reported_at".to_string(),
            Value::String(wall.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
    }
    bound_every_string_leaf(&mut bag);
    bag
}

/// How long the breaker has been open, in ms on the MONOTONIC clock:
/// `now - since_instant`, measured when the bag is built. The onset is the
/// ORIGINAL trip, the same one `breaker.since` names: a half-open trial that
/// fails re-opens the breaker and keeps `since` (`circuit.rs`
/// `reopen_locked`), so this never resets to a per-spell value capped near
/// the backoff. `None` (a JSON `null` in the bag) while Closed, or when the
/// trip's instant is unknown. Saturating, so a trip instant after `now` reads
/// 0: never negative, and it never panics.
///
/// The clock is `std::time::Instant`, `CLOCK_MONOTONIC` on Linux, which
/// stops during a suspend. Across a suspend this UNDERSTATES the time open:
/// the safe direction for coord's sustain, which it can delay but never
/// inflate.
fn open_for_ms(breaker: Option<&CircuitSnapshot>, now: Instant) -> Option<u64> {
    let breaker = breaker?;
    if breaker.state == CircuitState::Closed {
        return None;
    }
    let open_for = now.saturating_duration_since(breaker.since_instant?);
    Some(u64::try_from(open_for.as_millis()).unwrap_or(u64::MAX))
}

/// Redact, THEN cut to [`RUNNER_TEXT_MAX_CHARS`] characters. The order is the
/// point: see the module doc.
///
/// Two redactors run. `profiles::redact_dsn_password`, the runner's DSN
/// redactor, covers both DSN spellings and runs over each DSN-shaped token
/// only ([`redact_dsn_tokens`]). `session::redact::redact_secrets` then runs
/// over the whole text and masks any URL userinfo, `password=`-style pair,
/// bearer or JWT the first one did not.
pub(crate) fn bounded_runner_text(raw: &str) -> String {
    let dsn_redacted = redact_dsn_tokens(raw);
    let redacted = String::from_utf8_lossy(&crate::session::redact::redact_secrets(
        dsn_redacted.as_bytes(),
    ))
    .into_owned();
    truncate_chars(&redacted, RUNNER_TEXT_MAX_CHARS)
}

/// At most `max` characters, cut on a char boundary. A cut text ends in `…`,
/// so a reader can tell it was cut.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `profiles::redact_dsn_password` over each whitespace-delimited token that
/// holds a DSN (`scheme://` or `password=`), never over the whole message.
///
/// `redact_dsn_password` reads its input as ONE DSN: given a whole error, it
/// pairs the first `://` with the first later `@` anywhere in the text. So
/// `connect to postgres://db.internal:5432/qontinui_db failed: role "svc@prod"
/// does not exist` lost its port, database and cause, which is the text an
/// operator reads beside a `degraded` verdict. A DSN holds no whitespace, so
/// one token is the span: the redactor still sees a password that holds a
/// `/`, which `redact_secrets`' userinfo pattern stops at. Whitespace is
/// kept exactly. Each token is built char by char rather than sliced out of
/// `raw`, so no byte index can land inside a multibyte character
/// (`clippy::string_slice`, denied in `Cargo.toml`).
fn redact_dsn_tokens(raw: &str) -> String {
    fn flush(out: &mut String, token: &mut String) {
        if token.contains("://") || token.contains("password=") {
            out.push_str(&qontinui_runner_lib::profiles::redact_dsn_password(token));
        } else {
            out.push_str(token);
        }
        token.clear();
    }
    let mut out = String::with_capacity(raw.len());
    let mut token = String::new();
    for c in raw.chars() {
        if c.is_whitespace() {
            flush(&mut out, &mut token);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut out, &mut token);
    out
}

/// [`bounded_runner_text`] over every string leaf of `v`, at every depth.
/// Keys, numbers, booleans and `null` are left alone.
fn bound_every_string_leaf(v: &mut Value) {
    match v {
        Value::String(s) => *s = bounded_runner_text(s),
        Value::Array(items) => items.iter_mut().for_each(bound_every_string_leaf),
        Value::Object(map) => map.values_mut().for_each(bound_every_string_leaf),
        // Numbers, booleans and the absent value carry no text.
        _ => {}
    }
}

/// `camelCase` -> `snake_case`, for one key. Keys already in snake_case
/// (`schema_ok`) pass through unchanged.
fn snake_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Every object key in `v`, at every depth, through [`snake_case`].
fn snake_case_keys(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (snake_case(&k), snake_case_keys(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(snake_case_keys).collect()),
        other => other,
    }
}

/// The `details` one tick sends. It holds EXACTLY ONE key, and its value is
/// never `null`. The key is [`RUNNER_DATABASE_KEY`] with the bag, or
/// [`RUNNER_DATABASE_ERROR_KEY`] with the reason the bag could not be built:
/// the builder panicked, or it returned something other than an object.
pub(crate) fn report_details(build: impl FnOnce() -> Value) -> Map<String, Value> {
    let mut details = Map::new();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(build)) {
        Ok(bag) if bag.is_object() => {
            details.insert(RUNNER_DATABASE_KEY.to_string(), bag);
        }
        Ok(_) => {
            details.insert(
                RUNNER_DATABASE_ERROR_KEY.to_string(),
                Value::String("the database report was not a JSON object".to_string()),
            );
        }
        Err(payload) => {
            let why = format!(
                "building the database report panicked: {}",
                crate::worker_supervisor::panic_payload_message(payload.as_ref())
            );
            details.insert(
                RUNNER_DATABASE_ERROR_KEY.to_string(),
                Value::String(bounded_runner_text(&why)),
            );
        }
    }
    details
}

/// This process's report, from the cached snapshot. No database I/O.
fn current_details() -> Map<String, Value> {
    report_details(|| {
        let snapshot = crate::database::pg::health::pg_health_snapshot();
        runner_database_bag(
            &snapshot,
            crate::embedded_pg::db_arm(),
            crate::embedded_pg::embedded_port(),
            crate::embedded_pg::embedded_data_root(),
            env!("RUNNER_BUILD_ID"),
            Instant::now(),
            Utc::now(),
        )
    })
}

/// One tick. `failing` carries the previous tick's outcome, so a coord that
/// stays unreachable logs one `warn!` per failure streak and `debug!` after.
async fn publish_once(failing: &mut bool) {
    // Gated on EVERY tick, not only at spawn: a secondary sends nothing at all,
    // not even a `null`.
    if !super::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
        return;
    }
    match super::status_report::post_runner_report(current_details()).await {
        Ok(super::status_report::ReportPost::Sent) => {
            if std::mem::take(failing) {
                info!("fleet::database_report: publishing again after a failure streak");
            }
        }
        Ok(super::status_report::ReportPost::NotSent(why)) => {
            debug!("fleet::database_report: not sent: {why}");
        }
        Err(e) => {
            if std::mem::replace(failing, true) {
                debug!("fleet::database_report: publish failed (streak continues): {e}");
            } else {
                warn!(
                    "fleet::database_report: publish failed (best-effort; retried every tick): {e}"
                );
            }
        }
    }
}

/// Start the 60 s `runner_database` publisher on the CURRENT runtime.
/// `main.rs` puts it on the dedicated `fleet-heartbeat` thread, whose runtime
/// no sweep blocks. A no-op on a secondary instance.
pub fn spawn_database_report_publisher() {
    if !super::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
        info!(
            "fleet::database_report: SECONDARY instance — not starting. Only the instance that \
             owns shared machine-wide state reports the runner database to coord."
        );
        return;
    }
    info!(
        "fleet::database_report: starting, interval={}s",
        PUBLISH_EVERY.as_secs()
    );
    crate::worker_supervisor::spawn_supervised_with_heartbeat(
        "fleet.database_report_publisher",
        |hb| async move {
            let mut tick = tokio::time::interval(PUBLISH_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut failing = false;
            loop {
                tick.tick().await;
                hb.tick();
                publish_once(&mut failing).await;
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::pg_circuit::{CauseClass, ProbeObservation};
    use serde_json::json;
    use std::time::SystemTime;

    const ARMS: [(DbArm, &str); 5] = [
        (DbArm::Unknown, "unknown"),
        (DbArm::External, "external"),
        (DbArm::EmbeddedOwned, "embedded-owned"),
        (DbArm::EmbeddedAttached, "embedded-attached"),
        (DbArm::Degraded, "degraded"),
    ];

    fn wall() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-27T03:37:28Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn system_time(rfc3339: &str) -> SystemTime {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
            .into()
    }

    fn probe(age: Duration, reachable: bool, now: Instant) -> ProbeObservation {
        ProbeObservation {
            at: SystemTime::now() - age,
            at_instant: now - age,
            duration: Duration::from_millis(3),
            reachable: Some(reachable),
            error: (!reachable).then(|| "refused".to_string()),
        }
    }

    fn breaker(state: CircuitState, last_error: Option<&str>) -> CircuitSnapshot {
        let open = state != CircuitState::Closed;
        CircuitSnapshot {
            state,
            since: open.then(|| system_time("2026-11-24T16:03:53Z")),
            since_instant: None,
            cause: open.then(|| "runtime".to_string()),
            last_error: last_error.map(str::to_string),
            retry_after: open.then_some(Duration::from_secs(15)),
            failed_trials: if open { 3 } else { 0 },
            trips: 1,
            wait_timeouts: 0,
            wait_timeout_last_at: None,
            backend_errors: 0,
            ambiguous_errors: 0,
        }
    }

    fn snapshot(b: Option<CircuitSnapshot>, class: CauseClass, now: Instant) -> PgHealthSnapshot {
        PgHealthSnapshot {
            // A pool exists exactly when a breaker does (`pg_health_snapshot`).
            configured: b.is_some(),
            probe: b
                .is_some()
                .then(|| probe(Duration::from_secs(2), false, now)),
            schema: None,
            breaker: b,
            cause_class: class,
        }
    }

    fn bag(s: &PgHealthSnapshot, arm: DbArm, root: Option<&str>, now: Instant) -> Value {
        runner_database_bag(
            s,
            arm,
            None,
            root.map(str::to_string),
            "test-build",
            now,
            wall(),
        )
    }

    /// nawa's `/health` `data.database` block at 2026-11-27T03:37:28Z, verbatim
    /// in every field the incident evidence quotes.
    fn nawa(now: Instant) -> PgHealthSnapshot {
        PgHealthSnapshot {
            configured: true,
            probe: Some(probe(Duration::from_secs(2), false, now)),
            schema: None,
            breaker: Some(CircuitSnapshot {
                state: CircuitState::Open,
                since: Some(system_time("2026-11-24T16:03:53Z")),
                since_instant: None,
                cause: Some("boot".to_string()),
                last_error: Some(
                    "PostgreSQL connection failed: Error occurred while creating a new object: \
                     error connecting to server <- Connection refused (os error 111)"
                        .to_string(),
                ),
                retry_after: Some(Duration::from_secs(15)),
                failed_trials: 14786,
                trips: 1,
                wait_timeouts: 0,
                wait_timeout_last_at: None,
                backend_errors: 0,
                ambiguous_errors: 0,
            }),
            cause_class: CauseClass::ConnectRefused,
        }
    }

    /// Test 1: all five arms × breaker {null, closed, open, half_open}. The
    /// arm is an argument, so no global is touched.
    #[test]
    fn every_arm_and_every_breaker_state_passes_through_verbatim() {
        let now = Instant::now();
        let breakers: [(Option<CircuitState>, Option<&str>); 4] = [
            (None, None),
            (Some(CircuitState::Closed), Some("closed")),
            (Some(CircuitState::Open), Some("open")),
            (Some(CircuitState::HalfOpen), Some("half_open")),
        ];
        for (arm, arm_str) in ARMS {
            for (state, state_str) in breakers {
                let s = snapshot(
                    state.map(|st| breaker(st, Some("error connecting to server"))),
                    CauseClass::ConnectRefused,
                    now,
                );
                let b = bag(&s, arm, None, now);
                assert_eq!(b["arm"], json!(arm_str), "{arm:?} × {state:?}");
                match state_str {
                    None => {
                        // A null breaker stays null; it is never a closed one.
                        assert!(b["breaker"].is_null(), "{arm:?}: {b}");
                        assert!(b["reachable"].is_null(), "{arm:?}: {b}");
                    }
                    Some(st) => assert_eq!(b["breaker"]["state"], json!(st), "{arm:?}: {b}"),
                }
                assert_eq!(b["build_id"], json!("test-build"));
                assert_eq!(b["reported_at"], json!("2026-11-27T03:37:28Z"));
            }
        }
    }

    /// Test 2: nawa's incident, rebuilt, reads as nawa read.
    #[test]
    fn nawas_verbatim_block_reads_degraded_open_boot() {
        let now = Instant::now();
        let b = runner_database_bag(
            &nawa(now),
            DbArm::Degraded,
            None,
            Some("/home/jspinak/.local/share/com.qontinui.runner/embedded-pg".to_string()),
            "4d524e71b",
            now,
            wall(),
        );
        assert_eq!(b["arm"], "degraded");
        assert_eq!(b["reachable"], false);
        assert_eq!(b["schema_ok"], Value::Null);
        assert_eq!(b["breaker"]["state"], "open");
        assert_eq!(b["breaker"]["cause"], "boot");
        assert_eq!(b["breaker"]["cause_class"], "connect_refused");
        assert_eq!(b["breaker"]["since"], "2026-11-24T16:03:53Z");
        assert_eq!(b["breaker"]["failed_trials"], 14786);
        assert_eq!(b["breaker"]["trips"], 1);
        assert_eq!(
            b["breaker"]["last_error"],
            "PostgreSQL connection failed: Error occurred while creating a new object: \
             error connecting to server <- Connection refused (os error 111)"
        );
        assert_eq!(
            b["embedded_data_root"],
            "/home/jspinak/.local/share/com.qontinui.runner/embedded-pg"
        );
        assert_eq!(b["build_id"], "4d524e71b");
        assert_eq!(b["stale"], false);
    }

    fn last_error_of(raw: &str) -> String {
        let now = Instant::now();
        let s = snapshot(
            Some(breaker(CircuitState::Open, Some(raw))),
            CauseClass::ConnectRefused,
            now,
        );
        bag(&s, DbArm::External, None, now)["breaker"]["last_error"]
            .as_str()
            .expect("a string last_error")
            .to_string()
    }

    /// Test 3: a DSN in `last_error` is redacted, and the text is bounded.
    #[test]
    fn a_dsn_password_never_leaves_and_the_text_is_bounded() {
        // The long case FIRST, with the DSN at the START, so the password sits
        // inside the 300 characters that are kept: this assertion fails when
        // redaction is removed.
        let long = format!("postgres://u:secret@h/db {}", "x".repeat(1000));
        let out = last_error_of(&long);
        assert!(!out.contains("secret"), "secret survived: {out}");
        assert_eq!(out.chars().count(), RUNNER_TEXT_MAX_CHARS);
        assert!(out.ends_with('…'), "a cut text says so: {out}");

        for raw in [
            "PostgreSQL connection failed: postgres://u:secret@h/db: connection refused",
            // libpq's key=value spelling.
            "connect failed: host=db.internal password=secret dbname=qontinui_db",
            // A password holding a `/`, which the userinfo pattern stops at.
            "connect to postgres://u:se/cret@h/db failed",
        ] {
            let out = last_error_of(raw);
            assert!(
                !out.contains("secret") && !out.contains("se/cret"),
                "secret survived: {out}"
            );
            assert!(out.chars().count() <= RUNNER_TEXT_MAX_CHARS, "{out}");
        }
    }

    /// The DSN redactor runs over the DSN's own token, never the whole message:
    /// a DSN with no userinfo, followed by an unrelated `@`, keeps its port, its
    /// database and the cause.
    #[test]
    fn a_userinfo_free_dsn_keeps_its_port_database_and_cause() {
        let raw = r#"connect to postgres://db.internal:5432/qontinui_db failed: role "svc@prod" does not exist"#;
        assert_eq!(bounded_runner_text(raw), raw);
        assert_eq!(last_error_of(raw), raw);
        // Redacting a DSN leaves the text around it, whitespace included.
        assert_eq!(
            bounded_runner_text("a\tpostgres://u:secret@h:5432/db  failed: x@y"),
            bounded_runner_text("a\tpostgres://u:***@h:5432/db  failed: x@y"),
        );
        assert!(
            bounded_runner_text("a\tpostgres://u:secret@h:5432/db  failed: x@y")
                .ends_with(":5432/db  failed: x@y")
        );
        // Multibyte text on both sides of a DSN, split by a multibyte
        // whitespace (U+00A0), comes through intact; only the password goes.
        let out = bounded_runner_text("échec\u{a0}postgres://ü:secret@h/db\u{a0}— réseau");
        assert!(!out.contains("secret"), "secret survived: {out}");
        assert!(out.starts_with("échec\u{a0}postgres://"), "{out}");
        assert!(out.ends_with("@h/db\u{a0}— réseau"), "{out}");
    }

    /// Every string leaf is bounded, at every depth, so a free-text field the
    /// serializer gains later is redacted with no list to update. Non-strings
    /// are untouched.
    #[test]
    fn every_string_leaf_is_bounded_at_every_depth() {
        let mut v = json!({
            "future_free_text": "connect to postgres://u:secret@h/db failed",
            "nested": {"list": ["password=secret", "kept"], "n": 7, "b": true, "z": null},
            "long": "y".repeat(1000),
        });
        bound_every_string_leaf(&mut v);
        let wire = v.to_string();
        assert!(!wire.contains("secret"), "secret survived: {wire}");
        assert_eq!(v["nested"]["list"][1], "kept");
        assert_eq!(v["nested"]["n"], 7);
        assert_eq!(v["nested"]["b"], true);
        assert!(v["nested"]["z"].is_null());
        assert_eq!(
            v["long"].as_str().unwrap().chars().count(),
            RUNNER_TEXT_MAX_CHARS
        );
    }

    /// Test 3, the straddle case: the DSN sits across the 300-char cut, so a
    /// cut taken FIRST would drop the `@` and leave `secret` where no redactor
    /// matches it. Only redact-then-truncate passes.
    #[test]
    fn a_dsn_straddling_the_cut_is_redacted_because_redaction_runs_first() {
        // Slide the DSN across the cut one character at a time, so that for
        // SOME offset `secret` sits wholly before the cut and its `@` after
        // it, whatever exact length the cut keeps.
        let mut straddled = 0;
        for pad in (RUNNER_TEXT_MAX_CHARS - 40)..=(RUNNER_TEXT_MAX_CHARS - 10) {
            let raw = format!(
                "{} postgres://u:secret@db.internal:5432/qontinui_db refused",
                "e".repeat(pad)
            );
            let at = raw.find('@').expect("an @");
            let secret_end = raw.find("secret").expect("secret") + "secret".len();
            if secret_end < RUNNER_TEXT_MAX_CHARS && at >= RUNNER_TEXT_MAX_CHARS - 1 {
                straddled += 1;
            }
            let out = last_error_of(&raw);
            assert!(!out.contains("secret"), "pad {pad}: secret survived: {out}");
            assert!(out.chars().count() <= RUNNER_TEXT_MAX_CHARS, "{out}");
        }
        assert!(straddled > 0, "no offset put the DSN across the cut");
        // Multi-byte text is cut on a char boundary, never mid-codepoint.
        let wide = "é".repeat(RUNNER_TEXT_MAX_CHARS * 2);
        assert_eq!(
            bounded_runner_text(&wide).chars().count(),
            RUNNER_TEXT_MAX_CHARS
        );
    }

    /// Test 4: a stale snapshot is UNKNOWN: `stale: true`, `reachable: null`.
    #[test]
    fn a_stale_snapshot_reads_stale_and_reachable_null() {
        let now = Instant::now();
        let s = PgHealthSnapshot {
            configured: true,
            probe: Some(probe(Duration::from_secs(31), true, now)),
            schema: None,
            breaker: Some(breaker(CircuitState::Closed, None)),
            cause_class: CauseClass::Unknown,
        };
        let b = bag(&s, DbArm::EmbeddedOwned, None, now);
        assert_eq!(b["stale"], true);
        assert!(b["reachable"].is_null(), "{b}");
    }

    /// The production half of this file, comments dropped: a needle written in
    /// a test or in prose must not satisfy or trip a source assertion.
    fn prod_code() -> String {
        let src = include_str!("database_report.rs");
        let prod = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map(|(before, _)| before)
            .unwrap_or(src);
        prod.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Test 5 (with `fleet::tests::every_machine_scoped_writer_consults_the_guard`,
    /// which pins the guard itself): what a tick sends never carries a null
    /// `runner_database`. Behaviourally, for both arms. At the source, no
    /// production line spells a null at all.
    #[test]
    fn the_post_path_never_sends_a_null_runner_database() {
        let ok = report_details(|| json!({"arm": "degraded"}));
        assert_eq!(ok.len(), 1);
        assert!(ok[RUNNER_DATABASE_KEY].is_object());

        let panicked = report_details(|| panic!("boom"));
        assert_eq!(panicked.len(), 1, "{panicked:?}");
        assert!(!panicked.contains_key(RUNNER_DATABASE_KEY), "{panicked:?}");
        assert!(panicked[RUNNER_DATABASE_ERROR_KEY]
            .as_str()
            .is_some_and(|s| s.contains("boom")));

        let not_an_object = report_details(|| Value::Null);
        assert_eq!(not_an_object.len(), 1);
        assert!(!not_an_object.contains_key(RUNNER_DATABASE_KEY));
        assert!(not_an_object[RUNNER_DATABASE_ERROR_KEY].is_string());

        for details in [ok, panicked, not_an_object] {
            assert!(details.values().all(|v| !v.is_null()), "{details:?}");
        }

        let squeezed: String = prod_code().chars().filter(|c| !c.is_whitespace()).collect();
        for needle in ["Value::Null", "json!(null)", ":null"] {
            assert!(
                !squeezed.contains(needle),
                "the production path spells `{needle}`"
            );
        }
        // The one door out, which itself refuses a null-valued key.
        assert!(squeezed.contains("status_report::post_runner_report(current_details())"));
    }

    /// Test 6: the publisher does no database I/O. It names no `PgDb`, no
    /// pool, and no connection checkout.
    #[test]
    fn the_publisher_names_no_pg_db_and_no_pool_checkout() {
        let code = prod_code();
        for needle in [
            "PgDb",
            "pg_db",
            "try_global",
            ".pool()",
            "probe_now",
            ".get().await",
            "sqlx",
            "tokio_postgres",
            "deadpool",
        ] {
            assert!(
                !code.contains(needle),
                "fleet/database_report.rs names `{needle}`"
            );
        }
        assert!(code.contains("pg_health_snapshot()"));
    }

    /// Every leaf of `v`, as `(JSON pointer, value)`.
    fn leaves(v: &Value, at: String, out: &mut Vec<(String, Value)>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    leaves(child, format!("{at}/{k}"), out);
                }
            }
            other => out.push((at, other.clone())),
        }
    }

    /// Test 7: `/health` and the bag come out of one serializer. Every field
    /// `/health` serves is in the bag under its snake_case name with the same
    /// value, and the bag adds only `build_id`, `reported_at` and
    /// `breaker.open_for_ms`.
    #[test]
    fn health_and_the_bag_share_one_serializer() {
        let now = Instant::now();
        let mut s = nawa(now);
        if let Some(b) = s.breaker.as_mut() {
            b.wait_timeouts = 2;
            b.wait_timeout_last_at = Some(system_time("2026-11-24T16:10:00Z"));
            b.backend_errors = 5;
        }
        let root = Some("/data/embedded-pg".to_string());
        let health = database_health_json_with(&s, now, DbArm::Degraded, Some(5433), root.clone());
        let b = runner_database_bag(&s, DbArm::Degraded, Some(5433), root, "b1", now, wall());

        let mut health_leaves = Vec::new();
        leaves(&health, String::new(), &mut health_leaves);
        assert!(health_leaves.len() >= 20, "{health_leaves:?}");
        for (pointer, value) in &health_leaves {
            let snake: String = pointer
                .split('/')
                .map(snake_case)
                .collect::<Vec<_>>()
                .join("/");
            assert_eq!(
                b.pointer(&snake),
                Some(value),
                "`/health` {pointer} is not the bag's {snake}"
            );
        }
        let mut bag_leaves = Vec::new();
        leaves(&b, String::new(), &mut bag_leaves);
        assert_eq!(
            bag_leaves.len(),
            health_leaves.len() + 3,
            "the bag adds exactly build_id, reported_at and breaker.open_for_ms"
        );
        // `/health` itself is unchanged: it serves no open-for field.
        assert!(
            health_leaves.iter().all(|(p, _)| !p.contains("openFor")
                && !p.contains("open_for")
                && !p.contains("sinceInstant")
                && !p.contains("since_instant")),
            "{health_leaves:?}"
        );
        assert!(b.pointer("/breaker/open_for_ms").is_some(), "{b}");

        // And `/health`'s own entry point is the same serializer over the
        // process globals, while the publisher builds through the bag.
        let health_src = include_str!("../database/pg/health.rs");
        let wrapper = health_src
            .split_once("pub fn database_health_json(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("database_health_json in health.rs");
        assert!(wrapper.contains("database_health_json_with("), "{wrapper}");
        assert!(prod_code().contains("database_health_json_with("));
    }

    /// `breaker.open_for_ms` is the MONOTONIC time open, measured when the bag
    /// is built: about the elapsed time for a real tripped circuit, exact for a
    /// known instant, `null` while closed or when the instant is unknown, and
    /// 0 (never negative, never a panic) for an instant after `now`.
    #[test]
    fn open_for_ms_is_monotonic_time_open_and_null_while_closed() {
        use qontinui_runner_lib::circuit::{Circuit, Outcome};
        use qontinui_runner_lib::pg_circuit::PG_CIRCUIT_CONFIG;

        // A real circuit, tripped: the snapshot carries the trip's instant.
        let c = Circuit::new(PG_CIRCUIT_CONFIG);
        c.record(Outcome::TripFailure("error connecting to server".into()));
        let tripped = c.snapshot();
        assert_eq!(tripped.since_instant, c.since_instant());
        let now = Instant::now() + Duration::from_secs(90);
        let s = snapshot(Some(tripped), CauseClass::ConnectRefused, now);
        let ms = bag(&s, DbArm::Degraded, None, now)["breaker"]["open_for_ms"]
            .as_u64()
            .expect("a number while open");
        assert!((90_000..95_000).contains(&ms), "about 90 s open: {ms}");

        // A known instant: nawa, open 59 h 33 m 35 s.
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(214_415);
        for state in [CircuitState::Open, CircuitState::HalfOpen] {
            let mut b = breaker(state, None);
            b.since_instant = Some(t0);
            let out = bag(
                &snapshot(Some(b), CauseClass::ConnectRefused, now),
                DbArm::Degraded,
                None,
                now,
            );
            assert_eq!(
                out["breaker"]["open_for_ms"],
                json!(214_415_000u64),
                "{state:?}"
            );
            assert!(
                out.pointer("/breaker/openForMs").is_none(),
                "snake_case only: {out}"
            );
        }

        // Closed is null, even if an instant were carried; so is an unknown one.
        let mut closed = breaker(CircuitState::Closed, None);
        closed.since_instant = Some(t0);
        let unknown = breaker(CircuitState::Open, None);
        for b in [closed, unknown, Circuit::new(PG_CIRCUIT_CONFIG).snapshot()] {
            let out = bag(
                &snapshot(Some(b), CauseClass::ConnectRefused, now),
                DbArm::External,
                None,
                now,
            );
            let field = out
                .pointer("/breaker/open_for_ms")
                .expect("the key is present");
            assert!(field.is_null(), "{out}");
        }

        // A trip instant after `now` saturates to 0.
        let mut ahead = breaker(CircuitState::Open, None);
        ahead.since_instant = Some(now + Duration::from_secs(5));
        let out = bag(
            &snapshot(Some(ahead), CauseClass::ConnectRefused, now),
            DbArm::Degraded,
            None,
            now,
        );
        assert_eq!(out["breaker"]["open_for_ms"], json!(0u64));
    }

    /// `open_for_ms` counts from the ORIGINAL trip, the same onset as
    /// `breaker.since`: open -> half_open -> re-open does not reset it. nawa
    /// re-tripped 14786 times; a per-spell value would cap near the 15 s
    /// backoff and hide the outage.
    #[test]
    fn open_for_ms_counts_from_the_first_trip_across_half_open_re_trips() {
        use qontinui_runner_lib::circuit::{Circuit, Outcome};
        use qontinui_runner_lib::pg_circuit::PG_CIRCUIT_CONFIG;
        use std::sync::Arc;

        let c = Arc::new(Circuit::new(PG_CIRCUIT_CONFIG));
        c.record(Outcome::TripFailure("error connecting to server".into()));
        let first = c.since_instant().expect("open since the first trip");
        let first_wall = c.snapshot().since;
        // Let the clock move, so a reset onset would be a DIFFERENT instant
        // and `open_for_ms` below would read less than 90 000.
        std::thread::sleep(Duration::from_millis(5));
        let now = first + Duration::from_secs(90);
        let open_for = |snap: CircuitSnapshot| {
            let s = snapshot(Some(snap), CauseClass::ConnectRefused, now);
            bag(&s, DbArm::Degraded, None, now)["breaker"]["open_for_ms"].clone()
        };
        for retrip in 1..=3u32 {
            let trial = c.begin_trial().expect("a trial slot while open");
            let half_open = c.snapshot();
            assert_eq!(half_open.state, CircuitState::HalfOpen);
            assert_eq!(
                open_for(half_open),
                json!(90_000u64),
                "half_open, re-trip {retrip}"
            );
            c.finish_trial(trial, Err("refused".to_string()));
            let reopened = c.snapshot();
            assert_eq!(reopened.state, CircuitState::Open);
            assert_eq!(reopened.failed_trials, retrip);
            assert_eq!(reopened.since_instant, Some(first), "re-trip {retrip}");
            assert_eq!(
                reopened.since, first_wall,
                "the same onset as breaker.since"
            );
            assert_eq!(
                open_for(reopened),
                json!(90_000u64),
                "re-opened, re-trip {retrip}"
            );
        }
    }

    #[test]
    fn snake_case_is_a_mechanical_projection() {
        assert_eq!(snake_case("causeClass"), "cause_class");
        assert_eq!(snake_case("embeddedDataRoot"), "embedded_data_root");
        assert_eq!(snake_case("schema_ok"), "schema_ok");
        assert_eq!(snake_case("arm"), "arm");
    }

    #[test]
    fn the_report_ticks_every_sixty_seconds() {
        assert_eq!(PUBLISH_EVERY, Duration::from_secs(60));
    }
}
