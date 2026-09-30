//! **A source pin cannot be written against its own text.**
//!
//! A *source pin* is a test that reads a module's own source with
//! `include_str!` and asserts something about the production code in it — "the
//! writer calls the guard", "no call site discards the slot", "this wrapper only
//! delegates". Such a test lives in the SAME file it scans, so every string it
//! searches for is also sitting in its own test module: a negative assertion
//! matches its own needle, and a positive one can be satisfied by the test's own
//! `const`. Occurrence 17 of the dossier this module closes was exactly that —
//! `the_public_spawn_wrapper_only_delegates` found its `SIGNATURE` in its own
//! test `const` once the real wrapper was renamed, sliced a 22,879-char
//! pseudo-body to the end of the test module, and reddened only by accident.
//!
//! The defence existed as private copies (`prod_part` in `fleet.rs` and
//! `wedge_diagnostics.rs`, a dozen hand-rolled `split("#[cfg(test)]")` cuts), and
//! nothing made the next pin find it. [`ProdSource`] is that defence as a TYPE:
//! the only way to hold a self-included source is through [`ProdSource::of`],
//! which has already removed the test half, or [`ProdSource::whole`], which says
//! in greppable text why this pin must see it. The meta-pin at the bottom of this
//! file (a file that includes no source of its own, so it cannot match itself)
//! fails the build of any test that `include_str!`s its own basename without
//! going through one of the two.
//!
//! Plan `2026-09-20-rust-verification-residuals-cached-green-straddles-the-edit-target-count-unfloored-and-source-pins-scan-themselves`,
//! Phase 5, D4.

use std::borrow::Cow;
use std::ops::{Deref, Range};

/// The production half of one Rust source file.
///
/// Derefs to `str`, so every `&str` method (`find`, `lines`, `contains`,
/// `split_once`, indexing) works on it directly; pass `&src` where a function
/// takes `&str`.
#[derive(Debug, Clone)]
pub(crate) struct ProdSource<'a> {
    text: Cow<'a, str>,
}

impl<'a> ProdSource<'a> {
    /// The production half of `src`: CRLF-normalized, with every test module
    /// removed.
    ///
    /// **What counts as a test module.** A line that is exactly `#[cfg(test)]`
    /// (or `#[cfg(all(test, …))]`), followed — after zero or more further
    /// attribute lines such as `#[allow(…)]` or `#[path = …]`, and comment lines
    /// — by a `mod` item: inline `mod tests { … }` (removed through its matching
    /// brace) or external `mod tests;` (removed as one declaration). A
    /// `#[cfg(test)]` on any OTHER item — a test-only helper `fn` in production
    /// position — is NOT a test module and stays: the precedent every private
    /// copy of this rule shared, and the reason `fleet.rs`'s copy split on the
    /// module header rather than the bare attribute.
    ///
    /// **Every test module, not only the first.** Many files here interleave
    /// production code with several test modules (`settings.rs` has production
    /// code after fourteen of them; `main.rs` declares an external test module
    /// at line ~115). Cutting at the first marker would silently drop that
    /// production code, so a negative pin would stop seeing it. Removing each
    /// module by brace matching keeps it.
    ///
    /// Braces are matched over CODE only: string literals (plain, byte, raw
    /// `r#"…"#`), char literals and comments are skipped, so a `"mod tests {"`
    /// fixture inside a test module cannot unbalance the cut.
    ///
    /// # Panics
    ///
    /// When a `#[test]` attribute is still present in code after the cut —
    /// which covers a file containing `#[test]` with no recognizable test module
    /// at all. A silent fallback to the whole file is how a reformatted marker
    /// used to turn the cut into a no-op; this makes it loud instead.
    #[track_caller]
    pub(crate) fn of(src: &'a str) -> Self {
        match Self::try_of(src) {
            Ok(prod) => prod,
            Err(why) => panic!("ProdSource::of: {why}"),
        }
    }

    /// [`ProdSource::of`] without the panic, for a caller walking files it did
    /// not choose (a directory scan meets whole-file test modules such as
    /// `foo/tests.rs`, which legitimately have no production half to cut to).
    pub(crate) fn try_of(src: &'a str) -> Result<Self, String> {
        let normalized = normalize(src);
        let stripped = strip_test_modules(&normalized)?;
        if let Some(line) = first_code_test_attr(stripped.as_deref().unwrap_or(&normalized)) {
            return Err(format!(
                "a `#[test]` survives the cut at production-half line {line}: this file \
                 has a test that is not inside a `#[cfg(test)] mod …` this cutter \
                 recognizes (or no test module at all). Fix the file's test module \
                 header, or use `ProdSource::whole(src, \"<why>\")` if this pin really \
                 must read the test half."
            ));
        }
        let text = match (stripped, normalized) {
            (None, normalized) => normalized,
            (Some(owned), _) => Cow::Owned(owned),
        };
        Ok(Self { text })
    }

    /// The WHOLE file, test half included — the explicit, greppable opt-out.
    ///
    /// `reason` must say why this pin has to read test code (e.g. a lint over
    /// every string literal in the file, tests included). CRLF is still
    /// normalized.
    pub(crate) fn whole(src: &'a str, reason: &'static str) -> Self {
        assert!(
            !reason.trim().is_empty(),
            "ProdSource::whole needs a reason: say why this pin must read the test half"
        );
        Self {
            text: normalize(src),
        }
    }

    /// The text as a plain `&str`.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Whole-line `//` comments dropped, then every whitespace character.
    ///
    /// Comments go FIRST and deliberately: a module's docs often name the
    /// forbidden spelling in prose (they have to — that is what a reader must be
    /// warned about), and a pin that cannot tell a warning from a call site fails
    /// on the documentation that explains it. Whitespace then goes so a pin
    /// matches regardless of how `rustfmt` wrapped a call.
    pub(crate) fn squeezed(&self) -> String {
        squeeze(&self.text)
    }

    /// The braced body following `signature`: from the first code `{` after it
    /// through its matching `}`, both included.
    ///
    /// # Panics
    ///
    /// - unless `signature` occurs EXACTLY ONCE in the production half — zero
    ///   means it was renamed (the pin must move with it), two means the pin
    ///   cannot say which one it is pinning;
    /// - when no code `{` follows the signature, or it is never closed;
    /// - when the body's byte length falls outside `len`. The bound is
    ///   TWO-SIDED on purpose: occurrence 17's failure was a RUNAWAY — a
    ///   22,879-char slice — which a lower bound alone cannot see.
    #[track_caller]
    pub(crate) fn body_of(&self, signature: &str, len: Range<usize>) -> &str {
        let text: &str = &self.text;
        let hits: Vec<usize> = text.match_indices(signature).map(|(i, _)| i).collect();
        let &[start] = hits.as_slice() else {
            panic!(
                "body_of: expected exactly one occurrence of `{signature}` in the production \
                 half, found {}. Zero: it was renamed or reformatted — move this pin with \
                 it. More than one: this pin cannot tell which it is pinning.",
                hits.len()
            );
        };
        let mask = code_mask(text);
        let from = start + signature.len();
        let open = (from..text.len())
            .find(|&i| mask[i] && text.as_bytes()[i] == b'{')
            .unwrap_or_else(|| panic!("body_of: no `{{` follows `{signature}`"));
        let close = matching_brace(text, &mask, open)
            .unwrap_or_else(|| panic!("body_of: the body after `{signature}` is never closed"));
        let body = text
            .get(open..=close)
            .expect("brace offsets are ASCII, so they are char boundaries");
        assert!(
            len.contains(&body.len()),
            "body_of: the body of `{signature}` is {} bytes, outside the bound {len:?} — a \
             runaway slice (or a gutted body) is the defect, not the function",
            body.len()
        );
        body
    }
}

impl Deref for ProdSource<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl AsRef<str> for ProdSource<'_> {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for ProdSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// [`ProdSource::squeezed`] for a slice already cut from a production half
/// (typically a [`ProdSource::body_of`] result).
pub(crate) fn squeeze(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn normalize(src: &str) -> Cow<'_, str> {
    if src.contains("\r\n") {
        Cow::Owned(src.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(src)
    }
}

/// `mask[i]` is true when byte `i` is CODE — not inside a comment, a string
/// literal or a char literal. Delimiters of literals and comments are non-code
/// too. Lifetimes (`'a`) are code.
///
/// Limitation, stated rather than hidden: a char literal is recognized as `'`,
/// one char (or a `\` escape), `'`. Anything else after a `'` is read as a
/// lifetime. That is exact for valid Rust.
fn code_mask(s: &str) -> Vec<bool> {
    let b = s.as_bytes();
    let n = b.len();
    let mut mask = vec![false; n];
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < n {
        let c = b[i];
        let prev_ident = i > 0 && is_ident(b[i - 1]);
        // Line comment.
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (nesting, as Rust's are).
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0usize;
            while i < n {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Raw string: r"…", r#"…"#, br#"…"#.
        if !prev_ident && (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r'))) {
            let mut j = if c == b'b' { i + 2 } else { i + 1 };
            let mut hashes = 0usize;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                j += 1;
                while j < n {
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // Plain or byte string.
        if c == b'"' || (c == b'b' && !prev_ident && b.get(i + 1) == Some(&b'"')) {
            let mut j = if c == b'"' { i + 1 } else { i + 2 };
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
            continue;
        }
        // Char literal vs lifetime.
        if c == b'\'' {
            if b.get(i + 1) == Some(&b'\\') {
                // Escaped char: skip the escape's next byte, then to the close.
                let mut j = i + 3;
                while j < n && b[j] != b'\'' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            if let Some(&lead) = b.get(i + 1) {
                let width = utf8_width(lead);
                if b.get(i + 1 + width) == Some(&b'\'') {
                    i += 2 + width;
                    continue;
                }
            }
        }
        mask[i] = true;
        i += 1;
    }
    mask
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// The offset of the `}` matching the code `{` at `open`.
fn matching_brace(s: &str, mask: &[bool], open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    for i in open..b.len() {
        if !mask[i] {
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Byte offset of the start of each line.
fn line_starts(s: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(s.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Whether the first non-blank byte of `line` (starting at `at`) is code.
fn line_is_code(line: &str, at: usize, mask: &[bool]) -> bool {
    let indent = line.len() - line.trim_start().len();
    mask.get(at + indent).copied().unwrap_or(false)
}

fn is_test_cfg(trimmed: &str) -> bool {
    trimmed == "#[cfg(test)]"
        || ((trimmed.starts_with("#[cfg(all(test,") || trimmed.starts_with("#[cfg(all(test)"))
            && trimmed.ends_with(")]"))
}

/// `Some(true)` for `mod x {`, `Some(false)` for `mod x;`, `None` otherwise.
/// Accepts a visibility prefix (`pub`, `pub(crate)`, `pub(super)`).
fn mod_item_kind(trimmed: &str) -> Option<bool> {
    let mut rest = trimmed;
    if let Some(r) = rest.strip_prefix("pub") {
        let r = r.trim_start();
        rest = match r.strip_prefix('(') {
            Some(inner) => inner.split_once(')')?.1.trim_start(),
            None => r,
        };
    }
    let rest = rest.strip_prefix("mod ")?.trim_start();
    let name_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if name_len == 0 {
        return None;
    }
    let after = rest.get(name_len..)?.trim_start();
    if after.starts_with('{') {
        Some(true)
    } else if after.starts_with(';') {
        Some(false)
    } else {
        None
    }
}

/// Remove every test module. `Ok(None)` when there was nothing to remove.
fn strip_test_modules(s: &str) -> Result<Option<String>, String> {
    let mask = code_mask(s);
    let starts = line_starts(s);
    let lines: Vec<&str> = s.split('\n').collect();
    let mut remove: Vec<Range<usize>> = Vec::new(); // line index ranges
    let mut k = 0;
    while k < lines.len() {
        let trimmed = lines[k].trim();
        if !(is_test_cfg(trimmed) && line_is_code(lines[k], starts[k], &mask)) {
            k += 1;
            continue;
        }
        // Skip further attributes and comment lines.
        let mut j = k + 1;
        while j < lines.len() {
            let t = lines[j].trim();
            let is_attr = t.starts_with("#[") && line_is_code(lines[j], starts[j], &mask);
            if is_attr || t.starts_with("//") {
                j += 1;
            } else {
                break;
            }
        }
        let Some(kind) = lines
            .get(j)
            .filter(|l| line_is_code(l, starts[j], &mask))
            .and_then(|l| mod_item_kind(l.trim()))
        else {
            // `#[cfg(test)]` on a non-mod item: not a cut point.
            k += 1;
            continue;
        };
        let end_line = if kind {
            let open = starts[j]
                + lines[j]
                    .find('{')
                    .expect("mod_item_kind saw a `{` on this line");
            let close = matching_brace(s, &mask, open).ok_or_else(|| {
                format!("the test module opened at line {} is never closed", j + 1)
            })?;
            starts.partition_point(|&st| st <= close) - 1
        } else {
            j
        };
        remove.push(k..end_line + 1);
        k = end_line + 1;
    }
    if remove.is_empty() {
        return Ok(None);
    }
    let mut out = String::with_capacity(s.len());
    let mut ranges = remove.iter().peekable();
    let mut first = true;
    for (idx, line) in lines.iter().enumerate() {
        while ranges.peek().is_some_and(|r| r.end <= idx) {
            ranges.next();
        }
        if ranges.peek().is_some_and(|r| r.contains(&idx)) {
            continue;
        }
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(line);
    }
    Ok(Some(out))
}

/// The 1-based line of the first `#[test]` attribute in CODE position.
fn first_code_test_attr(s: &str) -> Option<usize> {
    let mask = code_mask(s);
    let starts = line_starts(s);
    s.split('\n')
        .enumerate()
        .find(|(i, l)| l.trim() == "#[test]" && line_is_code(l, starts[*i], &mask))
        .map(|(i, _)| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- cut points ----

    #[test]
    fn an_inline_test_module_is_removed() {
        let src =
            "fn prod() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { prod(); }\n}\n";
        let p = ProdSource::of(src);
        assert!(p.contains("fn prod()"));
        assert!(!p.contains("fn t()"), "test module survived: {p}");
    }

    #[test]
    fn an_external_test_module_declaration_is_removed() {
        let src = "fn prod() {}\n\n#[cfg(test)]\nmod tests;\n\nfn after() {}\n";
        let p = ProdSource::of(src);
        assert!(!p.contains("mod tests;"));
        assert!(
            p.contains("fn after()"),
            "production after the decl was lost"
        );
    }

    #[test]
    fn attribute_and_comment_lines_between_the_marker_and_the_mod_are_part_of_it() {
        let src = "fn prod() {}\n#[cfg(test)]\n#[path = \"x_tests.rs\"]\n#[allow(clippy::all)]\n// why\nmod x_tests;\n#[cfg(test)]\n#[allow(dead_code)]\npub(crate) mod helpers {\n    #[test]\n    fn t() {}\n}\nfn tail() {}\n";
        let p = ProdSource::of(src);
        assert!(!p.contains("x_tests"), "{p}");
        assert!(!p.contains("helpers"), "{p}");
        assert!(p.contains("fn prod()") && p.contains("fn tail()"), "{p}");
    }

    #[test]
    fn cfg_test_on_a_non_mod_item_is_not_a_cut_point() {
        let src = "#[cfg(test)]\nfn test_only_helper() {}\n\nfn prod() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";
        let p = ProdSource::of(src);
        assert!(
            p.contains("test_only_helper"),
            "a cfg(test) fn is not a module"
        );
        assert!(
            p.contains("fn prod()"),
            "production after a cfg(test) fn was cut"
        );
        assert!(!p.contains("fn t()"));
    }

    #[test]
    fn production_between_two_test_modules_is_kept() {
        let src = "fn a() {}\n#[cfg(test)]\nmod a_tests {\n    #[test]\n    fn ta() {}\n}\nfn b() {}\n#[cfg(all(test, unix))]\nmod b_tests {\n    #[test]\n    fn tb() {}\n}\n";
        let p = ProdSource::of(src);
        assert!(p.contains("fn a()") && p.contains("fn b()"), "{p}");
        assert!(!p.contains("fn ta()") && !p.contains("fn tb()"), "{p}");
    }

    #[test]
    fn crlf_is_normalized_and_still_cut() {
        let src =
            "fn prod() {}\r\n#[cfg(test)]\r\nmod tests {\r\n    #[test]\r\n    fn t() {}\r\n}\r\n";
        let p = ProdSource::of(src);
        assert!(!p.contains('\r'));
        assert!(p.contains("fn prod()") && !p.contains("fn t()"), "{p}");
    }

    #[test]
    fn braces_inside_literals_and_comments_do_not_unbalance_the_cut() {
        let src = concat!(
            "fn prod() {}\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    const A: &str = \"}}}\";\n",
            "    const B: &str = r#\"\n}\n\"#;\n",
            "    const C: char = '}';\n",
            "    // }\n",
            "    /* } */\n",
            "    #[test]\n",
            "    fn t<'a>(x: &'a str) {}\n",
            "}\n",
            "fn after() {}\n",
        );
        let p = ProdSource::of(src);
        assert!(p.contains("fn after()"), "{p}");
        assert!(!p.contains("fn t<"), "{p}");
    }

    #[test]
    #[should_panic(expected = "a `#[test]` survives the cut")]
    fn a_file_with_tests_but_no_cut_point_panics() {
        // The marker was "reformatted" onto the mod line: nothing to cut at.
        let src = "fn prod() {}\n#[cfg(test)] mod tests {\n    #[test]\n    fn t() {}\n}\n";
        let _ = ProdSource::of(src);
    }

    #[test]
    fn a_file_without_tests_is_returned_whole() {
        let src = "fn prod() {}\n";
        assert_eq!(ProdSource::of(src).as_str(), src);
    }

    #[test]
    fn whole_keeps_the_test_half() {
        let src = "fn prod() {}\r\n#[cfg(test)]\r\nmod tests {}\r\n";
        let p = ProdSource::whole(src, "fixture: whole must not cut");
        assert!(p.contains("mod tests {}") && !p.contains('\r'));
    }

    #[test]
    fn squeezed_drops_comment_lines_then_whitespace() {
        let p = ProdSource::of("// let _ = forbidden();\nlet  slot =\n    enter();\n");
        assert_eq!(p.squeezed(), "letslot=enter();");
    }

    // ---- body_of: occurrence 17 ----

    /// The shape occurrence 17 failed on: the signature once in production and
    /// once inside a `const` in the test module. Against the production half
    /// `body_of` sees exactly one; against the whole file it would see two.
    const OCCURRENCE_17: &str = concat!(
        "pub fn spawn_blocking_tracked<F, R>(f: F) -> R {\n",
        "    spawn_blocking_tracked_in(&TABLE, f)\n",
        "}\n",
        "\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    const SIGNATURE: &str = \"pub fn spawn_blocking_tracked<F, R>(f: F)\";\n",
        "    #[test]\n",
        "    fn the_public_spawn_wrapper_only_delegates() {\n",
        "        // tokio::task::spawn_blocking( would be the regression\n",
        "    }\n",
        "}\n",
    );

    #[test]
    fn occurrence_17_body_of_finds_exactly_the_production_signature() {
        let p = ProdSource::of(OCCURRENCE_17);
        let body = p.body_of("pub fn spawn_blocking_tracked<F, R>(f: F)", 10..200);
        assert_eq!(body, "{\n    spawn_blocking_tracked_in(&TABLE, f)\n}");
    }

    #[test]
    #[should_panic(expected = "found 0")]
    fn a_renamed_signature_is_a_loud_zero_not_a_match_in_the_test_const() {
        let renamed = OCCURRENCE_17.replacen(
            "pub fn spawn_blocking_tracked<F, R>(f: F) -> R",
            "pub fn spawn_blocking_tracked<F, R>(func: F) -> R",
            1,
        );
        let p = ProdSource::of(&renamed);
        let _ = p.body_of("pub fn spawn_blocking_tracked<F, R>(f: F)", 10..200);
    }

    #[test]
    #[should_panic(expected = "outside the bound")]
    fn a_runaway_body_is_refused_by_the_upper_bound() {
        // A body far longer than the pin's declared bound: the 22,879-char
        // pseudo-body of occurrence 17 was this direction.
        let runaway = format!("pub fn wrapper() {{\n{}}}\n", "    step();\n".repeat(2_000));
        let p = ProdSource::of(&runaway);
        let _ = p.body_of("pub fn wrapper()", 10..400);
    }

    #[test]
    fn body_of_skips_braces_in_literals() {
        let p = ProdSource::of("fn f() {\n    let s = \"}\";\n    g('{');\n}\nfn h() {}\n");
        assert_eq!(
            p.body_of("fn f()", 1..100),
            "{\n    let s = \"}\";\n    g('{');\n}"
        );
    }

    // ---- the meta-pin ----

    /// **Every self-including pin goes through `ProdSource`.**
    ///
    /// Walks `src/` and fails on any `include_str!` of a file's OWN basename that
    /// is not immediately wrapped as `ProdSource::of(` or `ProdSource::whole(`.
    /// This file includes no source, so it cannot match itself, and its needle
    /// is assembled with `concat!` so it holds no contiguous forbidden literal.
    #[test]
    fn every_self_including_pin_goes_through_source_pin() {
        const OPEN: &str = concat!("include_str", "!(\"");
        const WRAPPERS: [&str; 2] = [
            concat!("ProdSource::", "of("),
            concat!("ProdSource::", "whole("),
        ];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files_scanned = 0usize;
        let mut wrapped = 0usize;
        let mut offenders = Vec::new();
        for entry in walkdir::WalkDir::new(&root).into_iter().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            files_scanned += 1;
            let base = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a walked .rs path has a UTF-8 file name");
            let needle = format!("{OPEN}{base}\")");
            for (at, _) in text.match_indices(&needle) {
                let before = text.get(..at).unwrap_or_default().trim_end();
                if WRAPPERS.iter().any(|w| before.ends_with(w)) {
                    wrapped += 1;
                } else {
                    let line = text.get(..at).unwrap_or_default().matches('\n').count() + 1;
                    let rel = path.strip_prefix(&root).unwrap_or(path);
                    offenders.push(format!("{}:{line}", rel.display()));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "a source pin includes its OWN file without `ProdSource`: {offenders:?}. \
             Wrap it as `crate::source_pin::ProdSource::of(…)` so it scans the \
             production half only, or `ProdSource::whole(…, \"<why>\")` if it must \
             read its test half — a pin over its own raw text can match its own \
             needles (occurrence 17)."
        );
        // Floors, so a moved `src/` root or a broken needle cannot pass over
        // nothing. Baseline 2026-09-29 at qontinui-runner e51a27086: 1,576 `.rs`
        // files under src/ and 71 wrapped self-includes across 37 files.
        assert!(
            files_scanned >= 1_400,
            "the meta-pin scanned only {files_scanned} .rs files — it is walking the \
             wrong tree and would pass vacuously"
        );
        assert!(
            wrapped >= 60,
            "the meta-pin recognized only {wrapped} wrapped self-includes — its needle \
             has probably stopped matching the real spelling"
        );
    }
}
