//! Regression gate: **the runner must not author `coord.*` schema.**
//!
//! `alembic` (in `qontinui-web/backend/alembic/versions/`) is the *sole*
//! author of the `coord.*` schema. The runner asserts the schema is present at
//! boot via `PgDb::require_table` and hard-fails if a migration hasn't run — it
//! never silently self-heals. This was a deliberate posture change (plan
//! `2026-05-29-delete-stale-rust-table-self-heals.md`, resolved on
//! *robustness*): dual-authoring made the live schema race-determined
//! ("whichever DDL runs first wins"), the exact mechanism that detonated as the
//! live `coord.tenant_policies` 500.
//!
//! This test scans `src/` (i.e. `src-tauri/src/`) and FAILS if production
//! (non-`#[cfg(test)]`) Rust authors `coord.*` schema. Unlike the coord repo,
//! the runner's production code now authors **zero** `coord.*` schema, so the
//! allowlist is **EMPTY** — the gate enforces "no `coord.*` schema authoring in
//! runner production code at all". The only remaining occurrence is a
//! `#[cfg(test)]` fixture in `database/pg/tasks.rs`
//! (`create_tasks_identity_hash_for_test`), which is correctly ignored.
//!
//! # The allowlist
//!
//! Empty, and it must stay empty: any NEW `coord.*` DDL in production runner
//! Rust fails the gate. (If a genuinely single-authored coord-internal runner
//! table ever appears with no alembic migration, add it here with
//! justification and track it in the follow-up migration plan — but the
//! runner has none today.)
//!
//! # What is detected (case-sensitive, on `coord.` targets)
//!
//! - `CREATE TABLE ... coord.<table>`
//! - `ALTER TABLE coord.<table>` (token carries the `ADD COLUMN IF NOT EXISTS`
//!   column when present)
//! - `CREATE INDEX ... ON coord.<table>` / `CREATE UNIQUE INDEX ... ON coord.`
//! - `CREATE SCHEMA IF NOT EXISTS coord` (collapsed to one `SCHEMA` entry/file)
//!
//! # What is IGNORED (legitimate, not schema authoring)
//!
//! - Lines inside a `#[cfg(test)]` region (tracked via brace depth from the
//!   `#[cfg(test)]` attribute) — test self-provisioning against an ephemeral
//!   throwaway PG is the blessed pattern (`canonical-db-behind-alembic-head`).
//! - Pure comment lines (`//`, `//!`, `*` doc-comment continuations, `/*`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Frozen baseline: EMPTY for the runner. The runner's production code authors
/// no `coord.*` schema. Keep it empty; any new entry needs a justification and
/// a follow-up-plan tracking note.
const ALLOWLIST: &[(&str, &str)] = &[];

/// Directory (relative to the crate root, i.e. `src-tauri/`) that the gate
/// scans.
const SRC_DIR: &str = "src";

#[test]
fn rust_does_not_author_coord_schema() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join(SRC_DIR);
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    files.sort();

    let mut live: BTreeSet<(String, String)> = BTreeSet::new();
    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for token in scan_production_ddl(&text) {
            live.insert((rel.clone(), token));
        }
    }

    let allow: BTreeSet<(String, String)> = ALLOWLIST
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();

    let new: Vec<_> = live.difference(&allow).collect();
    let removed: Vec<_> = allow.difference(&live).collect();

    if !new.is_empty() || !removed.is_empty() {
        let mut msg = String::new();
        if !new.is_empty() {
            msg.push_str(
                "\nRust must not author coord.* schema; alembic is the sole author.\n\
                 If this is a genuinely single-authored coord-internal table with no\n\
                 alembic migration, add it to the ALLOWLIST with justification and\n\
                 track it in the follow-up migration plan.\n\n\
                 NEW coord.* schema authoring detected in production Rust:\n",
            );
            for (p, t) in &new {
                msg.push_str(&format!("    (\"{p}\", \"{t}\"),\n"));
            }
        }
        if !removed.is_empty() {
            msg.push_str(
                "\nALLOWLIST entries no longer present in src/ — shrink the allowlist\n\
                 (it must trend to zero as the follow-up migration plan moves these\n\
                 to alembic). Remove these stale entries:\n",
            );
            for (p, t) in &removed {
                msg.push_str(&format!("    (\"{p}\", \"{t}\"),\n"));
            }
        }
        panic!("{msg}");
    }
}

/// Recursively collect `*.rs` files under `dir`.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Scan one file's text and return the set of production (non-`#[cfg(test)]`,
/// non-comment) `coord.*` schema-authoring tokens it contains.
fn scan_production_ddl(text: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeSet::new();

    let mut in_corpus = false;
    let mut cfg_test_active = false;
    let mut cfg_test_depth: i32 = 0;
    let mut pending_cfg_test = false;
    let mut depth: i32 = 0;

    for (i, raw) in lines.iter().enumerate() {
        let line = *raw;
        let stripped = line.trim();

        // Mirror the coord gate's corpus carve-out so the two gates stay
        // identical in behaviour (the runner has no such const today).
        if line.contains("PG_SCHEMA_CORPUS") && line.contains("const") {
            in_corpus = true;
        }
        if stripped.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        if in_corpus {
            if stripped == "\";" {
                in_corpus = false;
            }
            depth += opens - closes;
            continue;
        }

        if !is_comment(line) {
            if let Some((kind, table)) = classify(line) {
                if !cfg_test_active {
                    let token = match kind {
                        Kind::Schema => "SCHEMA".to_string(),
                        Kind::Alter => match find_added_column(&lines, i) {
                            Some(col) => format!("{table}.{col}"),
                            None => table,
                        },
                        _ => table,
                    };
                    out.insert(token);
                }
            }
        }

        let new_depth = depth + opens - closes;
        if pending_cfg_test && opens > 0 {
            cfg_test_active = true;
            cfg_test_depth = depth;
            pending_cfg_test = false;
        }
        if cfg_test_active && new_depth <= cfg_test_depth {
            cfg_test_active = false;
        }
        depth = new_depth;
    }

    out
}

fn is_comment(line: &str) -> bool {
    let s = line.trim_start();
    s.starts_with("//") || s.starts_with('*') || s.starts_with("/*")
}

#[derive(Clone, Copy)]
enum Kind {
    Schema,
    Table,
    Index,
    Alter,
}

/// Classify a single line. Case-sensitive, targets `coord.` only.
fn classify(line: &str) -> Option<(Kind, String)> {
    if line.contains("CREATE SCHEMA IF NOT EXISTS coord") {
        return Some((Kind::Schema, "coord".to_string()));
    }
    if let Some(t) = table_after(line, "ON coord.") {
        if line.contains("CREATE UNIQUE INDEX") || line.contains("CREATE INDEX") {
            return Some((Kind::Index, t));
        }
    }
    if line.contains("CREATE TABLE") {
        if let Some(t) = table_after(line, "coord.") {
            return Some((Kind::Table, t));
        }
    }
    if let Some(t) = table_after(line, "ALTER TABLE coord.") {
        return Some((Kind::Alter, t));
    }
    None
}

/// Extract the `coord.<table>` identifier immediately following `marker`.
fn table_after(line: &str, marker: &str) -> Option<String> {
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// For an `ALTER TABLE` at `start`, find the `ADD COLUMN IF NOT EXISTS <col>`
/// column on this or the next few lines.
fn find_added_column(lines: &[&str], start: usize) -> Option<String> {
    let needle = "ADD COLUMN IF NOT EXISTS ";
    for line in lines.iter().skip(start).take(4) {
        if let Some(idx) = line.find(needle) {
            let rest = &line[idx + needle.len()..];
            let col: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !col.is_empty() {
                return Some(col);
            }
        }
    }
    None
}
