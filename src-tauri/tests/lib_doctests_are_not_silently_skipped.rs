//! Regression gate: **the lib crate carries no doctest that CI would run —
//! because CI no longer runs doctests at all.**
//!
//! `test` in `.github/workflows/ci.yml` builds with `cargo test --no-run` and
//! then executes the binaries that build produced, never invoking cargo a
//! second time (plan
//! `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`,
//! Phase 1; the second invocation pays a full rebuild because the package is
//! an input to itself). `--no-run` does not build doctests, so they have no
//! `Executable` line to recover and are simply never run.
//!
//! That cost nothing on the day it shipped: the whole doctest target was one
//! `ignore`d block (`accessibility::query::QueryBuilder`). What it leaves is a
//! trap — the first real doctest anyone writes in the lib, or the first module
//! carrying one that moves from the bin tree into the lib (plan
//! `2026-08-06-runner-move-bin-module-tree-into-lib-crate` does exactly that,
//! and the bin tree holds dozens of `rust` fences today), compiles nowhere and
//! runs nowhere, with no warning. This test is the warning. It closes the cheap
//! half of plan-library follow-up edge `2bd27517-55f9-4cc3-9aee-667426a349c7`;
//! the real fix (running doctests again) waits on the rebuild defect, edges
//! `ba91179e-…` / `fbd78cd3-…`.
//!
//! # What counts
//!
//! rustdoc only tests the LIB target, so the sweep walks the lib's module tree
//! from `src/lib.rs` — `mod x;` declarations, inline `mod x { … }` blocks and
//! `#[path]` attributes — rather than globbing `src/`, which would flag every
//! bin-only fence. A fenced block in a `///` or `//!` comment counts when
//! rustdoc would treat it as Rust (an empty info string, `rust`, or only
//! rustdoc's own attributes) and it is not `ignore`d. `no_run` and
//! `compile_fail` count too: they are compile-checked doctests, and nothing
//! compiles them either.
//!
//! The walk is fail-loud rather than best-effort: a `mod x;` it cannot resolve
//! to a file, a block doc comment (`/** */`, `/*! */`) or a `#[doc = …]`
//! attribute — forms it does not read fences from — fails the test instead of
//! being skipped, because a skipped file is exactly how a doctest would hide.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

// ---------------------------------------------------------------------------
// A small lexer: enough of Rust's token rules to tell code from comments and
// literals, so a brace or a `mod` inside a string is never read as structure.
// ---------------------------------------------------------------------------

/// One `//` comment, with the text after the two slashes.
struct LineComment {
    line: usize,
    text: String,
}

struct Lexed {
    /// The source with every comment and every string/char literal's contents
    /// replaced by spaces. Newlines are preserved, so line numbers agree.
    code: String,
    line_comments: Vec<LineComment>,
    /// Lines on which a `/** */` or `/*! */` doc comment opens.
    block_doc_lines: Vec<usize>,
}

fn lex(src: &str) -> Lexed {
    let chars: Vec<char> = src.chars().collect();
    let mut code = String::with_capacity(src.len());
    let mut line_comments = Vec::new();
    let mut block_doc_lines = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;

    // Blank a run of chars into `code`, keeping newlines.
    fn blank(code: &mut String, line: &mut usize, c: char) {
        if c == '\n' {
            code.push('\n');
            *line += 1;
        } else {
            code.push(' ');
        }
    }

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        // Line comment.
        if c == '/' && next == Some('/') {
            let start = i + 2;
            let mut j = start;
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            line_comments.push(LineComment {
                line,
                text: chars[start..j].iter().collect(),
            });
            for _ in i..j {
                code.push(' ');
            }
            i = j;
            continue;
        }

        // Block comment (nesting, as Rust's are).
        if c == '/' && next == Some('*') {
            let third = chars.get(i + 2).copied();
            let fourth = chars.get(i + 3).copied();
            // `/**` is a doc comment unless it is `/***…` or the empty `/**/`.
            let outer_doc = third == Some('*') && fourth != Some('*') && fourth != Some('/');
            let inner_doc = third == Some('!');
            if outer_doc || inner_doc {
                block_doc_lines.push(line);
            }
            let mut depth = 0usize;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    code.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    code.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut code, &mut line, chars[i]);
                    i += 1;
                }
            }
            continue;
        }

        // Raw string: r"…", r#"…"#, br"…", cr"…" (the prefix letters are code).
        if c == 'r' && (next == Some('"') || next == Some('#')) && !prev_is_ident(&chars, i) {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                code.push('r');
                for _ in 0..hashes {
                    code.push('#');
                }
                code.push('"');
                j += 1;
                loop {
                    if j >= chars.len() {
                        break;
                    }
                    if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
                        code.push('"');
                        for _ in 0..hashes {
                            code.push('#');
                        }
                        j += 1 + hashes;
                        break;
                    }
                    blank(&mut code, &mut line, chars[j]);
                    j += 1;
                }
                i = j;
                continue;
            }
            // `r#ident` (a raw identifier) — plain code.
        }

        // Ordinary string literal (also the tail of b"…" / c"…").
        if c == '"' {
            code.push('"');
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\\' {
                    blank(&mut code, &mut line, chars[j]);
                    if let Some(&e) = chars.get(j + 1) {
                        blank(&mut code, &mut line, e);
                    }
                    j += 2;
                    continue;
                }
                if chars[j] == '"' {
                    code.push('"');
                    j += 1;
                    break;
                }
                blank(&mut code, &mut line, chars[j]);
                j += 1;
            }
            i = j;
            continue;
        }

        // Char literal vs lifetime/label: `'x'`, `'\n'`, `'\u{..}'` are
        // literals; `'a` followed by anything but a closing quote is not.
        if c == '\'' {
            if next == Some('\\') {
                // Start past the escaped character itself, so `'\''` closes
                // on its own final quote rather than on the escaped one.
                let mut j = i + 3;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                for _ in i..=j.min(chars.len() - 1) {
                    code.push(' ');
                }
                i = j + 1;
                continue;
            }
            if next.is_some() && chars.get(i + 2) == Some(&'\'') {
                code.push_str("   ");
                i += 3;
                continue;
            }
        }

        if c == '\n' {
            line += 1;
        }
        code.push(c);
        i += 1;
    }

    Lexed {
        code,
        line_comments,
        block_doc_lines,
    }
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    // `br"…"` / `cr"…"` keep their raw-string meaning; any other identifier
    // character before the `r` makes it part of a longer identifier.
    match i.checked_sub(1).map(|p| chars[p]) {
        Some('b') | Some('c') => i
            .checked_sub(2)
            .map(|p| chars[p].is_alphanumeric() || chars[p] == '_')
            .unwrap_or(false),
        Some(p) => p.is_alphanumeric() || p == '_',
        None => false,
    }
}

// ---------------------------------------------------------------------------
// The module walk.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Sweep {
    files: BTreeSet<PathBuf>,
    /// (file, line, info string) for every fenced block that rustdoc would
    /// build as a doctest and not ignore.
    live_doctests: Vec<(PathBuf, usize, String)>,
    /// (file, line, info string) for `ignore`d Rust blocks — reported as
    /// ignored tests, which is harmless, but counted so the sweep can be
    /// checked against CI's own last measurement.
    ignored_doctests: Vec<(PathBuf, usize, String)>,
    /// Anything the walk could not read. Non-empty fails the test.
    problems: Vec<String>,
}

fn ident_after(code: &str, kw_end: usize) -> Option<(String, usize)> {
    let rest = &code[kw_end..];
    let trimmed = rest.trim_start();
    let mut offset = kw_end + (rest.len() - trimmed.len());
    // A raw identifier (`mod r#type;`) names the module `type`.
    let trimmed = match trimmed.strip_prefix("r#") {
        Some(t) => {
            offset += 2;
            t
        }
        None => trimmed,
    };
    let len = trimmed
        .char_indices()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .map(|(i, c)| i + c.len_utf8())
        .last()?;
    let name = trimmed[..len].to_string();
    Some((name, offset + len))
}

/// Byte offsets of `mod` used as a keyword (whole word) in a code line.
fn mod_keywords(code_line: &str) -> Vec<usize> {
    let bytes = code_line.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(pos) = code_line[start..].find("mod") {
        let at = start + pos;
        let before_ok = at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let after = at + 3;
        let after_ok = after < bytes.len() && (bytes[after] as char).is_whitespace();
        if before_ok && after_ok {
            out.push(at);
        }
        start = at + 3;
    }
    out
}

fn walk(sweep: &mut Sweep, file: &Path, is_crate_root: bool) {
    let file = file.to_path_buf();
    if !sweep.files.insert(file.clone()) {
        return;
    }
    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            sweep.problems.push(format!("{}: unreadable: {e}", file.display()));
            return;
        }
    };
    let lexed = lex(&src);
    let raw_lines: Vec<&str> = src.lines().collect();

    for l in &lexed.block_doc_lines {
        sweep.problems.push(format!(
            "{}:{l}: a block doc comment (`/** */` / `/*! */`) — this sweep does not read fences from that form, so it cannot vouch for the doctests in it; rewrite it as `///` / `//!`",
            file.display()
        ));
    }

    // `mod.rs` and the crate root own their directory; `foo.rs` owns `foo/`.
    let dir = file.parent().unwrap().to_path_buf();
    let stem = file.file_stem().unwrap().to_string_lossy().to_string();
    let own_dir = if is_crate_root || stem == "mod" {
        dir.clone()
    } else {
        dir.join(&stem)
    };

    // Inline-module stack: (name, brace depth at which the block closes).
    let mut inline: Vec<(String, i64)> = Vec::new();
    let mut depth: i64 = 0;
    let mut pending_path: Option<String> = None;

    for (idx, code_line) in lexed.code.lines().enumerate() {
        let line_no = idx + 1;
        let raw = raw_lines.get(idx).copied().unwrap_or("");
        let trimmed_code = code_line.trim_start();

        if trimmed_code.starts_with("#[") || trimmed_code.starts_with("#![") {
            let compact: String = trimmed_code.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.starts_with("#[path=") {
                // The string contents were blanked in `code`; read them raw.
                if let (Some(a), Some(b)) = (raw.find('"'), raw.rfind('"')) {
                    if b > a {
                        pending_path = Some(raw[a + 1..b].to_string());
                    }
                }
            }
            if compact.starts_with("#[doc=") || compact.starts_with("#![doc=") {
                sweep.problems.push(format!(
                    "{}:{line_no}: a `#[doc = …]` attribute — this sweep does not read fences from attribute docs, so it cannot vouch for them",
                    file.display()
                ));
            }
        }

        let mods = mod_keywords(code_line);
        let mut cursor = 0usize;
        for kw in mods {
            // Braces before this `mod` on the same line still count.
            depth += brace_delta(&code_line[cursor..kw]);
            let Some((name, after)) = ident_after(code_line, kw + 3) else {
                sweep.problems.push(format!(
                    "{}:{line_no}: a `mod` keyword not followed by a module name this sweep can read — it cannot vouch for a module it cannot resolve",
                    file.display()
                ));
                cursor = kw + 3;
                continue;
            };
            let tail = code_line[after..].trim_start();
            let base = inline
                .iter()
                .fold(own_dir.clone(), |d, (n, _)| d.join(n));
            if tail.starts_with(';') {
                let target = match pending_path.take() {
                    Some(p) if inline.is_empty() => dir.join(p),
                    Some(p) => base.join(p),
                    None => {
                        let flat = base.join(format!("{name}.rs"));
                        if flat.exists() {
                            flat
                        } else {
                            base.join(&name).join("mod.rs")
                        }
                    }
                };
                if target.exists() {
                    walk(sweep, &target, false);
                } else {
                    sweep.problems.push(format!(
                        "{}:{line_no}: `mod {name};` resolves to no file (looked for {}) — the sweep cannot vouch for a module it cannot find",
                        file.display(),
                        target.display()
                    ));
                }
                cursor = after;
            } else if tail.starts_with('{') {
                // The block's own `{` is counted below with the rest of the
                // line; it closes when depth returns to where it is now.
                inline.push((name, depth));
                pending_path = None;
                cursor = after;
            } else {
                // `mod x` with its `;` or `{` on a later line. rustfmt never
                // writes that; refusing it keeps the walk from guessing.
                sweep.problems.push(format!(
                    "{}:{line_no}: `mod {name}` is not followed by `;` or `{{` on the same line — the sweep reads module declarations one line at a time and cannot resolve this one",
                    file.display()
                ));
                cursor = after;
            }
        }
        for ch in code_line[cursor..].chars() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    while inline.last().is_some_and(|(_, d)| depth <= *d) {
                        inline.pop();
                    }
                }
                _ => {}
            }
        }
        if !trimmed_code.is_empty() && !trimmed_code.starts_with("#[") && !trimmed_code.starts_with("#![") {
            pending_path = None;
        }
    }

    scan_doc_fences(sweep, &file, &lexed.line_comments);
}

fn brace_delta(s: &str) -> i64 {
    s.chars().fold(0, |acc, c| match c {
        '{' => acc + 1,
        '}' => acc - 1,
        _ => acc,
    })
}

// ---------------------------------------------------------------------------
// Fences.
// ---------------------------------------------------------------------------

/// rustdoc's own attribute words in a fence's info string. Any word outside
/// this set (`text`, `json`, `bash`, …) makes the block "not Rust" unless a
/// Rust word is also present — rustdoc's `LangString` rule.
fn is_rustdoc_word(w: &str) -> bool {
    matches!(
        w,
        "rust"
            | "should_panic"
            | "no_run"
            | "ignore"
            | "test_harness"
            | "compile_fail"
            | "standalone_crate"
            | "allow_fail"
    ) || w.starts_with("ignore-")
        || w.starts_with("edition")
        || (w.len() == 5 && w.starts_with('E') && w[1..].chars().all(|c| c.is_ascii_digit()))
}

/// (is Rust, is ignored) for a fence info string.
fn classify(info: &str) -> (bool, bool) {
    let words: Vec<&str> = info
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let seen_rust = words.iter().any(|w| is_rustdoc_word(w));
    let seen_other = words.iter().any(|w| !is_rustdoc_word(w));
    let is_rust = !seen_other || seen_rust;
    let ignored = words.iter().any(|w| *w == "ignore" || w.starts_with("ignore-"));
    (is_rust, ignored)
}

/// A CommonMark fence line: up to three spaces, then a run of three or more
/// backticks or tildes. Returns (fence char, run length, info string).
fn fence(doc_text: &str) -> Option<(char, usize, String)> {
    let indent = doc_text.len() - doc_text.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &doc_text[indent..];
    let ch = rest.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = rest.chars().take_while(|c| *c == ch).count();
    if run < 3 {
        return None;
    }
    let info = rest[run..].trim().to_string();
    // A backtick fence's info string may not itself contain a backtick —
    // "```` ```x` ```` is inline code" opens no block.
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, run, info))
}

fn scan_doc_fences(sweep: &mut Sweep, file: &Path, comments: &[LineComment]) {
    // Group the doc comments into blocks: consecutive lines of one kind
    // (`///` or `//!`). A gap or a switch of kind ends a block, and rustdoc
    // renders each block as its own Markdown document.
    let mut block: Vec<(usize, String)> = Vec::new();
    let mut last: Option<(usize, bool)> = None;
    for c in comments {
        // `///x` is an outer doc comment but `////x` is not; `//!x` is inner.
        let (is_doc, inner, body) = if let Some(b) = c.text.strip_prefix('!') {
            (true, true, b)
        } else if let Some(b) = c.text.strip_prefix('/') {
            (!b.starts_with('/'), false, b)
        } else {
            (false, false, c.text.as_str())
        };
        if !is_doc {
            continue;
        }
        let continues = last.is_some_and(|(l, k)| l + 1 == c.line && k == inner);
        if !continues && !block.is_empty() {
            scan_doc_block(sweep, file, &std::mem::take(&mut block));
        }
        last = Some((c.line, inner));
        block.push((c.line, body.to_string()));
    }
    if !block.is_empty() {
        scan_doc_block(sweep, file, &block);
    }
}

fn indent_of(s: &str) -> usize {
    s.len() - s.trim_start_matches(' ').len()
}

/// The content column of a list-item line (`- x`, `* x`, `+ x`, `1. x`,
/// `1) x`), or `None` if the line is not one.
fn list_content_col(text: &str) -> Option<usize> {
    let ind = indent_of(text);
    let rest = &text[ind..];
    let marker = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || !rest[digits..].starts_with(['.', ')']) {
            return None;
        }
        digits + 1
    };
    let after = &rest[marker..];
    let gap = after.len() - after.trim_start_matches(' ').len();
    if gap == 0 && !after.is_empty() {
        return None;
    }
    Some(ind + marker + gap.clamp(1, 4))
}

/// An ATX heading: up to three spaces, one to six `#`, then a space or the
/// end of the line. `#[derive]` in prose is not one.
fn is_heading(text: &str) -> bool {
    if indent_of(text) > 3 {
        return false;
    }
    let t = text.trim_start_matches(' ');
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ')
}

fn scan_doc_block(sweep: &mut Sweep, file: &Path, lines: &[(usize, String)]) {
    // rustdoc removes the block's COMMON indentation before rendering, so a
    // block indented four spaces on every line is prose, not code.
    let common = lines
        .iter()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(_, t)| indent_of(&t.replace('\t', "    ")))
        .min()
        .unwrap_or(0);

    // (fence char, run length, opening line, info) while inside a fence.
    let mut open: Option<(char, usize, usize, String)> = None;
    // An indented code block may start only where a paragraph could not be
    // continued: after a blank line, a heading, a closed fence, or at the
    // top of the block.
    let mut can_start_code = true;
    // Content column of the innermost list item seen, if any. Indented text
    // under a list item is continuation or a nested list, not code, unless
    // it reaches four columns past the item's content.
    let mut list_col: Option<usize> = None;
    // The same "may a code block start here" state, inside a blockquote.
    let mut quote_can_start_code = true;

    for (line, raw) in lines {
        let expanded = raw.replace('\t', "    ");
        let text = expanded.get(common..).unwrap_or("");
        let blank = text.trim().is_empty();

        if open.is_none() && !blank {
            let ind = indent_of(text);
            let code_col_before = list_col.map_or(4, |c| c + 4);
            // A list marker indented to the code column is code text, not an
            // item: `-` four spaces in after a blank line is an indented code
            // block that happens to start with a dash.
            let item = list_content_col(text).filter(|_| ind < code_col_before);
            if let Some(col) = item {
                list_col = Some(col);
            } else if ind == 0 {
                list_col = None;
            }
            let code_col = list_col.map_or(4, |c| c + 4);
            let indented_code = can_start_code && item.is_none() && ind >= code_col;
            // A fence nested deeper than three columns (in a list item) is
            // still a fence to rustdoc, but this sweep only classifies
            // top-level ones.
            let deep_fence = ind > 3 && fence(text.trim_start()).is_some();
            // A blockquote holding a fence, or an indented block after `>`.
            let quoted = text
                .trim_start()
                .strip_prefix('>')
                .map(|q| q.strip_prefix(' ').unwrap_or(q));
            let quoted_code = quoted.is_some_and(|q| {
                fence(q.trim_start()).is_some()
                    || (quote_can_start_code && q.starts_with("    ") && !q.trim().is_empty())
            });
            quote_can_start_code = quoted.is_some_and(|q| q.trim().is_empty() || is_heading(q));
            if indented_code || deep_fence || quoted_code {
                sweep.problems.push(format!(
                    "{}:{line}: an indented, list-nested or blockquoted code block in a doc comment — rustdoc may test it and this sweep does not classify that form; rewrite it as a top-level fence (```ignore / ```text if it is not a test)",
                    file.display()
                ));
            }
        }

        match (&open, fence(text)) {
            (None, Some((ch, run, info))) => {
                open = Some((ch, run, *line, info));
                can_start_code = false;
                continue;
            }
            (Some((och, orun, _, _)), Some((ch, run, info)))
                if ch == *och && run >= *orun && info.is_empty() =>
            {
                let o = open.take().unwrap();
                record(sweep, file, o.2, &o.3);
                can_start_code = true;
                continue;
            }
            _ => {}
        }
        if open.is_none() {
            can_start_code = blank || is_heading(text);
        }
    }
    if let Some(o) = open.take() {
        record(sweep, file, o.2, &o.3);
    }
}

fn record(sweep: &mut Sweep, file: &Path, line: usize, info: &str) {
    let (is_rust, ignored) = classify(info);
    if !is_rust {
        return;
    }
    let entry = (file.to_path_buf(), line, info.to_string());
    if ignored {
        sweep.ignored_doctests.push(entry);
    } else {
        sweep.live_doctests.push(entry);
    }
}

fn sweep_lib() -> Sweep {
    let mut sweep = Sweep::default();
    let root = src_dir().join("lib.rs");
    assert!(root.exists(), "lib crate root not found at {}", root.display());
    walk(&mut sweep, &root, true);
    sweep
}

fn rel(p: &Path) -> String {
    p.strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
        .unwrap_or(p)
        .display()
        .to_string()
}

#[test]
fn the_lib_has_no_doctest_that_ci_would_silently_skip() {
    let sweep = sweep_lib();

    assert!(
        sweep.problems.is_empty(),
        "the lib module walk could not read every file, so it cannot vouch that no doctest is hiding:\n  {}",
        sweep.problems.join("\n  ")
    );

    // Non-vacuity: the lib held 91 files when this was written. A walk that
    // stops at lib.rs (a broken resolver) must not pass by finding nothing.
    assert!(
        sweep.files.len() >= 20,
        "the lib module walk reached only {} file(s) — the resolver is broken, not the tree small",
        sweep.files.len()
    );

    assert!(
        sweep.live_doctests.is_empty(),
        "these doc-comment code blocks are DOCTESTS of the lib crate, and CI does not run doctests: \
         `test` builds with `cargo test --no-run` and executes the binaries it produced, which never \
         include doctests (a second cargo invocation pays a full rebuild — see the `Run Rust tests` \
         step in .github/workflows/ci.yml). So each of these would compile nowhere and run nowhere.\n  {}\n\
         Pick one per block: fence it ```ignore (or ```text if it is not meant to be Rust), move the \
         check into a #[test], or restore doctest execution in CI (plan-library follow-up \
         2bd27517-55f9-4cc3-9aee-667426a349c7).",
        sweep
            .live_doctests
            .iter()
            .map(|(f, l, i)| format!("{}:{l} (```{i})", rel(f)))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The sweep against CI's own last measurement: the only doctest the lib
/// carried when the split shipped was the `ignore`d `QueryBuilder` example,
/// reported by rustdoc as `accessibility::query::QueryBuilder (line 141)`.
/// If the walk stops finding it, it has stopped reading that part of the tree.
#[test]
fn the_sweep_finds_the_one_ignored_doctest_ci_last_reported() {
    let sweep = sweep_lib();
    let found = sweep.ignored_doctests.iter().any(|(f, _, _)| {
        f.ends_with(Path::new("accessibility").join("query").join("mod.rs"))
    });
    assert!(
        found,
        "expected the ignored `QueryBuilder` doctest in src/accessibility/query/mod.rs; the sweep found ignored doctests at: {:?}",
        sweep
            .ignored_doctests
            .iter()
            .map(|(f, l, _)| format!("{}:{l}", rel(f)))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// The pieces, pinned directly — each of these is a way the gate above could
// pass vacuously.
// ---------------------------------------------------------------------------

#[test]
fn fence_info_strings_classify_as_rustdoc_does() {
    assert_eq!(classify(""), (true, false));
    assert_eq!(classify("rust"), (true, false));
    assert_eq!(classify("no_run"), (true, false));
    assert_eq!(classify("compile_fail,E0308"), (true, false));
    assert_eq!(classify("rust,ignore"), (true, true));
    assert_eq!(classify("ignore"), (true, true));
    assert_eq!(classify("ignore-windows"), (true, true));
    assert_eq!(classify("text"), (false, false));
    assert_eq!(classify("json"), (false, false));
    assert_eq!(classify("rust,edition2021"), (true, false));
}

#[test]
fn a_backtick_run_with_a_backtick_in_its_info_is_not_a_fence() {
    assert!(fence("```").is_some());
    assert!(fence("```rust").is_some());
    assert!(fence("~~~").is_some());
    assert!(fence("``` ```x` ```` is inline code").is_none());
    assert!(fence("    ```").is_none(), "four spaces is an indented code block, not a fence");
    assert!(fence("``").is_none());
}

#[test]
fn the_lexer_does_not_read_structure_out_of_literals_or_comments() {
    let src = "const Q: char = '\\'';
const A: &str = \"mod fake;\";\nconst B: &str = r#\"{ mod x; \"#;\n/* mod y; { */\nlet c = '{';\nfn f<'a>(x: &'a str) {}\n/// ```\n/// let x = 1;\n/// ```\n";
    let lexed = lex(src);
    assert!(!lexed.code.contains("fake"), "string contents must be blanked");
    assert!(!lexed.code.contains("mod x"), "raw string contents must be blanked");
    assert!(!lexed.code.contains("mod y"), "block comment contents must be blanked");
    assert_eq!(brace_delta(&lexed.code), 0, "only the fn body's balanced braces are code");
    assert_eq!(lexed.code.lines().count(), src.lines().count(), "line numbers must survive");
    assert_eq!(lexed.line_comments.len(), 3);
    assert_eq!(lexed.line_comments[0].line, 7);
}

#[test]
fn a_rust_fence_in_a_doc_comment_is_recorded_and_a_text_one_is_not() {
    let src = "/// ```\n/// let x = 1;\n/// ```\nfn a() {}\n//! ```text\n//! hi\n//! ```\n//// ```\n//// not a doc comment\n//// ```\n";
    let lexed = lex(src);
    let mut sweep = Sweep::default();
    scan_doc_fences(&mut sweep, Path::new("x.rs"), &lexed.line_comments);
    assert_eq!(sweep.live_doctests.len(), 1, "{:?}", sweep.live_doctests);
    assert_eq!(sweep.live_doctests[0].1, 1);
}

#[test]
fn an_escaped_quote_char_literal_does_not_leak_the_next_literal() {
    // `'\''` must close on its own final quote. Closing on the escaped one
    // left a stray quote that swallowed the next literal's opener, so the `{`
    // below leaked into code and skewed every inline-module depth after it.
    let lexed = lex(r"f(['\'','{']);");
    assert_eq!(brace_delta(&lexed.code), 0, "{:?}", lexed.code);
}

#[test]
fn doc_code_forms_the_sweep_cannot_classify_are_refused_not_skipped() {
    for src in [
        "/// Example:\n///\n///     let x = 1;\n",
        "/// > ```\n/// > let x = 1;\n/// > ```\n",
        "/// - item\n///\n///     ```\n///     let x = 1;\n///     ```\n",
        "/// # Examples\n///     let x = 1;\n",
        "/// ```text\n/// hi\n/// ```\n///     let x = 1;\n",
        "/// > quote\n/// >\n/// >     let x = 1;\n",
        "/// Ex:\n///\n///     - 1\n",
    ] {
        let lexed = lex(src);
        let mut sweep = Sweep::default();
        scan_doc_fences(&mut sweep, Path::new("x.rs"), &lexed.line_comments);
        assert!(!sweep.problems.is_empty(), "not refused: {src:?}");
    }
    // Ordinary prose that is indented without being code must not be
    // refused: a wrapped paragraph line, a list item's continuation
    // paragraph, a nested list, and a block indented as a whole (rustdoc
    // strips the common indentation).
    for src in [
        "/// A sentence that\n///     wraps with indentation.\n",
        "/// - first item\n///\n///     continuation paragraph.\n",
        "/// - outer\n///\n///     - inner item\n",
        "/// 1. step\n///\n///    more about the step.\n",
        "///     Everything indented.\n///     Still prose.\n",
        "/// #[derive] is used\n///     here too.\n",
        "/// > para that\n/// >     wraps.\n",
    ] {
        let lexed = lex(src);
        let mut sweep = Sweep::default();
        scan_doc_fences(&mut sweep, Path::new("x.rs"), &lexed.line_comments);
        assert!(sweep.problems.is_empty(), "refused prose {src:?}: {:?}", sweep.problems);
    }
}

#[test]
fn a_mod_form_the_walk_cannot_read_is_refused_not_skipped() {
    let dir = std::env::temp_dir().join(format!("doctest-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("lib.rs");
    std::fs::write(&root, "mod split\n{\n}\n").unwrap();
    let mut sweep = Sweep::default();
    walk(&mut sweep, &root, true);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!sweep.problems.is_empty(), "a `mod x` with its brace on the next line must be refused");
}
