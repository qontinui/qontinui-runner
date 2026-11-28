//! Spawn-key guard (plan
//! `2026-11-10-the-spawn-contract-names-the-runners-by-name-spawn-keys`,
//! Phase 3): the runner's by-name spawn keys and the shared spawn contract's
//! `runner_keys` are the same set, checked in both directions.
//!
//! The runner vetoes a spawn whenever a DISABLED agent-registry row carries the
//! name the spawn is authorized under (`agent_authorization`
//! `most_restrictive_disable`). coord's retire door and web's `--retire-orphans`
//! refuse to disable any name in the contract's `runner_keys`, so a key the
//! runner authorizes under that the contract does not list is a row an operator
//! could retire and so block every spawn under it.
//!
//! * Direction 1 — every STATIC key the runner authorizes under is in
//!   `contract().runner_keys`: each `authorize_spawn(Some("<literal>"), ..)`
//!   call and each arm of `mcp/sessions.rs` `role_slash_command` (the closed
//!   allow-list of `/sessions/spawn` roles, which `authorize_spawn` receives as
//!   `req.role`).
//! * Direction 2 — every `runner_keys` entry has such a site, so a key that is
//!   no longer authorized anywhere does not linger in the contract.
//!
//! Test-only (`#[cfg(test)]` in `main.rs`); the source walk is
//! `runner_spawn_sites::load_sources`.
//!
//! ## Known limits (a source scan, not a type check)
//!
//! * Only a string LITERAL directly inside `Some(..)` as the first argument of
//!   `authorize_spawn(` is seen. A key reached through a `const`, a variable or
//!   a `format!` is not; the looping supervisor's `Some(&def.name)` is such a
//!   dynamic site, and its display names cannot be stored under the registry
//!   grammar, so they need no entry (plan S2).
//! * `authorize_fanout_spawn*` calls take no agent name and are not scanned.
//! * Comments and `#[cfg(test)] mod` bodies are blanked before the scan, and this
//!   file is not scanned (its fixtures spell call sites).
//! * Direction 2 counts a `role_slash_command` arm as a site without proving that
//!   `mcp/sessions.rs` still passes `req.role` to `authorize_spawn`.

use crate::runner_spawn_sites::{comment_mask, load_sources, test_spans, Sources};
use regex::Regex;
use std::collections::BTreeSet;

const SESSIONS_REL: &str = "mcp/sessions.rs";

/// `source` with comment lines and `#[cfg(test)] mod` bodies blanked, lines kept.
fn code_only(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let comments = comment_mask(&lines);
    let spans = test_spans(&lines);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let in_test = spans.iter().any(|&(a, b)| i >= a && i < b);
            if comments[i] || in_test {
                ""
            } else {
                *l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `authorize_spawn(Some("<literal>"), ..)` literal, with its file.
fn literal_sites(sources: &Sources) -> BTreeSet<(String, String)> {
    let re = Regex::new(r#"authorize_spawn\(\s*Some\(\s*"([^"]+)"\s*\)"#).expect("regex");
    sources
        .iter()
        // This file's own fixtures spell call sites; they are not sites.
        .filter(|(rel, _)| rel.as_str() != "spawn_key_guard.rs")
        .flat_map(|(rel, src)| {
            re.captures_iter(&code_only(src))
                .map(|c| (rel.clone(), c[1].to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Every string-literal arm of `role_slash_command`'s `match`.
fn role_arms(sources: &Sources) -> BTreeSet<String> {
    let src = sources
        .get(SESSIONS_REL)
        .unwrap_or_else(|| panic!("{SESSIONS_REL} is not among the scanned sources"));
    let code = code_only(src);
    let start = code
        .find("fn role_slash_command")
        .expect("role_slash_command exists");
    let body = &code[start..];
    let end = body.find("\n}\n").expect("role_slash_command ends");
    let arm = Regex::new(r#"(?m)^\s*"([^"]+)"\s*=>"#).expect("regex");
    arm.captures_iter(&body[..end])
        .map(|c| c[1].to_string())
        .collect()
}

/// Every static key the runner authorizes a spawn under.
fn runner_sites(sources: &Sources) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = literal_sites(sources).into_iter().map(|(_, k)| k).collect();
    keys.extend(role_arms(sources));
    keys
}

/// The two directions against `declared`. Empty = clean.
fn violations(sources: &Sources, declared: &BTreeSet<String>) -> Vec<String> {
    let sites = runner_sites(sources);
    let mut out = Vec::new();
    for key in sites.difference(declared) {
        out.push(format!(
            "the runner authorizes a spawn under `{key}`, which is not in the spawn \
             contract's runner_keys: an operator-authored row of that name could be \
             retired and would then block every spawn under it. Add it to \
             qontinui-schemas rust/src/spawn_authorization.json `runner_keys`"
        ));
    }
    for key in declared.difference(&sites) {
        out.push(format!(
            "runner_keys declares `{key}`, but no authorize_spawn(Some(\"{key}\"), ..) \
             site or role_slash_command arm in the runner uses it: remove it from the \
             contract or restore the site"
        ));
    }
    out
}

fn contract_keys() -> BTreeSet<String> {
    qontinui_types::spawn_authorization::contract()
        .runner_keys
        .iter()
        .map(|k| k.agent_name.clone())
        .collect()
}

#[test]
fn runner_by_name_spawn_keys_match_the_contract_in_both_directions() {
    let declared = contract_keys();
    assert!(!declared.is_empty(), "the contract declares no runner_keys");
    let sources = load_sources();
    let v = violations(&sources, &declared);
    assert!(v.is_empty(), "{}", v.join("\n"));
}

/// The scan must see the sites it exists to guard, or a green run is vacuous.
#[test]
fn the_scan_sees_the_known_sites() {
    let sources = load_sources();
    let literals: BTreeSet<String> = literal_sites(&sources)
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    assert!(literals.contains("runner-instance"), "{literals:?}");
    assert!(literals.contains("orchestration-fix-agent"), "{literals:?}");
    assert_eq!(
        role_arms(&sources),
        ["auto-review", "implement-plan", "summarize-session"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );
}

fn synthetic(extra: &str) -> Sources {
    let mut s = load_sources();
    s.insert("synthetic_site.rs".into(), extra.into());
    s
}

/// Mutation 1: an uncontracted literal turns the guard red.
#[test]
fn an_uncontracted_literal_site_is_reported() {
    let sources = synthetic(
        "async fn f() {\n    authorize_spawn(\n        Some(\"brand-new-key\"),\n        p,\n        o,\n    )\n    .await;\n}\n",
    );
    let v = violations(&sources, &contract_keys());
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("brand-new-key"), "{v:?}");
}

/// Mutation 2: a contracted key with no site turns the guard red.
#[test]
fn a_contracted_key_with_no_site_is_reported() {
    let mut declared = contract_keys();
    declared.insert("key-with-no-site".into());
    let v = violations(&load_sources(), &declared);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("key-with-no-site"), "{v:?}");
}

/// A literal in a comment or a `#[cfg(test)] mod` is not a site.
#[test]
fn comments_and_test_modules_are_not_sites() {
    let sources = synthetic(
        "// authorize_spawn(Some(\"in-comment\"), p, o)\n#[cfg(test)]\nmod tests {\n    fn t() {\n        authorize_spawn(Some(\"in-test\"), p, o);\n    }\n}\n",
    );
    assert_eq!(violations(&sources, &contract_keys()), Vec::<String>::new());
}

/// A new `role_slash_command` arm absent from the contract turns the guard red.
#[test]
fn an_uncontracted_role_arm_is_reported() {
    let mut sources = load_sources();
    let sessions = sources.get_mut(SESSIONS_REL).expect("sessions.rs");
    *sessions = sessions.replacen(
        "        _ => None,\n    }\n}",
        "        \"new-role\" => Some(\"/new-role\"),\n        _ => None,\n    }\n}",
        1,
    );
    let v = violations(&sources, &contract_keys());
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("new-role"), "{v:?}");
}
