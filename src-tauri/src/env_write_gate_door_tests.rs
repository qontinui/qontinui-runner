//! Door-level regression tests for `qontinui_runner_lib::env_write_gate`, in
//! the runner BIN's test binary — where the doors live and where the gate is
//! armed at runtime rather than by `cfg(test)`.
//!
//! Each test holds `isolated_ambient()` on its own thread, so the process
//! environment names THIS fixture's directory and no other fixture can be
//! live, then enters a door from a thread that holds no `env_lock` — the shape
//! a measured run caught writing whole default `settings.json` documents into
//! other tests' fixtures (plan
//! `2026-11-11-unguarded-test-threads-write-settings-json-into-a-live-fixtures-config-dir`).
//! The worker's refusal count comes back through the join.
//!
//! Why the assertions are deterministic: while this test holds the fixture it
//! holds `env_lock`, so every other thread in the process holds none, and the
//! gate refuses every one of them at every settings door. Nothing but this
//! test's own thread can put a `settings.json` into the fixture.
//!
//! What is compared is the settings file and its atomic-write temps, not the
//! whole fixture root: five keys alias that root (`HOME` among them), so a
//! sibling test's unrelated write under `$HOME` could change it, and that
//! write is outside this gate (plan, Risks R3).

use qontinui_runner_lib::env_write_gate::{refusals_on_this_thread, reported_lines};
use std::path::Path;

/// Run `f` on a fresh named thread (which holds no `env_lock`) and return its
/// result with the number of refusals that thread made.
fn run_unguarded<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> (T, u64) {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            assert_eq!(
                crate::test_env::env_lock_depth(),
                0,
                "the worker must hold no env_lock, or it proves nothing"
            );
            let out = f();
            (out, refusals_on_this_thread())
        })
        .expect("spawn the unguarded worker")
        .join()
        .expect("the unguarded worker must not panic")
}

/// Every entry in `dir` a settings door writes: `settings.json` and its
/// atomic-write temps (`settings.json.tmp.…`).
fn settings_traces(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("settings.json"))
                .collect()
        })
        .unwrap_or_default()
}

/// The census's top door: `workspace_root()` → `config_facade::get_setting` →
/// `load_settings` → `load_settings_full`, whose FreshInstall persist wrote 17
/// of the 32 measured documents. Refused from the worker; ALLOWED on this
/// thread, which holds the fixture — so the door is a writer here, and the
/// gate only refuses the thread that holds no lock.
#[test]
fn an_unguarded_worker_reaching_workspace_root_persists_nothing_into_the_fixture() {
    let amb = crate::test_env::isolated_ambient();
    let settings_json = amb.dir().join("settings.json");
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());

    let ((), refused) = run_unguarded("env-write-gate-door-workspace-root", || {
        let _ = crate::workspace_paths::workspace_root();
    });
    assert!(
        refused >= 1,
        "the worker's FreshInstall persist must be refused, got {refused} refusals"
    );
    assert_eq!(
        settings_traces(amb.dir()),
        Vec::<String>::new(),
        "an unguarded worker wrote a settings document into the live fixture"
    );

    // Positive control on the SAME fixture.
    let before = refusals_on_this_thread();
    let _ = crate::workspace_paths::workspace_root();
    assert!(
        settings_json.is_file(),
        "a guarded thread's FreshInstall load persists — the door must be a writer here"
    );
    assert_eq!(
        refusals_on_this_thread(),
        before,
        "a guarded thread is never refused"
    );
}

/// The `coord_sync` drain loop's door, entered from where the measured writes
/// came from: a multi-thread runtime's `tokio-rt-worker`, which no test can
/// hand its `env_lock`.
#[test]
fn an_unguarded_tokio_worker_reaching_get_cloud_sync_enabled_persists_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("build a multi-thread runtime");
    let (thread, refused) = rt.block_on(async {
        tokio::spawn(async {
            let before = refusals_on_this_thread();
            let _ = crate::settings::get_cloud_sync_enabled();
            (
                std::thread::current().name().map(str::to_string),
                refusals_on_this_thread() - before,
            )
        })
        .await
        .expect("the worker task must not panic")
    });
    drop(rt);
    assert_eq!(
        thread.as_deref(),
        Some("tokio-rt-worker"),
        "the door must run on a runtime worker, the shape the census measured"
    );
    assert!(
        refused >= 1,
        "the worker's persist must be refused, got {refused}"
    );
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());
}

/// An explicit door returns the refusal — never a silent `Ok` that would tell
/// the caller "saved" — and neither reads nor writes.
#[test]
fn an_unguarded_update_settings_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let name = "env-write-gate-door-update-settings";
    let (verdict, refused) = run_unguarded(name, || {
        crate::settings::update_settings(|s| s.setup_completed = true)
    });
    let err = verdict.expect_err("an unguarded update must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains(&format!("thread `{name}`")), "{err}");
    assert!(err.contains("`settings::update_settings`"), "{err}");
    assert_eq!(refused, 1);
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());
}

/// The tier writer in the lib, through the bin door every in-process caller
/// uses.
#[test]
fn an_unguarded_promote_tier_to_account_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let (verdict, refused) = run_unguarded("env-write-gate-door-promote-tier", || {
        crate::settings::promote_tier_to_account().map_err(|e| e.to_string())
    });
    let err = verdict.expect_err("an unguarded tier promotion must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains("`profiles::promote_tier_to_account`"), "{err}");
    assert_eq!(refused, 1);
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());
}

/// The env-resolved roster writer. `cfg(unix)` for the reason every roster
/// writing test is (`commands::ai_settings` `usage_report_scope_tests`):
/// Windows resolves the known folders once per process and ignores a later
/// `USERPROFILE`, so the roster path would not resolve inside this fixture.
#[cfg(unix)]
#[test]
fn an_unguarded_claude_accounts_save_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let _xdg = crate::test_env::EnvVarRestore::capture(&["XDG_CONFIG_HOME"]);
    std::env::set_var("XDG_CONFIG_HOME", amb.dir().join("xdg-config"));
    let roster = crate::claude_accounts::claude_accounts_file_path().expect("a roster path");
    assert!(
        roster.starts_with(amb.dir()),
        "the roster must resolve inside the fixture, or a broken gate writes the box's: {}",
        roster.display()
    );
    let roster_dir = roster.parent().expect("a roster dir").to_path_buf();

    let (verdict, refused) = run_unguarded("env-write-gate-door-roster-save", || {
        crate::claude_accounts::save(&crate::claude_accounts::ClaudeAccountsFile {
            claude_config_dirs: vec!["/accounts/of/the/worker".to_string()],
            ..Default::default()
        })
    });
    let err = verdict.expect_err("an unguarded roster save must be refused");
    assert!(err.contains("`claude_accounts::save`"), "{err}");
    assert_eq!(refused, 1);
    assert!(
        !roster.exists(),
        "the refused roster write landed: {}",
        roster.display()
    );
    assert!(
        !roster_dir.exists(),
        "a refused write creates nothing, not even the roster's directory"
    );
}

/// Under `strict_canary()` the refusal is a panic carrying the sentence.
#[test]
fn a_strict_unguarded_worker_panics_with_the_refusal() {
    let amb = crate::test_env::isolated_ambient();
    let name = "env-write-gate-door-strict";
    let payload = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(|| {
            let _strict = crate::test_env::strict_canary();
            let _ = crate::settings::update_settings(|s| s.setup_completed = true);
        })
        .expect("spawn the strict worker")
        .join()
        .expect_err("a strict unguarded write must panic");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .expect("the panic carries the formatted sentence");
    assert!(message.contains("holds no env_lock"), "{message}");
    assert!(message.contains(&format!("thread `{name}`")), "{message}");
    assert!(message.contains("`settings::update_settings`"), "{message}");
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());
}

/// The refusal line is written ONCE per thread, door and caller, however often
/// that caller is refused — and through `write_all` on stderr, which libtest's
/// capture does not intercept, so it reaches the suite log from this passing
/// test (confirmed by hand in a captured run; see the plan's Phase 1).
#[test]
fn a_repeated_refusal_writes_one_line() {
    let _amb = crate::test_env::isolated_ambient();
    let name = "env-write-gate-door-print-once";
    let ((), refused) = run_unguarded(name, || {
        for _ in 0..3 {
            let _ = crate::settings::update_settings(|s| s.setup_completed = true);
        }
    });
    assert_eq!(refused, 3, "every refusal is counted");
    assert_eq!(reported_lines(name, "settings::update_settings"), 1);
}
