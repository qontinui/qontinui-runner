//! Host-derived parallelism sizing for one CI dispatch — re-exported from
//! [`qontinui_types::host_sizing`] — and, since plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`
//! Phase 7, the CI node's concurrency read from coord's capacity model.
//!
//! The sizing rule (3 GiB per cargo build token, 1 GiB per test process, the
//! 8 GiB small-host floor, the per-slot suggestion and the 1/N `share`) used to
//! live here. It moved to `qontinui-schemas/rust` so the runner, the supervisor
//! and the bash build wrappers (through a golden table pinned there) all size
//! cargo from ONE formula. Its rationale and its unit tests live with it; this
//! module keeps only the CI lane's import path. Plan
//! `2026-10-02-cargo-jobs-supervisor-build-pool-and-physical-disk-floors-follow-the-machine`,
//! design decision D2.
//!
//! `probe` comes from the `host-probe` feature, which the runner enables on
//! its `qontinui-types` dependency.
//!
//! ## The model arm (plan 2026-11-05 Phase 7)
//!
//! An unset `ci_node.max_concurrent_builds` used to resolve to the static
//! [`suggested_concurrent_builds`] — `max(1, min(cpus/4, mem/12 GiB))`, blind to
//! what else the box is doing. It now resolves to the capacity model's answer
//! for THIS device ([`suggested_concurrent_builds_now`]), read from the cached
//! row [`crate::fleet_capacity`] keeps and the D9 dial the fleet-policy poller
//! keeps (`mcp::fleet_policy_poller::capacity_model_dial`):
//!
//! ```text
//! concurrency = clamp(Lane B dispatches already running when coord's sample was taken
//!                     + the row's served slots.C1,
//!                     1, the static partition)
//! ```
//!
//! * **C1, not C1 + C2.** Lane B builds coord's merge candidates and nothing
//!   else (coord `ci_dispatch`, rule R3), which coord's CI vocabulary classes
//!   C1. The served `slots.C1` and `slots.C2` are two readings of the SAME free
//!   capacity (each "how many units of this class fit in what is free"), so
//!   their sum would count the free box twice.
//! * **Plus what was running at the SAMPLE, and only that.** A slot count is
//!   FREE capacity, net of every build the sample saw; the concurrency is a
//!   TOTAL the executor compares the running count with (`admission`) and coord
//!   compares active dispatches with (the advertised `max_concurrent_ci_jobs`).
//!   So the dispatches the sample had already SEEN are added back. One admitted
//!   after it is not in it: its capacity is still inside `slots.C1`, and
//!   counting it again would admit it twice. The sample's instant is the read's
//!   `fetched_at` less coord's `cache_age_secs` and the row's `sample_age_s`
//!   ([`model_inputs`]; coord stamps a sample when it arrives). And a sample
//!   sees a build only gradually — its CPU figure is a ~30 s average and a
//!   build's memory ramps for minutes — so a dispatch counts as seen only once
//!   it had run [`SAMPLE_RAMP`] before the sample; a younger one's cost is
//!   treated as still inside `slots.C1`. The count is taken against the node's
//!   own start times (`admission::running_started_by`), so it only falls as
//!   those builds finish — a burst after the sample cannot raise it. Every error
//!   left is on the safe side (a build that finished, or ramped, since the
//!   sample is under-counted) and lasts until the next read.
//! * **Never above the static partition, never below 1.** The executor sizes
//!   each dispatch as one of N equal host shares, N the static suggestion
//!   (`CiNodeSettings::dispatch_share_for`). A model total above N would
//!   oversubscribe the host through those shares, so the model LOWERS the
//!   partition on a loaded box and never raises it. The floor of 1 is the one
//!   admission and coord's `ci_dispatch` already apply, so the number the node
//!   advertises is the number it admits.
//! * **The labelled UNKNOWN fallback is the static formula**, used — and named
//!   ([`ConcurrencySource::Static`]) — whenever the model does not answer: no
//!   read yet, capacity data older than [`MODEL_STALE_AFTER`] (the read's own
//!   age plus coord's cache age plus the sample's), an UNKNOWN row or
//!   `slots.C1`, a row or an answer that does not say how old it is, a dial
//!   never read in this process, or a dial that does not permit consumer
//!   [`CONSUMER`] (D9).
//! * **A failed refetch keeps the last KNOWN row** while it is younger than
//!   [`MODEL_STALE_AFTER`] ([`read_to_size_from`]): one coord hiccup does not
//!   drop the node to the static partition and back.
//! * **Logged when it changes.** Each change of the answer — the model's count
//!   beside the static one, or the reason the model did not answer (its numbers
//!   aside, so a stale read's growing age is one answer) — is logged once: D9's
//!   per-decision record, for a consumer whose decision is a count.
//!
//! An explicit `ci_node.max_concurrent_builds` still always wins
//! (`CiNodeSettings`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::info;

pub(crate) use qontinui_types::host_sizing::{
    derive, probe, share, suggested_concurrent_builds, suggestion_with_limit, HostCapacity,
    HostSizing,
};

use crate::fleet_capacity::CapacityRead;

/// The D9 consumer this sizing is: coord's `CapacityConsumer::CiPlacement`
/// ("CI and background builds are placed and sized by free capacity").
pub(crate) const CONSUMER: &str = "ci_placement";

/// Capacity data older than this sizes nothing. The age is the read's own
/// (the budget republisher refreshes it every [`crate::fleet_capacity::CACHE_TTL`])
/// plus coord's cache age plus the age of the sample coord computed the row
/// from, which coord itself serves UNKNOWN past its own staleness bound.
pub(crate) const MODEL_STALE_AFTER: Duration = Duration::from_secs(180);

/// How long a dispatch must have run before a capacity sample counts as having
/// seen it: the sample's CPU figure is a ~30 s average and a build's memory
/// ramps for minutes, so a younger dispatch's cost is treated as still inside
/// the served `slots.C1` (the safe side: it is not added back).
pub(crate) const SAMPLE_RAMP: Duration = Duration::from_secs(120);

/// Where a CI-node concurrency came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConcurrencySource {
    /// The capacity model: the Lane B dispatches coord's sample had seen (run
    /// [`SAMPLE_RAMP`] before it), plus the C1 slots it served, clamped to
    /// `[1, static_partition]`.
    Model {
        running_at_sample: u32,
        slots_c1: u32,
        static_partition: u32,
    },
    /// The static host formula — the labelled UNKNOWN fallback — and why the
    /// model did not answer.
    Static { reason: String },
}

impl ConcurrencySource {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            ConcurrencySource::Model { .. } => "model",
            ConcurrencySource::Static { .. } => "static",
        }
    }
}

/// Why the D9 dial keeps [`CONSUMER`] on its legacy reader, or `None` when it
/// permits the model. `dial` is the `capacity_model` block coord served (the
/// poller keeps the last one a 2xx carried); `None` = never read in this
/// process, which D9 never resolves to `enforce`. PURE.
pub(crate) fn dial_refusal(dial: Option<&serde_json::Value>) -> Option<String> {
    let Some(dial) = dial else {
        return Some(
            "the capacity-model dial has not been read in this process; D9 keeps the legacy \
             reader until it is"
                .to_string(),
        );
    };
    let permitted = dial
        .get("permitted_consumers")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|list| list.iter().any(|c| c.as_str() == Some(CONSUMER)));
    (!permitted).then(|| {
        format!(
            "the capacity-model dial does not permit {CONSUMER} (mode {})",
            dial.get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unstated")
        )
    })
}

/// What one capacity read gives the CI node's sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelInputs {
    /// When coord's sample behind the row was taken.
    pub(crate) sample_at: DateTime<Utc>,
    /// The C1 slots the row served.
    pub(crate) slots_c1: u32,
}

/// A served age in seconds, when it is one.
fn served_age(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().filter(|age| age.is_finite() && *age >= 0.0)
}

/// The model's inputs from one capacity read and the dial, or why the model
/// does not answer. PURE.
pub(crate) fn model_inputs(
    read: &CapacityRead,
    dial: Option<&serde_json::Value>,
) -> Result<ModelInputs, String> {
    if let Some(why) = dial_refusal(dial) {
        return Err(why);
    }
    let Some(row) = read.known_row() else {
        return Err(format!(
            "the capacity row is UNKNOWN: {}",
            read.reason.as_deref().unwrap_or("no reason served")
        ));
    };
    let sample_age = served_age(&row["sample_age_s"])
        .ok_or_else(|| "the capacity row does not say how old its sample is".to_string())?;
    let coord_age = read
        .coord
        .as_ref()
        .and_then(|answer| served_age(&answer["cache_age_secs"]))
        .ok_or_else(|| "coord's answer does not say how old its cache is".to_string())?;
    let age = read.cache_age_secs + coord_age + sample_age;
    // A NaN read age is stale too.
    if age.is_nan() || age > MODEL_STALE_AFTER.as_secs_f64() {
        return Err(format!(
            "the capacity data is stale: {age:.0}s old (the read {:.0}s, coord's cache \
             {coord_age:.0}s, the sample {sample_age:.0}s; stale after {}s)",
            read.cache_age_secs,
            MODEL_STALE_AFTER.as_secs()
        ));
    }
    let fetched_at = DateTime::parse_from_rfc3339(&read.fetched_at)
        .map_err(|e| format!("the read's fetched_at does not parse: {e}"))?
        .with_timezone(&Utc);
    // Bounded by MODEL_STALE_AFTER above, so the millisecond count cannot overflow.
    let before_fetch = chrono::Duration::milliseconds(((coord_age + sample_age) * 1000.0) as i64);
    let c1 = &row["slots"]["C1"];
    let slots_c1 = match c1["state"].as_str() {
        Some("known") => c1["value"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| "slots.C1 is known but carries no count".to_string())?,
        Some("unknown") => {
            return Err(format!(
                "slots.C1 is UNKNOWN: {}",
                c1["reason"].as_str().unwrap_or("no reason served")
            ))
        }
        _ => return Err("the capacity row serves no slots.C1".to_string()),
    };
    Ok(ModelInputs {
        sample_at: fetched_at - before_fetch,
        slots_c1,
    })
}

/// Lane B's concurrency: the dispatches running at the sample plus the free C1
/// slots, never above the static partition and never below 1. PURE.
pub(crate) fn lane_b_total(running_at_sample: u32, slots_c1: u32, static_partition: u32) -> u32 {
    running_at_sample
        .saturating_add(slots_c1)
        .min(static_partition)
        .max(1)
}

/// The suggestion from its inputs: the model's Lane B total when it answers,
/// else the static formula with the reason it did not. `running_started_by`
/// counts the dispatches running now that had started by an instant (asked for
/// [`SAMPLE_RAMP`] before the sample), and only when the model answers. PURE
/// over its arguments.
fn answer_from(
    read: Option<&CapacityRead>,
    dial: Option<&serde_json::Value>,
    running_started_by: impl FnOnce(DateTime<Utc>) -> usize,
    host: HostCapacity,
) -> (u32, ConcurrencySource) {
    let static_partition = suggested_concurrent_builds(host);
    let inputs = read
        .ok_or_else(|| "no capacity read in this process yet".to_string())
        .and_then(|read| model_inputs(read, dial));
    match inputs {
        Ok(inputs) => {
            let seen_by = inputs.sample_at
                - chrono::Duration::from_std(SAMPLE_RAMP)
                    .unwrap_or_else(|_| chrono::Duration::zero());
            let running_at_sample = u32::try_from(running_started_by(seen_by)).unwrap_or(u32::MAX);
            (
                lane_b_total(running_at_sample, inputs.slots_c1, static_partition),
                ConcurrencySource::Model {
                    running_at_sample,
                    slots_c1: inputs.slots_c1,
                    static_partition,
                },
            )
        }
        Err(reason) => (static_partition, ConcurrencySource::Static { reason }),
    }
}

/// The read to size from: `current` when its row is KNOWN, else `kept` — the
/// last read whose row was, aged by how long it has been kept — while that is
/// still no older than [`MODEL_STALE_AFTER`]; past it, `current`, so the
/// reason given is today's. PURE.
fn read_to_size_from(
    current: Option<CapacityRead>,
    kept: Option<(&CapacityRead, Duration)>,
) -> Option<CapacityRead> {
    if current
        .as_ref()
        .is_some_and(|read| read.known_row().is_some())
    {
        return current;
    }
    match kept {
        Some((read, kept_for)) => {
            let mut aged = read.clone();
            aged.cache_age_secs += kept_for.as_secs_f64();
            if aged.cache_age_secs <= MODEL_STALE_AFTER.as_secs_f64() {
                Some(aged)
            } else {
                current
            }
        }
        None => current,
    }
}

/// [`crate::fleet_capacity::cached`], through [`read_to_size_from`] and the
/// last read whose row was KNOWN.
fn usable_read() -> Option<CapacityRead> {
    static LAST_KNOWN: Mutex<Option<(Instant, CapacityRead)>> = Mutex::new(None);
    let current = crate::fleet_capacity::cached();
    let Ok(mut last) = LAST_KNOWN.lock() else {
        return current;
    };
    if let Some(read) = current.as_ref().filter(|read| read.known_row().is_some()) {
        *last = Some((Instant::now(), read.clone()));
        return current;
    }
    read_to_size_from(
        current,
        last.as_ref().map(|(at, read)| (read, at.elapsed())),
    )
}

/// The CI node's suggested concurrency for an unset
/// `ci_node.max_concurrent_builds`: the capacity model's Lane B total when it
/// answers, else the static [`suggested_concurrent_builds`] — labelled either
/// way. Synchronous: it reads the cached row and dial and never waits on
/// coord. NOT pure, and it takes `ci_state`'s lock (through
/// `admission::running_started_by`), so no caller may hold that lock.
pub(crate) fn suggested_concurrent_builds_now(host: HostCapacity) -> (u32, ConcurrencySource) {
    let read = usable_read();
    let dial = crate::mcp::fleet_policy_poller::capacity_model_dial();
    let answer = answer_from(
        read.as_ref(),
        dial.as_ref().map(|d| &d.dial),
        super::admission::running_started_by,
        host,
    );
    log_on_change(&answer, suggested_concurrent_builds(host));
    answer
}

/// A reason with every run of digits (and the points between them) collapsed
/// to `#`: a stale read's growing age is one logged answer, while a different
/// reason — another UNKNOWN cause — is another. PURE.
fn reason_kind(reason: &str) -> String {
    let mut out = String::with_capacity(reason.len());
    let mut in_number = false;
    for c in reason.chars() {
        if c.is_ascii_digit() || (in_number && c == '.') {
            if !in_number {
                out.push('#');
                in_number = true;
            }
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

/// The key a logged answer is compared by. PURE.
fn answer_key(answer: &(u32, ConcurrencySource)) -> String {
    match &answer.1 {
        ConcurrencySource::Model {
            running_at_sample,
            slots_c1,
            static_partition,
        } => format!(
            "model {} = {running_at_sample} + {slots_c1} | {static_partition}",
            answer.0
        ),
        ConcurrencySource::Static { reason } => {
            format!("static {} | {}", answer.0, reason_kind(reason))
        }
    }
}

/// D9's record for a sizing consumer: each change of the answer is logged
/// once, the model's count beside the static formula's.
fn log_on_change(answer: &(u32, ConcurrencySource), static_count: u32) {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let key = answer_key(answer);
    {
        let Ok(mut last) = LAST.lock() else {
            return;
        };
        if last.as_deref() == Some(key.as_str()) {
            return;
        }
        *last = Some(key);
    }
    match &answer.1 {
        ConcurrencySource::Model {
            running_at_sample,
            slots_c1,
            static_partition,
        } => info!(
            consumer = CONSUMER,
            model_count = answer.0,
            static_count,
            running_at_sample,
            slots_c1,
            static_partition,
            "ci_node: concurrency from the capacity model, beside the static formula's (D9)"
        ),
        ConcurrencySource::Static { reason } => info!(
            consumer = CONSUMER,
            static_count,
            reason = %reason,
            "ci_node: concurrency from the static formula; the capacity model did not answer"
        ),
    }
}

/// Keep the cached row warm for the CI node's sizing. Called from the budget
/// republisher's loop, the one periodic publisher
/// (`fleet::spawn_budget_republisher`); it asks coord only when the kept read
/// is stale ([`crate::fleet_capacity::current`]).
pub(crate) async fn refresh_model() {
    let _ = crate::fleet_capacity::current().await;
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    const FETCHED_AT: &str = "2026-11-13T04:00:00.000Z";

    fn read_with(row: Option<Value>, age_secs: f64, coord: Option<Value>) -> CapacityRead {
        let known = row.is_some();
        CapacityRead {
            state: if known { "known" } else { "unknown" },
            device_id: Some("6f1d2c3b-0a9e-4d8c-b7a6-5f4e3d2c1b0a".into()),
            row,
            coord,
            reason: (!known).then(|| "no coord".to_string()),
            fetched_at: FETCHED_AT.into(),
            cache_age_secs: age_secs,
            cache_ttl_secs: 60,
        }
    }

    /// A read whose coord answer was cached 5 s.
    fn read(row: Option<Value>, age_secs: f64) -> CapacityRead {
        read_with(
            row,
            age_secs,
            Some(json!({"state": "known", "cache_age_secs": 5.0})),
        )
    }

    /// A row computed from a sample 25 s old.
    fn row(c1: Value) -> Option<Value> {
        Some(json!({
            "device_id": "6f1d2c3b-0a9e-4d8c-b7a6-5f4e3d2c1b0a",
            "sample_age_s": 25.0,
            "slots": {"S": {"state": "known", "value": 9}, "C1": c1},
        }))
    }

    fn dial(permitted: &[&str]) -> Value {
        json!({"mode": "enforce", "permitted_consumers": permitted})
    }

    const FRESH: f64 = 20.0;

    /// The model reads the served C1 slots and dates the sample: the read's
    /// fetch less coord's cache age (5 s) less the sample's age (25 s).
    #[test]
    fn the_inputs_are_the_served_c1_slots_and_the_samples_instant() {
        let on = dial(&["agent_placement", CONSUMER]);
        let inputs = model_inputs(
            &read(row(json!({"state": "known", "value": 3})), FRESH),
            Some(&on),
        )
        .expect("the model answers");
        assert_eq!(inputs.slots_c1, 3);
        let fetched = DateTime::parse_from_rfc3339(FETCHED_AT)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(fetched - inputs.sample_at, chrono::Duration::seconds(30));
    }

    /// The total: what ran at the sample plus the free C1 slots — never above
    /// the static partition (the executor's N), never below 1.
    #[test]
    fn the_total_is_running_at_the_sample_plus_c1_slots_clamped_to_the_partition() {
        // 2 running at the sample and 3 free C1 slots run 5.
        assert_eq!(lane_b_total(2, 3, 12), 5);
        // A full box: nothing beyond what ran (0 slots).
        assert_eq!(lane_b_total(4, 0, 12), 4);
        // An idle, full-looking box still admits one, as admission and coord do.
        assert_eq!(lane_b_total(0, 0, 12), 1);
        // The model lowers the static partition; it never raises it.
        assert_eq!(lane_b_total(10, 30, 12), 12);
        assert_eq!(lane_b_total(u32::MAX, u32::MAX, 12), 12);
    }

    /// Every way the model does not answer is an Err naming why — the caller
    /// then uses the static formula, labelled. None of them is a zero.
    #[test]
    fn every_unanswered_case_names_why_and_is_never_zero() {
        let on = dial(&[CONSUMER]);
        let ok_row = || row(json!({"state": "known", "value": 3}));
        let old_sample = || {
            let mut r = ok_row().unwrap();
            r["sample_age_s"] = json!(170.0);
            Some(r)
        };
        let ageless = || {
            let mut r = ok_row().unwrap();
            r.as_object_mut().unwrap().remove("sample_age_s");
            Some(r)
        };
        let cases: Vec<(CapacityRead, Option<Value>, &str)> = vec![
            (
                read(
                    row(json!({"state": "unknown", "reason": "stale sample"})),
                    FRESH,
                ),
                Some(on.clone()),
                "stale sample",
            ),
            (read(None, FRESH), Some(on.clone()), "no coord"),
            (
                read(Some(json!({"device_id": "d", "sample_age_s": 1.0})), FRESH),
                Some(on.clone()),
                "serves no slots.C1",
            ),
            (read(ok_row(), 181.0), Some(on.clone()), "stale"),
            // The read is fresh; the sample behind it is not.
            (read(old_sample(), FRESH), Some(on.clone()), "stale"),
            (
                read(ageless(), FRESH),
                Some(on.clone()),
                "how old its sample is",
            ),
            (
                read_with(ok_row(), FRESH, Some(json!({"state": "known"}))),
                Some(on.clone()),
                "how old its cache is",
            ),
            (
                read(ok_row(), FRESH),
                Some(dial(&["agent_placement"])),
                "does not permit ci_placement",
            ),
            (
                read(ok_row(), FRESH),
                Some(json!({"mode": "off", "permitted_consumers": []})),
                "mode off",
            ),
            (read(ok_row(), FRESH), None, "has not been read"),
        ];
        for (r, d, why) in cases {
            match model_inputs(&r, d.as_ref()) {
                Err(reason) => assert!(reason.contains(why), "{reason} vs {why}"),
                Ok(v) => panic!("{why}: the model must not answer, got {v:?}"),
            }
        }
    }

    const HOST: HostCapacity = HostCapacity {
        mem_bytes: Some(368 * 1024 * 1024 * 1024),
        cpus: 48,
    };

    /// The composition, with its inputs injected: the running count is asked
    /// for at the SAMPLE's instant, and the static formula answers — labelled —
    /// when there is no read or the dial keeps the legacy reader.
    #[test]
    fn the_answer_counts_running_at_the_sample_and_falls_back_labelled() {
        let on = dial(&[CONSUMER]);
        let r = read(row(json!({"state": "known", "value": 3})), FRESH);
        let fetched = DateTime::parse_from_rfc3339(FETCHED_AT)
            .unwrap()
            .with_timezone(&Utc);
        let mut asked = None;
        let (n, source) = answer_from(
            Some(&r),
            Some(&on),
            |at| {
                asked = Some(at);
                2
            },
            HOST,
        );
        assert_eq!(n, 5);
        assert_eq!(
            source,
            ConcurrencySource::Model {
                running_at_sample: 2,
                slots_c1: 3,
                static_partition: suggested_concurrent_builds(HOST),
            }
        );
        // The sample (30 s before the fetch) has seen what ran SAMPLE_RAMP before it.
        assert_eq!(
            asked,
            Some(fetched - chrono::Duration::seconds(30) - chrono::Duration::seconds(120))
        );
        assert_eq!(SAMPLE_RAMP, Duration::from_secs(120));

        for (no_read, no_dial, why) in [
            (None, Some(&on), "no capacity read"),
            (Some(&r), None, "has not been read"),
        ] {
            let (n, source) = answer_from(no_read, no_dial, |_| panic!("not asked"), HOST);
            assert_eq!(n, suggested_concurrent_builds(HOST));
            assert!(
                matches!(&source, ConcurrencySource::Static { reason } if reason.contains(why)),
                "{source:?}"
            );
        }
    }

    /// A stale read logs once, not once a call: its reason's changing age is
    /// not part of the key.
    #[test]
    fn a_stale_reasons_changing_age_is_one_logged_answer() {
        let at = |age: &str| {
            answer_key(&(
                12,
                ConcurrencySource::Static {
                    reason: format!("the capacity data is stale: {age}s old (the read 1s)"),
                },
            ))
        };
        assert_eq!(at("181"), at("240"));
        let model = |running| {
            answer_key(&(
                5,
                ConcurrencySource::Model {
                    running_at_sample: running,
                    slots_c1: 3,
                    static_partition: 12,
                },
            ))
        };
        assert_ne!(model(2), model(1));
        assert_ne!(model(2), at("181"));
        // A different reason the row is UNKNOWN is a different answer.
        let unknown = |why: &str| {
            answer_key(&(
                12,
                ConcurrencySource::Static {
                    reason: format!("the capacity row is UNKNOWN: {why}"),
                },
            ))
        };
        assert_ne!(unknown("coord unreachable"), unknown("no_plan_yet"));
    }

    /// A failed refetch keeps sizing from the last KNOWN read while that is
    /// still fresh enough, aged by how long it was kept; past that, today's
    /// read (and its reason) answers.
    #[test]
    fn a_failed_refetch_keeps_the_last_known_read_while_it_is_fresh() {
        let known = read(row(json!({"state": "known", "value": 3})), FRESH);
        let failed = read(None, 0.0);
        let kept = read_to_size_from(
            Some(failed.clone()),
            Some((&known, Duration::from_secs(30))),
        )
        .expect("a read");
        assert!(kept.known_row().is_some());
        assert_eq!(kept.cache_age_secs, FRESH + 30.0);
        let too_old = read_to_size_from(
            Some(failed.clone()),
            Some((&known, Duration::from_secs(170))),
        )
        .expect("a read");
        assert!(
            too_old.known_row().is_none(),
            "past MODEL_STALE_AFTER: today's read"
        );
        // A KNOWN current read is used as it is; nothing kept, the current one.
        let fresh = read(row(json!({"state": "known", "value": 1})), 1.0);
        assert_eq!(
            read_to_size_from(Some(fresh.clone()), Some((&known, Duration::ZERO))),
            Some(fresh)
        );
        assert_eq!(read_to_size_from(Some(failed.clone()), None), Some(failed));
        assert_eq!(read_to_size_from(None, None), None);
    }
}
