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
//! answer, from both ends:
//!
//! * every NON-TEST call to a per-asset provisioner sits in its one allowed
//!   caller ([`ALLOWED_CALLERS`]) — `provision_session_assets` for the three
//!   session-level provisioners — and that fn calls all three;
//! * every spawn path in [`REQUIRED_CALLERS`] calls the entry point, and no
//!   unlisted fn does, so a site that silently drops the call, or a new site
//!   that nobody registered, goes red.
//!
//! Test-only: the whole module is `#[cfg(test)]` in `main.rs`. It reuses the
//! source loader and masking helpers of [`crate::runner_spawn_sites`] and so
//! shares its documented limits (a source scan, not a type check), but scans
//! calls itself: unlike that module's scanner it keeps a call written on a
//! fn-declaration line (a one-line `fn f() { provision_x(..) }`) and skips only
//! the provisioner's OWN declaration.

use crate::runner_spawn_sites::{
    comment_mask, fn_body, fn_decl_re, load_sources, test_spans, token_re, Sources,
};
use std::collections::BTreeSet;

type Site = (String, String);

/// Each per-asset provisioner and the ONE non-test `(file, fn)` allowed to call
/// it.
const ALLOWED_CALLERS: &[(&str, (&str, &str))] = &[
    ("provision_agent_definitions(", (ENTRY_FILE, ENTRY_FN)),
    (
        "provision_fleet_commands_for_session(",
        (ENTRY_FILE, ENTRY_FN),
    ),
    (
        "provision_fleet_skills_for_session(",
        (ENTRY_FILE, ENTRY_FN),
    ),
    // The embedded floor is one layer of the agent-definition provisioner, so a
    // second caller would be a second, unguarded agent-def write path.
    (
        "provision_fleet_agents_into(",
        ("agent_runtime.rs", "provision_agent_definitions_from_root"),
    ),
];

/// The entry point, as `(file, fn)`.
const ENTRY_FILE: &str = "session_assets.rs";
const ENTRY_FN: &str = "provision_session_assets";

/// The entry point's call tokens: the sync fn and its async twin.
const ENTRY_TOKENS: &[&str] = &[
    "provision_session_assets(",
    "provision_session_assets_off_runtime(",
];

/// The async twin, which calls the sync entry point and is a caller by design.
const ENTRY_TWIN: (&str, &str) = (ENTRY_FILE, "provision_session_assets_off_runtime");

/// Every spawn path that provisions session assets. Adding a spawn path that
/// should provision them is adding a row here AND the call; removing a call is
/// red until its row goes too.
const REQUIRED_CALLERS: &[(&str, &str)] = &[
    ("agent_runtime.rs", "run_agent_subprocess"),
    ("agent_runtime.rs", "run_continuation_terminal"),
    ("agent_runtime.rs", "run_continuation_headless"),
    (
        "looping_agent_supervisor.rs",
        "spawn_looping_agent_terminal",
    ),
    ("scheduler_remote_agent.rs", "launch"),
    ("agent_worktree/isolated_edit.rs", "provision_session_cwd"),
];

/// This file names every token in string literals; it is not a caller.
const SELF_FILE: &str = "session_asset_sites.rs";

fn site(file: &str, func: &str) -> Site {
    (file.to_string(), func.to_string())
}

/// Every `(file, enclosing fn)` making a non-test call of `token` (`name(`).
/// Comment lines and `#[cfg(test)] mod` spans are blanked first. A match on a
/// fn-declaration line is a call unless that line declares `name` itself.
fn calls_of(sources: &Sources, token: &str) -> BTreeSet<Site> {
    let re = token_re(token);
    let decl = fn_decl_re();
    let name = token.trim_end_matches('(');
    let mut out = BTreeSet::new();
    for (file, src) in sources {
        if file == SELF_FILE {
            continue;
        }
        let lines: Vec<&str> = src.lines().collect();
        let spans = test_spans(&lines);
        let comments = comment_mask(&lines);
        let code: Vec<&str> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if comments[i] || spans.iter().any(|(s, e)| i >= *s && i < *e) {
                    ""
                } else {
                    *l
                }
            })
            .collect();
        let joined = code.join("\n");
        for m in re.find_iter(&joined) {
            let line = joined.as_bytes()[..m.start()]
                .iter()
                .filter(|b| **b == b'\n')
                .count();
            if decl.captures(code[line]).is_some_and(|c| &c[2] == name) {
                continue;
            }
            let enclosing = (0..=line)
                .rev()
                .find_map(|j| decl.captures(lines[j]).map(|c| c[2].to_string()))
                .unwrap_or_else(|| "<file scope>".to_string());
            out.insert((file.clone(), enclosing));
        }
    }
    out
}

/// `token -> callers` that are not the token's allowed caller. PURE.
fn provisioner_calls_outside_their_caller(sources: &Sources) -> Vec<(&'static str, Site)> {
    ALLOWED_CALLERS
        .iter()
        .flat_map(|(token, (file, func))| {
            let allowed = site(file, func);
            calls_of(sources, token)
                .into_iter()
                .filter(move |s| *s != allowed)
                .map(move |s| (*token, s))
        })
        .collect()
}

/// The session-level provisioners the entry point's body does NOT call — all
/// of them when the entry point does not exist. PURE.
fn provisioners_missing_from_the_entry_point(sources: &Sources) -> Vec<&'static str> {
    let body = sources
        .get(ENTRY_FILE)
        .and_then(|src| fn_body(src, ENTRY_FN))
        .unwrap_or_default();
    ALLOWED_CALLERS
        .iter()
        .filter(|(_, caller)| *caller == (ENTRY_FILE, ENTRY_FN))
        .map(|(token, _)| *token)
        .filter(|token| !token_re(token).is_match(&body))
        .collect()
}

/// `(missing, unlisted)`: [`REQUIRED_CALLERS`] rows that no longer call the
/// entry point, and non-test callers of it that have no row. PURE.
fn entry_point_callers_vs_roster(sources: &Sources) -> (Vec<Site>, Vec<Site>) {
    let found: BTreeSet<Site> = ENTRY_TOKENS
        .iter()
        .flat_map(|t| calls_of(sources, t))
        .collect();
    let mut listed: BTreeSet<Site> = REQUIRED_CALLERS
        .iter()
        .map(|(f, func)| site(f, func))
        .collect();
    let missing = listed.difference(&found).cloned().collect();
    listed.insert(site(ENTRY_TWIN.0, ENTRY_TWIN.1));
    let unlisted = found.difference(&listed).cloned().collect();
    (missing, unlisted)
}

#[test]
fn every_session_asset_provision_goes_through_one_entry_point() {
    let stray = provisioner_calls_outside_their_caller(&load_sources());
    assert!(
        stray.is_empty(),
        "per-asset provisioners called outside their one allowed caller — route the \
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

#[test]
fn every_listed_spawn_path_calls_the_entry_point_and_no_other_does() {
    let sources = load_sources();
    let (missing, unlisted) = entry_point_callers_vs_roster(&sources);
    assert!(
        missing.is_empty(),
        "spawn paths in REQUIRED_CALLERS that no longer provision session assets — a \
         session spawned there would lack agents/commands/skills: {missing:#?}"
    );
    assert!(
        unlisted.is_empty(),
        "callers of the session-asset entry point with no REQUIRED_CALLERS row — add \
         the row so dropping the call later goes red: {unlisted:#?}"
    );
}

/// Mutation twin for the roster: remove the call from one real spawn path and
/// the guard must name exactly that path.
#[test]
fn a_spawn_path_that_drops_the_entry_point_is_caught() {
    let mut sources = load_sources();
    let file = "scheduler_remote_agent.rs".to_string();
    let src = sources
        .get(&file)
        .expect("scheduler_remote_agent.rs")
        .clone();
    assert!(
        src.contains("provision_session_assets_off_runtime("),
        "fixture: the scheduler launch path must call the entry point today"
    );
    sources.insert(
        file,
        src.replace(
            "provision_session_assets_off_runtime(",
            "removed_provision(",
        ),
    );
    let (missing, unlisted) = entry_point_callers_vs_roster(&sources);
    assert_eq!(missing, vec![site("scheduler_remote_agent.rs", "launch")]);
    assert!(unlisted.is_empty());
}

/// Mutation twin for the provisioner scan: a synthetic spawn path provisioning
/// inline (multi-line, and one-line on its own declaration) must be named;
/// comments, test modules and the allowed callers must not be; and an empty
/// entry-point shim cannot satisfy the second assertion.
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
        "agent_runtime.rs".to_string(),
        "fn provision_agent_definitions_from_root(dst: &Path) {\n    \
         provision_fleet_agents_into(dst, &tracked);\n}\n"
            .to_string(),
    );
    sources.insert(
        "new_spawn_path.rs".to_string(),
        "async fn spawn_new_kind(workdir: &str) {\n    \
         // provision_fleet_skills_for_session(workdir) — a comment is not a call\n    \
         crate::fleet_commands::provision_fleet_commands_for_session(\n        workdir,\n    );\n}\n\
         fn one_liner(d: &Path) { crate::fleet_agents::provision_fleet_agents_into(d, &t); }\n\
         #[cfg(test)]\nmod tests {\n    fn t() { super::provision_agent_definitions(\"x\"); }\n}\n"
            .to_string(),
    );
    assert_eq!(
        provisioner_calls_outside_their_caller(&sources),
        vec![
            (
                "provision_fleet_commands_for_session(",
                site("new_spawn_path.rs", "spawn_new_kind")
            ),
            (
                "provision_fleet_agents_into(",
                site("new_spawn_path.rs", "one_liner")
            ),
        ],
    );
    assert!(provisioners_missing_from_the_entry_point(&sources).is_empty());

    sources.insert(
        ENTRY_FILE.to_string(),
        format!("pub(crate) fn {ENTRY_FN}(_workdir: &str) {{}}\n"),
    );
    assert_eq!(
        provisioners_missing_from_the_entry_point(&sources),
        vec![
            "provision_agent_definitions(",
            "provision_fleet_commands_for_session(",
            "provision_fleet_skills_for_session(",
        ]
    );
}
