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
//! ## The mechanism id is the doctor's
//!
//! `mechanism` is published as the doctor's CANONICAL id — the record's own
//! `mechanism` field, else its file stem, mapped through the doctor's alias
//! table ([`doctor_mechanism_id`], a transcription of `capability-doctor.sh`
//! `stem_to_mech`, in the same order the doctor tries them) — so
//! `render-memory-cache.json` and a record saying `memory-cache-renderer` both
//! publish `memory_cache_renderer`, the id the doctor's own report uses. A
//! record the doctor's table does not know publishes its own `mechanism` field
//! (else its stem) verbatim; the doctor calls such a record unregistered, and
//! so can a reader. `record` carries the file stem, so two records that bind to
//! one mechanism stay distinguishable.
//!
//! ## What leaves the box
//!
//! A record file larger than [`MAX_RECORD_BYTES`] is not parsed (UNKNOWN,
//! naming the size); every `reason` is cut at [`REASON_MAX_CHARS`] characters on
//! a character boundary; and errors name the record's FILE NAME, never a path
//! — this is published off-box, and the home directory is not coord's business.
//!
//! ## Present-and-empty vs absent, and the key triple
//!
//! An absent directory is `Looked(vec![])` — "looked, nothing recorded" — and
//! is published as `[]`. Only a directory that could not be resolved or read
//! is [`CapabilityRead::CouldNotLook`]. The publisher ALWAYS writes all three
//! keys — `capability`, `capability_error`, `capability_omitted` — with the
//! unused ones as JSON `null`: coord merges `details` per top-level key and a
//! `null` removes it, so this is what retires a previous pass's error or
//! overflow count instead of leaving it beside a fresh array.

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

/// Largest record file parsed. The doctor's records are a few hundred bytes;
/// a bigger file is not one of them, and is not read into a heartbeat.
pub(crate) const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// Longest `reason` published, in characters.
pub(crate) const REASON_MAX_CHARS: usize = 512;

/// `capability-doctor.sh` `stem_to_mech`, transcribed: the doctor's canonical
/// mechanism id for a hyphenated record name, or `None` for a name its table
/// does not register. Keep in step with that function (qontinui-claude-config
/// `scripts/capability-doctor.sh`); a missing arm publishes the record's own
/// name, which is honest but unregistered.
pub(crate) fn doctor_mechanism_id(name: &str) -> Option<&'static str> {
    Some(match name {
        "memory-cache-renderer" | "render-memory-cache" => "memory_cache_renderer",
        "plan-cache-renderer" | "render-plan-cache" => "plan_cache_renderer",
        "steering-cache-renderer" | "render-steering-cache" => "steering_cache_renderer",
        "landed-not-live-refresh" | "landed-not-live-staleness" | "refresh-landed-not-live" => {
            "landed_not_live_refresh"
        }
        "service-manager" | "dev-start" => "service_manager",
        "cargo-sweep" | "cargo-stale-target-sweep" | "install-cargo-sweep" => "cargo_sweep",
        "return-to-main-sweep" | "return-to-main" => "return_to_main_sweep",
        "findings-steward" | "schedule-findings-steward" => "findings_steward",
        "hooks-doctor" | "skills-doctor" => "hooks_doctor",
        "coord-doctor" | "coord-credential-doctor" => "coord_doctor",
        "coord-native-tools" | "native-coord-tools" => "coord_native_tools",
        "steward-serving-read" | "merge-train-steward-serving" => "steward_serving_read",
        "python-spawned-bash" | "bash-resolve" => "python_spawned_bash",
        "harness-links" | "workspace-root-links" | "bootstrap-machine" => "harness_links",
        "install-guard-hooks" | "installer-guard-hooks" => "installer_guard_hooks",
        "install-claude-settings" | "installer-claude-settings" => "installer_claude_settings",
        "install-claude-accounts" | "installer-claude-accounts" => "installer_claude_accounts",
        "install-repo-git-hooks" | "installer-repo-git-hooks" => "installer_repo_git_hooks",
        "install-agent-skills" | "installer-agent-skills" => "installer_agent_skills",
        "install-pwsh-linux" | "installer-pwsh-linux" => "installer_pwsh_linux",
        "plans-dir-setting" | "plans-dir" | "paths-plans-dir" => "plans_dir_setting",
        "plan-corpus-invariant" | "plan-corpus" => "plan_corpus_invariant",
        "shared-node-modules" | "node-modules-doctor" => "shared_node_modules",
        _ => return None,
    })
}

/// The doctor's binding order: the record's `mechanism` field (underscores
/// read as hyphens), else the file stem; the first the alias table knows wins.
/// Neither known: the field verbatim, else the stem.
fn canonical_mechanism(field: Option<&str>, stem: &str) -> String {
    field
        .and_then(|f| doctor_mechanism_id(&f.replace('_', "-")))
        .or_else(|| doctor_mechanism_id(stem))
        .map(str::to_string)
        .unwrap_or_else(|| field.unwrap_or(stem).to_string())
}

/// `s` cut to [`REASON_MAX_CHARS`] characters (so always on a char
/// boundary), with `…` marking a cut.
fn bounded_reason(s: String) -> String {
    if s.chars().nth(REASON_MAX_CHARS).is_none() {
        return s;
    }
    let mut cut: String = s.chars().take(REASON_MAX_CHARS).collect();
    cut.push('…');
    cut
}

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

/// One published record: `{mechanism, record, state, reason, written_at}`.
///
/// `mechanism` is the doctor's canonical id (see the module docs); `record`
/// is the file stem it was read from. `written_at` is the record's own stamp
/// normalised to `%Y-%m-%dT%H:%M:%SZ`, or `null` when the record carries none
/// that parses (the raw value is then quoted in `reason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CapabilityEntry {
    pub mechanism: String,
    pub record: String,
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
            return CapabilityRead::CouldNotLook(format!(
                "listing the capability directory failed: {e}"
            ))
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in listing {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return CapabilityRead::CouldNotLook(format!(
                    "listing the capability directory failed mid-walk: {e}"
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
            capability_entry(&stem, read_record(p, &stem), now)
        })
        .collect();
    CapabilityRead::Looked { entries, omitted }
}

/// Read one record file, bounded by [`MAX_RECORD_BYTES`]. Errors name the
/// file, never its directory.
fn read_record(path: &Path, stem: &str) -> Result<serde_json::Value, String> {
    use std::io::Read;
    let name = format!("{stem}.json");
    let file =
        std::fs::File::open(path).map_err(|e| format!("record {name} is unreadable: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("record {name} is unreadable: {e}"))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(format!(
            "record {name} is larger than {} KiB, so it was not parsed",
            MAX_RECORD_BYTES / 1024
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("record {name} is not JSON: {e}"))
}

/// Map one record (or the error reading it) to its published entry, with the
/// reason bounded to [`REASON_MAX_CHARS`].
pub(crate) fn capability_entry(
    stem: &str,
    parsed: Result<serde_json::Value, String>,
    now: DateTime<Utc>,
) -> CapabilityEntry {
    let mut entry = capability_entry_unbounded(stem, parsed, now);
    entry.reason = bounded_reason(entry.reason);
    entry
}

/// Map one record (or the error reading it) to its published entry.
///
/// Pure: the clock is threaded in, so the ageing boundary is testable.
fn capability_entry_unbounded(
    stem: &str,
    parsed: Result<serde_json::Value, String>,
    now: DateTime<Utc>,
) -> CapabilityEntry {
    let unknown = |mechanism: String, reason: String, written_at: Option<String>| CapabilityEntry {
        mechanism,
        record: stem.to_string(),
        state: CapabilityState::Unknown,
        reason,
        written_at,
    };
    let record = match parsed {
        Ok(v) => v,
        Err(e) => return unknown(canonical_mechanism(None, stem), e, None),
    };
    let Some(obj) = record.as_object() else {
        return unknown(
            canonical_mechanism(None, stem),
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
    let mechanism = canonical_mechanism(text("mechanism"), stem);
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
            record: stem.to_string(),
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

/// Write this module's three keys into the heartbeat `details` object, ALWAYS
/// all three (see the module docs):
///
/// - looked: `capability` = the array, `capability_error` = `null`,
///   `capability_omitted` = the overflow count, or `null` when nothing was cut;
/// - could not look (or could not serialise what it read): `capability` =
///   `null`, `capability_error` = why, `capability_omitted` = `null`.
pub(crate) fn publish_into(
    details: &mut serde_json::Map<String, serde_json::Value>,
    read: CapabilityRead,
) {
    use serde_json::Value;
    let (array, error, omitted) = match read {
        CapabilityRead::Looked { entries, omitted } => match serde_json::to_value(entries) {
            Ok(v) => (
                v,
                Value::Null,
                if omitted > 0 {
                    serde_json::json!(omitted)
                } else {
                    Value::Null
                },
            ),
            Err(e) => (
                Value::Null,
                Value::String(format!("serialising the capability records failed: {e}")),
                Value::Null,
            ),
        },
        CapabilityRead::CouldNotLook(why) => (Value::Null, Value::String(why), Value::Null),
    };
    details.insert("capability".to_string(), array);
    details.insert("capability_error".to_string(), error);
    details.insert("capability_omitted".to_string(), omitted);
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
        assert_eq!(
            e.mechanism, "memory_cache_renderer",
            "the doctor's canonical id"
        );
        assert_eq!(e.record, "render-memory-cache");
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
        assert_eq!(
            e.mechanism, "plan_cache_renderer",
            "bound by stem via the alias table"
        );
        assert_eq!(e.record, "render-plan-cache");
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
        assert!(
            !entries[0].reason.contains(&dir.display().to_string()),
            "a file name, never a path: {}",
            entries[0].reason
        );
        assert!(entries[0].reason.contains("a.json"));
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
    fn unregistered_records_keep_their_own_name_and_the_table_matches_the_doctor() {
        let now = at("2026-09-30T12:00:00Z");
        let rec = json!({"mechanism": "coord_inbox_drain", "state": "OPERATIVE",
            "reason": "drained", "written_at": "2026-09-30T11:00:00Z"});
        let e = capability_entry("coord-inbox-drain", Ok(rec), now);
        assert_eq!(e.mechanism, "coord_inbox_drain");
        assert_eq!(e.record, "coord-inbox-drain");
        // Underscores in the field read as hyphens, as the doctor reads them.
        assert_eq!(
            canonical_mechanism(Some("landed_not_live_staleness"), "x"),
            "landed_not_live_refresh"
        );
        // The field wins over the stem when both are registered.
        assert_eq!(
            canonical_mechanism(Some("cargo-sweep"), "render-plan-cache"),
            "cargo_sweep"
        );
        assert_eq!(doctor_mechanism_id("fleet-skill-bundle-parity"), None);
    }

    #[test]
    fn oversize_records_are_unknown_and_reasons_are_bounded_on_a_char_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.json");
        std::fs::write(&big, vec![b' '; (MAX_RECORD_BYTES + 1) as usize]).unwrap();
        let err = read_record(&big, "big").unwrap_err();
        assert!(err.contains("larger than 64 KiB"), "{err}");
        assert!(!err.contains(&tmp.path().display().to_string()));

        let now = at("2026-09-30T12:00:00Z");
        let long = "é".repeat(REASON_MAX_CHARS + 50);
        let rec = json!({"mechanism": "m", "state": "DEGRADED", "reason": long,
            "written_at": "2026-09-30T11:00:00Z"});
        let e = capability_entry("m", Ok(rec), now);
        assert_eq!(e.reason.chars().count(), REASON_MAX_CHARS + 1);
        assert!(e.reason.ends_with('…'));
    }

    #[test]
    fn details_always_carry_all_three_keys_with_the_unused_ones_null() {
        let entry = CapabilityEntry {
            mechanism: "m".into(),
            record: "m-file".into(),
            state: CapabilityState::InoperativeOnThisMachine,
            reason: "r".into(),
            written_at: None,
        };
        let mut d = serde_json::Map::new();
        publish_into(
            &mut d,
            CapabilityRead::Looked {
                entries: vec![entry.clone()],
                omitted: 0,
            },
        );
        assert_eq!(
            serde_json::Value::Object(d),
            json!({"capability": [{"mechanism": "m", "record": "m-file",
                "state": "INOPERATIVE-ON-THIS-MACHINE", "reason": "r", "written_at": null}],
                "capability_error": null, "capability_omitted": null})
        );
        let mut d = serde_json::Map::new();
        publish_into(
            &mut d,
            CapabilityRead::Looked {
                entries: vec![],
                omitted: 3,
            },
        );
        assert_eq!(
            serde_json::Value::Object(d),
            json!({"capability": [], "capability_error": null, "capability_omitted": 3})
        );
        let mut d = serde_json::Map::new();
        publish_into(&mut d, CapabilityRead::CouldNotLook("no home".into()));
        assert_eq!(
            serde_json::Value::Object(d),
            json!({"capability": null, "capability_error": "no home",
                "capability_omitted": null})
        );
    }
}
