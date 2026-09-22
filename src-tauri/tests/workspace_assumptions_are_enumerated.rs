//! The static workspace-assumption sweep: a checked-in ROSTER, not a gate.
//!
//! Phase 4 of plan
//! `2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports`.
//!
//! ## Why this test exists
//!
//! `capability_manifest::CAPABILITY_SPECS` reports only what someone thought to
//! list — the parent plan's admitted blind spot. This test is the other half: a
//! text scan of `src-tauri/src` (no binary, no model, no scoring — the shape of
//! `tauri_commands_are_registered.rs`) for a FIXED table of literal patterns that
//! each encode one way the runner can silently assume it is running on a
//! developer's workspace rather than on an operator's published install:
//!
//! | class id                | pattern family |
//! |-------------------------|----------------|
//! | `repo_layout`           | sibling-repo names as path components in string literals (`ui-bridge` only as a bare name, `../ui-bridge` or `ui-bridge/packages`); `QONTINUI_ROOT` reads |
//! | `dev_ports`             | `:8000` `:3001` `:9875` `:5432` `:5433` `:6379` `:9000` as host:port literals |
//! | `supervisor_dependency` | `9875` / supervisor URL, port or client call sites |
//! | `plans_dir`             | `.plans_dir` / `QONTINUI_PLANS_DIR` reads |
//! | `tenant_literal`        | UUID literals |
//! | `os_bound_tooling`      | `powershell` `pwsh` `cmd.exe` `taskkill` `schtasks` `.ps1` literals outside `cfg(windows)`; and a `cfg(windows)` fn with no non-windows sibling of the same name in its file |
//! | `machine_path`          | drive-letter and `/home/<name>` / `/Users/<name>` literals |
//!
//! Excluded from the scan: `#[cfg(test)]` items (inline modules, fns, `use`s),
//! whole files declared through `#[cfg(test)] mod x;` or carrying
//! `#![cfg(test)]`, everything outside `src/` (so `tests/` and the planted
//! fixtures), and every comment (line, block and doc).
//!
//! ## What fails, and what does not
//!
//! The ONLY failure is ROSTER STALENESS — a hit with no row in
//! `docs/workspace-assumptions.json`, or a row with no hit — plus the two vacuity
//! guards (the `scanned_files` floor, and every class matching its own planted
//! fixture). It does **not** fail on `unreviewed` or `defect` counts: the plan
//! gates nothing (parent Non-goal 2). It prints
//! `unreviewed=<n> defect=<n> scanned_files=<n> skipped_files=<n (reason)>`, so
//! "0 hits" is distinguishable from "scanned nothing".
//!
//! ## Dispositions
//!
//! Every row carries a disposition from `docs/workspace-assumptions.dispositions.toml`,
//! keyed by `(class, file, symbol)` — SYMBOL-keyed, so line drift does not orphan
//! them — one of `unreviewed` (the default for a key with no entry),
//! `fallback_correct`, `dev_only_surface`, or `defect(<plan stem>)`. A disposition
//! entry whose key no longer matches any hit is itself staleness.
//!
//! `repo_layout` rows whose enclosing symbol is named in a `CAPABILITY_SPECS`
//! `anchor` carry that row's id in `capability`; a `repo_layout` row with no
//! capability is visibly a gap in the manifest's roster.
//!
//! ## Regenerating
//!
//! ```text
//! UPDATE_WORKSPACE_ASSUMPTIONS=1 bash <root>/qontinui-claude-config/scripts/cargo-guard.sh \
//!     test --test workspace_assumptions_are_enumerated
//! ```
//!
//! rewrites `docs/workspace-assumptions.{json,md}` from the scan plus the
//! dispositions file. Review the diff like any other: a new row is a new
//! workspace assumption someone just shipped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

/// The plan whose Phase 4 this is — the stem `defect(...)` dispositions cite
/// when no dedicated defect plan exists yet.
const PLAN_STEM: &str = "2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports";

/// Vacuity floor on the number of `.rs` files actually scanned.
///
/// Derived at authoring (2026-09-22, `784129948`): `git ls-files 'src-tauri/src/*.rs'`
/// listed 1569 files; the sweep scanned 1553 and skipped 16 (14 declared through
/// `#[cfg(test)] mod x;`, 2 carrying `#![cfg(test)]`). The floor sits well under
/// that so ordinary deletions do not
/// trip it, and far above zero so a broken walker, a moved root or an empty dir
/// cannot pass with "0 hits".
const SCANNED_FILES_FLOOR: usize = 1200;

const ROSTER_JSON: &str = "../docs/workspace-assumptions.json";
const ROSTER_MD: &str = "../docs/workspace-assumptions.md";
const DISPOSITIONS_TOML: &str = "../docs/workspace-assumptions.dispositions.toml";
const FIXTURE_DIR: &str = "tests/fixtures/workspace-assumptions";
const UPDATE_ENV: &str = "UPDATE_WORKSPACE_ASSUMPTIONS";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ===========================================================================
// The class table — fixed, literal, no scoring.
// ===========================================================================

/// What a pattern is matched against.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The code text of a line with comments removed (string literals kept).
    Code,
    /// The raw source text of each string literal, one literal at a time.
    Literal,
}

struct PatternSpec {
    target: Target,
    pattern: &'static str,
    /// A literal/line also matching this is NOT a hit for this pattern.
    exclude: Option<&'static str>,
}

struct ClassSpec {
    id: &'static str,
    patterns: &'static [PatternSpec],
}

const CLASSES: &[ClassSpec] = &[
    ClassSpec {
        id: "repo_layout",
        patterns: &[
            // A sibling repo as a path component (or the whole literal). URLs and
            // `owner/repo` slugs are references to a repo, not to a layout.
            PatternSpec {
                target: Target::Literal,
                pattern: r"(?:^|[/\\])(?:qontinui-(?:claude-config|dev-notes|schemas|web|coord|supervisor|mcp|inspect|stack|mobile|devtools|root)|multistate)(?:[/\\]|$)",
                exclude: Some(r"://|^qontinui/"),
            },
            // The `ui-bridge` sibling repo — as a bare `.join("ui-bridge")`, a
            // `../ui-bridge` relative path, or its `ui-bridge/packages` tree. NOT
            // every `ui-bridge` path segment: the runner's own `/ui-bridge/...`
            // HTTP routes and `{base}/ui-bridge/sdk/...` URLs are not layout.
            PatternSpec {
                target: Target::Literal,
                pattern: r"^ui-bridge$|\.\.[/\\]ui-bridge\b|(?:^|[/\\])ui-bridge[/\\]packages\b",
                exclude: Some(r"://"),
            },
            PatternSpec {
                target: Target::Code,
                pattern: r"\bQONTINUI_(?:WORKSPACE_)?ROOT\b",
                exclude: None,
            },
        ],
    },
    ClassSpec {
        id: "dev_ports",
        patterns: &[PatternSpec {
            target: Target::Literal,
            pattern: r"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|host\.docker\.internal):(?:8000|3001|9875|5432|5433|6379|9000)\b",
            exclude: None,
        }],
    },
    ClassSpec {
        id: "supervisor_dependency",
        patterns: &[
            PatternSpec {
                target: Target::Code,
                pattern: r"\b9875\b",
                // `\u{9875}` is a CJK character escape, not a port.
                exclude: Some(r"\\u\{9875\}"),
            },
            PatternSpec {
                target: Target::Code,
                pattern: r"(?i)\b(?:get_|default_)?supervisor_?(?:url|base|port|endpoint|client|api|host|addr|socket_addr)\b|\bSupervisorClient\b|\bQONTINUI_SUPERVISOR\w*",
                exclude: None,
            },
        ],
    },
    ClassSpec {
        id: "plans_dir",
        patterns: &[PatternSpec {
            target: Target::Code,
            pattern: r"\.plans_dir\b|\bQONTINUI_PLANS_DIR\b",
            exclude: None,
        }],
    },
    ClassSpec {
        id: "tenant_literal",
        patterns: &[PatternSpec {
            target: Target::Literal,
            pattern: r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            exclude: None,
        }],
    },
    ClassSpec {
        id: "os_bound_tooling",
        // The second half of this class — a `cfg(windows)` fn with no
        // non-windows sibling — is structural, see `scan_source`.
        patterns: &[PatternSpec {
            target: Target::Literal,
            pattern: r"(?i)\b(?:powershell|pwsh|taskkill|schtasks)\b|\bcmd\.exe\b|\.ps1\b",
            exclude: None,
        }],
    },
    ClassSpec {
        id: "machine_path",
        patterns: &[
            PatternSpec {
                target: Target::Literal,
                pattern: r"(?:^|[^A-Za-z0-9])[A-Za-z]:(?:\\{1,2}|/)[A-Za-z_]",
                exclude: None,
            },
            PatternSpec {
                target: Target::Literal,
                pattern: r"/home/[A-Za-z_][A-Za-z0-9_.-]*|/Users/[A-Za-z]",
                exclude: None,
            },
        ],
    },
];

const OS_BOUND: &str = "os_bound_tooling";

struct CompiledClass {
    id: &'static str,
    patterns: Vec<(Target, Regex, Option<Regex>)>,
}

fn compile_classes() -> Vec<CompiledClass> {
    CLASSES
        .iter()
        .map(|c| CompiledClass {
            id: c.id,
            patterns: c
                .patterns
                .iter()
                .map(|p| {
                    (
                        p.target,
                        Regex::new(p.pattern).expect("class pattern compiles"),
                        p.exclude.map(|e| Regex::new(e).expect("exclude compiles")),
                    )
                })
                .collect(),
        })
        .collect()
}

// ===========================================================================
// Lexer — comments out, strings kept, literals extracted.
// ===========================================================================

#[derive(Default, Clone)]
struct Line {
    /// Comments removed; string literals kept verbatim.
    code: String,
    /// Comments removed; string-literal CONTENTS blanked. Structure is read here.
    skel: String,
    /// Raw source text of every string literal that STARTS on this line.
    literals: Vec<String>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lex(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().collect();
    let mut lines = vec![Line::default()];
    let n = chars.len();
    let mut i = 0;

    macro_rules! cur {
        () => {
            lines.last_mut().expect("at least one line")
        };
    }

    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if c == '\n' {
            lines.push(Line::default());
            i += 1;
            continue;
        }
        // Line comment (incl. doc comments).
        if c == '/' && next == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (nested).
        if c == '/' && next == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '\n' {
                    lines.push(Line::default());
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 1;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        // Raw string: r"..." / r#"..."# (optionally b-prefixed; the `b` was
        // already emitted as an ordinary char).
        let prev_ident = i > 0 && is_ident(chars[i - 1]) && !(chars[i - 1] == 'b' && (i < 2 || !is_ident(chars[i - 2])));
        if c == 'r' && !prev_ident {
            let mut j = i + 1;
            while j < n && chars[j] == '#' {
                j += 1;
            }
            if j < n && chars[j] == '"' {
                let hashes = j - (i + 1);
                let start_line = lines.len() - 1;
                for &ch in &chars[i..=j] {
                    cur!().code.push(ch);
                    cur!().skel.push(ch);
                }
                i = j + 1;
                let mut buf = String::new();
                loop {
                    if i >= n {
                        break;
                    }
                    let ch = chars[i];
                    if ch == '"' && (0..hashes).all(|k| chars.get(i + 1 + k) == Some(&'#')) {
                        for &cc in &chars[i..i + 1 + hashes] {
                            cur!().code.push(cc);
                            cur!().skel.push(cc);
                        }
                        i += 1 + hashes;
                        break;
                    }
                    if ch == '\n' {
                        lines.push(Line::default());
                    } else {
                        cur!().code.push(ch);
                        cur!().skel.push(' ');
                    }
                    buf.push(ch);
                    i += 1;
                }
                lines[start_line].literals.push(buf);
                continue;
            }
        }
        // Ordinary string.
        if c == '"' {
            let start_line = lines.len() - 1;
            cur!().code.push('"');
            cur!().skel.push('"');
            i += 1;
            let mut buf = String::new();
            while i < n {
                let ch = chars[i];
                if ch == '\\' && i + 1 < n {
                    let esc = chars[i + 1];
                    buf.push(ch);
                    buf.push(esc);
                    cur!().code.push(ch);
                    cur!().skel.push(' ');
                    if esc == '\n' {
                        lines.push(Line::default());
                    } else {
                        cur!().code.push(esc);
                        cur!().skel.push(' ');
                    }
                    i += 2;
                    continue;
                }
                if ch == '"' {
                    cur!().code.push('"');
                    cur!().skel.push('"');
                    i += 1;
                    break;
                }
                if ch == '\n' {
                    lines.push(Line::default());
                } else {
                    cur!().code.push(ch);
                    cur!().skel.push(' ');
                }
                buf.push(ch);
                i += 1;
            }
            lines[start_line].literals.push(buf);
            continue;
        }
        // Char literal vs lifetime.
        if c == '\'' {
            let is_char = match next {
                Some('\\') => true,
                Some(_) => chars.get(i + 2) == Some(&'\''),
                None => false,
            };
            if is_char {
                let mut j = i + 1;
                if chars[j] == '\\' {
                    j += 2;
                }
                while j < n && chars[j] != '\'' && chars[j] != '\n' {
                    j += 1;
                }
                // Blank the whole char literal in skel (it may be '{' or '"').
                for &ch in &chars[i..=j.min(n - 1)] {
                    cur!().code.push(ch);
                    cur!().skel.push(if ch == '\'' { '\'' } else { ' ' });
                }
                i = j + 1;
                continue;
            }
        }
        cur!().code.push(c);
        cur!().skel.push(c);
        i += 1;
    }
    lines
}

// ===========================================================================
// Structure — enclosing symbol, cfg(test) / cfg(windows) regions.
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CfgKind {
    Test,
    Windows,
    NotWindows,
}

fn attr_kind(attr: &str) -> Option<CfgKind> {
    let a: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    if a.starts_with("#[cfg(test)]") || a.starts_with("#[cfg(all(test,") {
        return Some(CfgKind::Test);
    }
    if a.starts_with("#[cfg(windows)]")
        || a.starts_with("#[cfg(target_os=\"windows\")]")
        || a.starts_with("#[cfg(all(windows,")
        || a.starts_with("#[cfg(all(target_os=\"windows\",")
    {
        return Some(CfgKind::Windows);
    }
    if a.starts_with("#[cfg(not(windows))]")
        || a.starts_with("#[cfg(not(target_os=\"windows\"))]")
        || a.starts_with("#[cfg(unix)]")
        || a.starts_with("#[cfg(target_os=\"linux\")]")
        || a.starts_with("#[cfg(target_os=\"macos\")]")
        || a.starts_with("#[cfg(any(unix")
        || a.starts_with("#[cfg(any(target_os=\"linux\"")
        || a.starts_with("#[cfg(any(target_os=\"macos\"")
    {
        return Some(CfgKind::NotWindows);
    }
    None
}

/// Split leading outer attributes off a trimmed code line. Returns the
/// attributes and the remaining item text.
fn split_attrs(line: &str) -> (Vec<String>, String) {
    let mut attrs = Vec::new();
    let mut rest = line.trim();
    while rest.starts_with("#[") {
        let mut depth = 0;
        let mut end = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            // Multi-line attribute: treat the whole line as attribute text.
            attrs.push(rest.to_string());
            return (attrs, String::new());
        };
        attrs.push(rest[..=end].to_string());
        rest = rest[end + 1..].trim_start();
    }
    (attrs, rest.to_string())
}

struct Region {
    kind: CfgKind,
    start_depth: i32,
    entered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    class: String,
    file: String,
    symbol: String,
    excerpt: String,
}

struct Structure {
    fn_re: Regex,
    impl_re: Regex,
    const_re: Regex,
    item_re: Regex,
    mod_decl_re: Regex,
    path_attr_re: Regex,
}

impl Structure {
    fn new() -> Self {
        Self {
            fn_re: Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            impl_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(impl|trait)\b(.*)$").unwrap(),
            const_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(const|static)\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            item_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(struct|enum|union|type|trait|mod|macro_rules!)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            mod_decl_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap(),
            path_attr_re: Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).unwrap(),
        }
    }

    /// The type name an `impl`/`trait` header names, e.g.
    /// `impl<T> Foo for Bar<T> where …` → `Bar`.
    fn impl_name(&self, skel_trimmed: &str) -> Option<String> {
        let caps = self.impl_re.captures(skel_trimmed)?;
        let mut rest = caps.get(2)?.as_str().trim_start().to_string();
        if caps.get(1)?.as_str() == "impl" && rest.starts_with('<') {
            let mut depth = 0;
            let mut cut = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            cut = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = rest[cut..].to_string();
        }
        if let Some(idx) = rest.rfind(" for ") {
            rest = rest[idx + 5..].to_string();
        }
        let head: String = rest
            .trim_start()
            .trim_start_matches('&')
            .trim_start_matches("dyn ")
            .chars()
            .take_while(|c| is_ident(*c) || *c == ':')
            .collect();
        let name = head.rsplit("::").next().unwrap_or("").to_string();
        (!name.is_empty()).then_some(name)
    }

    /// `(mod name, #[path] override)` for every `mod x;` declaration carrying
    /// the given cfg kind.
    fn cfg_mod_decls(&self, lines: &[Line], want: CfgKind) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        for line in lines {
            let (attrs, rest) = split_attrs(&line.code);
            pending.extend(attrs);
            if rest.is_empty() {
                continue;
            }
            if pending.iter().any(|a| attr_kind(a) == Some(want)) {
                if let Some(c) = self.mod_decl_re.captures(&rest) {
                    let path = pending
                        .iter()
                        .find_map(|a| self.path_attr_re.captures(a).map(|p| p[1].to_string()));
                    out.push((c[1].to_string(), path));
                }
            }
            pending.clear();
        }
        out
    }

    fn inner_cfg(&self, lines: &[Line], want: CfgKind) -> bool {
        lines.iter().any(|l| {
            let t: String = l.code.chars().filter(|c| !c.is_whitespace()).collect();
            match want {
                CfgKind::Test => t.starts_with("#![cfg(test)]"),
                CfgKind::Windows => {
                    t.starts_with("#![cfg(windows)]") || t.starts_with("#![cfg(target_os=\"windows\")]")
                }
                CfgKind::NotWindows => false,
            }
        })
    }
}

fn excerpt_of(code: &str) -> String {
    let collapsed = code.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 140 {
        let cut: String = collapsed.chars().take(137).collect();
        format!("{cut}...")
    } else {
        collapsed
    }
}

/// Scan one source file's text. `whole_file_windows` suppresses the literal
/// half of `os_bound_tooling` for a file that only compiles on Windows.
fn scan_source(
    st: &Structure,
    classes: &[CompiledClass],
    rel: &str,
    src: &str,
    whole_file_windows: bool,
) -> Vec<Hit> {
    let lines = lex(src);
    let mut hits = Vec::new();

    let mut depth: i32 = 0;
    let mut bracket: i32 = 0;
    // (name, depth at which its body opened)
    let mut fn_stack: Vec<(String, i32)> = Vec::new();
    let mut impl_stack: Vec<(String, i32)> = Vec::new();
    let mut pending_fn: Option<String> = None;
    let mut pending_impl: Option<String> = None;
    let mut const_ctx: Option<(String, i32)> = None;
    let mut pending_attrs: Vec<String> = Vec::new();
    let mut region: Option<Region> = None;
    // cfg(windows) / non-windows fn names, for the sibling rule.
    let mut windows_fns: Vec<(String, String)> = Vec::new(); // (qualified symbol, bare name)
    let mut other_os_fns: BTreeSet<String> = BTreeSet::new();

    for line in &lines {
        let skel_t = line.skel.trim();

        // ---- attributes / item starts --------------------------------------
        let mut line_fn_name: Option<String> = st.fn_re.captures(&line.skel).map(|c| c[1].to_string());
        if region.is_none() {
            let (attrs, rest) = split_attrs(&line.code);
            let had_attrs = !attrs.is_empty();
            pending_attrs.extend(attrs);
            if !rest.is_empty() && !pending_attrs.is_empty() {
                let kinds: Vec<CfgKind> = pending_attrs.iter().filter_map(|a| attr_kind(a)).collect();
                let kind = if kinds.contains(&CfgKind::Test) {
                    Some(CfgKind::Test)
                } else if kinds.contains(&CfgKind::Windows) {
                    Some(CfgKind::Windows)
                } else if kinds.contains(&CfgKind::NotWindows) {
                    Some(CfgKind::NotWindows)
                } else {
                    None
                };
                match kind {
                    Some(CfgKind::NotWindows) => {
                        if let Some(name) = &line_fn_name {
                            other_os_fns.insert(name.clone());
                        }
                    }
                    Some(k) => {
                        if k == CfgKind::Windows {
                            if let Some(name) = &line_fn_name {
                                let q = match impl_stack.last() {
                                    Some((t, _)) => format!("{t}::{name}"),
                                    None => name.clone(),
                                };
                                windows_fns.push((q, name.clone()));
                            }
                        }
                        region = Some(Region {
                            kind: k,
                            start_depth: depth,
                            entered: false,
                        });
                    }
                    None => {}
                }
                pending_attrs.clear();
            } else if !rest.is_empty() || (!had_attrs && !skel_t.is_empty()) {
                pending_attrs.clear();
            }
        }

        let in_test = matches!(&region, Some(r) if r.kind == CfgKind::Test);
        let in_windows = whole_file_windows || matches!(&region, Some(r) if r.kind == CfgKind::Windows);

        // ---- symbol for hits on this line ----------------------------------
        if fn_stack.is_empty() && const_ctx.is_none() {
            if let Some(c) = st.const_re.captures(skel_t) {
                if !matches!(&c[2], "fn" | "unsafe" | "async" | "extern") {
                    const_ctx = Some((format!("{} {}", &c[1], &c[2]), depth));
                }
            }
        }
        let symbol = {
            let sig_fn = line_fn_name.as_ref().or(pending_fn.as_ref());
            let base = if let Some(name) = sig_fn {
                Some(name.clone())
            } else if let Some((name, _)) = fn_stack.last() {
                Some(name.clone())
            } else {
                const_ctx.as_ref().map(|(n, _)| n.clone())
            };
            // The impl enclosing the fn (not one opened inside it).
            let fn_depth = fn_stack.last().map_or(i32::MAX, |(_, d)| *d);
            let owner = if sig_fn.is_some() {
                impl_stack.last()
            } else {
                impl_stack.iter().rev().find(|(_, d)| *d <= fn_depth)
            };
            match (base, owner) {
                (Some(b), Some((t, _))) => format!("{t}::{b}"),
                (Some(b), None) => b,
                (None, Some((t, _))) => format!("{t}::<impl>"),
                (None, None) => {
                    if let Some(c) = st.item_re.captures(skel_t) {
                        format!("{} {}", &c[1], &c[2])
                    } else if let Some(t) = st.impl_name(skel_t) {
                        format!("{t}::<impl>")
                    } else {
                        "<module>".to_string()
                    }
                }
            }
        };

        // ---- hits ----------------------------------------------------------
        if !in_test {
            for class in classes {
                if class.id == OS_BOUND && in_windows {
                    continue;
                }
                let mut excerpt: Option<String> = None;
                for (target, re, exclude) in &class.patterns {
                    let excluded = |t: &str| exclude.as_ref().is_some_and(|e| e.is_match(t));
                    excerpt = match target {
                        Target::Code => (re.is_match(&line.code) && !excluded(&line.code)).then(|| excerpt_of(&line.code)),
                        Target::Literal => line.literals.iter().find_map(|lit| {
                            let m = re.find(lit)?;
                            if excluded(lit) {
                                return None;
                            }
                            // A multi-line literal: quote the literal's own line
                            // holding the match, not the line that opens it.
                            Some(if lit.contains('\n') {
                                let start = lit[..m.start()].rfind('\n').map_or(0, |i| i + 1);
                                let end = lit[m.start()..].find('\n').map_or(lit.len(), |i| m.start() + i);
                                excerpt_of(&lit[start..end])
                            } else {
                                excerpt_of(&line.code)
                            })
                        }),
                    };
                    if excerpt.is_some() {
                        break;
                    }
                }
                if let Some(excerpt) = excerpt {
                    hits.push(Hit {
                        class: class.id.to_string(),
                        file: rel.to_string(),
                        symbol: symbol.clone(),
                        excerpt,
                    });
                }
            }
        }

        // ---- structure -----------------------------------------------------
        if let Some(name) = line_fn_name.take() {
            pending_fn = Some(name);
        }
        if let Some(name) = st.impl_name(skel_t) {
            pending_impl = Some(name);
        }
        for ch in line.skel.chars() {
            match ch {
                '(' | '[' => bracket += 1,
                ')' | ']' => bracket = (bracket - 1).max(0),
                '{' => {
                    depth += 1;
                    if let Some(name) = pending_fn.take() {
                        fn_stack.push((name, depth));
                    } else if let Some(name) = pending_impl.take() {
                        impl_stack.push((name, depth));
                    }
                    if let Some(r) = region.as_mut() {
                        if !r.entered && depth == r.start_depth + 1 {
                            r.entered = true;
                        }
                    }
                }
                '}' => {
                    depth -= 1;
                    while fn_stack.last().is_some_and(|(_, d)| *d > depth) {
                        fn_stack.pop();
                    }
                    while impl_stack.last().is_some_and(|(_, d)| *d > depth) {
                        impl_stack.pop();
                    }
                    if const_ctx.as_ref().is_some_and(|(_, d)| *d > depth) {
                        const_ctx = None;
                    }
                    if region.as_ref().is_some_and(|r| r.entered && depth == r.start_depth) {
                        region = None;
                    }
                }
                ';' if bracket == 0 => {
                    pending_fn = None;
                    pending_impl = None;
                    if const_ctx.as_ref().is_some_and(|(_, d)| *d == depth) {
                        const_ctx = None;
                    }
                    if region.as_ref().is_some_and(|r| !r.entered && depth == r.start_depth) {
                        region = None;
                    }
                }
                _ => {}
            }
        }
    }

    // ---- os_bound_tooling, structural half ---------------------------------
    if !whole_file_windows {
        for (q, bare) in windows_fns {
            if !other_os_fns.contains(&bare) {
                hits.push(Hit {
                    class: OS_BOUND.to_string(),
                    file: rel.to_string(),
                    symbol: q,
                    excerpt: format!("cfg(windows)-only fn `{bare}` has no cfg(not(windows)) sibling in this file"),
                });
            }
        }
    }
    hits
}

// ===========================================================================
// The walk.
// ===========================================================================

#[derive(Default)]
struct ScanResult {
    hits: Vec<Hit>,
    scanned_files: usize,
    /// reason -> count
    skipped: BTreeMap<String, usize>,
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Is `file` a crate root (its `mod x;` children live beside it)?
fn is_mod_root(src_root: &Path, file: &Path) -> bool {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "mod.rs" || name == "lib.rs" || name == "main.rs" {
        return true;
    }
    // src/bin/<x>.rs is a crate root too.
    file.parent() == Some(src_root.join("bin").as_path())
}

/// Scan every `.rs` file under `src_root`. `rel_base` is what hit paths are made
/// relative to (the crate root, so paths read `src/...`).
fn scan_tree(src_root: &Path, rel_base: &Path) -> ScanResult {
    let st = Structure::new();
    let classes = compile_classes();
    let mut files = Vec::new();
    walk(src_root, &mut files);
    files.sort();

    // Pass 1: read + lex every file; collect cfg(test)/cfg(windows) mod decls.
    let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut result = ScanResult::default();
    let mut test_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut test_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut windows_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut windows_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut inner_test: BTreeSet<PathBuf> = BTreeSet::new();

    for f in &files {
        let Ok(src) = fs::read_to_string(f) else {
            *result.skipped.entry("unreadable".into()).or_default() += 1;
            continue;
        };
        let lines = lex(&src);
        if st.inner_cfg(&lines, CfgKind::Test) {
            inner_test.insert(f.clone());
        }
        if st.inner_cfg(&lines, CfgKind::Windows) {
            windows_files.insert(f.clone());
        }
        let parent = f.parent().unwrap_or(src_root).to_path_buf();
        let child_dir = if is_mod_root(src_root, f) {
            parent.clone()
        } else {
            parent.join(f.file_stem().unwrap_or_default())
        };
        for (kind, set_f, set_d) in [
            (CfgKind::Test, &mut test_files, &mut test_dirs),
            (CfgKind::Windows, &mut windows_files, &mut windows_dirs),
        ] {
            for (name, path) in st.cfg_mod_decls(&lines, kind) {
                if let Some(p) = path {
                    set_f.insert(parent.join(p));
                } else {
                    set_f.insert(child_dir.join(format!("{name}.rs")));
                    set_d.insert(child_dir.join(&name));
                }
            }
        }
        sources.insert(f.clone(), src);
    }

    for (f, src) in &sources {
        let under = |dirs: &BTreeSet<PathBuf>| dirs.iter().any(|d| f.starts_with(d));
        if inner_test.contains(f) {
            *result.skipped.entry("#![cfg(test)]".into()).or_default() += 1;
            continue;
        }
        if test_files.contains(f) || under(&test_dirs) {
            *result.skipped.entry("cfg(test) mod decl".into()).or_default() += 1;
            continue;
        }
        let rel = f
            .strip_prefix(rel_base)
            .unwrap_or(f)
            .display()
            .to_string()
            .replace('\\', "/");
        let win = windows_files.contains(f) || under(&windows_dirs);
        result.hits.extend(scan_source(&st, &classes, &rel, src, win));
        result.scanned_files += 1;
    }
    result
}

fn check_floor(result: &ScanResult) -> Result<(), String> {
    if result.scanned_files < SCANNED_FILES_FLOOR {
        return Err(format!(
            "VACUOUS SCAN: scanned_files={} is below the floor of {SCANNED_FILES_FLOOR}. \
             The walker, the scan root or the cfg(test) exclusion is broken — this is \
             never a pass with zero hits.",
            result.scanned_files
        ));
    }
    Ok(())
}

// ===========================================================================
// Dispositions + roster.
// ===========================================================================

#[derive(Debug, Deserialize)]
struct DispositionsFile {
    #[serde(default)]
    disposition: Vec<DispositionEntry>,
}

#[derive(Debug, Deserialize)]
struct DispositionEntry {
    class: String,
    file: String,
    symbol: String,
    disposition: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    class: String,
    file: String,
    symbol: String,
    excerpt: String,
    count: usize,
    disposition: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Roster {
    schema: u32,
    generator: String,
    plan: String,
    rows: Vec<Row>,
}

fn valid_disposition(d: &str) -> bool {
    if matches!(d, "unreviewed" | "fallback_correct" | "dev_only_surface") {
        return true;
    }
    let re = Regex::new(r"^defect\(\d{4}-\d{2}-\d{2}-[a-z0-9-]+\)$").unwrap();
    re.is_match(d)
}

type Key = (String, String, String);

fn load_dispositions(root: &Path) -> Result<BTreeMap<Key, (String, Option<String>)>, String> {
    let path = root.join(DISPOSITIONS_TOML);
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let parsed: DispositionsFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for e in parsed.disposition {
        if !valid_disposition(&e.disposition) {
            return Err(format!(
                "invalid disposition {:?} for ({}, {}, {}) — must be unreviewed | fallback_correct \
                 | dev_only_surface | defect(<plan stem>)",
                e.disposition, e.class, e.file, e.symbol
            ));
        }
        let key = (e.class.clone(), e.file.clone(), e.symbol.clone());
        if out.insert(key, (e.disposition, e.note)).is_some() {
            return Err(format!("duplicate disposition key ({}, {}, {})", e.class, e.file, e.symbol));
        }
    }
    Ok(out)
}

/// `(capability id, identifier tokens of its anchor)` for every CAPABILITY_SPECS
/// row, read as TEXT out of `capability_manifest.rs` (this test builds no binary).
fn capability_anchors(root: &Path) -> Vec<(String, BTreeSet<String>)> {
    let src = fs::read_to_string(root.join("src/capability_manifest.rs")).unwrap_or_default();
    let Some(start) = src.find("pub const CAPABILITY_SPECS") else {
        return Vec::new();
    };
    let body = &src[start..];
    let body = &body[..body.find("\n];").unwrap_or(body.len())];
    let id_re = Regex::new(r#"\bid:\s*"([a-z_]+)""#).unwrap();
    let anchor_re = Regex::new(r#"\banchor:\s*"([^"]*)""#).unwrap();
    let ids: Vec<String> = id_re.captures_iter(body).map(|c| c[1].to_string()).collect();
    let anchors: Vec<String> = anchor_re.captures_iter(body).map(|c| c[1].to_string()).collect();
    ids.into_iter()
        .zip(anchors)
        .map(|(id, a)| {
            let toks = a
                .split(|c: char| !is_ident(c))
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();
            (id, toks)
        })
        .collect()
}

/// A `repo_layout` row feeds a capability when its enclosing symbol's final
/// segment AND its module (file stem or a parent dir) are both named in that
/// capability's anchor. Direct naming only — a caller two frames up is not
/// traced, which is the gap the `capability`-less rows make visible.
fn capability_for(hit: &Hit, anchors: &[(String, BTreeSet<String>)]) -> Option<String> {
    if hit.class != "repo_layout" {
        return None;
    }
    let sym = hit
        .symbol
        .rsplit("::")
        .next()
        .unwrap_or("")
        .rsplit(' ')
        .next()
        .unwrap_or("");
    let path = Path::new(&hit.file);
    let mut modules: Vec<String> = path
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if let Some(stem) = path.file_stem() {
        modules.push(stem.to_string_lossy().to_string());
    }
    anchors
        .iter()
        .find(|(_, toks)| toks.contains(sym) && modules.iter().any(|m| toks.contains(m)))
        .map(|(id, _)| id.clone())
}

struct Built {
    roster: Roster,
    orphan_dispositions: Vec<Key>,
}

fn build_roster(root: &Path, hits: &[Hit]) -> Result<Built, String> {
    let dispositions = load_dispositions(root)?;
    let anchors = capability_anchors(root);

    let mut counted: BTreeMap<Hit, usize> = BTreeMap::new();
    for h in hits {
        *counted.entry(h.clone()).or_default() += 1;
    }
    let mut used: BTreeSet<Key> = BTreeSet::new();
    let rows: Vec<Row> = counted
        .into_iter()
        .map(|(h, count)| {
            let key = (h.class.clone(), h.file.clone(), h.symbol.clone());
            let (disposition, note) = match dispositions.get(&key) {
                Some((d, n)) => {
                    used.insert(key);
                    (d.clone(), n.clone())
                }
                None => ("unreviewed".to_string(), None),
            };
            Row {
                capability: capability_for(&h, &anchors),
                class: h.class,
                file: h.file,
                symbol: h.symbol,
                excerpt: h.excerpt,
                count,
                disposition,
                note,
            }
        })
        .collect();
    let orphan_dispositions = dispositions.keys().filter(|k| !used.contains(*k)).cloned().collect();
    Ok(Built {
        roster: Roster {
            schema: 1,
            generator: format!(
                "GENERATED by src-tauri/tests/workspace_assumptions_are_enumerated.rs — \
                 regenerate with {UPDATE_ENV}=1; edit dispositions in \
                 docs/workspace-assumptions.dispositions.toml, never here"
            ),
            plan: PLAN_STEM.to_string(),
            rows,
        },
        orphan_dispositions,
    })
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('`', "'")
}

fn render_md(roster: &Roster) -> String {
    let mut out = String::new();
    out.push_str("# Workspace assumptions — the static roster\n\n");
    out.push_str(
        "<!-- GENERATED by src-tauri/tests/workspace_assumptions_are_enumerated.rs. \
         Do not edit by hand: set dispositions in \
         docs/workspace-assumptions.dispositions.toml and regenerate with \
         UPDATE_WORKSPACE_ASSUMPTIONS=1. -->\n\n",
    );
    out.push_str(&format!(
        "Every place `src-tauri/src` (outside `cfg(test)` and comments) matches one of the \
         fixed workspace-assumption patterns — Phase 4 of plan `{PLAN_STEM}`. A roster, not \
         a gate: the test fails only when this file is stale, never on a count. \
         `docs/workspace-assumptions.json` is the machine-readable twin.\n\n"
    ));
    out.push_str("Dispositions: `unreviewed` (not yet triaged), `fallback_correct` (the \
                  assumption degrades correctly off a workspace), `dev_only_surface` (only a \
                  developer box reaches it), `defect(<plan stem>)` (a user-facing path that \
                  depends on a workspace, cited to the plan that owns the fix).\n\n");

    let mut by_class: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for c in CLASSES {
        by_class.insert(c.id, Vec::new());
    }
    for r in &roster.rows {
        by_class.entry(r.class.as_str()).or_default().push(r);
    }

    out.push_str("| class | rows | hits | unreviewed | fallback_correct | dev_only_surface | defect |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for c in CLASSES {
        let rows = &by_class[c.id];
        let n = |p: &dyn Fn(&str) -> bool| rows.iter().filter(|r| p(&r.disposition)).count();
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            c.id,
            rows.len(),
            rows.iter().map(|r| r.count).sum::<usize>(),
            n(&|d| d == "unreviewed"),
            n(&|d| d == "fallback_correct"),
            n(&|d| d == "dev_only_surface"),
            n(&|d| d.starts_with("defect(")),
        ));
    }
    out.push('\n');

    for c in CLASSES {
        let rows = &by_class[c.id];
        out.push_str(&format!("## `{}` ({} rows)\n\n", c.id, rows.len()));
        if rows.is_empty() {
            out.push_str("_No hits._\n\n");
            continue;
        }
        let cap_col = c.id == "repo_layout";
        if cap_col {
            out.push_str("| file | symbol | excerpt | n | disposition | capability |\n|---|---|---|---:|---|---|\n");
        } else {
            out.push_str("| file | symbol | excerpt | n | disposition |\n|---|---|---|---:|---|\n");
        }
        for r in rows {
            let disp = match &r.note {
                Some(n) => format!("{} — {}", r.disposition, md_escape(n)),
                None => r.disposition.clone(),
            };
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | {} | {}",
                r.file,
                md_escape(&r.symbol),
                md_escape(&r.excerpt),
                r.count,
                disp
            ));
            if cap_col {
                out.push_str(&format!(
                    " | {} |\n",
                    r.capability
                        .as_deref()
                        .map_or("— (no CAPABILITY_SPECS row)".to_string(), |c| format!("`{c}`"))
                ));
            } else {
                out.push_str(" |\n");
            }
        }
        out.push('\n');
    }
    out
}

fn to_json(roster: &Roster) -> String {
    let mut s = serde_json::to_string_pretty(roster).expect("roster serializes");
    s.push('\n');
    s
}

fn row_ident(r: &Row) -> String {
    format!("[{}] {} :: {} :: {}", r.class, r.file, r.symbol, r.excerpt)
}

fn staleness(checked: &str, fresh: &Roster) -> Vec<String> {
    let old: Vec<Row> = serde_json::from_str::<Roster>(checked).map(|r| r.rows).unwrap_or_default();
    let old_map: BTreeMap<String, &Row> = old.iter().map(|r| (row_ident(r), r)).collect();
    let new_map: BTreeMap<String, &Row> = fresh.rows.iter().map(|r| (row_ident(r), r)).collect();
    let mut out = Vec::new();
    for (k, r) in &new_map {
        match old_map.get(k) {
            None => out.push(format!("HIT WITH NO ROW: {k}  (enclosing fn `{}` in {})", r.symbol, r.file)),
            Some(o) if o != r => out.push(format!(
                "ROW CHANGED: {k}  (count {}→{}, disposition {}→{}, capability {:?}→{:?})",
                o.count, r.count, o.disposition, r.disposition, o.capability, r.capability
            )),
            _ => {}
        }
    }
    for k in old_map.keys() {
        if !new_map.contains_key(k) {
            out.push(format!("ROW WITH NO HIT: {k}"));
        }
    }
    out
}

// ===========================================================================
// Tests.
// ===========================================================================

#[test]
fn workspace_assumption_roster_is_fresh() {
    let root = crate_root();
    let result = scan_tree(&root.join("src"), &root);

    let built = build_roster(&root, &result.hits).unwrap_or_else(|e| panic!("{e}"));
    let rows = &built.roster.rows;
    let unreviewed = rows.iter().filter(|r| r.disposition == "unreviewed").count();
    let defect = rows.iter().filter(|r| r.disposition.starts_with("defect(")).count();
    let skipped_total: usize = result.skipped.values().sum();
    let skipped_detail = result
        .skipped
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "unreviewed={unreviewed} defect={defect} scanned_files={} skipped_files={skipped_total} ({skipped_detail})",
        result.scanned_files
    );
    let mut per_class: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in rows {
        let e = per_class.entry(r.class.as_str()).or_default();
        e.0 += 1;
        e.1 += r.count;
    }
    for (c, (n_rows, n_hits)) in &per_class {
        println!("  {c}: rows={n_rows} hits={n_hits}");
    }

    check_floor(&result).unwrap_or_else(|e| panic!("{e}"));

    let json = to_json(&built.roster);
    let md = render_md(&built.roster);
    let json_path = root.join(ROSTER_JSON);
    let md_path = root.join(ROSTER_MD);

    if std::env::var(UPDATE_ENV).is_ok_and(|v| v == "1") {
        assert!(
            built.orphan_dispositions.is_empty(),
            "dispositions with no matching hit (fix the key or delete the entry): {:#?}",
            built.orphan_dispositions
        );
        fs::write(&json_path, &json).expect("write roster json");
        fs::write(&md_path, &md).expect("write roster md");
        println!("regenerated {} and {}", json_path.display(), md_path.display());
        return;
    }

    let checked_json = fs::read_to_string(&json_path).unwrap_or_default().replace("\r\n", "\n");
    let checked_md = fs::read_to_string(&md_path).unwrap_or_default().replace("\r\n", "\n");

    let mut problems = staleness(&checked_json, &built.roster);
    for (c, f, s) in &built.orphan_dispositions {
        problems.push(format!("DISPOSITION WITH NO HIT: ({c}, {f}, {s}) in {DISPOSITIONS_TOML}"));
    }
    if problems.is_empty() && checked_json != json {
        problems.push("docs/workspace-assumptions.json differs from a fresh render (header/order)".into());
    }
    if checked_md != md {
        problems.push("docs/workspace-assumptions.md differs from a fresh render".into());
    }
    assert!(
        problems.is_empty(),
        "\nThe workspace-assumption roster is STALE ({} problem(s)):\n\n  {}\n\n\
         A new row is a new workspace assumption: review it, give it a disposition in \
         docs/workspace-assumptions.dispositions.toml if you can, then regenerate with\n  \
         {UPDATE_ENV}=1 cargo-guard.sh test --test workspace_assumptions_are_enumerated\n",
        problems.len(),
        problems.join("\n  ")
    );
}

/// Every class matches its own planted fixture — so a class whose regex rots to
/// matching nothing cannot hide behind "0 hits". The fixtures live under
/// `tests/`, which the real scan never walks.
#[test]
fn every_class_matches_its_planted_fixture() {
    let root = crate_root();
    let st = Structure::new();
    let classes = compile_classes();
    for class in CLASSES {
        let path = root.join(FIXTURE_DIR).join(format!("{}.rs.txt", class.id));
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()));
        let hits = scan_source(&st, &classes, "fixture.rs", &src, false);
        let mine: Vec<&Hit> = hits.iter().filter(|h| h.class == class.id).collect();
        assert!(
            !mine.is_empty(),
            "class `{}` matched nothing in its planted fixture {} — the pattern is broken",
            class.id,
            path.display()
        );
        assert!(
            mine.iter().all(|h| h.symbol.starts_with("planted")),
            "class `{}` fixture hits must be attributed to a `planted*` fn, got {:?}",
            class.id,
            mine
        );
    }
}

/// Everything under `cfg(test)`, `cfg(windows)` (for the literal half of
/// `os_bound_tooling`) and in comments is excluded.
#[test]
fn excluded_regions_yield_no_hits() {
    let root = crate_root();
    let path = root.join(FIXTURE_DIR).join("excluded.rs.txt");
    let src = fs::read_to_string(&path).expect("excluded fixture");
    let hits = scan_source(&Structure::new(), &compile_classes(), "fixture.rs", &src, false);
    assert!(hits.is_empty(), "excluded regions produced hits: {hits:#?}");
}

/// Pointing the scan root at an empty dir fails on the floor rather than passing
/// with zero hits.
#[test]
fn empty_scan_root_fails_on_the_scanned_files_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = scan_tree(dir.path(), dir.path());
    assert_eq!(result.scanned_files, 0);
    assert!(result.hits.is_empty());
    let err = check_floor(&result).expect_err("an empty scan root must fail the floor");
    assert!(err.contains("VACUOUS SCAN"), "{err}");
}
