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
//!
//! The door tests observe the refusal only where a caller of the door can: the
//! `Err` (or the per-entry error) the door returns and the file it did not
//! write. Where a door's refusal leaves nothing a caller can see (the
//! FreshInstall persist is skipped quietly), a positive control on the guarded
//! thread shows the same door IS a writer, so an empty fixture cannot be
//! vacuous. They never read the gate's own counters, so each one enters where
//! the population enters and goes red with the gate removed. Only
//! `a_repeated_refusal_writes_one_line`, which is about counting and line-once
//! printing, reads them.
//!
//! What these tests do NOT pin: removing ONE internal gate while the others
//! stay. With the `load_settings_full` persist gate alone removed, the persist
//! reaches `save_settings`, whose `get_settings_path` gate still refuses it, so
//! no file lands; the seed gate in `claude_accounts::load_with_migration` sits
//! behind a process-wide `Once` that an earlier guarded loader may already have
//! completed. Both are proven by forcing the whole gate open (plan Phase 1).
//!
//! The positive controls that run a settings LOAD (workspace-root, tokio) can
//! complete the roster seed, which resolves the platform config dir, so on unix
//! they point `XDG_CONFIG_HOME` into the fixture first and are `cfg(unix)` for
//! the reason the roster test is: Windows resolves the known folders once per
//! process and would seed the box's own roster.
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
/// result.
fn on_unguarded_thread<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            assert_eq!(
                crate::test_env::env_lock_depth(),
                0,
                "the worker must hold no env_lock, or it proves nothing"
            );
            f()
        })
        .expect("spawn the unguarded worker")
        .join()
        .expect("the unguarded worker must not panic")
}

/// [`on_unguarded_thread`], also returning the number of refusals that thread
/// made. Only the counting tests use it.
fn run_unguarded_counted<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> (T, u64) {
    on_unguarded_thread(name, move || {
        let out = f();
        (out, refusals_on_this_thread())
    })
}

/// Point `XDG_CONFIG_HOME` inside `fixture` for the guard's life, so a guarded
/// settings load that completes the roster seed writes the fixture's roster,
/// never the box's.
#[cfg(unix)]
fn xdg_config_home_inside(fixture: &Path) -> crate::test_env::EnvVarRestore {
    let restore = crate::test_env::EnvVarRestore::capture(&["XDG_CONFIG_HOME"]);
    std::env::set_var("XDG_CONFIG_HOME", fixture.join("xdg-config"));
    restore
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
    #[cfg(unix)]
    let _xdg = xdg_config_home_inside(amb.dir());
    let settings_json = amb.dir().join("settings.json");
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());

    on_unguarded_thread("env-write-gate-door-workspace-root", || {
        let _ = crate::workspace_paths::workspace_root();
    });
    assert_eq!(
        settings_traces(amb.dir()),
        Vec::<String>::new(),
        "an unguarded worker wrote a settings document into the live fixture"
    );

    // Positive control on the SAME fixture: the guarded thread's identical
    // call persists, so the empty fixture above is the gate's refusal and not
    // a door that never writes.
    #[cfg(unix)]
    {
        let _ = crate::workspace_paths::workspace_root();
        assert!(
            settings_json.is_file(),
            "a guarded thread's FreshInstall load persists — the door must be a writer here"
        );
    }
    #[cfg(not(unix))]
    let _ = settings_json;
}

/// The `coord_sync` drain loop's door, entered the way the measured writes
/// were: `egress::transcript_sync_permitted()` → `transcript_gate_with` →
/// `settings::get_cloud_sync_enabled()`, on a multi-thread runtime's
/// `tokio-rt-worker`, which no test can hand its `env_lock`. The egress pin is
/// thread-local, so the worker takes it: with the tenant switch ON the gate
/// reads the user's consent from `settings.json`.
#[test]
fn an_unguarded_tokio_worker_reaching_get_cloud_sync_enabled_persists_nothing() {
    let amb = crate::test_env::isolated_ambient();
    #[cfg(unix)]
    let _xdg = xdg_config_home_inside(amb.dir());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("build a multi-thread runtime");
    let thread = rt.block_on(async {
        tokio::spawn(async {
            let _tenant_switch = crate::egress::test_support::pin(
                crate::egress::Flow::TranscriptSync,
                crate::egress::Level::On,
            );
            let _ = crate::egress::transcript_sync_permitted();
            std::thread::current().name().map(str::to_string)
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
    assert_eq!(
        settings_traces(amb.dir()),
        Vec::<String>::new(),
        "an unguarded runtime worker wrote a settings document into the live fixture"
    );

    // Positive control on the SAME fixture: the same chain on this guarded
    // thread persists, so the worker's empty fixture is the gate's refusal.
    #[cfg(unix)]
    {
        let _tenant_switch = crate::egress::test_support::pin(
            crate::egress::Flow::TranscriptSync,
            crate::egress::Level::On,
        );
        let _ = crate::egress::transcript_sync_permitted();
        assert!(
            amb.dir().join("settings.json").is_file(),
            "a guarded thread's load through the drain loop's chain persists — the door must be a writer here"
        );
    }
}

/// An explicit door returns the refusal — never a silent `Ok` that would tell
/// the caller "saved" — and neither reads nor writes.
#[test]
fn an_unguarded_update_settings_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let name = "env-write-gate-door-update-settings";
    let verdict = on_unguarded_thread(name, || {
        crate::settings::update_settings(|s| s.setup_completed = true)
    });
    let err = verdict.expect_err("an unguarded update must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains(&format!("thread `{name}`")), "{err}");
    assert!(err.contains("`settings::update_settings`"), "{err}");
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());

    // Positive control: the guarded thread's update writes.
    crate::settings::update_settings(|s| s.setup_completed = true)
        .expect("a guarded update is never refused");
    assert!(amb.dir().join("settings.json").is_file());
}

/// The tier writer in the lib, through the bin door every in-process caller
/// uses.
#[test]
fn an_unguarded_promote_tier_to_account_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let verdict = on_unguarded_thread("env-write-gate-door-promote-tier", || {
        crate::settings::promote_tier_to_account().map_err(|e| e.to_string())
    });
    let err = verdict.expect_err("an unguarded tier promotion must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains("`profiles::promote_tier_to_account`"), "{err}");
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

    let verdict = on_unguarded_thread("env-write-gate-door-roster-save", || {
        crate::claude_accounts::save(&crate::claude_accounts::ClaudeAccountsFile {
            claude_config_dirs: vec!["/accounts/of/the/worker".to_string()],
            ..Default::default()
        })
    });
    let err = verdict.expect_err("an unguarded roster save must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains("`claude_accounts::save`"), "{err}");
    assert!(
        !roster.exists(),
        "the refused roster write landed: {}",
        roster.display()
    );
    assert!(
        !roster_dir.exists(),
        "a refused write creates nothing, not even the roster's directory"
    );

    // Positive control: the guarded thread's save writes the roster.
    crate::claude_accounts::save(&crate::claude_accounts::ClaudeAccountsFile::default())
        .expect("a guarded roster save is never refused");
    assert!(roster.is_file(), "the door must be a writer here");
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
    let ((), refused) = run_unguarded_counted(name, || {
        for _ in 0..3 {
            let _ = crate::settings::update_settings(|s| s.setup_completed = true);
        }
    });
    assert_eq!(refused, 3, "every refusal is counted");
    assert_eq!(reported_lines(name, "settings::update_settings"), 1);
}

/// Every whole-document save resolves its file through
/// `settings::get_settings_path`, which refuses before its `create_dir_all`.
#[test]
fn an_unguarded_save_settings_returns_the_refusal_and_writes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let verdict = on_unguarded_thread("env-write-gate-door-save-settings", || {
        crate::settings::save_settings(&crate::settings::Settings::default())
    });
    let err = verdict.expect_err("an unguarded whole-document save must be refused");
    assert!(err.contains("holds no env_lock"), "{err}");
    assert!(err.contains("`settings::get_settings_path`"), "{err}");
    assert_eq!(settings_traces(amb.dir()), Vec::<String>::new());

    // Positive control: the guarded thread's save writes.
    crate::settings::save_settings(&crate::settings::Settings::default())
        .expect("a guarded save is never refused");
    assert!(amb.dir().join("settings.json").is_file());
}

/// `backup::restore_backup` resolves every destination from the env and writes
/// without `get_settings_path`, so it gates each destination itself: a refused
/// entry becomes that entry's error and nothing is restored. `cfg(unix)` for
/// the roster test's reason: the destinations live under the platform config
/// dir, which only `XDG_CONFIG_HOME` moves into this fixture.
#[cfg(unix)]
#[test]
fn an_unguarded_restore_backup_restores_nothing() {
    let amb = crate::test_env::isolated_ambient();
    let _xdg = crate::test_env::EnvVarRestore::capture(&["XDG_CONFIG_HOME"]);
    std::env::set_var("XDG_CONFIG_HOME", amb.dir().join("xdg-config"));
    let base = qontinui_runner_lib::instance_env::runner_config_base().expect("a config base");
    assert!(
        base.starts_with(amb.dir()),
        "the restore target must resolve inside the fixture: {}",
        base.display()
    );
    let settings_json = base.join("settings.json");

    // A real backup carrying settings.json, made on this (guarded) thread.
    std::fs::create_dir_all(&base).expect("the config base");
    std::fs::write(&settings_json, br#"{"setup_completed":true}"#).expect("seed settings.json");
    let (archive, made) = crate::backup::create_backup().expect("a backup");
    assert!(
        made.files_backed_up.contains(&"settings.json".to_string()),
        "the backup must carry settings.json: {:?}",
        made.files_backed_up
    );
    std::fs::remove_file(&settings_json).expect("clear the restore target");

    let restored = on_unguarded_thread("env-write-gate-door-restore-backup", {
        let archive = archive.clone();
        move || crate::backup::restore_backup(&archive)
    });
    let restored = restored.expect("the archive opens");
    assert!(
        restored.files_restored.is_empty(),
        "nothing may be restored: {:?}",
        restored.files_restored
    );
    let refusals = restored
        .errors
        .iter()
        .filter(|e| e.contains("holds no env_lock") && e.contains("`backup::restore_backup`"))
        .count();
    assert_eq!(
        refusals,
        made.files_backed_up.len(),
        "every backed-up entry's error must be the refusal: {:?}",
        restored.errors
    );
    assert!(
        !settings_json.exists(),
        "the refused restore wrote {}",
        settings_json.display()
    );

    // Positive control: the guarded thread's restore of the same archive writes.
    let guarded = crate::backup::restore_backup(&archive).expect("the archive opens");
    assert!(
        guarded
            .files_restored
            .contains(&"settings.json".to_string()),
        "a guarded restore must restore settings.json: {:?}",
        guarded
    );
    assert!(settings_json.is_file());
}
