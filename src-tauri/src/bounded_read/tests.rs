//! The bounded-read census and the codec glue's unit tests.
//!
//! The census is a source scan, DB-free, over `src-tauri/src`. See
//! [`super::triage`] for what it enforces and why; the short form is that a
//! NEW `limit` default cannot land unclamped, a NEW `ReadLimit` cannot land
//! untriaged, and an agent-facing read cannot land undisclosed without
//! raising a ceiling that only falls.

use super::triage::{Disclosure, Door, Site, NOT_A_BOUND, TRIAGE, UNDISCLOSED_AGENT_SITES_CEILING};
use super::*;
use chrono::TimeZone;
use qontinui_types::page::{BoundKind, CursorScope};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ============================================================================
// The source walk
// ============================================================================

/// The runner crate holds ~1500 `.rs` files; a walker that lost the tree must
/// fail loudly rather than report a clean census.
const MIN_FILES_WALKED: usize = 1000;

/// Directories whose every route is merged into the :9876 router (or is the
/// GraphQL resolver set / the embedded MCP server). A `ReadLimit` declared in
/// one of them is reachable by an agent, whatever its row says.
const AGENT_DOOR_PATHS: &[&str] = &[
    "src/mcp/",
    "src/mcp_api.rs",
    "src/mcp_embedded.rs",
    "src/spec_api/",
    "src/trace_api/",
    "src/state_discovery/",
    "src/graphql/",
];

/// Tokens whose presence in a function body means it serves the shared
/// envelope: it builds one (`BoundedReadMeta`, `.meta()`), merges one
/// (`insert_into(`), or calls a builder that returns one (`keyset_page(`, and
/// `commands::ai_data`'s `log_page(`).
const DISCLOSURE_MARKERS: &[&str] = &[
    "BoundedReadMeta",
    ".meta()",
    "insert_into(",
    "keyset_page(",
    "log_page(",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One source file: its path relative to `src-tauri/` (forward slashes), the
/// text with comments blanked, and the text with comments AND string/char
/// literal contents blanked. Blanking replaces each byte with a space (keeping
/// newlines), so an offset in one is the same position in the others.
struct Source {
    rel: String,
    code: String,
    skeleton: String,
}

fn walk_sources() -> Vec<Source> {
    let root = manifest_dir();
    let mut out = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = rel_path(&root, &path);
            // This module spells the needles in its own prose and tests.
            if rel == "src/bounded_read.rs" || rel.starts_with("src/bounded_read/") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            let code = blank(&text, false);
            let skeleton = blank(&text, true);
            out.push(Source {
                rel,
                code,
                skeleton,
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    assert!(
        out.len() >= MIN_FILES_WALKED,
        "walked only {} .rs files (floor {MIN_FILES_WALKED}) — the walker lost the tree",
        out.len()
    );
    out
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Blank comments (`//…`, `/*…*/`) and, with `literals`, the CONTENTS of
/// string, raw-string and char literals, byte for byte (newlines kept). A
/// small lexer, not a parser: it only has to keep a `{` inside a format string
/// or a SQL literal from unbalancing a function body.
fn blank(src: &str, literals: bool) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let wipe = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in out.iter_mut().take(to).skip(from) {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                wipe(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                wipe(&mut out, i, end);
                i = end;
            }
            b'r' if matches!(b.get(i + 1), Some(b'#') | Some(b'"'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                let open = i + 1 + hashes;
                if b.get(open) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let close = format!("\"{}", "#".repeat(hashes));
                let end = src[open + 1..]
                    .find(&close)
                    .map_or(b.len(), |p| open + 1 + p);
                if literals {
                    wipe(&mut out, open + 1, end);
                }
                i = end + close.len();
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                if literals {
                    wipe(&mut out, i + 1, j.min(b.len()));
                }
                i = j + 1;
            }
            b'\'' => {
                // A char literal is `'x'` or `'\…'`; anything else is a lifetime.
                let len = if b.get(i + 1) == Some(&b'\\') {
                    b[i + 2..].iter().position(|&c| c == b'\'').map(|p| p + 3)
                } else if b.get(i + 2) == Some(&b'\'') {
                    Some(3)
                } else {
                    src[i + 1..]
                        .chars()
                        .next()
                        .filter(|c| c.len_utf8() > 1)
                        .and_then(|c| {
                            (b.get(i + 1 + c.len_utf8()) == Some(&b'\''))
                                .then_some(c.len_utf8() + 2)
                        })
                };
                match len {
                    Some(n) => {
                        if literals {
                            wipe(&mut out, i + 1, i + n - 1);
                        }
                        i += n;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("blanking only replaces whole bytes with spaces")
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// The `fn` keyword's position and the body span `{ … }` of the innermost
/// `fn` enclosing `offset`, found in the literal-blanked skeleton so braces
/// balance.
fn enclosing_fn(skeleton: &str, offset: usize) -> Option<(usize, usize, usize)> {
    static FN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let fn_re = FN.get_or_init(|| Regex::new(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*").expect("regex"));
    let bytes = skeleton.as_bytes();
    let mut best = None;
    for m in fn_re.find_iter(&skeleton[..offset]) {
        let Some(open_rel) = skeleton[m.end()..].find(['{', ';']) else {
            continue;
        };
        let open = m.end() + open_rel;
        if bytes[open] != b'{' {
            continue;
        }
        let mut depth = 0usize;
        let mut close = None;
        for (k, &c) in bytes.iter().enumerate().skip(open) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(close) = close {
            if open < offset && offset < close {
                best = Some((m.start(), open, close));
            }
        }
    }
    best
}

// ============================================================================
// The census
// ============================================================================

/// Every `limit` default the runner spells by hand. Two spellings: a typed
/// `limit…unwrap_or(` and a stringly `.get("limit")…unwrap_or(` (a
/// `HashMap<String, String>` query or a JSON-args map).
fn unclamped_limit_sites(sources: &[Source]) -> Vec<String> {
    let typed = Regex::new(r"\blimit[A-Za-z0-9_]*\s*\.\s*unwrap_or(?:_default|_else)?\s*\(")
        .expect("regex");
    let stringly = Regex::new(r#"\.get\(\s*"limit"\s*\)"#).expect("regex");
    let mut hits = Vec::new();
    for src in sources {
        if NOT_A_BOUND.iter().any(|(file, _)| *file == src.rel) {
            continue;
        }
        for m in typed.find_iter(&src.code) {
            hits.push(format!("{}:{}", src.rel, line_of(&src.code, m.start())));
        }
        for m in stringly.find_iter(&src.code) {
            let rest = &src.code[m.end()..];
            let stmt_end = rest.find([';', '{']).unwrap_or(rest.len());
            if rest[..stmt_end].contains(".unwrap_or") {
                hits.push(format!("{}:{}", src.rel, line_of(&src.code, m.start())));
            }
        }
    }
    hits
}

#[test]
fn every_limit_default_resolves_through_a_read_limit() {
    let sources = walk_sources();
    let hits = unclamped_limit_sites(&sources);
    assert!(
        hits.is_empty(),
        "a `limit` default spelled by hand is a default without a ceiling — declare a \
         `const NAME: ReadLimit = ReadLimit::new(default, max)` beside it, resolve the caller's \
         limit through it, and add its row to bounded_read/triage.rs (plan \
         2026-09-05-every-bounded-read-is-a-page-that-reads-as-a-corpus, Phase 6):\n  {}",
        hits.join("\n  ")
    );
}

#[test]
fn not_a_bound_entries_are_live() {
    let sources = walk_sources();
    let stringly = Regex::new(r#"\.get\(\s*"limit"\s*\)"#).expect("regex");
    for (file, why) in NOT_A_BOUND {
        let src = sources.iter().find(|s| s.rel == *file).unwrap_or_else(|| {
            panic!("NOT_A_BOUND names {file}, which no longer exists — drop it")
        });
        assert!(
            stringly.is_match(&src.code),
            "NOT_A_BOUND names {file} ({why}) but it no longer reads a `limit` — drop the entry"
        );
    }
}

/// `(file, const name)` for every `ReadLimit` constant in the tree.
fn declared_limits(sources: &[Source]) -> BTreeSet<(String, String)> {
    let decl = Regex::new(r"\bconst\s+([A-Z][A-Z0-9_]*)\s*:\s*ReadLimit\s*=").expect("regex");
    sources
        .iter()
        .flat_map(|src| {
            decl.captures_iter(&src.code)
                .map(|c| (src.rel.clone(), c[1].to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn every_read_limit_is_triaged_and_every_row_is_live() {
    let sources = walk_sources();
    let declared = declared_limits(&sources);
    assert!(
        declared.len() >= 50,
        "found only {} ReadLimit constants — the census lost the tree",
        declared.len()
    );
    let triaged: BTreeSet<(String, String)> = TRIAGE
        .iter()
        .map(|s| (s.file.to_string(), s.limit.to_string()))
        .collect();
    assert_eq!(triaged.len(), TRIAGE.len(), "a TRIAGE row is duplicated");
    let untriaged: Vec<_> = declared.difference(&triaged).collect();
    assert!(
        untriaged.is_empty(),
        "ReadLimit constants with no row in bounded_read/triage.rs — classify each as \
         Door::Agent (reachable from :9876 / GraphQL / the embedded MCP server) or \
         Door::RunnerUi, and say what it discloses:\n  {untriaged:?}"
    );
    let stale: Vec<_> = triaged.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "bounded_read/triage.rs rows whose constant no longer exists — delete them:\n  {stale:?}"
    );
}

#[test]
fn a_read_limit_in_an_agent_door_is_class_a() {
    for site in TRIAGE {
        if AGENT_DOOR_PATHS.iter().any(|p| site.file.starts_with(p)) {
            assert_eq!(
                site.door,
                Door::Agent,
                "{} {} sits behind the :9876 router, so an agent reads it",
                site.file,
                site.limit
            );
        }
        if site.door == Door::RunnerUi {
            assert!(
                !matches!(site.disclosure, Disclosure::Pending(_)),
                "{} {}: Pending is the class-A allowlist; a runner-UI read is NotOwed or \
                 Envelope",
                site.file,
                site.limit
            );
        }
    }
}

/// Each function that resolves `site`'s constant, by body text.
fn resolving_fns<'a>(sources: &'a [Source], site: &Site) -> Vec<(usize, &'a str)> {
    let Some(src) = sources.iter().find(|s| s.rel == site.file) else {
        return Vec::new();
    };
    let needle = Regex::new(&format!(r"\b{}\s*\.\s*resolve\s*\(", site.limit)).expect("regex");
    needle
        .find_iter(&src.code)
        .map(|m| {
            let (fn_at, open, close) =
                enclosing_fn(&src.skeleton, m.start()).unwrap_or_else(|| {
                    panic!(
                        "{}:{}: {}.resolve( outside any fn",
                        site.file,
                        line_of(&src.code, m.start()),
                        site.limit
                    )
                });
            (
                fn_at,
                line_of(&src.code, m.start()),
                &src.code[open..=close],
            )
        })
        // A `#[test]` that exercises the constant is not a read.
        .filter(|(fn_at, _, _)| !src.code[..*fn_at].trim_end().ends_with("#[test]"))
        .map(|(_, line, body)| (line, body))
        .collect()
}

#[test]
fn disclosure_matches_the_triage_and_the_allowlist_only_shrinks() {
    let sources = walk_sources();
    let mut pending = 0;
    for site in TRIAGE {
        let fns = resolving_fns(&sources, site);
        assert!(
            !fns.is_empty(),
            "{} {} is declared but never resolved — a ReadLimit nothing applies bounds nothing",
            site.file,
            site.limit
        );
        for (line, body) in fns {
            let discloses = DISCLOSURE_MARKERS.iter().any(|m| body.contains(m));
            match site.disclosure {
                Disclosure::Envelope => assert!(
                    discloses,
                    "{}:{line}: {} is triaged Envelope but the fn resolving it serves no \
                     BoundedReadMeta",
                    site.file, site.limit
                ),
                Disclosure::Pending(_) => assert!(
                    !discloses,
                    "{}:{line}: {} now serves BoundedReadMeta — flip its row to Envelope and \
                     lower UNDISCLOSED_AGENT_SITES_CEILING",
                    site.file, site.limit
                ),
                Disclosure::NotOwed(_) => {}
            }
        }
        if matches!(site.disclosure, Disclosure::Pending(_)) {
            pending += 1;
        }
    }
    assert_eq!(
        pending, UNDISCLOSED_AGENT_SITES_CEILING,
        "the undisclosed agent-door allowlist holds {pending} rows against a ceiling of \
         {UNDISCLOSED_AGENT_SITES_CEILING}. It only falls: a door that starts disclosing lowers \
         the ceiling in the same change, and a NEW agent read lands disclosed"
    );
}

#[test]
fn the_census_scanner_sees_both_spellings_and_ignores_prose() {
    let src = |code: &str| Source {
        rel: "src/x.rs".into(),
        code: blank(code, false),
        skeleton: blank(code, true),
    };
    let sources = [
        src("fn a(limit: Option<u32>) { let n = limit.unwrap_or(50); }"),
        src("fn b(q: Q) { let n = q.limit\n    .unwrap_or(5); }"),
        src("fn c(p: P) { let n: usize = p\n  .get(\"limit\")\n  .and_then(|s| s.parse().ok())\n  .unwrap_or(100); }"),
        src("// limit.unwrap_or(50) in prose\nfn d() {}"),
        src("fn e(p: P) { if let Some(l) = p.get(\"limit\") { x.unwrap_or(1); } }"),
        src("fn f(t: T) { let n = time_limit_secs.unwrap_or(0); }"),
    ];
    assert_eq!(
        unclamped_limit_sites(&sources),
        vec!["src/x.rs:1", "src/x.rs:1", "src/x.rs:2"]
    );
}

#[test]
fn enclosing_fn_balances_braces_inside_literals() {
    let text = "fn outer() {\n    let s = \"{ not a brace\";\n    let c = '{';\n    let r = r#\"}\"#;\n    LIMIT.resolve(x);\n}\nfn after() {}\n";
    let skeleton = blank(text, true);
    let at = text.find("LIMIT").expect("needle");
    let (_, open, close) = enclosing_fn(&skeleton, at).expect("inside outer");
    assert_eq!(&text[open..=open], "{");
    assert!(text[open..=close].contains("LIMIT.resolve"));
    assert!(!text[open..=close].contains("fn after"));
}

// ============================================================================
// D8: every keyset this module walks is immutable
// ============================================================================

/// `(table, key columns)` for each keyset walk Phase 6 introduced. A walk on a
/// column some statement updates drops rows silently (plan D8; coord
/// `2794f010d`), so no `UPDATE <table> SET` — nor an `INSERT … ON CONFLICT DO
/// UPDATE SET` — may assign one.
const KEYSET_KEYS: &[(&str, &[&str])] = &[
    ("task_run_mcp_calls", &["created_at", "id"]),
    ("orchestrator_checkpoints", &["created_at", "id"]),
    ("flow_executions", &["started_at", "instance_id"]),
    ("learning_outcomes", &["created_at", "id"]),
];

#[test]
fn keyset_keys_have_no_update_site() {
    let root = manifest_dir();
    let mut corpus: Vec<(String, String)> = walk_sources()
        .into_iter()
        .map(|s| (s.rel, s.code))
        .collect();
    // Raw sources too: the statements live inside string literals.
    for (rel, text) in corpus.iter_mut() {
        *text = std::fs::read_to_string(root.join(&*rel)).expect("re-read source");
    }
    for entry in std::fs::read_dir(root.join("queries")).expect("queries dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("sql") {
            corpus.push((
                rel_path(&root, &path),
                std::fs::read_to_string(&path).expect("read sql"),
            ));
        }
    }
    for (table, keys) in KEYSET_KEYS {
        let t = regex::escape(table);
        let update = Regex::new(&format!(
            r#"(?is)\bUPDATE\s+(?:project\.)?{t}\b\s+SET\s+(.*?)(?:\bWHERE\b|\bRETURNING\b|;|")"#
        ))
        .expect("regex");
        let upsert = Regex::new(&format!(
            r#"(?is)\bINSERT\s+INTO\s+(?:project\.)?{t}\b[^;"]*?\bDO\s+UPDATE\s+SET\s+(.*?)(?:\bWHERE\b|\bRETURNING\b|;|")"#
        ))
        .expect("regex");
        let insert =
            Regex::new(&format!(r"(?i)\bINSERT\s+INTO\s+(?:project\.)?{t}\b")).expect("regex");
        assert!(
            corpus.iter().any(|(_, text)| insert.is_match(text)),
            "no INSERT INTO {table} found — the scan is vacuous for it"
        );
        for (rel, text) in &corpus {
            for caps in update.captures_iter(text).chain(upsert.captures_iter(text)) {
                let set = &caps[1];
                for key in *keys {
                    let assigns =
                        Regex::new(&format!(r"(?i)(?:^|[\s,]){}\s*=", regex::escape(key)))
                            .expect("regex");
                    assert!(
                        !assigns.is_match(set),
                        "{rel}: a statement assigns {table}.{key}, which a keyset walk sorts \
                         on — that UPDATE would move unread rows across the cursor"
                    );
                }
            }
        }
    }
}

// ============================================================================
// ReadLimit and the keyset glue
// ============================================================================

const TEST_LIMIT: ReadLimit = ReadLimit::new(20, 100);

#[test]
fn read_limit_defaults_clamps_and_keeps_the_callers_type() {
    assert_eq!(TEST_LIMIT.resolve(None::<u32>), 20_u32);
    assert_eq!(TEST_LIMIT.resolve(Some(7_usize)), 7_usize);
    assert_eq!(TEST_LIMIT.resolve(Some(0_i64)), 1);
    assert_eq!(TEST_LIMIT.resolve(Some(-4_i32)), 1);
    assert_eq!(TEST_LIMIT.resolve(Some(10_000_u32)), 100);
    assert_eq!(TEST_LIMIT.resolve(Some(u64::MAX)), 100);
    assert_eq!(TEST_LIMIT.resolve(Some(i64::MIN)), 1);
}

struct TestWalk;
impl SortKey for TestWalk {
    const ID: &'static str = "runner.test:created_at,id:desc";
}

const ID_A: &str = "0b7c8e2a-1f3d-4c5e-9a6b-7c8d9e0f1a2b";
const ID_B: &str = "1c8d9f3b-2a4e-4d6f-8b7c-8d9e0f1a2b3c";
const ID_C: &str = "2d9e0a4c-3b5f-4e7a-9c8d-9e0f1a2b3c4d";

fn row(id: &str, micros: i64) -> (String, DateTime<Utc>) {
    (
        id.to_string(),
        Utc.timestamp_micros(micros)
            .single()
            .expect("valid instant"),
    )
}

fn pos(r: &(String, DateTime<Utc>)) -> Result<KeysetPosition, String> {
    row_position(&r.0, r.1)
}

fn scope(run: &str) -> ScopeFingerprint<TestWalk> {
    CursorScope::<TestWalk>::new()
        .opt_str("run", Some(run))
        .finish()
}

#[test]
fn a_first_page_with_an_agreeing_count_is_exact_and_cursors_from_the_last_kept_row() {
    let s = scope("r");
    let rows = vec![row(ID_A, 3), row(ID_B, 2), row(ID_C, 1)];
    let page = keyset_page(rows, 2, Some(3), &s, pos).expect("canonical ids");
    let meta = page.meta();
    assert_eq!(meta.bound_kind, BoundKind::Exact);
    assert_eq!(meta.total, Some(3));
    assert_eq!(meta.truncated, Some(true));
    assert_eq!(meta.shown, 2);
    let token = meta.next_cursor.expect("a truncated walk carries a cursor");
    let after = decode_cursor(&s, Some(&token))
        .expect("the same scope decodes it")
        .expect("a token decodes to a position");
    assert_eq!(
        after.id.to_string(),
        ID_B,
        "the last KEPT row, never the probe"
    );
    assert_eq!(keyset_binds(Some(after)).1.as_deref(), Some(ID_B));
}

#[test]
fn a_count_the_probe_contradicts_yields_to_the_probe() {
    // The count says everything fits, but the probe row arrived: a writer
    // appended between the two statements.
    let page = keyset_page(
        vec![row(ID_A, 3), row(ID_B, 2), row(ID_C, 1)],
        2,
        Some(2),
        &scope("r"),
        pos,
    )
    .expect("canonical ids");
    assert_eq!(page.meta().bound_kind, BoundKind::AtLeast);
    assert_eq!(page.meta().total, None);
}

#[test]
fn a_later_page_is_at_least_then_complete() {
    let s = scope("r");
    let mid = keyset_page(vec![row(ID_A, 3), row(ID_B, 2)], 1, None, &s, pos).expect("ids");
    assert_eq!(mid.meta().bound_kind, BoundKind::AtLeast);
    assert!(mid.meta().next_cursor.is_some());
    let last = keyset_page(vec![row(ID_C, 1)], 1, None, &s, pos).expect("ids");
    assert_eq!(last.meta().bound_kind, BoundKind::Complete);
    assert_eq!(last.meta().truncated, Some(false));
    assert_eq!(last.meta().next_cursor, None);
}

#[test]
fn a_non_canonical_id_refuses_to_mint_a_cursor_that_would_skip_rows() {
    let upper = ID_B.to_uppercase();
    let err = keyset_page(
        vec![row(ID_A, 3), row(&upper, 2), row(ID_C, 1)],
        2,
        None,
        &scope("r"),
        pos,
    )
    .expect_err("an id the TEXT comparison would misplace");
    assert!(err.contains("canonical"), "{err}");
    // A page that needs no cursor does not care.
    assert!(keyset_page(vec![row("not-a-uuid", 1)], 2, None, &scope("r"), pos).is_ok());
}

#[test]
fn a_cursor_from_another_scope_is_the_typed_refusal() {
    let minted = scope("run-1").encode(KeysetPosition {
        at: Utc.timestamp_micros(5).single().expect("instant"),
        id: uuid::Uuid::parse_str(ID_A).expect("uuid"),
    });
    let err = decode_cursor(&scope("run-2"), Some(&minted)).expect_err("other scope");
    assert_eq!(err.code(), "cursor_malformed");
    assert!(err.refusal("surface").contains("cursor"));
    assert!(decode_cursor(&scope("run-1"), None)
        .expect("no cursor is the first page")
        .is_none());
    assert_eq!(keyset_binds(None), (None, None));
}
