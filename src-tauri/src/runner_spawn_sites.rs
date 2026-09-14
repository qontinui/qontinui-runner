//! Runner spawn-site guard — `runner_spawn_sites.txt` (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, Phase 3).
//!
//! Every non-test fn that reaches a session spawn — `TerminalManager::create`,
//! `create_tracked_terminal_session_backend`, `ClaudeSession::spawn`, or a
//! declared helper wrapping one of them — must be listed with its class. An
//! `autonomous` site must pass coord's device drain gate on the way in; an
//! `operator` site must be one of the runner UI's own Tauri commands. A new spawn
//! path is therefore red until it declares how the drain applies to it — the
//! mechanism coord's `spawn_admission_sites.txt` applies to coord's publish seam.
//!
//! Test-only: the whole module is `#[cfg(test)]` in `main.rs`.

use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SITES_REL: &str = "src/runner_spawn_sites.txt";
const SELF_FILE: &str = "runner_spawn_sites.rs";
const CLASSES: &[&str] = &["autonomous", "operator", "helper", "exempt"];

/// Calls that pass the drain gate. `authorize_spawn` and the fan-out variants
/// run [`crate::coord_drain_state::drain_gate`] first for a `DrainAdmission::Origin`.
const GATE_CALLS: &[&str] = &[
    "drain_gate(",
    "drain_gate_for_work(",
    "authorize_spawn(",
    "authorize_fanout_spawn(",
    "authorize_fanout_spawn_with_budget(",
];

/// Text that makes a gate call NOT a drain gate for an autonomous site.
const GATE_DEFEATERS: &[&str] = &[
    "DrainAdmission::Exempt",
    "SpawnOrigin::OperatorTerminal",
    "SpawnOrigin::OperatorChat",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    class: String,
    detail: String,
}

type Sources = BTreeMap<String, String>;
type Rows = BTreeMap<(String, String), Row>;

fn src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn load_sources() -> Sources {
    let root = src_root();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(
        files.len() > 100,
        "walked {} files — a vacuous scan",
        files.len()
    );
    files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(&p).expect("read source"))
        })
        .collect()
}

fn is_comment(line: &str) -> bool {
    let s = line.trim_start();
    s.starts_with("//") || s.starts_with('*') || s.starts_with("/*")
}

/// Line spans covered by a `#[cfg(test)] mod … { … }`.
fn test_spans(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("#[cfg(test)]") {
            continue;
        }
        let Some(open) = (i + 1..(i + 8).min(lines.len()))
            .find(|&j| lines[j].contains("mod ") && lines[j].contains('{'))
        else {
            continue;
        };
        let mut depth = 0usize;
        for (j, l) in lines.iter().enumerate().skip(open) {
            if is_comment(l) {
                continue;
            }
            depth += l.matches('{').count();
            depth = depth.saturating_sub(l.matches('}').count());
            if depth == 0 {
                spans.push((open, j + 1));
                break;
            }
        }
    }
    spans
}

fn fn_decl_re() -> Regex {
    Regex::new(r"^(\s*)(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
        .expect("fn decl regex")
}

/// The spawn primitives. Multi-line: a receiver on one line and `.create(` on
/// the next is still a call.
fn primitive_res() -> Vec<Regex> {
    [
        r"\b(?:terminal_manager|create_manager|tm|manager)\s*\.\s*create\s*\(",
        r"\bcreate_tracked_terminal_session_backend\s*\(",
        r"\bClaudeSession::spawn\s*\(",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("primitive regex"))
    .collect()
}

/// A literal call token (`foo(`, `.start_inner(`) as a regex that tolerates
/// whitespace before the paren.
fn token_re(token: &str) -> Regex {
    let (head, paren) = match token.strip_suffix('(') {
        Some(h) => (h, r"\s*\("),
        None => (token, ""),
    };
    let boundary = if head.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        r"\b"
    } else {
        ""
    };
    Regex::new(&format!("{boundary}{}{paren}", regex::escape(head))).expect("token regex")
}

/// `(file, fn)` for every production call of any pattern in `patterns`.
fn scan_calls(sources: &Sources, patterns: &[Regex]) -> BTreeSet<(String, String)> {
    let decl = fn_decl_re();
    let mut out = BTreeSet::new();
    for (file, src) in sources {
        if file == SELF_FILE {
            continue;
        }
        let lines: Vec<&str> = src.lines().collect();
        let spans = test_spans(&lines);
        // Blank comment and test-module lines, keeping line numbering, so a
        // multi-line call can be matched on the joined text.
        let code: Vec<&str> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if is_comment(l) || spans.iter().any(|(s, e)| i >= *s && i < *e) {
                    ""
                } else {
                    *l
                }
            })
            .collect();
        let joined = code.join("\n");
        for re in patterns {
            for m in re.find_iter(&joined) {
                let line = joined[..m.start()].matches('\n').count();
                if decl.is_match(code[line]) {
                    continue;
                }
                let enclosing = (0..=line)
                    .rev()
                    .find_map(|j| decl.captures(lines[j]).map(|c| c[2].to_string()))
                    .unwrap_or_else(|| "<file scope>".to_string());
                out.insert((file.clone(), enclosing));
            }
        }
    }
    out
}

/// Every site: primitive callers, then callers of each `helper` row's token.
fn scan_sites(sources: &Sources, rows: &Rows) -> BTreeSet<(String, String)> {
    let mut patterns = primitive_res();
    patterns.extend(
        rows.values()
            .filter(|r| r.class == "helper")
            .map(|r| token_re(&r.detail)),
    );
    scan_calls(sources, &patterns)
}

/// The text of `fn name` in `src`, comment lines dropped: its declaration line
/// to the next fn declaration at the same or a shallower indentation.
fn fn_body(src: &str, name: &str) -> Option<String> {
    let decl = fn_decl_re();
    let lines: Vec<&str> = src.lines().collect();
    let (start, indent) = lines.iter().enumerate().find_map(|(i, l)| {
        decl.captures(l)
            .filter(|c| &c[2] == name)
            .map(|c| (i, c[1].len()))
    })?;
    let end = (start + 1..lines.len())
        .find(|&j| {
            decl.captures(lines[j])
                .is_some_and(|c| c[1].len() <= indent)
        })
        .unwrap_or(lines.len());
    Some(
        lines[start..end]
            .iter()
            .filter(|l| !is_comment(l))
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// A chain hop: `fn` in the site's file, or `path.rs::fn` in another file.
fn resolve_hop<'a>(hop: &'a str, site_file: &'a str) -> (&'a str, &'a str) {
    match hop.rsplit_once("::") {
        Some((file, func)) if file.ends_with(".rs") => (file, func),
        _ => (site_file, hop),
    }
}

/// Autonomous rows whose gate chain does not hold. PURE over the sources.
fn autonomous_sites_bypassing_the_gate(sources: &Sources, rows: &Rows) -> Vec<String> {
    let mut out = Vec::new();
    for ((file, func), row) in rows.iter().filter(|(_, r)| r.class == "autonomous") {
        let hops: Vec<&str> = row.detail.split('>').map(str::trim).collect();
        if hops.iter().any(|h| h.is_empty() || *h == "-") {
            out.push(format!("{file}:{func} (empty gate chain)"));
            continue;
        }
        let (gate_file, gate_fn) = resolve_hop(hops[0], file);
        let Some(gate_body) = sources.get(gate_file).and_then(|s| fn_body(s, gate_fn)) else {
            out.push(format!(
                "{file}:{func} (gate fn {gate_file}:{gate_fn} not found)"
            ));
            continue;
        };
        if !GATE_CALLS.iter().any(|c| gate_body.contains(c)) {
            out.push(format!("{file}:{func} (no drain gate call in {gate_fn})"));
            continue;
        }
        if let Some(d) = GATE_DEFEATERS.iter().find(|d| gate_body.contains(*d)) {
            out.push(format!("{file}:{func} ({gate_fn} gates with {d})"));
            continue;
        }
        // Each hop calls the next; the last hop calls the site unless it is it.
        let mut chain: Vec<(&str, &str)> = hops.iter().map(|h| resolve_hop(h, file)).collect();
        if chain.last() != Some(&(file.as_str(), func.as_str())) {
            chain.push((file.as_str(), func.as_str()));
        }
        for pair in chain.windows(2) {
            let ((caller_file, caller), (_, callee)) = (pair[0], pair[1]);
            let calls = sources
                .get(caller_file)
                .and_then(|s| fn_body(s, caller))
                .is_some_and(|b| token_re(&format!("{callee}(")).is_match(&b));
            if !calls {
                out.push(format!(
                    "{file}:{func} (gate chain broken: {caller} does not call {callee})"
                ));
                break;
            }
        }
    }
    out
}

/// Operator rows that are not a `#[tauri::command]`.
fn operator_sites_not_tauri_commands(sources: &Sources, rows: &Rows) -> Vec<String> {
    let decl = fn_decl_re();
    rows.iter()
        .filter(|(_, r)| r.class == "operator")
        .filter_map(|((file, func), _)| {
            let src = sources.get(file)?;
            let lines: Vec<&str> = src.lines().collect();
            let at = lines
                .iter()
                .position(|l| decl.captures(l).is_some_and(|c| &c[2] == func));
            // Only the attribute/doc block directly above the declaration: a
            // blank or code line ends it, so a neighbouring fn's attribute
            // never counts.
            let is_command = at.is_some_and(|i| {
                lines[..i]
                    .iter()
                    .rev()
                    .take_while(|l| {
                        let t = l.trim_start();
                        t.starts_with("#[") || t.starts_with("///")
                    })
                    .any(|l| l.trim_start().starts_with("#[tauri::command"))
            });
            (!is_command).then(|| format!("{file}:{func}"))
        })
        .collect()
}

fn parse_sites(text: &str) -> Rows {
    let mut out = BTreeMap::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cells: Vec<&str> = line.splitn(4, '|').map(str::trim).collect();
        assert_eq!(
            cells.len(),
            4,
            "runner_spawn_sites.txt:{}: expected `file | fn | class | detail`, got {line:?}",
            n + 1
        );
        assert!(
            CLASSES.contains(&cells[2]),
            "runner_spawn_sites.txt:{}: class {:?} is not one of {CLASSES:?}",
            n + 1,
            cells[2]
        );
        assert!(
            !cells[3].is_empty(),
            "runner_spawn_sites.txt:{}: empty detail",
            n + 1
        );
        if cells[2] == "exempt" || cells[2] == "helper" {
            assert_ne!(
                cells[3],
                "-",
                "runner_spawn_sites.txt:{}: an {} row must say {}",
                n + 1,
                cells[2],
                if cells[2] == "exempt" {
                    "why"
                } else {
                    "its call token"
                }
            );
        }
        let prev = out.insert(
            (cells[0].to_string(), cells[1].to_string()),
            Row {
                class: cells[2].to_string(),
                detail: cells[3].to_string(),
            },
        );
        assert!(
            prev.is_none(),
            "runner_spawn_sites.txt:{}: duplicate row",
            n + 1
        );
    }
    out
}

fn load_rows() -> Rows {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SITES_REL);
    parse_sites(&std::fs::read_to_string(path).expect("read runner_spawn_sites.txt"))
}

#[test]
fn every_spawn_site_is_listed_and_no_row_is_a_ghost() {
    let sources = load_sources();
    let rows = load_rows();
    let found = scan_sites(&sources, &rows);
    assert!(
        found.len() >= 20,
        "found only {} spawn sites — scan broken?",
        found.len()
    );
    let listed: BTreeSet<(String, String)> = rows.keys().cloned().collect();
    let unlisted: Vec<_> = found.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "spawn sites with no runner_spawn_sites.txt row — declare `file | fn | class | detail` \
         (autonomous sites must pass the coord device drain gate): {unlisted:#?}"
    );
    let ghosts: Vec<_> = listed.difference(&found).collect();
    assert!(
        ghosts.is_empty(),
        "rows naming no current spawn site — delete them: {ghosts:#?}"
    );
}

#[test]
fn every_autonomous_site_passes_the_drain_gate() {
    let rows = load_rows();
    assert!(
        rows.values().filter(|r| r.class == "autonomous").count() >= 10,
        "too few autonomous rows — the guard would be vacuous"
    );
    let violations = autonomous_sites_bypassing_the_gate(&load_sources(), &rows);
    assert!(
        violations.is_empty(),
        "autonomous spawn sites that bypass the coord device drain gate: {violations:#?}"
    );
}

#[test]
fn every_operator_site_is_a_tauri_command() {
    let violations = operator_sites_not_tauri_commands(&load_sources(), &load_rows());
    assert!(
        violations.is_empty(),
        "`operator` is reserved for the runner UI's own Tauri commands (D3) — an HTTP or \
         relay door is autonomous: {violations:#?}"
    );
}

/// Mutation check: delete the gate call from the steward start path and the
/// guard must go red on exactly that row.
#[test]
fn removing_the_drain_gate_from_start_steward_fails_the_guard() {
    let mut sources = load_sources();
    let rows = load_rows();
    let file = "mcp/steward.rs".to_string();
    let src = sources.get(&file).expect("mcp/steward.rs").clone();
    let marker = "drain_gate_for_work(";
    let at = src
        .find("async fn start_steward(")
        .and_then(|fn_at| src[fn_at..].find(marker).map(|m| fn_at + m))
        .expect("start_steward calls drain_gate_for_work");
    let mutated = format!("{}removed_gate({}", &src[..at], &src[at + marker.len()..]);
    sources.insert(file, mutated);
    assert_eq!(
        autonomous_sites_bypassing_the_gate(&sources, &rows),
        vec!["mcp/steward.rs:start_steward (no drain gate call in start_steward)".to_string()],
        "exactly the mutated site must be reported"
    );
}

#[test]
fn a_new_primitive_caller_and_a_helper_caller_are_found_but_test_modules_are_not() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "new_door.rs".to_string(),
        "pub async fn open_door(tm: &T) {\n    let _ = tm\n        .create(a, b);\n}\n\
         fn recipe() {\n    let _ = ClaudeSession::spawn(x);\n}\n\
         fn uses_recipe() {\n    recipe();\n}\n\
         #[cfg(test)]\nmod tests {\n    fn t() { recipe(); tm.create(a); }\n}\n"
            .to_string(),
    );
    let mut rows = BTreeMap::new();
    rows.insert(
        ("new_door.rs".to_string(), "recipe".to_string()),
        Row {
            class: "helper".into(),
            detail: "recipe(".into(),
        },
    );
    let found: Vec<_> = scan_sites(&sources, &rows).into_iter().collect();
    assert_eq!(
        found,
        vec![
            ("new_door.rs".to_string(), "open_door".to_string()),
            ("new_door.rs".to_string(), "recipe".to_string()),
            ("new_door.rs".to_string(), "uses_recipe".to_string()),
        ]
    );
}

#[test]
fn a_broken_chain_an_exempt_admission_or_an_operator_origin_fails_the_guard() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "x.rs".to_string(),
        "async fn gate_then_forget() {\n    let d = authorize_spawn(None, p, SpawnOrigin::Steward).await;\n}\n\
         async fn orphan_site() {\n    tm.create(a);\n}\n\
         async fn exempted() {\n    authorize_spawn(None, p, DrainAdmission::Exempt { why: \"x\" }).await;\n    tm.create(a);\n}\n\
         async fn as_operator() {\n    authorize_spawn(None, p, SpawnOrigin::OperatorChat).await;\n    tm.create(a);\n}\n\
         async fn gated() {\n    if drain_gate(o).allows() { helper_call(); }\n}\n\
         fn helper_call() {\n    tm.create(a);\n}\n"
            .to_string(),
    );
    let row = |detail: &str| Row {
        class: "autonomous".into(),
        detail: detail.into(),
    };
    let mut rows = BTreeMap::new();
    rows.insert(
        ("x.rs".into(), "orphan_site".into()),
        row("gate_then_forget"),
    );
    rows.insert(("x.rs".into(), "exempted".into()), row("exempted"));
    rows.insert(("x.rs".into(), "as_operator".into()), row("as_operator"));
    rows.insert(("x.rs".into(), "helper_call".into()), row("gated"));
    assert_eq!(
        autonomous_sites_bypassing_the_gate(&sources, &rows),
        vec![
            "x.rs:as_operator (as_operator gates with SpawnOrigin::OperatorChat)".to_string(),
            "x.rs:exempted (exempted gates with DrainAdmission::Exempt)".to_string(),
            "x.rs:orphan_site (gate chain broken: gate_then_forget does not call orphan_site)"
                .to_string(),
        ],
        "the gated chain passes; the other three fail for their own reason"
    );
}

#[test]
fn an_operator_row_must_be_a_tauri_command() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "c.rs".to_string(),
        "#[tauri::command]\npub async fn ui_new_terminal() {}\n\npub async fn http_door() {}\n"
            .to_string(),
    );
    let mut rows = BTreeMap::new();
    for f in ["ui_new_terminal", "http_door"] {
        rows.insert(
            ("c.rs".to_string(), f.to_string()),
            Row {
                class: "operator".into(),
                detail: "-".into(),
            },
        );
    }
    assert_eq!(
        operator_sites_not_tauri_commands(&sources, &rows),
        vec!["c.rs:http_door".to_string()]
    );
}
