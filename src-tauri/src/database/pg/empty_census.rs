//! Ratchet: a PostgreSQL read whose error arm becomes an empty or default
//! value is a NEW defect unless it is allowlisted here with a reason.
//!
//! Plan `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`,
//! Phase 6 ("A down database never reads as empty data"). While the breaker is
//! open every `database/pg` method fails fast with an error carrying
//! `DATABASE_UNAVAILABLE:`; a caller that turns that error into `[]`, `0`,
//! `false` or `""` reports the outage as data ("no AI running", "0 triggers",
//! "no conversation history"). Such a read must instead propagate the outage
//! (a 503 through the route's error path), render the field as explicit
//! `null` / "unknown", or — in a background loop — skip the tick. The
//! outage-aware spellings are [`super::unavailable::UnlessUnavailable`]
//! (`default_unless_unavailable`, `or_unless_unavailable`,
//! `or_else_unless_unavailable`), which this census does not match.
//!
//! # The census pattern
//!
//! A site is `<receiver>.<method>(<args>).await.<fallback>` — multi-line
//! aware, since rustfmt splits the chain one call per line — where:
//!
//! - the call start matches [`CALL_START`]; `<args>` is paren-balanced (string
//!   literals skipped), and what follows the closing paren matches
//!   [`FALLBACK`] (`unwrap_or_default()`, `unwrap_or_else(|…|`,
//!   `unwrap_or_else(path)`, `unwrap_or(`, `ok()`);
//! - `<method>` is a `pub async fn` declared under `src/database/pg/` (the
//!   `PgDb` method layer);
//! - `<receiver>` is `db`, `PgDb::global()`, `PgDb::try_global()`, any
//!   identifier or zero-argument accessor call whose name contains `pg`
//!   (`pg_db`, `pg`, `pg_clone`, `cra_pg_db`, `state.pg_db()`, …;
//!   `state.app_state.pg_db.x()` matches at `pg_db`), or `self` inside
//!   `src/database/pg/` itself. Candidate starts may overlap (the scan resumes
//!   at each match's method name), so `state.pg_db().x()` is seen at `pg_db()`.
//!
//! The walk is `src-tauri/src/**/*.rs` (`clorinde` excluded, this file
//! excluded because it spells the needles). Census at Phase 6, base
//! `6043840b2`: 108 sites before (pg_db 47, pg 32, other `*pg*` receivers 23
//! of which 11 the `pg_db()` accessor, self 5, db 1; PgDb::global() /
//! PgDb::try_global() 0), 26 after — exactly [`ALLOWLIST`].
//!
//! Allowlist entries are keyed by file + the site's whitespace-stripped text
//! (from the receiver through the fallback head), never by line number, and
//! carry the exact number of sites with that key in that file. The test fails
//! when a site is not covered (a new instance) and when an entry covers more
//! sites than exist (a stale entry): lower or delete the entry when a site is
//! fixed; never raise it.
//!
//! # Known limits
//!
//! The scan is textual, not a parse. Char literals (`')'`), raw strings and
//! comments inside a call's arguments can unbalance the paren match and hide
//! a site; a fallback reached through an intermediate adapter
//! (`.await.map(f).unwrap_or_default()`), an `if let Ok(..)` / `match` arm, or
//! a receiver whose name does not contain `pg` (a `conn`, a renamed binding)
//! is not matched. Two sites with identical text in one file share one
//! allowlist entry and are told apart only by its count.
//!
//! Declared `#[cfg(test)]` in `database/pg/mod.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use regex::Regex;

/// The receiver + method + open paren of a candidate PG call.
/// The receiver is an identifier, optionally a zero-argument accessor call.
const CALL_START: &str = r"\b(PgDb::global\(\)|PgDb::try_global\(\)|[A-Za-z_][A-Za-z0-9_]*(?:\(\))?)\s*\.\s*([a-z_][a-z0-9_]*)\s*(?:::<[^>]*>\s*)?\(";

/// What must follow the call's closing paren for it to be a census site.
const FALLBACK: &str = r"^\s*\.await\s*\.\s*(unwrap_or_default\(\)|unwrap_or_else\(\s*\|[^|]*\||unwrap_or\(|ok\(\)|unwrap_or_else\(\s*[A-Za-z_][A-Za-z0-9_:]*\s*\))";

/// Declares the `PgDb` method names (the method layer lives in `database/pg/`).
const PG_METHOD: &str = r"pub(?:\(crate\))?\s+async\s+fn\s+([a-z_0-9]+)";

/// One reasoned exception: `count` sites in `file` whose whitespace-stripped
/// text is `site`.
struct Allowed {
    /// Path relative to `src-tauri/src`, `/`-separated.
    file: &'static str,
    site: &'static str,
    count: usize,
    reason: &'static str,
}

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "instance_manager.rs",
        site: r"pg_db.get_all_runner_instances().await.unwrap_or_default()",
        count: 2,
        reason: "allocate_port: DB ports are a reservation hint beside the live is_port_in_use \
                 check; get_unified_instances: DB rows are an addendum to the settings.json slots, \
                 each probed live",
    },
    Allowed {
        file: "scheduler_service.rs",
        site: r"pg.record_watcher_run(watcher_id,Some(&result_text)).await.unwrap_or_else(|e|",
        count: 1,
        reason: "a write whose error is logged; it produces no value for anyone to read",
    },
    Allowed {
        file: "mcp/canvas.rs",
        site: r"pg_db.get_running_task_runs(None).await.unwrap_or_default()",
        count: 1,
        reason: "an unreadable running task falls back to the literal task_run_id \"unknown\"",
    },
    Allowed {
        file: "mcp/contexts.rs",
        site: r"pg_db.get_project_context(pid,None,15).await.unwrap_or_default()",
        count: 1,
        reason: "optional prompt enrichment, documented to yield None when PG is unavailable",
    },
    Allowed {
        file: "mcp/contexts.rs",
        site: r"pg_db.search_observations(query,None,10).await.unwrap_or_default()",
        count: 1,
        reason: "optional prompt enrichment, documented to yield None when PG is unavailable",
    },
    Allowed {
        file: "mcp/file_registry.rs",
        site: r"pg_db.get_sessions_for_files(&candidates).await.unwrap_or_default()",
        count: 1,
        reason: "the #[cfg(test)] build_report helper, not a caller-visible path (the \
                 probe_conflicts handler it mirrors answers 503)",
    },
    Allowed {
        file: "memory/unified_query.rs",
        site: r"pg.get_mental_models(limit).await.unwrap_or_default()",
        count: 1,
        reason: "secondary score-boost read; the primary observation search just before it owns \
                 the failure path",
    },
    Allowed {
        file: "unified_workflow_executor/loop_controller.rs",
        site: r"cra_pg_db.get_source_workflow_name(&cra_task_run_id,&cra_workflow_name).await.unwrap_or_else(|_|",
        count: 1,
        reason: "falls back to the run's own workflow name (a real value, not empty); the \
                 analysis that follows reads PG and fails on its own",
    },
    Allowed {
        file: "unified_workflow_executor/phases/mod.rs",
        site: r#"pg_db.list_task_knowledge(execution_id,Some("observation"),false,false).await.unwrap_or_default()"#,
        count: 1,
        reason: "iteration-history prompt enrichment; a missing section degrades the prompt and \
                 nothing acts on its absence",
    },
    Allowed {
        file: "unified_workflow_executor/phases/mod.rs",
        site: r#"pg_db.list_task_knowledge(execution_id,Some("solution"),false,false).await.unwrap_or_default()"#,
        count: 1,
        reason: "iteration-history prompt enrichment; a missing section degrades the prompt and \
                 nothing acts on its absence",
    },
    Allowed {
        file: "workflow_generation/generator.rs",
        site: r"pg_clone.list_user_skills().await.unwrap_or_default()",
        count: 1,
        reason: "generation prompt enrichment with a documented built-in-skills fallback",
    },
    Allowed {
        file: "workflow_generation/generator.rs",
        site: r#"pg_clone.get_active_rules("verification",Some("check_rules")).await.unwrap_or_default()"#,
        count: 1,
        reason: "generation prompt enrichment; no rules means the section is omitted",
    },
    Allowed {
        file: "workflow_generation/schema_context.rs",
        site: r"pg_clone.search_unified_workflows_for_examples(&desc,3).await.unwrap_or_default()",
        count: 2,
        reason: "RAG examples for the generation prompt; none means the section is omitted",
    },
    Allowed {
        file: "workflow_generation/schema_context.rs",
        site: r"pg_clone.load_knowledge_for_step_types(&refs).await.unwrap_or_default()",
        count: 1,
        reason: "step-type knowledge for the generation prompt; none means the section is omitted",
    },
    Allowed {
        file: "workflow_generation/hardener/mod.rs",
        site: r#"pg_clone.get_active_rules("hardener",Some("conversion_rules")).await.unwrap_or_default()"#,
        count: 1,
        reason: "hardener prompt rules with a documented built-in fallback",
    },
    Allowed {
        file: "workflow_generation/hardener/mod.rs",
        site: r#"pg_clone.get_active_rules("hardener",Some("critical_rules")).await.unwrap_or_default()"#,
        count: 1,
        reason: "hardener prompt rules with a documented built-in fallback",
    },
    Allowed {
        file: "graphql/subscription.rs",
        site: r"pg_db.get_task_run(&trid).await.ok()",
        count: 1,
        reason: "subscription poll: an unreadable status is not terminal, so polling continues \
                 (TTL-bounded); nothing acts on the None",
    },
    Allowed {
        file: "mcp/unified_workflows.rs",
        site: r"pg_db.get_unified_workflow(&id).await.ok()",
        count: 2,
        reason: "update/delete feedback-tracking lookup that only logs; the update/delete itself \
                 reports its own database error",
    },
    Allowed {
        file: "unified_workflow_executor/multi_agent_pipeline_loop.rs",
        site: r"pg_db.get_active_prompt(agent_type).await.ok()",
        count: 1,
        reason: "prompt-registry variant with the documented default-prompt fallback",
    },
    Allowed {
        file: "unified_workflow_executor/task_lifecycle.rs",
        site: r"pg2.get_workflow_name_for_task_run(&exec_id2).await.ok()",
        count: 1,
        reason: "best-effort convergence snapshot: skipped on any failure, nothing acts on the \
                 absence (the store would fail on the same outage)",
    },
    Allowed {
        file: "unified_workflow_executor/phases/mod.rs",
        site: r"pg_db.get_universal_fixes_by_text(&query_text,5).await.ok()",
        count: 1,
        reason: "universal-fix prompt enrichment; a missing section degrades the prompt only",
    },
    Allowed {
        file: "unified_workflow_executor/phases/mod.rs",
        site: r"pg_db.get_universal_fixes(5).await.ok()",
        count: 1,
        reason: "universal-fix prompt enrichment; a missing section degrades the prompt only",
    },
    Allowed {
        file: "unified_workflow_executor/phases/mod.rs",
        site: r"pg.get_impact_analysis(&wf_c,&fp_c).await.ok()",
        count: 1,
        reason: "impact-radius prompt enrichment; a missing line degrades the prompt only",
    },
];

/// `src-tauri/src` holds ~1600 `.rs` files; a walker that lost the tree must
/// fail loudly rather than report a clean zero.
const MIN_FILES_WALKED: usize = 1000;
/// `database/pg` declares ~1080 `pub async fn`s.
const MIN_PG_METHODS: usize = 500;

const EXCLUDED_DIR_NAMES: [&str; 1] = ["clorinde"];
const THIS_FILE: &str = "database/pg/empty_census.rs";

/// One census site.
#[derive(Debug)]
struct Site {
    line: usize,
    /// Whitespace-stripped text from the receiver through the fallback head.
    key: String,
}

/// Every `.rs` file under `src-tauri/src` as `(relative path, contents)`.
fn walk_sources() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
                if !EXCLUDED_DIR_NAMES.contains(&name) {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            if rel == THIS_FILE {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            out.push((rel, text));
        }
    }
    out
}

/// The `PgDb` method names: every `pub async fn` declared under `database/pg/`.
fn pg_method_names(sources: &[(String, String)]) -> BTreeSet<String> {
    let decl = Regex::new(PG_METHOD).expect("PG_METHOD");
    sources
        .iter()
        .filter(|(rel, _)| rel.starts_with("database/pg/"))
        .flat_map(|(_, text)| {
            decl.captures_iter(text)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn receiver_in_scope(receiver: &str, rel: &str) -> bool {
    matches!(receiver, "db" | "PgDb::global()" | "PgDb::try_global()")
        || receiver.to_ascii_lowercase().contains("pg")
        || (receiver == "self" && rel.starts_with("database/pg/"))
}

/// Byte index just past the paren matching the `(` at `open`, skipping string
/// literals (with `\` escapes).
fn close_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes.get(i).copied()? {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes.get(i).copied()? != b'"' {
                    if bytes.get(i).copied()? == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The census sites in one file.
fn census_sites(rel: &str, text: &str, methods: &BTreeSet<String>) -> Vec<Site> {
    let start = Regex::new(CALL_START).expect("CALL_START");
    let fallback = Regex::new(FALLBACK).expect("FALLBACK");
    let bytes = text.as_bytes();
    let mut sites = Vec::new();
    // Overlapping candidates: resume at each match's METHOD name, so in
    // `state.pg_db().x()` the non-PG `state.pg_db(` candidate does not hide
    // the `pg_db().x(` one.
    let mut pos = 0;
    while let Some(caps) = start.captures_at(text, pos) {
        let whole = caps.get(0).expect("match");
        pos = caps.get(2).expect("method group").start();
        if !methods.contains(&caps[2]) || !receiver_in_scope(&caps[1], rel) {
            continue;
        }
        // The match ends on the call's `(`.
        let Some(after) = close_paren(bytes, whole.end() - 1) else {
            continue;
        };
        let Some(m) = text.get(after..).and_then(|tail| fallback.find(tail)) else {
            continue;
        };
        let key: String = text
            .get(whole.start()..after + m.end())
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let line = bytes
            .get(..whole.start())
            .map_or(0, |b| b.iter().filter(|&&c| c == b'\n').count())
            + 1;
        sites.push(Site { line, key });
    }
    sites
}

#[test]
fn pg_error_to_empty_census() {
    let sources = walk_sources();
    assert!(
        sources.len() >= MIN_FILES_WALKED,
        "walked only {} files — the walk lost src-tauri/src",
        sources.len()
    );
    let methods = pg_method_names(&sources);
    assert!(
        methods.len() >= MIN_PG_METHODS,
        "found only {} PgDb methods — the method-name scan is broken",
        methods.len()
    );

    // (file, key) -> lines
    let mut found: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (rel, text) in &sources {
        for site in census_sites(rel, text, &methods) {
            found
                .entry((rel.clone(), site.key))
                .or_default()
                .push(site.line);
        }
    }

    let mut problems = Vec::new();
    let mut allowed: BTreeMap<(String, String), usize> = BTreeMap::new();
    for a in ALLOWLIST {
        assert!(
            !a.reason.trim().is_empty(),
            "{} {}: empty reason",
            a.file,
            a.site
        );
        *allowed
            .entry((a.file.to_string(), a.site.to_string()))
            .or_default() += a.count;
    }
    for ((file, key), lines) in &found {
        let budget = allowed
            .get(&(file.clone(), key.clone()))
            .copied()
            .unwrap_or(0);
        if lines.len() > budget {
            problems.push(format!(
                "NEW: {file}:{lines:?} `{key}` — a PG read error becomes an empty value. \
                 Propagate the outage (`default_unless_unavailable()?` / a 503), render the \
                 field as null/unknown, or skip the tick; allowlist only where empty is the \
                 honest answer, with a reason"
            ));
        }
    }
    for ((file, key), budget) in &allowed {
        let have = found
            .get(&(file.clone(), key.clone()))
            .map_or(0, |l| l.len());
        if have < *budget {
            problems.push(format!(
                "STALE: allowlist names {budget} site(s) `{key}` in {file}, {have} exist — \
                 lower or delete the entry"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    let total: usize = found.values().map(Vec::len).sum();
    assert!(
        total > 0,
        "the census found nothing — the pattern is broken"
    );
}

#[test]
fn pg_error_to_empty_census_sees_a_multi_line_site() {
    let methods: BTreeSet<String> = ["get_task_output".to_string()].into();
    let src = "fn f() {\n    let o = state\n        .app_state\n        .pg_db\n        \
               .get_task_output(&id)\n        .await\n        .unwrap_or_default();\n}\n";
    let sites = census_sites("mcp/x.rs", src, &methods);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert_eq!(
        sites[0].key,
        "pg_db.get_task_output(&id).await.unwrap_or_default()"
    );
    assert_eq!(sites[0].line, 4);
}

#[test]
fn pg_error_to_empty_census_matches_every_fallback_and_skips_the_outage_aware_forms() {
    let methods: BTreeSet<String> = ["m".to_string()].into();
    let hits = [
        "pg.m(a, (b, \")\")).await.unwrap_or(0)",
        "pg_clone.m().await.unwrap_or_else(|e| { warn!(\"{e}\"); vec![] })",
        "pg.m().await.unwrap_or_else(|_| x)",
        "db.m().await.unwrap_or_default()",
        "pg.m().await.ok()",
        "pg.m().await.unwrap_or_else(Vec::new)",
        "state.pg_db().m().await.unwrap_or_default()",
    ];
    for src in hits {
        assert_eq!(census_sites("a.rs", src, &methods).len(), 1, "{src}");
    }
    // An accessor receiver is keyed at the accessor, not at its owner.
    assert_eq!(
        census_sites(
            "a.rs",
            "state\n    .pg_db()\n    .m(&id)\n    .await\n    .unwrap_or_default()",
            &methods
        )[0]
        .key,
        "pg_db().m(&id).await.unwrap_or_default()"
    );
    let misses = [
        "pg.m().await.default_unless_unavailable()?",
        "pg.m().await.or_unless_unavailable(0)?",
        "pg.m().await?",
        "pg.other().await.unwrap_or_default()",
        "client.m().await.unwrap_or_default()",
        "self.m().await.unwrap_or_default()",
        "pg.m().await.map(|r| r.len()).unwrap_or_default()",
    ];
    for src in misses {
        assert!(census_sites("a.rs", src, &methods).is_empty(), "{src}");
    }
    // `self` counts only inside the method layer itself.
    assert_eq!(
        census_sites(
            "database/pg/x.rs",
            "self.m().await.unwrap_or_default()",
            &methods
        )
        .len(),
        1
    );
}
