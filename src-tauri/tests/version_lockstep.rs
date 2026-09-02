//! Regression gate: **the runner's version lives in four files and they must
//! agree.**
//!
//! A release bump (most recently `1.0.10 -> 1.0.11`, PR #1313) is a four-line
//! diff across four files. Nothing enforced that all four moved together — the
//! invariant lived only in the bump PR's description, which is exactly the
//! place a future bump will not read. This test is that enforcement, and it
//! runs inside the existing `cargo test` job, so it needs no workflow edit.
//!
//! # The four sites, and what each one actually feeds
//!
//! None of these is decorative — every one of them reaches a different runtime
//! or packaging surface, so a drift between any two is observable in
//! production, not just untidy:
//!
//! | Site | Consumer |
//! |---|---|
//! | `src-tauri/Cargo.toml` `[package] version` | `env!("CARGO_PKG_VERSION")` — the startup log line, crash reports (`crash_observability.rs`), the OTel resource `service.version` (`otel.rs`), the env-agent's `runner_crate_version` (`env_agent/collectors.rs`), the backup manifest `app_version`, and the `current_version` that `check_for_updates` hands the UI |
//! | `src-tauri/tauri.conf.json` `version` | Tauri names **every bundle** from this, so it decides the shipped asset filenames and therefore the updater manifest's download URLs |
//! | `package.json` `version` | `vite.config.ts` `define` bakes it into `__APP_VERSION__` -> `src/lib/appInfo.ts` `APP_VERSION` -> the `runnerVersion` reported from `managers/event-handlers/executionHandlers.ts` |
//! | `Cargo.lock` `qontinui-runner` entry | a `--locked` build fails outright when it disagrees with `Cargo.toml`; without `--locked` cargo silently rewrites it, leaving a dirty tree mid-CI |
//!
//! The two worst drifts are between `Cargo.toml` and `tauri.conf.json`. A
//! shipped build would then *name its assets* one version while *reporting*
//! another: the update-check dialog shows `current_version` from
//! `CARGO_PKG_VERSION` while `tauri_plugin_updater` compares against the
//! `tauri.conf.json` version, so the UI and the updater disagree about what is
//! installed. And a `package.json` left behind makes `runnerVersion`
//! telemetry attribute a release's data to the previous one.
//!
//! # What this test deliberately does NOT cover
//!
//! The **tag**. `v<version>` is not a file in the tree, so it cannot be
//! checked from here. `.github/workflows/release.yml`'s "Resolve release
//! version" step already asserts the resolved tag matches
//! `tauri.conf.json` — and because this test makes the four files agree with
//! each other, that single assertion now transitively covers all four. The two
//! checks compose; neither is redundant.
//!
//! Sibling workspace members (`crates/*`, `src-tauri/clorinde`) carry their own
//! independent versions and are intentionally out of scope — only the shipped
//! app's version is in lockstep.

use std::path::{Path, PathBuf};

/// The version cargo compiled this crate at — i.e. `[package] version` in
/// `src-tauri/Cargo.toml`, read through the exact door the runtime consumers
/// use rather than by re-parsing the manifest.
const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Repo root: `CARGO_MANIFEST_DIR` is `src-tauri/`, so its parent is the
/// checkout root that holds `package.json` and `Cargo.lock`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri always has a parent")
        .to_path_buf()
}

/// Top-level `"version"` of a JSON file.
fn json_version(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{} has no top-level string \"version\"", path.display()))
        .to_string()
}

/// The `version` of one `[[package]]` entry in a `Cargo.lock`.
///
/// Parsed rather than grepped: the file holds hundreds of `version = ...`
/// lines and only the one under `name = "qontinui-runner"` is ours.
fn lock_version(path: &Path, package: &str) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // `toml::from_str`, NOT `text.parse::<toml::Value>()` — see the same note
    // in `agent_worktree::census`.
    let value: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let packages = value
        .get("package")
        .and_then(|p| p.as_array())
        .unwrap_or_else(|| panic!("{} has no [[package]] array", path.display()));

    let mut found: Vec<&str> = packages
        .iter()
        .filter(|entry| entry.get("name").and_then(|n| n.as_str()) == Some(package))
        .filter_map(|entry| entry.get("version").and_then(|v| v.as_str()))
        .collect();

    match found.len() {
        1 => found.remove(0).to_string(),
        0 => panic!("{} has no `{package}` [[package]] entry", path.display()),
        // Two entries would make "the" locked version ambiguous, and the one
        // cargo picks is not something this test should guess at.
        n => panic!("{} has {n} `{package}` [[package]] entries", path.display()),
    }
}

#[test]
fn the_four_version_sites_agree() {
    let root = repo_root();

    let sites = [
        (
            "src-tauri/Cargo.toml [package] version",
            CRATE_VERSION.to_string(),
        ),
        (
            "src-tauri/tauri.conf.json version",
            json_version(&root.join("src-tauri").join("tauri.conf.json")),
        ),
        (
            "package.json version",
            json_version(&root.join("package.json")),
        ),
        (
            "Cargo.lock qontinui-runner version",
            lock_version(&root.join("Cargo.lock"), "qontinui-runner"),
        ),
    ];

    // `Cargo.toml` is the reference only because it is the one site cargo
    // itself validates; any of the four disagreeing is equally a defect.
    let (reference_name, reference) = (sites[0].0, sites[0].1.as_str());
    let drifted = sites[1..].iter().any(|(_, version)| version != reference);

    if drifted {
        let mut report = format!(
            "runner version is out of lockstep across its four sites.\n  \
             {reference_name} = {reference}  (reference)\n"
        );
        for (name, version) in &sites[1..] {
            let mark = if *version == reference { " " } else { "!" };
            report.push_str(&format!("{mark} {name} = {version}\n"));
        }
        report.push_str(
            "\nAll four must move together in the release-bump commit. Fix the \
             marked site(s), then re-run. See this file's header for what each \
             one feeds at runtime.",
        );
        panic!("{report}");
    }
}
