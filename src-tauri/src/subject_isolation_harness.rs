//! The harness/subject isolation test — plan
//! `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`
//! Phase 3 (D4's integration half; the source-scan half is
//! `ambient::tests::no_machine_global_dir_outside_the_seams`).
//!
//! # What it proves
//!
//! A HARNESS runner (no instance root) and a SUBJECT runner (launched under
//! `QONTINUI_INSTANCE_ROOT`) share one host. The subject is driven through the
//! runner's real writers — pairing (and the binding-store converge that once
//! absorbed and renamed the harness's `paired_user.json`), settings save, the
//! prompt and context libraries, the Claude account roster, a session name, a
//! dev-log write, the loopback key and the embedded-PG data root — and every
//! file and directory the harness owns must be **byte-identical** afterwards.
//! The subject's own writes must all land inside its root, and the children it
//! spawns must see its root (D5).
//!
//! # How
//!
//! Each runner is a CHILD PROCESS: this test binary re-executed on the ignored
//! [`driver`] test. A process of its own is what makes the subject's startup
//! contract (`apply_instance_root_env`, which mutates the process env) and the
//! runner's process-global caches (`paths::get_dev_logs_dir`) behave as they do
//! in a real launch, and it keeps both runs off the parent's env.
//!
//! The "machine" is a fixture: `HOME` and the XDG base dirs point into it, so
//! `dirs::*` — every platform directory a harness resolves — lands there.
//!
//! # The OS split, stated rather than implied
//!
//! On **unix** that redirection is complete, so the test drives both runners
//! for real and compares the harness tree byte for byte, and a planted call
//! site that writes outside the root fails it
//! ([`the_byte_identical_check_catches_a_planted_leak`]).
//!
//! On **Windows** `dirs` resolves through the known-folder API, which no
//! environment variable redirects. A real drive there would, on a regression,
//! write into the CI node's LIVE runner stores — the exact defect class this
//! plan closes (and the one qontinui-runner#1769 found test binaries doing). So
//! Windows runs the subject in RESOLVE-ONLY mode: every store the drive would
//! write is resolved through the runner's own resolver and must lie inside the
//! root, and nothing is written outside the fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `harness` | `subject` — which runner the [`driver`] child plays.
const MODE: &str = "SUBJECT_ISOLATION_MODE";
/// The fixture "machine" home the child's platform dirs resolve under.
const MACHINE_HOME: &str = "SUBJECT_ISOLATION_MACHINE_HOME";
/// The subject's instance root.
const ROOT: &str = "SUBJECT_ISOLATION_ROOT";
/// Where the child writes its report (one written path per line).
const REPORT: &str = "SUBJECT_ISOLATION_REPORT";
/// `1`: also write through a planted `dirs::config_dir()` call — a call site
/// the seam missed.
const PLANT: &str = "SUBJECT_ISOLATION_PLANT";
/// `1`: resolve every store, write nothing (Windows).
const RESOLVE_ONLY: &str = "SUBJECT_ISOLATION_RESOLVE_ONLY";

/// The subject's port: anything but the primary's.
const SUBJECT_PORT: &str = "9881";
const PLANTED_FILE: &str = "planted-by-a-missed-call-site.txt";

/// The child: one runner, driven through the real writers. Ignored, so a plain
/// `cargo test` never runs it on its own; [`run_driver`] runs it with
/// `--ignored --exact`.
#[test]
#[ignore = "the child process of the subject-isolation tests; run by them"]
fn driver() {
    let Ok(mode) = std::env::var(MODE) else {
        // Reached by `cargo test -- --include-ignored`: nothing to drive.
        eprintln!("subject_isolation_harness::driver: no {MODE}; not a child run, skipping");
        return;
    };
    let report = PathBuf::from(std::env::var(REPORT).expect("SUBJECT_ISOLATION_REPORT"));
    let machine_home = PathBuf::from(std::env::var(MACHINE_HOME).expect("MACHINE_HOME"));

    // The fixture takes the env lock and arms nothing it does not restore;
    // then the machine it points at is replaced by OUR fixture machine, and
    // every runner override is cleared so the run resolves like a launch.
    let _amb = crate::test_env::isolated_ambient();
    point_the_machine_at(&machine_home);
    for key in RUNNER_LOCATION_KEYS {
        std::env::remove_var(key);
    }
    for (key, _) in qontinui_runner_lib::instance_env::QONTINUI_HOME_CHILD_DIRS {
        std::env::remove_var(key);
    }
    // The keychain is machine-wide by service name; the harness run must not
    // touch the CI node's either.
    std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");

    let root = match mode.as_str() {
        "harness" => None,
        "subject" => {
            let root = PathBuf::from(std::env::var(ROOT).expect("SUBJECT_ISOLATION_ROOT"));
            std::env::set_var("QONTINUI_INSTANCE_ROOT", &root);
            std::env::set_var("QONTINUI_PORT", SUBJECT_PORT);
            let applied = qontinui_runner_lib::instance_env::apply_instance_root_env(true)
                .expect("the subject's launch satisfies the instance-root contract");
            assert_eq!(applied.as_deref(), Some(root.as_path()));
            Some(root)
        }
        other => panic!("unknown SUBJECT_ISOLATION_MODE {other:?}"),
    };

    let resolve_only = std::env::var(RESOLVE_ONLY).as_deref() == Ok("1");
    let mut written = if resolve_only {
        resolve_every_store()
    } else {
        drive_every_store(&mode)
    };
    if let Some(root) = &root {
        written.extend(children_see_the_root(root));
    }
    if std::env::var(PLANT).as_deref() == Ok("1") {
        // The shape the D4 source guard exists to refuse in production code.
        let leak = dirs::config_dir()
            .expect("a config dir")
            .join("com.qontinui.runner");
        std::fs::create_dir_all(&leak).expect("mkdir the planted dir");
        std::fs::write(leak.join(PLANTED_FILE), b"leak").expect("write the planted file");
    }

    let lines: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
    std::fs::write(&report, lines.join("\n")).expect("write the driver report");
}

/// The only parent variables a driver child inherits.
const CHILD_ENV_PASSTHROUGH: &[&str] = &[
    "PATH",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "ComSpec",
    "PATHEXT",
    "LANG",
    "RUST_BACKTRACE",
    // Where the test binary's dylibs are found (cargo/nextest set them).
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
];

/// Every location override a launcher or this test binary's sandbox may have
/// exported — cleared so the child resolves its stores exactly as a fresh
/// launch on the fixture machine would.
const RUNNER_LOCATION_KEYS: &[&str] = &[
    "QONTINUI_HOME",
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_EMBEDDED_PG_DIR",
    "QONTINUI_RUNNER_LOG_DIR",
    "QONTINUI_PANIC_LOG_DIR",
    "QONTINUI_PROMPTS_DIR",
    "WEBVIEW2_USER_DATA_FOLDER",
    "QONTINUI_INSTANCE_NAME",
    "QONTINUI_INSTANCE_ROOT",
    "QONTINUI_PRIMARY_PORT",
    "QONTINUI_PORT",
];

/// Point every platform directory `dirs` derives from the environment at the
/// fixture machine: `HOME` (unix, and macOS's `Library/`), the XDG base dirs
/// (Linux) and `USERPROFILE` (read by nothing in `dirs` on Windows — see the
/// module docs — but set so no Windows reader of it reaches the real one).
fn point_the_machine_at(home: &Path) {
    std::env::set_var("HOME", home);
    std::env::set_var("USERPROFILE", home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var("XDG_DATA_HOME", home.join(".local").join("share"));
    std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
    std::env::set_var("XDG_STATE_HOME", home.join(".local").join("state"));
}

/// The stores the drive writes, each through its OWN resolver, so a
/// resolve-only run checks exactly the destinations a write would hit.
fn resolve_every_store() -> Vec<PathBuf> {
    use qontinui_runner_lib::instance_env as ie;
    let mut out = vec![
        ie::secure_storage_dir()
            .expect("a secure-storage dir")
            .join("paired_user.json"),
        crate::settings::resolve_settings_path().expect("a settings path"),
        crate::claude_accounts::claude_accounts_file_path().expect("a roster path"),
        qontinui_runner_lib::embedded_pg::data_root(),
        qontinui_runner_lib::ambient::qontinui_dir().expect("a ~/.qontinui"),
    ];
    out.extend(qontinui_runner_lib::profile_cli::runner_loopback_key_path_for(9881));
    for kind in [
        ie::PlatformDir::Config,
        ie::PlatformDir::DataLocal,
        ie::PlatformDir::Data,
        ie::PlatformDir::Home,
        ie::PlatformDir::Videos,
    ] {
        out.extend(ie::platform_dir(kind));
    }
    out
}

/// Drive the runner's real writers and return what each wrote.
///
/// Every write carries `mode` in its content, so the harness's and the
/// subject's bytes always differ: a subject write that LEAKED onto a harness
/// file can never pass the byte-identical check by happening to write the same
/// default document.
fn drive_every_store(mode: &str) -> Vec<PathBuf> {
    use qontinui_runner_lib::instance_env as ie;
    let mut out = Vec::new();

    // Pairing: the binding store, the secure store, and the converge that
    // once renamed a harness's `paired_user.json` out from under it.
    let tenant = uuid::Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap();
    let resp: qontinui_runner_lib::pair::PairCompleteResponse =
        serde_json::from_value(serde_json::json!({
            "token": format!("{mode}.payload.signature"),
            "user_id": "66666666-7777-4888-9999-000000000000",
            "device_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            "tenant_id": tenant.to_string(),
        }))
        .expect("a pair response");
    // Bound first: `tests/interactive_signout_marker_guard.rs` scans for the
    // call text in every non-`#[cfg(test)]` region, and this whole file is an
    // out-of-line test module it cannot see as one. A fixture pairing has no
    // operator sign-out marker to clear.
    let persist = qontinui_runner_lib::pair::persist_pairing;
    persist(&resp, tenant).expect("persist the fixture pairing");
    let _ = qontinui_runner_lib::pair::converge_binding_store();
    let secure = ie::secure_storage_dir().expect("a secure-storage dir");
    out.push(secure.join("paired_user.json"));

    let settings = crate::settings::Settings {
        setup_completed: mode == "subject",
        ..Default::default()
    };
    crate::settings::save_settings(&settings).expect("save settings");
    out.push(crate::settings::resolve_settings_path().expect("a settings path"));

    crate::prompts::save_prompt_library(&crate::prompts::PromptLibrary {
        version: mode.to_string(),
        ..Default::default()
    })
    .expect("save prompts");
    crate::context::storage::save_user_context_library(
        &crate::context::types::UserContextLibrary {
            version: mode.to_string(),
            ..Default::default()
        },
    )
    .expect("save contexts");

    crate::claude_accounts::save(&crate::claude_accounts::ClaudeAccountsFile {
        config_dir: Some(format!("/accounts-of-the-{mode}")),
        ..Default::default()
    })
    .expect("save the roster");
    out.push(crate::claude_accounts::claude_accounts_file_path().expect("a roster path"));

    out.push(
        crate::agent_worktree::custody::write_session_name(
            "99999999-8888-4777-8666-555555555555",
            mode,
        )
        .expect("write a session name"),
    );

    let log = crate::paths::get_dev_logs_dir().join("subject-isolation.log");
    std::fs::write(&log, format!("a {mode} dev-log line\n")).expect("write a dev log");
    out.push(log);

    if let Some(key) = qontinui_runner_lib::profile_cli::runner_loopback_key_path_for(9881) {
        std::fs::create_dir_all(key.parent().expect("a key dir")).expect("mkdir the key dir");
        std::fs::write(&key, format!("loopback-key-of-the-{mode}"))
            .expect("write the loopback key");
        out.push(key);
    }

    let pg = qontinui_runner_lib::embedded_pg::data_root();
    std::fs::create_dir_all(&pg).expect("mkdir the embedded-pg root");
    out.push(pg);

    out
}

/// D5: a process the subject spawns inherits its root-derived locations.
/// Returns nothing to the report — it asserts in place.
fn children_see_the_root(root: &Path) -> Vec<PathBuf> {
    let keys = ["QONTINUI_HOME", "QONTINUI_SESSION_NAMES_DIR"];
    let output = if cfg!(windows) {
        // console-ok: a test-only child, never in a release build
        Command::new("cmd")
            .args([
                "/C",
                "echo %QONTINUI_HOME%& echo %QONTINUI_SESSION_NAMES_DIR%",
            ])
            .output()
    } else {
        // console-ok: a test-only child, never in a release build
        Command::new("sh")
            .args([
                "-c",
                "printf '%s\\n%s\\n' \"$QONTINUI_HOME\" \"$QONTINUI_SESSION_NAMES_DIR\"",
            ])
            .output()
    }
    .expect("spawn a child of the subject");
    let text = String::from_utf8_lossy(&output.stdout);
    let seen: Vec<&str> = text.lines().map(str::trim).collect();
    assert_eq!(seen.len(), keys.len(), "the child printed {text:?}");
    for (key, value) in keys.iter().zip(&seen) {
        assert!(
            Path::new(value).starts_with(root),
            "a child of the subject sees {key}={value:?}, outside the root {root:?}"
        );
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// The parent side
// ---------------------------------------------------------------------------

/// Run [`driver`] as a child process and return the paths it reported.
fn run_driver(fixture: &Path, mode: &str, extra: &[(&str, &str)]) -> Vec<PathBuf> {
    let report = fixture.join(format!("{mode}-report.txt"));
    let exe = std::env::current_exe().expect("this test binary");
    // console-ok: a test-only child, never in a release build
    let mut cmd = Command::new(exe);
    cmd.args([
        "subject_isolation_harness::driver",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    // Nothing of the parent's configuration reaches the child — only what a
    // process needs to start and find its tools. A snapshot-and-strip would
    // race any in-process test that sets a variable between the two.
    cmd.env_clear();
    for key in CHILD_ENV_PASSTHROUGH {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env(MODE, mode)
        .env(MACHINE_HOME, fixture.join("machine-home"))
        .env(ROOT, fixture.join("subject-root"))
        .env(REPORT, &report);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn the driver");
    assert!(
        out.status.success(),
        "the {mode} driver failed ({}):\n--- stdout\n{}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&report).expect("the driver wrote its report");
    text.lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Every file and directory under `dir`, keyed by its path relative to it;
/// a file maps to its bytes, a directory to `None`.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| {
            let rel = e.path().strip_prefix(dir).unwrap().to_path_buf();
            let bytes = e
                .file_type()
                .is_file()
                .then(|| std::fs::read(e.path()).expect("read a snapshot file"));
            (rel, bytes)
        })
        .collect()
}

/// What changed between two snapshots, one line per path.
fn differences(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, bytes) in before {
        match after.get(path) {
            None => out.push(format!("removed: {}", path.display())),
            Some(now) if now != bytes => out.push(format!("changed: {}", path.display())),
            Some(_) => {}
        }
    }
    for path in after.keys().filter(|p| !before.contains_key(*p)) {
        out.push(format!("created: {}", path.display()));
    }
    out
}

/// The harness runs first and populates the fixture machine, then the subject
/// runs beside it. Returns the harness tree before and after the subject, and
/// the subject's report.
#[cfg(unix)]
fn harness_then_subject(
    fixture: &Path,
    plant: bool,
) -> (
    BTreeMap<PathBuf, Option<Vec<u8>>>,
    BTreeMap<PathBuf, Option<Vec<u8>>>,
    Vec<PathBuf>,
) {
    let harness_report = run_driver(fixture, "harness", &[]);
    let machine = fixture.join("machine-home");
    for path in &harness_report {
        assert!(
            path.starts_with(&machine) && path.exists(),
            "the harness run must populate the fixture machine: {path:?}"
        );
    }
    let before = snapshot(&machine);
    let subject_extra: &[(&str, &str)] = if plant { &[(PLANT, "1")] } else { &[] };
    let subject_report = run_driver(fixture, "subject", subject_extra);
    let after = snapshot(&machine);
    (before, after, subject_report)
}

/// Plan D4/Phase 3: a subject driven through pairing, settings, the libraries,
/// the roster, a session name, a log, the loopback key and its PG root leaves
/// every harness file and directory byte-identical, and writes only inside its
/// own root.
#[cfg(unix)]
#[test]
fn a_subject_leaves_the_harness_byte_identical() {
    let fixture = tempfile::tempdir().expect("a fixture");
    let (before, after, subject_report) = harness_then_subject(fixture.path(), false);

    // Not vacuous: the harness really holds the stores a leak would hit.
    let names: Vec<String> = before
        .keys()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    for expected in ["paired_user.json", "settings.json", "claude-accounts.json"] {
        assert!(
            names.iter().any(|n| n == expected),
            "the harness run wrote no {expected}: {names:?}"
        );
    }
    let changed = differences(&before, &after);
    assert!(
        changed.is_empty(),
        "the subject touched the harness:\n  {}",
        changed.join("\n  ")
    );

    let root = fixture.path().join("subject-root");
    assert!(subject_report.len() >= 7, "{subject_report:?}");
    for path in &subject_report {
        assert!(
            path.starts_with(&root) && path.exists(),
            "a subject write landed outside its root, or not at all: {path:?}"
        );
    }
}

/// The verify step of Phase 3: the byte-identical check FAILS when a call
/// site that bypasses the seam writes outside the root.
#[cfg(unix)]
#[test]
fn the_byte_identical_check_catches_a_planted_leak() {
    let fixture = tempfile::tempdir().expect("a fixture");
    let (before, after, _) = harness_then_subject(fixture.path(), true);
    let changed = differences(&before, &after);
    assert!(
        changed.iter().any(|line| line.contains(PLANTED_FILE)),
        "a planted write into the harness's config dir went unseen: {changed:?}"
    );
}

/// Windows half (see the module docs for why it cannot write): every store the
/// drive writes resolves inside the subject's root, and the children it spawns
/// see the root.
#[cfg(windows)]
#[test]
fn a_subject_resolves_every_store_inside_its_root() {
    let fixture = tempfile::tempdir().expect("a fixture");
    let report = run_driver(fixture.path(), "subject", &[(RESOLVE_ONLY, "1")]);
    let root = fixture.path().join("subject-root");
    assert!(report.len() >= 8, "{report:?}");
    for path in &report {
        assert!(
            qontinui_runner_lib::instance_env::comparable_path(path, true).starts_with(
                qontinui_runner_lib::instance_env::comparable_path(&root, true)
            ),
            "a subject store resolves outside its root: {path:?}"
        );
    }
}
