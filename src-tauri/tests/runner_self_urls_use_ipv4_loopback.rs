//! Regression guard: the runner never addresses its own HTTP API — or a sibling
//! runner's — as `localhost`.
//!
//! ## Why this test exists
//!
//! The runner's HTTP API binds the IPv4 loopback ONLY
//! (`mcp_api.rs` `try_bind_port`, through `mcp::types::RUNNER_API_BIND_IP`).
//! Windows resolves `localhost` to `::1` FIRST, so every client that dials
//! `http://localhost:<runner port>` first attempts a socket nobody listens on.
//! Measured on a Windows fleet box (2026-10-25, live primary, `GET /health`):
//!
//! | client | penalty per NEW connection |
//! |---|---|
//! | reqwest, fresh `Client` (the runner's own self-call shape) | ~+305 ms (300 ms happy-eyeballs fallback) |
//! | curl | ~+205 ms |
//! | Python `urllib` / `requests`, PowerShell 5.1 | ~+2.0 s (no fallback: waits out the refused `::1` connect) |
//!
//! Plan `2026-10-25-runner-self-calls-dial-localhost-and-pay-a-doomed-ipv6-connect`
//! routed every such URL through one helper per language:
//!
//! - Rust: `mcp::types::runner_loopback_base(port)` (and `get_self_base_url`,
//!   `get_self_base_url_from_env`, `runner_ui_bridge_base`, built on it);
//! - frontend: `getApiBase()` / `useApiBase()` / `getWsBase()` (this runner's
//!   bound port) and `runnerBaseForPort(port)` (an explicit port) in
//!   `src/lib/runner-api.ts`;
//! - Python bridge: `runner_api_base()` in `python-bridge/runner_api.py`.
//!
//! This test is the backstop that keeps a NEW call site from spelling
//! `localhost` again. The helpers are the primary control; this is the guard.
//!
//! ## What it flags
//!
//! `localhost:` (any case) followed by EITHER a runner port literal — 9876 to
//! 9899, the primary and the secondary/temp range — OR an interpolation
//! (`{`, `${`, `%`), whose port this lexical check cannot know. It scans:
//!
//! - `src-tauri/src/**` — `.rs`, plus the shipped text assets `.md` / `.json`
//!   (prompt text and builtin context the runner hands to agents);
//! - `src/**` (the frontend) — `.ts`, `.tsx`, `.js`, `.mjs`, plus `.md` / `.json`;
//! - `python-bridge/**` — `.py`.
//!
//! In code files, comments are not scanned and test code is skipped: Rust
//! `#[cfg(test)]` regions and files a parent DECLARES with `#[cfg(test)] mod
//! name;` (never by file name — this crate has production `*_tests.rs` modules),
//! and, outside Rust, `tests/` directories, `*.test.*` / `*.spec.*`, `test_*.py`. In `.md` / `.json` assets every line is
//! scanned: they are text an agent will copy into a command.
//!
//! NOT scanned: `src-tauri/src/fleet_commands/`, `fleet_agents/`, `fleet_skills/`.
//! Those are vendored from qontinui-claude-config (the fleet-bundle sync keeps
//! them byte-identical to that repo), so they are fixed at the source and
//! guarded there by its lint check #14.
//!
//! ## The one exemption
//!
//! A `localhost:` URL that does NOT target a runner (a Vite dev server, a
//! target app's dev server, Sunshine) is exempt when the token
//! `runner-localhost-ok` appears IN A COMMENT on the same line or the line
//! immediately above, followed by a reason. That is qontinui-claude-config
//! check #14's marker, so the fleet has one spelling for "this localhost is
//! deliberate". The token inside a string literal does NOT exempt — the
//! escape hatch of a guard is the last place to accept a loose match.
//!
//! ## What it does NOT claim
//!
//! It is lexical. A URL assembled by concatenation (`"http://localhost:" + p`)
//! escapes it, and its comment/string tracking is a small hand lexer, not a
//! parser. A green run proves no line it SCANNED carries a flagged form; the
//! `scanned=` counts it prints, and the per-root floors below, are what make
//! "scanned nothing" distinguishable from "found nothing".
//!
//! ## Fixing a failure
//!
//! Rust: `crate::mcp::types::runner_loopback_base(port)` (or
//! `get_self_base_url(&app_state)` / `get_self_base_url_from_env()`).
//! Frontend: `getApiBase()` / `useApiBase()` / `getWsBase()`, or `runnerBaseForPort(port)`
//! for an explicit port, from `src/lib/runner-api.ts`.
//! Python: `runner_api_base()` from `python-bridge/runner_api.py`.
//! Prompt or asset text: spell `127.0.0.1:<port>`.
//! Not a runner at all: add `// runner-localhost-ok: <what it targets>`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The exemption token — qontinui-claude-config check #14's marker.
const MARKER: &str = "runner-localhost-ok";

/// Directories under `src-tauri/` that are vendored from qontinui-claude-config.
const VENDORED: &[&str] = &["src/fleet_commands", "src/fleet_agents", "src/fleet_skills"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    /// Rust: `//` and `/* */` comments, `"` strings (multi-line), raw strings.
    Rust,
    /// TypeScript / JavaScript: `//` and `/* */`, `'` `"` (single-line) and `` ` `` (multi-line).
    Js,
    /// Python: `#` comments, `'` `"` and triple-quoted strings.
    Py,
    /// A text asset (`.md`, `.json`): every character is scanned.
    Text,
}

struct Root {
    /// Relative to `CARGO_MANIFEST_DIR` (`src-tauri/`).
    rel: &'static str,
    exts: &'static [&'static str],
    /// A floor on files scanned, so a moved or emptied directory cannot pass vacuously.
    min_files: usize,
}

const ROOTS: &[Root] = &[
    Root {
        rel: "src",
        exts: &["rs", "md", "json"],
        min_files: 1000,
    },
    Root {
        rel: "../src",
        exts: &["ts", "tsx", "js", "mjs", "md", "json"],
        min_files: 1000,
    },
    Root {
        rel: "../python-bridge",
        exts: &["py"],
        min_files: 60,
    },
];

fn lang_for(path: &Path) -> Option<Lang> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => Some(Lang::Rust),
        Some("ts") | Some("tsx") | Some("js") | Some("mjs") => Some(Lang::Js),
        Some("py") => Some(Lang::Py),
        Some("md") | Some("json") => Some(Lang::Text),
        _ => None,
    }
}

/// Non-Rust paths skipped as test code by NAME (a file under a `tests/` dir,
/// `*.test.*`, `test_*.py`, …).
///
/// Rust files are NEVER skipped by name: this crate has production modules
/// named `*_tests.rs` (`mcp/image_quality_tests.rs`, `mcp/verification_tests.rs`,
/// `database/pg/verification_tests.rs` are all `pub mod`). A Rust file is test
/// code only when a parent DECLARES it under `#[cfg(test)] mod <name>;`
/// ([`declared_test_only_files`]) or the line sits in a `#[cfg(test)]` region.
fn is_test_path(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    let file = rel.rsplit('/').next().unwrap_or(&rel);
    if file.ends_with(".rs") {
        return false;
    }
    rel.split('/')
        .any(|seg| matches!(seg, "tests" | "__tests__" | "test_support" | "fixtures"))
        || file.contains(".test.")
        || file.contains(".spec.")
        || (file.starts_with("test_") && file.ends_with(".py"))
        || file.ends_with("_test.py")
        || file == "conftest.py"
}

/// Is there a forbidden `localhost:` form in `code`? Returns the column of the first one.
///
/// Case-insensitive on `localhost`. Forbidden after the colon: a port 9876–9899
/// not followed by another digit, or an interpolation opener (`{`, `$`, `%`).
fn forbidden_at(code: &str) -> Option<usize> {
    let lower = code.to_ascii_lowercase();
    let needle = "localhost:";
    let mut from = 0;
    while let Some(rel) = lower.get(from..).unwrap_or("").find(needle) {
        let at = from + rel;
        let after = &lower.as_bytes()[at + needle.len()..];
        let flagged = match after.first() {
            Some(b'{') | Some(b'$') | Some(b'%') => true,
            Some(b'9') => {
                let digits: Vec<u8> = after
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .copied()
                    .collect();
                if digits.len() == 4 {
                    let port: u32 = std::str::from_utf8(&digits)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    (9876..=9899).contains(&port)
                } else {
                    false
                }
            }
            _ => false,
        };
        if flagged {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

/// One source line, split by a small lexer into the text it scans (code AND
/// string contents — URLs live in strings) and the text of its comments.
#[derive(Default, Debug)]
struct LexedLine {
    /// Code and string contents (URLs live in strings) — what the matcher reads.
    scanned: String,
    /// Comment text only — where the exemption marker must sit.
    comment: String,
    /// Code outside strings and comments — what attributes and braces are read from.
    code: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Code,
    Block,
    /// A string delimited by `delim`; `hashes` counts a Rust raw string's `#`s
    /// (`usize::MAX` = not raw); `triple` is a Python triple quote.
    Str {
        delim: u8,
        raw_hashes: usize,
        triple: bool,
    },
}

fn lex(text: &str, lang: Lang) -> Vec<LexedLine> {
    let mut out = Vec::new();
    if lang == Lang::Text {
        for l in text.lines() {
            out.push(LexedLine {
                scanned: l.to_string(),
                comment: String::new(),
                code: l.to_string(),
            });
        }
        return out;
    }
    let mut state = State::Code;
    for line in text.lines() {
        let b = line.as_bytes();
        let mut ll = LexedLine::default();
        let mut i = 0;
        // A JS single/double-quoted string cannot span lines; reset at a new line.
        if lang == Lang::Js {
            if let State::Str { delim, .. } = state {
                if delim != b'`' {
                    state = State::Code;
                }
            }
        }
        while i < b.len() {
            let c = b[i];
            match state {
                State::Block => {
                    if c == b'*' && b.get(i + 1) == Some(&b'/') {
                        ll.comment.push_str("*/");
                        state = State::Code;
                        i += 2;
                    } else {
                        ll.comment.push(c as char);
                        i += 1;
                    }
                }
                State::Str {
                    delim,
                    raw_hashes,
                    triple,
                } => {
                    if raw_hashes == usize::MAX && c == b'\\' && lang != Lang::Text {
                        ll.scanned.push(c as char);
                        if let Some(n) = b.get(i + 1) {
                            ll.scanned.push(*n as char);
                        }
                        i += 2;
                        continue;
                    }
                    if c == delim {
                        if triple {
                            if b.get(i + 1) == Some(&delim) && b.get(i + 2) == Some(&delim) {
                                ll.scanned.push_str(line.get(i..i + 3).unwrap_or(""));
                                state = State::Code;
                                i += 3;
                                continue;
                            }
                        } else if raw_hashes != usize::MAX {
                            let closes = (0..raw_hashes).all(|k| b.get(i + 1 + k) == Some(&b'#'));
                            if closes {
                                ll.scanned
                                    .push_str(line.get(i..i + 1 + raw_hashes).unwrap_or(""));
                                state = State::Code;
                                i += 1 + raw_hashes;
                                continue;
                            }
                        } else {
                            ll.scanned.push(c as char);
                            state = State::Code;
                            i += 1;
                            continue;
                        }
                    }
                    ll.scanned.push(c as char);
                    i += 1;
                }
                State::Code => {
                    // Comments.
                    let line_comment = match lang {
                        Lang::Py => c == b'#',
                        // `scheme://` in JSX text is not a comment: a `//` right
                        // after `:` never opens one in either language.
                        _ => {
                            c == b'/' && b.get(i + 1) == Some(&b'/') && !(i > 0 && b[i - 1] == b':')
                        }
                    };
                    if line_comment {
                        ll.comment.push_str(line.get(i..).unwrap_or(""));
                        break;
                    }
                    if lang != Lang::Py && c == b'/' && b.get(i + 1) == Some(&b'*') {
                        ll.comment.push_str("/*");
                        state = State::Block;
                        i += 2;
                        continue;
                    }
                    // Strings.
                    if lang == Lang::Rust
                        && c == b'r'
                        && (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#'))
                    {
                        let mut j = i + 1;
                        while b.get(j) == Some(&b'#') {
                            j += 1;
                        }
                        if b.get(j) == Some(&b'"')
                            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
                        {
                            ll.scanned.push_str(line.get(i..=j).unwrap_or(""));
                            state = State::Str {
                                delim: b'"',
                                raw_hashes: j - i - 1,
                                triple: false,
                            };
                            i = j + 1;
                            continue;
                        }
                    }
                    if lang == Lang::Rust && c == b'\'' {
                        // A char literal ('x', '\n', '\''), else a lifetime: never a string.
                        if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
                            ll.scanned.push_str(line.get(i..i + 4).unwrap_or(""));
                            i += 4;
                            continue;
                        }
                        if b.get(i + 2) == Some(&b'\'') {
                            ll.scanned.push_str(line.get(i..i + 3).unwrap_or(""));
                            i += 3;
                            continue;
                        }
                        ll.scanned.push('\'');
                        i += 1;
                        continue;
                    }
                    let is_delim = match lang {
                        Lang::Rust => c == b'"',
                        Lang::Js => c == b'"' || c == b'\'' || c == b'`',
                        Lang::Py => c == b'"' || c == b'\'',
                        Lang::Text => false,
                    };
                    if is_delim {
                        let triple = lang == Lang::Py
                            && b.get(i + 1) == Some(&c)
                            && b.get(i + 2) == Some(&c);
                        if triple {
                            ll.scanned.push_str(line.get(i..i + 3).unwrap_or(""));
                            state = State::Str {
                                delim: c,
                                raw_hashes: usize::MAX,
                                triple: true,
                            };
                            i += 3;
                        } else {
                            ll.scanned.push(c as char);
                            state = State::Str {
                                delim: c,
                                raw_hashes: usize::MAX,
                                triple: false,
                            };
                            i += 1;
                        }
                        continue;
                    }
                    ll.scanned.push(c as char);
                    ll.code.push(c as char);
                    i += 1;
                }
            }
        }
        // A Python single-quoted string cannot span lines either.
        if lang == Lang::Py {
            if let State::Str { triple: false, .. } = state {
                state = State::Code;
            }
        }
        out.push(ll);
    }
    out
}

/// Line ranges (0-based, inclusive) of Rust `#[cfg(test)]`-gated inline modules,
/// plus the `#[cfg(test)]`-gated modules declared with `mod NAME;` — each as
/// `(NAME, the #[path = "…"] it carries, if any)`.
fn rust_test_regions(lines: &[LexedLine]) -> (Vec<(usize, usize)>, Vec<(String, Option<String>)>) {
    let mut regions = Vec::new();
    let mut declared = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let s = lines[i].code.trim();
        if s.starts_with("#![cfg(test)]") {
            regions.push((0, lines.len().saturating_sub(1)));
            return (regions, declared);
        }
        let gated = s.starts_with("#[cfg(test)]") || s.starts_with("#[cfg(all(test");
        if !gated {
            i += 1;
            continue;
        }
        // The item the attribute gates: skip further attributes and blank lines,
        // remembering a `#[path = "…"]` (its string lives in `scanned`, not `code`).
        let mut j = i + 1;
        let mut path_attr: Option<String> = None;
        // `#[cfg(test)] mod tests;` / `#[cfg(test)] mod tests { … }` on ONE line:
        // the gated item is the rest of the attribute's own line.
        let same_line = s.split_once(']').map(|(_, r)| r.trim()).unwrap_or("");
        if !same_line.is_empty() {
            j = i;
        }
        while same_line.is_empty() && j < lines.len() {
            let t = lines[j].code.trim();
            if t.is_empty() || t.starts_with("#[") {
                if t.starts_with("#[path") {
                    let full = lines[j].scanned.trim();
                    let quoted: Vec<&str> = full.split('"').collect();
                    if quoted.len() >= 3 {
                        path_attr = Some(quoted[1].to_string());
                    }
                }
                j += 1;
                continue;
            }
            break;
        }
        if j >= lines.len() {
            break;
        }
        let item = if same_line.is_empty() {
            lines[j].code.trim()
        } else {
            same_line
        };
        let item_body = item
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub(super) ")
            .trim_start_matches("pub ");
        if let Some(rest) = item_body.strip_prefix("mod ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if rest
                .get(name.len()..)
                .unwrap_or("")
                .trim_start()
                .starts_with(';')
            {
                declared.push((name, path_attr));
                i = j + 1;
                continue;
            }
        }
        // Brace-match the gated item over CODE text only (strings and comments
        // removed by the lexer, so a `{` inside a raw string cannot unbalance it).
        let mut depth: i64 = 0;
        let mut opened = false;
        let mut k = j;
        'outer: while k < lines.len() {
            for c in lines[k].code.bytes() {
                match c {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => {
                        depth -= 1;
                        if opened && depth == 0 {
                            break 'outer;
                        }
                    }
                    b';' if !opened => break 'outer, // a gated `use` or a bodiless item
                    _ => {}
                }
            }
            k += 1;
        }
        regions.push((i, k.min(lines.len().saturating_sub(1))));
        i = k + 1;
    }
    (regions, declared)
}

struct Hit {
    rel: String,
    line: usize,
    text: String,
}

/// Scan one file's text. `declared_test_files` are Rust files a `#[cfg(test)] mod x;`
/// elsewhere marks test-only.
fn scan_text(rel: &str, text: &str, lang: Lang, hits: &mut Vec<Hit>) {
    let lines = lex(text, lang);
    let regions = if lang == Lang::Rust {
        rust_test_regions(&lines).0
    } else {
        Vec::new()
    };
    for (idx, ll) in lines.iter().enumerate() {
        if regions.iter().any(|(a, b)| idx >= *a && idx <= *b) {
            continue;
        }
        if forbidden_at(&ll.scanned).is_none() {
            continue;
        }
        let exempt_here = ll.comment.contains(MARKER);
        // The line above exempts only when it is a COMMENT line: nothing alphanumeric
        // outside its comment (a JSX `{/* … */}` wrapper leaves only braces).
        let exempt_above = idx > 0
            && lines[idx - 1].comment.contains(MARKER)
            && !lines[idx - 1].scanned.chars().any(|c| c.is_alphanumeric());
        if exempt_here || exempt_above {
            continue;
        }
        hits.push(Hit {
            rel: rel.to_string(),
            line: idx + 1,
            text: text.lines().nth(idx).unwrap_or("").trim().to_string(),
        });
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                name,
                "node_modules"
                    | "target"
                    | "target-agent"
                    | "dist"
                    | ".git"
                    | "__pycache__"
                    | ".venv"
                    | "venv"
            ) {
                continue;
            }
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The Rust files some parent declares test-only with `#[cfg(test)] mod NAME;`
/// (a file that carries no `#[cfg(test)]` of its own, e.g. `mcp/origin_guard/tests.rs`).
///
/// Derived from the declarations themselves rather than kept as a hand list, so
/// a new test-module file needs no edit here and a production file can never be
/// waved through: only a path a `#[cfg(test)]` declaration resolves to is
/// returned (`mod x;` in `a/b.rs` → `a/b/x.rs` or `a/b/x/mod.rs`; in `mod.rs` /
/// `lib.rs` / `main.rs` → `a/x.rs` or `a/x/mod.rs`).
fn declared_test_only_files(sources: &[(PathBuf, String)]) -> HashSet<PathBuf> {
    let mut test_only = HashSet::new();
    for (p, text) in sources {
        let (_, declared) = rust_test_regions(&lex(text, Lang::Rust));
        let dir = p.parent().unwrap_or(Path::new("."));
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        for (name, path_attr) in declared {
            // `#[path = "x.rs"]` on a non-inline module is relative to the
            // directory of the file that declares it.
            if let Some(rel) = path_attr {
                test_only.insert(dir.join(rel));
                continue;
            }
            let own_dir = if matches!(stem, "mod" | "lib" | "main") {
                dir.to_path_buf()
            } else {
                dir.join(stem)
            };
            test_only.insert(own_dir.join(format!("{name}.rs")));
            test_only.insert(own_dir.join(&name).join("mod.rs"));
        }
    }
    test_only
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn no_runner_self_url_spells_localhost() {
    let base = manifest_dir();
    let mut hits = Vec::new();
    let mut summary = Vec::new();

    // Pass 1: Rust files a `#[cfg(test)] mod NAME;` declares test-only.
    let mut rust_files = Vec::new();
    walk(&base.join("src"), &mut rust_files);
    let rust_sources: Vec<(PathBuf, String)> = rust_files
        .into_iter()
        .filter(|p| lang_for(p) == Some(Lang::Rust))
        .filter_map(|p| fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    let test_only = declared_test_only_files(&rust_sources);

    for root in ROOTS {
        let root_dir = base.join(root.rel);
        let mut files = Vec::new();
        walk(&root_dir, &mut files);
        let mut scanned = 0usize;
        let mut skipped_test = 0usize;
        for p in files {
            let Some(ext) = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
            else {
                continue;
            };
            if !root.exts.contains(&ext.as_str()) {
                continue;
            }
            let rel_to_manifest = p
                .strip_prefix(&base)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if VENDORED
                .iter()
                .any(|v| rel_to_manifest.starts_with(&format!("{v}/")))
            {
                continue;
            }
            let rel_to_root = p
                .strip_prefix(&root_dir)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if is_test_path(&rel_to_root) || test_only.contains(&p) {
                skipped_test += 1;
                continue;
            }
            let Some(lang) = lang_for(&p) else { continue };
            let Ok(text) = fs::read_to_string(&p) else {
                continue;
            };
            scanned += 1;
            let display = format!("{}/{}", root.rel.trim_start_matches("../"), rel_to_root);
            let display = if root.rel == "src" {
                format!("src-tauri/{display}")
            } else {
                display
            };
            scan_text(&display, &text, lang, &mut hits);
        }
        println!(
            "root={} scanned={scanned} skipped_test={skipped_test} min_files={}",
            root.rel, root.min_files
        );
        summary.push((root.rel, scanned, root.min_files));
    }

    for (rel, scanned, min) in &summary {
        assert!(
            scanned >= min,
            "scanned only {scanned} files under {rel} (floor {min}): the directory moved or the walk broke, so a green run here would prove nothing"
        );
    }

    if !hits.is_empty() {
        let list: Vec<String> = hits
            .iter()
            .map(|h| format!("  {}:{}  {}", h.rel, h.line, h.text))
            .collect();
        panic!(
            "{} line(s) address a runner port as `localhost`. The runner binds 127.0.0.1 only, and on Windows \
             `localhost` tries ::1 first (+~0.3 s per new reqwest connection, +~2 s for Python/PowerShell).\n\
             Fix: Rust `crate::mcp::types::runner_loopback_base(port)` / `get_self_base_url*`; frontend \
             `getApiBase()` / `useApiBase()` / `getWsBase()` for this runner, `runnerBaseForPort(port)` for an \
             explicit port (src/lib/runner-api.ts); Python `runner_api_base()` \
             (python-bridge/runner_api.py); prompt/asset text: spell `127.0.0.1:<port>`. A URL that is NOT a runner: \
             add a `// {MARKER}: <what it targets>` comment on the line or the line above.\n{}",
            hits.len(),
            list.join("\n")
        );
    }
}

/// The matcher and the lexer, shown failing on known-bad input and passing on
/// known-good input — a guard that can only return "fine" proves nothing.
#[test]
fn the_guard_flags_every_forbidden_form_and_only_those() {
    let flag = |lang: Lang, src: &str| {
        let mut hits = Vec::new();
        scan_text("fixture", src, lang, &mut hits);
        !hits.is_empty()
    };
    // MUST flag.
    let bad: &[(Lang, &str)] = &[
        (
            Lang::Rust,
            r#"let u = format!("http://localhost:{}", port);"#,
        ),
        (
            Lang::Rust,
            r#"let u = format!("http://localhost:{port}/health");"#,
        ),
        (
            Lang::Rust,
            r#"client.get("http://localhost:9876/ui-bridge/sdk/elements")"#,
        ),
        (Lang::Rust, r#"let w = "ws://localhost:9877/events";"#),
        (Lang::Rust, r#"let p = "curl -s LocalHost:9899/health";"#),
        (Lang::Rust, "let s = r#\"curl http://localhost:9876/x\"#;"),
        (
            Lang::Rust,
            "let s = \"multi\nline http://localhost:9876/x\";",
        ),
        (
            Lang::Rust,
            r#"let u = "http://localhost:9876"; let n = "runner-localhost-ok";"#,
        ),
        (Lang::Js, "const API_BASE = \"http://localhost:9876\";"),
        (Lang::Js, "await fetch(`http://localhost:${port}/runners`);"),
        (
            Lang::Js,
            "const t = `first line\nhttp://localhost:${getApiPort()}/x`;",
        ),
        (Lang::Js, "const s = 'http://localhost:9878/x';"),
        (Lang::Py, "BASE_URL = \"http://localhost:9876\""),
        (Lang::Py, "url = f\"http://localhost:{port}/health\""),
        (Lang::Py, "url = \"http://localhost:%d/health\" % port"),
        (
            Lang::Py,
            "PROMPT = \"\"\"\nuse http://localhost:9876 for the API\n\"\"\"",
        ),
        (Lang::Text, "curl -s http://localhost:9876/health"),
        (
            Lang::Text,
            "{\"default\": \"http://localhost:9876/ui-bridge\"}",
        ),
        (Lang::Js, "<span>http://localhost:{proc.healthPort}</span>"),
    ];
    for (lang, src) in bad {
        assert!(
            flag(*lang, src),
            "the guard MISSED a forbidden form ({lang:?}): {src}"
        );
    }
    // MUST pass.
    let good: &[(Lang, &str)] = &[
        (Lang::Rust, r#"let u = format!("http://127.0.0.1:{}", port);"#),
        (Lang::Rust, r#"let u = "http://localhost:3001/build";"#),
        (Lang::Rust, r#"let u = "http://localhost:98765";"#),
        (Lang::Rust, r#"let u = "http://localhost:9875/health";"#),
        (Lang::Rust, "// e.g. http://localhost:9876/health"),
        (Lang::Rust, "/// doc: http://localhost:{port}"),
        (Lang::Rust, "/* http://localhost:9876 */ let a = 1;"),
        (Lang::Rust, "let a = 1; // http://localhost:9876"),
        (Lang::Rust, "let c = '\"'; // http://localhost:9876"),
        (Lang::Rust, r#"let u = format!("http://localhost:{}", vite_port); // runner-localhost-ok: Vite dev server"#),
        (Lang::Rust, "// runner-localhost-ok: a target app's dev server\nlet u = format!(\"http://localhost:{}\", p);"),
        (Lang::Rust, "#[cfg(test)]\nmod tests {\n    const U: &str = \"http://localhost:9876\";\n}"),
        (Lang::Rust, "#[cfg(test)] mod tests {\n    const U: &str = \"http://localhost:9876\";\n}"),
        (Lang::Js, "const u = `http://127.0.0.1:${port}/runners`;"),
        (Lang::Js, "// fetch(`http://localhost:${port}`)"),
        (Lang::Js, "const t = \"Don't\"; // http://localhost:9876"),
        (Lang::Js, "{/* runner-localhost-ok: a project process health port */}
<span>http://localhost:{proc.healthPort}</span>"),
        (Lang::Py, "# BASE = 'http://localhost:9876'"),
        (Lang::Py, "BASE = \"http://127.0.0.1:9876\""),
        (Lang::Text, "curl -s http://127.0.0.1:9876/health"),
    ];
    for (lang, src) in good {
        assert!(
            !flag(*lang, src),
            "the guard flagged an allowed form ({lang:?}): {src}"
        );
    }
}

#[test]
fn test_paths_are_recognised_by_name_outside_rust_only() {
    for p in [
        "foo/tests/bar.ts",
        "a/__tests__/b.tsx",
        "x.test.ts",
        "y.spec.tsx",
        "test_foo.py",
        "pkg/conftest.py",
    ] {
        assert!(is_test_path(p), "{p} should be treated as test code");
    }
    // Rust is never skipped by name: `mcp/image_quality_tests.rs` is a `pub mod`.
    for p in [
        "mcp/types.rs",
        "lib/runner-api.ts",
        "services/web_extraction_service.py",
        "a/tests.rs",
        "a/b_tests.rs",
        "mcp/image_quality_tests.rs",
    ] {
        assert!(!is_test_path(p), "{p} must not be skipped by name");
    }
}

/// A file a parent declares `#[cfg(test)] mod x;` is test-only; a same-named
/// file declared WITHOUT `#[cfg(test)]` (a production `*_tests.rs` module) is
/// not, and neither is a declaration that sits inside a string.
#[test]
fn test_only_module_files_come_from_cfg_test_declarations() {
    let src = |p: &str, t: &str| (PathBuf::from(p), t.to_string());
    let sources = vec![
        src("src/mcp/origin_guard.rs", "pub fn f() {}\n\n#[cfg(test)]\nmod tests;\n"),
        src("src/step_executor/mod.rs", "pub mod executor;\n#[cfg(test)]\nmod injected_step_tests;\n"),
        src("src/peer/mod.rs", "pub fn f() {}\n#[cfg(test)] mod one_line_tests;\n"),
        src(
            "src/step_executor/executor.rs",
            "#[cfg(test)]\n#[path = \"typed_dispatch_corpus_tests.rs\"]\nmod typed_dispatch_corpus_tests;\n",
        ),
        src(
            "src/mcp/mod.rs",
            "pub mod image_quality_tests;\nconst S: &str = \"#[cfg(test)] mod fake_tests;\";\n",
        ),
    ];
    let set = declared_test_only_files(&sources);
    assert!(
        set.contains(&PathBuf::from("src/mcp/origin_guard/tests.rs")),
        "{set:?}"
    );
    assert!(
        set.contains(&PathBuf::from("src/step_executor/injected_step_tests.rs"))
            && set.contains(&PathBuf::from("src/peer/one_line_tests.rs")),
        "{set:?}"
    );
    assert!(
        set.contains(&PathBuf::from(
            "src/step_executor/typed_dispatch_corpus_tests.rs"
        )),
        "a #[path] declaration resolves next to its parent: {set:?}"
    );
    assert!(
        !set.contains(&PathBuf::from("src/mcp/image_quality_tests.rs")),
        "a pub mod is production: {set:?}"
    );
    assert!(
        !set.contains(&PathBuf::from("src/mcp/fake_tests.rs")),
        "a declaration inside a string is not one: {set:?}"
    );
}
