//! G4 — the capability verdicts this box's hooks and doctor already record,
//! carried to coord (plan
//! `2026-09-20-the-second-ratchet-domain-is-operations-and-its-cost-is-compared-to-the-first`,
//! Phase 5).
//!
//! `capability-doctor.sh` computes OPERATIVE / DEGRADED /
//! INOPERATIVE-ON-THIS-MACHINE / UNKNOWN per mechanism and reads the state
//! records under `~/.qontinui/capability/*.json` — but every `curl` in it is a
//! read, so a box whose hooks have been silently dead for weeks (the
//! 2026-09-02 shape) is invisible to every other box. This module reads the
//! SAME records and publishes them as `details.capability` on the
//! device-status heartbeat.
//!
//! ## One directory resolver
//!
//! [`capability_state_dir_from`] is the single answer to "where do capability
//! records live". The runner's own writer (the fleet-skill-bundle parity
//! record, `fleet.rs`) and this reader both go through it, so the writer and
//! the reader cannot diverge — the exact divergence `capability-doctor.sh`'s
//! header records as having survived undetected until 2026-09-04.
//!
//! ## The ageing rule is the doctor's, not a second one
//!
//! A record older than [`CAPABILITY_STALE_SECS`] (the doctor's
//! `STALE_SECONDS=86400`), or one whose `written_at` is missing, unparseable
//! or in the future, is published as `UNKNOWN` with a reason naming its age
//! and what it last said — never as its stale state. A file that cannot be read
//! or parsed is `UNKNOWN` with the error. A state word outside the doctor's
//! vocabulary is `UNKNOWN` too: a reader must not have to guess what `OK`
//! meant.
//!
//! ## Present-and-empty vs absent
//!
//! An absent directory is `Looked(vec![])` — "looked, nothing recorded" — and
//! is published as `[]`. Only a directory that could not be resolved or read
//! is [`CapabilityRead::CouldNotLook`], which the publisher turns into an
//! omitted `capability` key plus a `capability_error` string, because coord
//! reads key-absence as "this build predates the key".

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

/// The doctor's own staleness bound (`capability-doctor.sh` `STALE_SECONDS`).
/// A record older than this is never taken as current.
pub(crate) const CAPABILITY_STALE_SECS: i64 = 86_400;

/// Most records published in one heartbeat. The doctor's registry is ~24
/// mechanisms; the bound keeps a directory someone filled with junk from
/// inflating every heartbeat. Overflow is COUNTED (`capability_omitted`),
/// never silently dropped.
pub(crate) const CAPABILITY_REPORT_BOUND: usize = 64;

/// The doctor's state vocabulary, spelled exactly as it prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum CapabilityState {
    #[serde(rename = "OPERATIVE")]
    Operative,
    #[serde(rename = "DEGRADED")]
    Degraded,
    #[serde(rename = "INOPERATIVE-ON-THIS-MACHINE")]
    InoperativeOnThisMachine,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl CapabilityState {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "OPERATIVE" => Some(Self::Operative),
            "DEGRADED" => Some(Self::Degraded),
            "INOPERATIVE-ON-THIS-MACHINE" => Some(Self::InoperativeOnThisMachine),
            "UNKNOWN" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// One published record: `{mechanism, state, reason, written_at}`.
///
/// `written_at` is the record's own stamp normalised to
/// `%Y-%m-%dT%H:%M:%SZ`, or `null` when the record carries none that parses
/// (the raw value is then quoted in `reason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CapabilityEntry {
    pub mechanism: String,
    pub state: CapabilityState,
    pub reason: String,
    pub written_at: Option<String>,
}

/// The outcome of one pass over the capability directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CapabilityRead {
    /// The directory was examined. `entries` is empty when it is absent or
    /// holds no records; `omitted` counts records beyond
    /// [`CAPABILITY_REPORT_BOUND`].
    Looked {
        entries: Vec<CapabilityEntry>,
        omitted: usize,
    },
    /// The directory could not be resolved or listed; the string says why.
    CouldNotLook(String),
}

/// Where capability records live: `$QONTINUI_CAPABILITY_STATE_DIR` first,
/// because that is the variable `capability-doctor.sh` READS, else
/// `~/.qontinui/capability`.
pub(crate) fn capability_state_dir() -> Option<PathBuf> {
    capability_state_dir_from(
        std::env::var("QONTINUI_CAPABILITY_STATE_DIR")
            .ok()
            .as_deref(),
        qontinui_runner_lib::ambient::qontinui_dir().as_deref(),
    )
}

/// Pure core of [`capability_state_dir`], so the precedence and the
/// blank-value branch are testable without mutating the process environment.
/// A blank override is not an override (a systemd `Environment=FOO=`).
pub(crate) fn capability_state_dir_from(
    state_dir: Option<&str>,
    qontinui_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(dir) = state_dir {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    qontinui_dir.map(|d| d.join("capability"))
}

/// Read every record in the resolved capability directory. Blocking IO.
pub(crate) fn read_capability_records(now: DateTime<Utc>) -> CapabilityRead {
    match capability_state_dir() {
        Some(dir) => read_capability_dir(&dir, now),
        None => CapabilityRead::CouldNotLook(
            "no capability directory resolves: $QONTINUI_CAPABILITY_STATE_DIR is unset and \
             there is no home directory for ~/.qontinui/capability"
                .to_string(),
        ),
    }
}

/// [`read_capability_records`] over an explicit directory.
///
/// Only `*.json` files count, and dotfiles are skipped: the hooks write via a
/// hidden `.<name>.json.<pid>` temp file and rename it, and the doctor's own
/// glob (`"$STATE_DIR"/*.json`) does not see those either.
pub(crate) fn read_capability_dir(dir: &Path, now: DateTime<Utc>) -> CapabilityRead {
    let listing = match std::fs::read_dir(dir) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return CapabilityRead::Looked {
                entries: Vec::new(),
                omitted: 0,
            }
        }
        Err(e) => {
            return CapabilityRead::CouldNotLook(format!("listing {} failed: {e}", dir.display()))
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in listing {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return CapabilityRead::CouldNotLook(format!(
                    "listing {} failed mid-walk: {e}",
                    dir.display()
                ))
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        // `metadata` follows a symlink, so a linked record counts as the file
        // it points at, exactly as the doctor's `[ -f "$f" ]` does.
        let path = entry.path();
        if std::fs::metadata(&path)
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    files.sort();
    let omitted = files.len().saturating_sub(CAPABILITY_REPORT_BOUND);
    let entries = files
        .iter()
        .take(CAPABILITY_REPORT_BOUND)
        .map(|p| {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read(p)
                .map_err(|e| format!("record {} is unreadable: {e}", p.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .map_err(|e| format!("record {} is not JSON: {e}", p.display()))
                });
            capability_entry(&stem, parsed, now)
        })
        .collect();
    CapabilityRead::Looked { entries, omitted }
}

/// Map one record (or the error reading it) to its published entry.
///
/// Pure: the clock is threaded in, so the ageing boundary is testable.
pub(crate) fn capability_entry(
    stem: &str,
    parsed: Result<serde_json::Value, String>,
    now: DateTime<Utc>,
) -> CapabilityEntry {
    let unknown = |mechanism: String, reason: String, written_at: Option<String>| CapabilityEntry {
        mechanism,
        state: CapabilityState::Unknown,
        reason,
        written_at,
    };
    let record = match parsed {
        Ok(v) => v,
        Err(e) => return unknown(stem.to_string(), e, None),
    };
    let Some(obj) = record.as_object() else {
        return unknown(
            stem.to_string(),
            "record is JSON but not an object".to_string(),
            None,
        );
    };
    let text = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    // The doctor binds a record by its `mechanism` field, failing that by its
    // file stem; the published id follows the same order.
    let mechanism = text("mechanism").unwrap_or(stem).to_string();
    let said_reason = text("reason").unwrap_or("(the record gives no reason)");
    let raw_state = text("state");

    let written = text("written_at");
    let parsed_at = written.and_then(|w| DateTime::parse_from_rfc3339(w).ok());
    let Some(at) = parsed_at.map(|d| d.with_timezone(&Utc)) else {
        return unknown(
            mechanism,
            format!(
                "written_at '{}' is missing or unparseable, so the record is not current \
                 (capability-doctor.sh STALE rule) — it last said {}: {said_reason}",
                written.unwrap_or(""),
                raw_state.unwrap_or("?"),
            ),
            None,
        );
    };
    let written_at = Some(at.to_rfc3339_opts(SecondsFormat::Secs, true));
    let age = (now - at).num_seconds();
    if age < 0 {
        return unknown(
            mechanism,
            format!(
                "written_at is {}s in the future, so the record is not current \
                 (capability-doctor.sh STALE rule) — it last said {}: {said_reason}",
                -age,
                raw_state.unwrap_or("?"),
            ),
            written_at,
        );
    }
    if age > CAPABILITY_STALE_SECS {
        return unknown(
            mechanism,
            format!(
                "record is STALE, {} h old (older than capability-doctor.sh's 24 h \
                 STALE_SECONDS) — it last said {}: {said_reason}",
                age / 3600,
                raw_state.unwrap_or("?"),
            ),
            written_at,
        );
    }
    match raw_state.and_then(CapabilityState::parse) {
        Some(state) => CapabilityEntry {
            mechanism,
            state,
            reason: said_reason.to_string(),
            written_at,
        },
        None => unknown(
            mechanism,
            format!(
                "record state '{}' is not in the doctor's vocabulary \
                 (OPERATIVE | DEGRADED | INOPERATIVE-ON-THIS-MACHINE | UNKNOWN) — \
                 it said: {said_reason}",
                raw_state.unwrap_or(""),
            ),
            written_at,
        ),
    }
}

/// Write this module's keys into the heartbeat `details` object.
///
/// `capability` (always an array when the directory was examined) and
/// `capability_omitted` (only when the bound cut records off); or, when it
/// could not look, `capability_error` alone.
pub(crate) fn publish_into(
    details: &mut serde_json::Map<String, serde_json::Value>,
    read: CapabilityRead,
) {
    match read {
        CapabilityRead::Looked { entries, omitted } => {
            details.insert(
                "capability".to_string(),
                serde_json::to_value(entries).unwrap_or_else(|_| serde_json::json!([])),
            );
            if omitted > 0 {
                details.insert("capability_omitted".to_string(), serde_json::json!(omitted));
            }
        }
        CapabilityRead::CouldNotLook(why) => {
            details.insert("capability_error".to_string(), serde_json::json!(why));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_fresh_record_publishes_its_own_state_and_reason() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"mechanism": "memory-cache-renderer", "state": "OPERATIVE",
            "reason": "render spawned detached", "written_at": "2026-09-30T11:00:00Z"});
        let e = capability_entry("render-memory-cache", Ok(rec), now);
        assert_eq!(e.mechanism, "memory-cache-renderer");
        assert_eq!(e.state, CapabilityState::Operative);
        assert_eq!(e.reason, "render spawned detached");
        assert_eq!(e.written_at.as_deref(), Some("2026-09-30T11:00:00Z"));
    }

    #[test]
    fn a_record_older_than_the_doctors_24h_rule_is_unknown_naming_its_age() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"mechanism": "cargo-sweep", "state": "DEGRADED",
            "reason": "size arm stood down", "written_at": "2026-09-28T10:00:00Z"});
        let e = capability_entry("cargo-sweep", Ok(rec), now);
        assert_eq!(e.state, CapabilityState::Unknown);
        assert!(e.reason.contains("STALE, 50 h old"), "{}", e.reason);
        assert!(
            e.reason.contains("DEGRADED: size arm stood down"),
            "{}",
            e.reason
        );
        // The stamp is still published so coord can age it itself.
        assert_eq!(e.written_at.as_deref(), Some("2026-09-28T10:00:00Z"));
    }

    #[test]
    fn exactly_24h_is_still_current_and_one_second_more_is_not() {
        let now = at("2026-09-30T12:00:00Z");
        let mk = |w: &str| json!({"mechanism": "m", "state": "OPERATIVE", "reason": "r", "written_at": w});
        assert_eq!(
            capability_entry("m", Ok(mk("2026-09-29T12:00:00Z")), now).state,
            CapabilityState::Operative
        );
        assert_eq!(
            capability_entry("m", Ok(mk("2026-09-29T11:59:59Z")), now).state,
            CapabilityState::Unknown
        );
    }

    #[test]
    fn offsets_and_fractional_seconds_parse_like_any_rfc3339_stamp() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"mechanism": "m", "state": "DEGRADED", "reason": "r",
            "written_at": "2026-09-30T13:30:00.123456+02:00"});
        let e = capability_entry("m", Ok(rec), now);
        assert_eq!(e.state, CapabilityState::Degraded);
        assert_eq!(e.written_at.as_deref(), Some("2026-09-30T11:30:00Z"));
    }

    #[test]
    fn missing_future_or_garbled_written_at_is_unknown() {
        let now = at("2026-09-30T12:00:00Z");
        for w in [
            json!(null),
            json!("yesterday"),
            json!("2026-09-30T13:00:00Z"),
        ] {
            let rec = json!({"mechanism": "m", "state": "OPERATIVE", "reason": "r",
                "written_at": w});
            let e = capability_entry("m", Ok(rec), now);
            assert_eq!(e.state, CapabilityState::Unknown, "written_at {w}");
        }
    }

    #[test]
    fn unreadable_unparseable_or_nonobject_records_are_unknown_under_their_stem() {
        let now = at("2026-09-30T12:00:00Z");
        let e = capability_entry("hook-x", Err("record x is not JSON: eof".into()), now);
        assert_eq!(e.mechanism, "hook-x");
        assert_eq!(e.state, CapabilityState::Unknown);
        assert!(e.reason.contains("not JSON"));
        assert_eq!(e.written_at, None);
        let e = capability_entry("hook-y", Ok(json!([1, 2])), now);
        assert_eq!(e.state, CapabilityState::Unknown);
        assert_eq!(e.mechanism, "hook-y");
    }

    #[test]
    fn a_state_word_outside_the_vocabulary_is_unknown_not_guessed() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"mechanism": "m", "state": "OK", "reason": "fine",
            "written_at": "2026-09-30T11:00:00Z"});
        let e = capability_entry("m", Ok(rec), now);
        assert_eq!(e.state, CapabilityState::Unknown);
        assert!(e.reason.contains("'OK'"));
    }

    #[test]
    fn a_record_with_no_mechanism_binds_to_its_file_stem() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"state": "INOPERATIVE-ON-THIS-MACHINE", "reason": "no pwsh",
            "written_at": "2026-09-30T11:00:00Z"});
        let e = capability_entry("render-plan-cache", Ok(rec), now);
        assert_eq!(e.mechanism, "render-plan-cache");
        assert_eq!(e.state, CapabilityState::InoperativeOnThisMachine);
    }

    #[test]
    fn the_directory_walk_reads_json_skips_temps_and_absent_is_empty() {
        let now = Utc::now();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("capability");
        assert_eq!(
            read_capability_dir(&dir, now),
            CapabilityRead::Looked {
                entries: vec![],
                omitted: 0
            },
            "an absent directory is looked-and-empty, never could-not-look"
        );
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        std::fs::write(
            dir.join("b.json"),
            json!({"mechanism": "b", "state": "OPERATIVE", "reason": "ok", "written_at": stamp})
                .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("a.json"), b"{not json").unwrap();
        std::fs::write(dir.join(".a.json.123"), b"").unwrap();
        std::fs::write(dir.join(".hidden.json"), b"{}").unwrap();
        std::fs::write(dir.join("cargo-sweep-finding.stamp"), b"x").unwrap();
        let CapabilityRead::Looked { entries, omitted } = read_capability_dir(&dir, now) else {
            panic!("expected Looked");
        };
        assert_eq!(omitted, 0);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mechanism, "a");
        assert_eq!(entries[0].state, CapabilityState::Unknown);
        assert_eq!(entries[1].mechanism, "b");
        assert_eq!(entries[1].state, CapabilityState::Operative);
    }

    #[test]
    fn the_bound_is_64_and_overflow_is_counted() {
        let now = Utc::now();
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..70 {
            std::fs::write(tmp.path().join(format!("m{i:03}.json")), b"{}").unwrap();
        }
        let CapabilityRead::Looked { entries, omitted } = read_capability_dir(tmp.path(), now)
        else {
            panic!("expected Looked");
        };
        assert_eq!(entries.len(), CAPABILITY_REPORT_BOUND);
        assert_eq!(omitted, 6);
    }

    #[test]
    fn a_path_that_is_a_file_is_could_not_look() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("capability");
        std::fs::write(&file, b"").unwrap();
        assert!(matches!(
            read_capability_dir(&file, Utc::now()),
            CapabilityRead::CouldNotLook(_)
        ));
    }

    #[test]
    fn the_state_dir_override_wins_and_a_blank_one_does_not() {
        let q = Path::new("/home/u/.qontinui");
        assert_eq!(
            capability_state_dir_from(Some("/state"), Some(q)),
            Some(PathBuf::from("/state"))
        );
        assert_eq!(
            capability_state_dir_from(Some("  "), Some(q)),
            Some(q.join("capability"))
        );
        assert_eq!(capability_state_dir_from(None, None), None);
    }

    #[test]
    fn details_shape_is_an_array_or_an_error_string_never_both() {
        let mut d = serde_json::Map::new();
        publish_into(
            &mut d,
            CapabilityRead::Looked {
                entries: vec![CapabilityEntry {
                    mechanism: "m".into(),
                    state: CapabilityState::InoperativeOnThisMachine,
                    reason: "r".into(),
                    written_at: None,
                }],
                omitted: 0,
            },
        );
        assert_eq!(
            serde_json::Value::Object(d),
            json!({"capability": [{"mechanism": "m",
                "state": "INOPERATIVE-ON-THIS-MACHINE", "reason": "r", "written_at": null}]})
        );
        let mut d = serde_json::Map::new();
        publish_into(
            &mut d,
            CapabilityRead::Looked {
                entries: vec![],
                omitted: 0,
            },
        );
        assert_eq!(serde_json::Value::Object(d), json!({"capability": []}));
        let mut d = serde_json::Map::new();
        publish_into(&mut d, CapabilityRead::CouldNotLook("no home".into()));
        assert_eq!(
            serde_json::Value::Object(d),
            json!({"capability_error": "no home"})
        );
    }
}
