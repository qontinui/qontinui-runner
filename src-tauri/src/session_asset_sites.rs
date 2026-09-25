//! Session-asset provisioning guard (plan
//! `2026-09-02-gate-continuation-sessions-get-no-subagent-definitions`).
//!
//! A spawned session's cwd receives three runner-bundled asset kinds: subagent
//! definitions (`.claude/agents`), fleet slash commands (`.claude/commands`) and
//! fleet skills (`.claude/skills`). Each spawn path used to call the per-asset
//! provisioners inline, and they drifted apart: only the agent-spawn path wrote
//! agent definitions, so a gate-continuation session could not spawn
//! `code-reviewer`, and the headless continuation arm wrote no fleet asset at
//! all. Nothing asserted the sequences agreed.
//!
//! This guard makes "which assets does a session get?" a question with one
//! answer: every NON-TEST call to a per-asset provisioner must sit inside
//! `provision_session_assets`, and that fn must call all three. A new spawn path
//! that provisions inline goes red here instead of silently shipping a subset.
//!
//! Test-only: the whole module is `#[cfg(test)]` in `main.rs`. It reuses the
//! source scanner of [`crate::runner_spawn_sites`], so it inherits that scan's
//! documented limits (a source scan, not a type check).

use crate::runner_spawn_sites::{fn_body, load_sources, scan_calls, token_re, Sources};
use std::collections::BTreeSet;

/// The per-asset provisioners. Only the entry point may call them.
const PROVISIONERS: &[&str] = &[
    "provision_agent_definitions(",
    "provision_fleet_commands_for_session(",
    "provision_fleet_skills_for_session(",
];

/// The one fn allowed to call [`PROVISIONERS`], as `(file, fn)`.
const ENTRY_FILE: &str = "session_assets.rs";
const ENTRY_FN: &str = "provision_session_assets";

/// This file names every provisioner in string literals; it is not a caller.
const SELF_FILE: &str = "session_asset_sites.rs";

/// Every `(file, fn)` making a non-test provisioner call, the entry point
/// excluded. PURE over the sources.
fn provisioning_calls_outside_the_entry_point(sources: &Sources) -> BTreeSet<(String, String)> {
    let mut sources = sources.clone();
    sources.remove(SELF_FILE);
    let patterns: Vec<_> = PROVISIONERS.iter().map(|p| token_re(p)).collect();
    scan_calls(&sources, &patterns)
        .into_iter()
        .filter(|(file, func)| !(file == ENTRY_FILE && func == ENTRY_FN))
        .collect()
}

/// The provisioners the entry point's body does NOT call — all of them when the
/// entry point does not exist. PURE over the sources.
fn provisioners_missing_from_the_entry_point(sources: &Sources) -> Vec<&'static str> {
    let body = sources
        .get(ENTRY_FILE)
        .and_then(|src| fn_body(src, ENTRY_FN))
        .unwrap_or_default();
    PROVISIONERS
        .iter()
        .filter(|p| !token_re(p).is_match(&body))
        .copied()
        .collect()
}

#[test]
fn every_session_asset_provision_goes_through_one_entry_point() {
    let stray = provisioning_calls_outside_the_entry_point(&load_sources());
    assert!(
        stray.is_empty(),
        "per-asset provisioners called outside `{ENTRY_FILE}::{ENTRY_FN}` — route the \
         spawn path through `crate::session_assets::{ENTRY_FN}` so every session gets \
         the same asset set: {stray:#?}"
    );
}

#[test]
fn the_entry_point_provisions_every_asset_kind() {
    let missing = provisioners_missing_from_the_entry_point(&load_sources());
    assert!(
        missing.is_empty(),
        "`{ENTRY_FILE}::{ENTRY_FN}` is missing or does not call {missing:?} — an entry \
         point that skips an asset kind reopens the asymmetry this guard exists for"
    );
}

/// Mutation twin: a synthetic spawn path provisioning inline must be named, and
/// the entry point's own calls must not be — or the guard is vacuous.
#[test]
fn an_inline_provision_on_a_new_spawn_path_is_caught() {
    let mut sources = Sources::new();
    sources.insert(
        ENTRY_FILE.to_string(),
        format!(
            "pub(crate) fn {ENTRY_FN}(workdir: &str) {{\n    \
             crate::agent_runtime::provision_agent_definitions(workdir);\n    \
             crate::fleet_commands::provision_fleet_commands_for_session(workdir);\n    \
             crate::fleet_skills::provision_fleet_skills_for_session(workdir);\n}}\n"
        ),
    );
    sources.insert(
        "new_spawn_path.rs".to_string(),
        "async fn spawn_new_kind(workdir: &str) {\n    \
         // provision_fleet_skills_for_session(workdir) — a comment is not a call\n    \
         crate::fleet_commands::provision_fleet_commands_for_session(\n        workdir,\n    );\n}\n\
         #[cfg(test)]\nmod tests {\n    fn t() { super::provision_agent_definitions(\"x\"); }\n}\n"
            .to_string(),
    );
    assert_eq!(
        provisioning_calls_outside_the_entry_point(&sources),
        BTreeSet::from([("new_spawn_path.rs".to_string(), "spawn_new_kind".to_string())]),
    );
    assert!(provisioners_missing_from_the_entry_point(&sources).is_empty());

    // And an empty shim cannot satisfy the second assertion.
    sources.insert(
        ENTRY_FILE.to_string(),
        format!("pub(crate) fn {ENTRY_FN}(_workdir: &str) {{}}\n"),
    );
    assert_eq!(
        provisioners_missing_from_the_entry_point(&sources),
        PROVISIONERS.to_vec()
    );
}
