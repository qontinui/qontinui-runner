//! Golden fixtures for what the installed Claude Code CLI really sends to a
//! hook command and to a `statusLine` command.
//!
//! Plan `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phase 1. The fixtures under `tests/fixtures/claude-events/<cli-version>/`
//! were recorded by hand with the probe in
//! `resources/session-restore/probe/` against a throwaway `claude` session run
//! outside the runner; `PROBE.md` beside them answers the plan's five
//! questions for that CLI version.
//!
//! # What a fixture is — and is not
//!
//! Each `<event>.json` is a TYPE SKELETON, never a payload:
//!
//! ```json
//! { "cli_version": "2.1.285", "channel": "hook", "event": "Stop",
//!   "variants": [ { "session_id": "string", "background_tasks": [], ... } ] }
//! ```
//!
//! A skeleton leaf is one of `"string"`, `"number"`, `"boolean"`, `"null"`; an
//! object maps key -> skeleton; an array is the list of distinct element
//! skeletons (empty when every observed array was empty). `variants` holds
//! every distinct shape observed for that event, because the CLI omits keys
//! by context (`scratchpad_dir` in `-p` mode, `rate_limits` before the first
//! API response, `current_usage: null` before any usage exists). This test
//! enforces the no-values rule mechanically: a fixture containing any leaf
//! that is not a type name — a prompt, a path, a session id — fails.
//!
//! # Extension point for later phases
//!
//! Phase 3 (hook ingest) and Phase 6 (statusline metrics) project these
//! payloads into their own structs. They extend THIS file:
//! `synthesize` turns a skeleton variant into a concrete JSON value of the
//! right shape, so a later phase adds one test per projection that runs
//! `serde_json::from_value::<TheirProjection>(synthesize(variant))` over every
//! variant of the events it consumes — see
//! `claude_event_fixtures::every_variant_synthesizes_to_an_object`, the
//! placeholder that already walks them. When the CLI moves and a new
//! `<version>/` directory is recorded, those tests re-run against it with no
//! edit here.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Hook events the Phase 1 probe registered, plus the statusLine. Every
/// probed CLI version's directory must carry a fixture for each: an event the
/// probe could not observe is recorded as UNKNOWN in `PROBE.md` and still
/// needs an explicit decision, not a silent gap.
const REQUIRED_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
    "statusline",
];

/// The five questions `PROBE.md` must answer (an answer may be UNKNOWN with
/// its reason; it may not be missing).
const REQUIRED_QUESTIONS: &[&str] = &["## Q1", "## Q2", "## Q3", "## Q4", "## Q5"];

const LEAF_TYPES: &[&str] = &["string", "number", "boolean", "null"];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude-events")
}

/// Every `<cli-version>/` directory under the fixture root, sorted.
fn version_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(fixtures_root())
        .expect("fixture root tests/fixtures/claude-events must exist")
        .map(|entry| entry.expect("readable fixture dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Every `<event>.json` in one version directory, sorted.
fn fixture_files(version_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(version_dir)
        .expect("readable version dir")
        .map(|entry| entry.expect("readable fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .expect("utf-8 fixture file name")
        .to_owned()
}

/// A version directory's full name. Not `file_stem`: `2.1.285` would lose
/// its `.285` to the "extension".
fn dir_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("utf-8 version directory name")
        .to_owned()
}

fn load(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{}: unreadable: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{}: not valid JSON: {err}", path.display()))
}

/// Assert `skeleton` is a type skeleton: every leaf a type NAME, never a value.
fn assert_skeleton(skeleton: &Value, at: &str) {
    match skeleton {
        Value::String(leaf) => assert!(
            LEAF_TYPES.contains(&leaf.as_str()),
            "{at}: leaf {leaf:?} is not a type name — fixtures must never carry values"
        ),
        Value::Array(elements) => {
            for (i, element) in elements.iter().enumerate() {
                assert_skeleton(element, &format!("{at}[{i}]"));
            }
        }
        Value::Object(fields) => {
            for (key, child) in fields {
                assert_skeleton(child, &format!("{at}.{key}"));
            }
        }
        other => panic!("{at}: {other} is a literal value, not a type skeleton"),
    }
}

/// A concrete JSON value with the shape `skeleton` describes — the input a
/// later phase's projection struct is deserialized from.
fn synthesize(skeleton: &Value) -> Value {
    match skeleton {
        Value::String(leaf) => match leaf.as_str() {
            "string" => Value::String(String::new()),
            "number" => Value::from(0),
            "boolean" => Value::Bool(false),
            _ => Value::Null,
        },
        Value::Array(elements) => Value::Array(elements.iter().map(synthesize).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, child)| (key.clone(), synthesize(child)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}

/// Wrapped in a module so `cargo test claude_event_fixtures` selects exactly
/// these tests (the filter matches test PATHS, not the binary name).
mod claude_event_fixtures {
    use super::*;

    #[test]
    fn at_least_one_probed_cli_version_is_recorded() {
        assert!(
            !version_dirs().is_empty(),
            "no <cli-version>/ directory under {}",
            fixtures_root().display()
        );
    }

    #[test]
    fn every_fixture_is_a_values_free_skeleton_of_its_event() {
        for dir in version_dirs() {
            let version = dir_name(&dir);
            let files = fixture_files(&dir);
            assert!(!files.is_empty(), "{}: no fixtures", dir.display());
            for path in files {
                let at = path.display().to_string();
                let doc = load(&path);
                let obj = doc
                    .as_object()
                    .unwrap_or_else(|| panic!("{at}: top level must be an object"));
                let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
                assert_eq!(
                    keys,
                    BTreeSet::from(["channel", "cli_version", "event", "variants"]),
                    "{at}: unexpected top-level keys"
                );
                assert_eq!(obj["cli_version"], Value::from(version.as_str()), "{at}: cli_version must match its directory");
                let event = file_stem(&path);
                assert_eq!(obj["event"], Value::from(event.as_str()), "{at}: event must match its file name");
                let channel = obj["channel"].as_str().unwrap_or_default();
                let expected_channel = if event == "statusline" { "statusline" } else { "hook" };
                assert_eq!(channel, expected_channel, "{at}: channel");

                let variants = obj["variants"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{at}: variants must be an array"));
                assert!(!variants.is_empty(), "{at}: no variants");
                for (i, variant) in variants.iter().enumerate() {
                    let vat = format!("{at} variants[{i}]");
                    let fields = variant
                        .as_object()
                        .unwrap_or_else(|| panic!("{vat}: a variant must be a keys->type map"));
                    assert_skeleton(variant, &vat);
                    assert_eq!(fields.get("session_id"), Some(&Value::from("string")), "{vat}: session_id");
                    if channel == "hook" {
                        assert_eq!(
                            fields.get("hook_event_name"),
                            Some(&Value::from("string")),
                            "{vat}: a hook payload names its event"
                        );
                    } else {
                        assert_eq!(fields.get("version"), Some(&Value::from("string")), "{vat}: statusline version");
                    }
                }
            }
        }
    }

    #[test]
    fn every_probed_version_covers_the_probed_events_and_answers_all_five_questions() {
        for dir in version_dirs() {
            let present: BTreeSet<String> = fixture_files(&dir).iter().map(|p| file_stem(p)).collect();
            for event in REQUIRED_EVENTS {
                assert!(present.contains(*event), "{}: no fixture for {event}", dir.display());
            }
            let probe_md = dir.join("PROBE.md");
            let text = fs::read_to_string(&probe_md)
                .unwrap_or_else(|err| panic!("{}: {err}", probe_md.display()));
            for question in REQUIRED_QUESTIONS {
                assert!(text.contains(question), "{}: missing section {question:?}", probe_md.display());
            }
        }
    }

    /// Placeholder the projection phases extend (see the module docs): every
    /// variant synthesizes to an object that round-trips through serde_json.
    /// Phase 3 adds `from_value::<AgentEventProjection>` over the hook events
    /// it ingests; Phase 6 adds `from_value::<StatuslineMetricsProjection>`
    /// over `statusline` variants.
    #[test]
    fn every_variant_synthesizes_to_an_object() {
        for dir in version_dirs() {
            for path in fixture_files(&dir) {
                let doc = load(&path);
                for variant in doc["variants"].as_array().into_iter().flatten() {
                    let payload = synthesize(variant);
                    assert!(payload.is_object(), "{}: synthesized payload is not an object", path.display());
                    let text = serde_json::to_string(&payload).expect("serializable");
                    let back: Value = serde_json::from_str(&text).expect("round-trips");
                    assert_eq!(back, payload);
                }
            }
        }
    }

    /// Phase 3: every recorded hook payload shape projects through the SAME
    /// allowlist projection the `POST /terminals/agent-event` route runs, to
    /// the event it names — and the projection reads nothing outside
    /// `PROJECTED_FIELDS`, so a synthesized payload carrying only foreign keys
    /// still projects, and one whose event is not ingested is refused.
    #[test]
    fn every_hook_variant_projects_through_the_agent_event_allowlist() {
        use qontinui_runner_lib::agent_event::{project, INGESTED_EVENTS, PROJECTED_FIELDS};
        for dir in version_dirs() {
            for path in fixture_files(&dir) {
                let event = file_stem(&path);
                if event == "statusline" {
                    continue;
                }
                let doc = load(&path);
                for variant in doc["variants"].as_array().into_iter().flatten() {
                    let mut payload = synthesize(variant);
                    // `synthesize` leaves strings empty; the event name is the
                    // one value a projection cannot do without.
                    payload["hook_event_name"] = Value::from(event.as_str());
                    let projected = project(&payload).unwrap_or_else(|e| {
                        panic!("{}: {event} did not project: {e:?}", path.display())
                    });
                    assert_eq!(projected.hook_event_name, event);
                    assert!(INGESTED_EVENTS.contains(&event.as_str()));
                    // Every key the CLI sent for a projected field has the
                    // TYPE the projection reads (a string), or is absent.
                    for field in PROJECTED_FIELDS {
                        if let Some(ty) = variant.get(field) {
                            if field != "agent_id" {
                                assert_eq!(
                                    ty,
                                    &Value::from("string"),
                                    "{}: {event}.{field} is not a string",
                                    path.display()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// `KNOWN_FIXTURE_CLI_VERSIONS` (what `HookDelivery::VersionMismatch`
    /// compares against) is exactly the set of recorded fixture directories.
    #[test]
    fn known_fixture_versions_match_the_fixture_directories() {
        let dirs: Vec<String> = version_dirs().iter().map(|d| dir_name(d)).collect();
        let mut known: Vec<String> = qontinui_runner_lib::agent_event::KNOWN_FIXTURE_CLI_VERSIONS
            .iter()
            .map(|v| v.to_string())
            .collect();
        known.sort();
        assert_eq!(dirs, known);
    }

    #[test]
    fn the_skeleton_checker_rejects_a_value() {
        let leaked = serde_json::json!({ "session_id": "3f1c-…", "cwd": "string" });
        let caught = std::panic::catch_unwind(|| assert_skeleton(&leaked, "leaked"));
        assert!(caught.is_err(), "a literal session id must fail the no-values rule");
        let number = serde_json::json!({ "used_percentage": 42 });
        assert!(std::panic::catch_unwind(|| assert_skeleton(&number, "number")).is_err());
    }
}
