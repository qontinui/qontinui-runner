//! Ratchet for the `tokio_postgres::Row::get` deny lint.
//!
//! Plan `2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor`,
//! Phase 3 step 3 (the runner half of Phase 1's coord ratchet). Source:
//! dossier:row-get-panic-kills-spawned-loop.
//!
//! `Row::get` panics on a NULL into a non-`Option`, a type mismatch, or a bad
//! index; inside a `tokio::spawn`ed loop that panic kills the task silently.
//! The repo-root `clippy.toml` disallows the method and `src-tauri/Cargo.toml`
//! raises `clippy::disallowed_methods` to `deny`, so a NEW site cannot land.
//! The sites that existed when the gate landed are grandfathered one fn at a
//! time with `#[expect(clippy::disallowed_methods, reason = …)]` (inserted by
//! `scripts/row-get-expect-sweep.py`, run once per required clippy leg —
//! ubuntu and `x86_64-pc-windows-msvc`; written in the four-line shape
//! `rustfmt` gives a 131-column attribute, since `cargo fmt -- --check` is a
//! required step here), and this module pins that attribute count as a
//! CEILING: it only falls. A fn that migrates to `try_get` must drop
//! its attribute (or `unfulfilled_lint_expectations`, also `deny`, reds the
//! build) and then lower [`BASELINE`] here.
//!
//! Three source-scan tests, DB-free, over the same directory the sweep counts
//! (`src-tauri/src`; `src-tauri/clorinde` is generated and outside it):
//!
//! 1. the attribute count is `<= BASELINE`, and non-vacuously `> 0`;
//! 2. no `#[allow(clippy::disallowed_methods` and no inner (`#![…]`)
//!    expectation anywhere — an `allow` never fires the
//!    unfulfilled-expectation check, and a crate- or module-level attribute
//!    leaves a whole file open to new sites;
//! 3. the gate itself is still wired: the repo-root `clippy.toml` lists
//!    `tokio_postgres::Row::get` (and no nearer `src-tauri/clippy.toml`
//!    shadows it — clippy stops at the FIRST config it finds walking up from
//!    `CARGO_MANIFEST_DIR`), and `src-tauri/Cargo.toml` still carries both
//!    deny levels — so the gate cannot be removed silently.
//!
//! This file is excluded from its own walk: it spells the needles as literals.
//! Declared `#[cfg(test)]` in `main.rs` beside the other test-only modules.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The Phase 3 baseline — the number of fn-level
    /// `#[expect(clippy::disallowed_methods, …)]` attributes the sweep placed
    /// under `src-tauri/src` on `origin/main` when the gate landed (both
    /// clippy legs). **Lower it when you migrate a fn to `try_get`; never
    /// raise it.** A new `Row::get` site is not grandfathered — it is a deny
    /// error until it is rewritten with `try_get`.
    const BASELINE: usize = 562;

    /// The attribute the sweep inserts (the prefix; the `reason` follows),
    /// compared against each attribute's WHITESPACE-FREE form so the
    /// single-line and the rustfmt four-line spellings both count.
    const EXPECT_NEEDLE: &str = "#[expect(clippy::disallowed_methods";
    /// The forbidden spellings: an `allow` (never checked for fulfilment) and
    /// any inner attribute (file/crate granularity).
    const FORBIDDEN_NEEDLES: [&str; 3] = [
        "#[allow(clippy::disallowed_methods",
        "#![expect(",
        "#![allow(clippy::disallowed_methods",
    ];

    /// The directory the sweep script counts over (relative to the repo
    /// root) — kept in lockstep with `RATCHET_DIRS` in
    /// `scripts/row-get-expect-sweep.py`.
    const DIRS: [&str; 1] = ["src-tauri/src"];
    /// Directory names skipped anywhere in the walk — lockstep with
    /// `EXCLUDED_DIR_NAMES` in the sweep script.
    const EXCLUDED_DIR_NAMES: [&str; 1] = ["clorinde"];

    /// A floor on the number of `.rs` files the walk must visit. `src-tauri/src`
    /// holds ~1500; a walker that lost the tree (a moved `CARGO_MANIFEST_DIR`,
    /// a renamed directory) must fail loudly rather than report a clean zero.
    const MIN_FILES_WALKED: usize = 1000;

    /// `src-tauri` — where `Cargo.toml` with the `[lints]` tables lives.
    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The repo root — where `clippy.toml` lives (step 1 verified the root
    /// location resolves from `src-tauri`).
    fn repo_root() -> PathBuf {
        manifest_dir().join("..")
    }

    /// Every `.rs` file under [`DIRS`], as `(path, contents)`, this file
    /// excluded. `read_dir` walk in the `agent_runtime.rs` shape.
    fn walk_sources() -> Vec<(PathBuf, String)> {
        let root = repo_root();
        let mut out = Vec::new();
        let mut stack: Vec<PathBuf> = DIRS.iter().map(|d| root.join(d)).collect();
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => panic!("read_dir {}: {e}", dir.display()),
            };
            for entry in entries {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
                    if EXCLUDED_DIR_NAMES.contains(&name) {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                // This file's own constants and doc comments spell the
                // needles; a self-scan would count prose as an attribute.
                if path.file_name().and_then(|f| f.to_str()) == Some("row_get_ratchet.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read source");
                out.push((path, text));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(
            out.len() >= MIN_FILES_WALKED,
            "walked only {} .rs files under {:?} (floor {MIN_FILES_WALKED}) — the walker lost the tree",
            out.len(),
            DIRS
        );
        out
    }

    /// Every attribute in `text` as `(line_no, compact)`: an attribute starts
    /// at a line whose trimmed form begins with `#[` or `#![`; a multi-line
    /// one (trimmed line ending in `(`) is joined with the following lines up
    /// to the one that is exactly `)]`. `compact` is the joined text with all
    /// whitespace removed, so a needle matches either spelling and prose
    /// quoting an attribute inside a comment does not. Mirrors
    /// `attribute_starts` in `scripts/row-get-expect-sweep.py`.
    fn attribute_starts(text: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim();
            if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
                let mut j = i;
                if trimmed.ends_with('(') {
                    while j + 1 < lines.len() && lines[j].trim() != ")]" {
                        j += 1;
                    }
                }
                let compact: String = lines[i..=j]
                    .iter()
                    .flat_map(|l| l.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                out.push((i + 1, compact));
                i = j + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The attributes in `text` whose compact form STARTS with `needle`.
    fn attribute_lines(text: &str, needle: &str) -> Vec<(usize, String)> {
        attribute_starts(text)
            .into_iter()
            .filter(|(_, compact)| compact.starts_with(needle))
            .collect()
    }

    /// The file with `#`-comment lines dropped, so a commented-out gate does
    /// not read as a wired one.
    fn toml_code(text: &str) -> String {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_matcher_counts_both_spellings_and_not_prose() {
        let text = "\
fn a() {}
    #[expect(clippy::disallowed_methods, reason = \"x\")]
    fn b() {}
    #[expect(
        clippy::disallowed_methods,
        reason = \"legacy Row::get\"
    )]
    fn c() {}
    /// prose: #[expect(clippy::disallowed_methods, …)] is what the sweep writes
    // #[allow(clippy::disallowed_methods)] in a comment is not an attribute either
    #[allow(
        clippy::disallowed_methods
    )]
    fn d() {}
    #[cfg(test)]
    fn e() {}
";
        let expects = attribute_lines(text, EXPECT_NEEDLE);
        assert_eq!(
            expects.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![2, 4],
            "one single-line and one four-line expect, prose excluded: {expects:?}"
        );
        let allows = attribute_lines(text, FORBIDDEN_NEEDLES[0]);
        assert_eq!(allows.len(), 1, "the multi-line allow is found: {allows:?}");
        assert_eq!(allows[0].0, 11);
        assert_eq!(
            attribute_starts(text).len(),
            4,
            "expect, expect, allow, cfg"
        );
    }

    #[test]
    fn the_expect_count_only_falls() {
        let sources = walk_sources();
        let mut per_file: Vec<(String, usize)> = Vec::new();
        let mut total = 0usize;
        for (path, text) in &sources {
            let n = attribute_lines(text, EXPECT_NEEDLE).len();
            if n > 0 {
                per_file.push((path.display().to_string(), n));
                total += n;
            }
        }
        // Non-vacuity: while the baseline is above zero the walk must find
        // attributes, or the ceiling is being compared against a walker that
        // read nothing.
        if BASELINE > 0 {
            assert!(
                total > 0,
                "BASELINE is {BASELINE} but the walk found no `{EXPECT_NEEDLE}` line — the walker or the needle is wrong"
            );
        }
        assert!(
            total <= BASELINE,
            "the count only falls — lower the baseline when you migrate a fn. \
             Found {total} `{EXPECT_NEEDLE}` attribute(s), BASELINE is {BASELINE}. A count ABOVE the \
             baseline means a new `tokio_postgres::Row::get` site was grandfathered with an \
             `#[expect]` instead of being written with `try_get`; rewrite it. Per file: {per_file:?}"
        );
        if total < BASELINE {
            eprintln!(
                "row_get_ratchet: {total} attribute(s) < BASELINE {BASELINE} — lower BASELINE in \
                 src-tauri/src/row_get_ratchet.rs to {total}"
            );
        }
    }

    #[test]
    fn no_allow_and_no_inner_attribute_bypasses_the_gate() {
        let sources = walk_sources();
        let mut hits: Vec<String> = Vec::new();
        for (path, text) in &sources {
            for needle in FORBIDDEN_NEEDLES {
                for (line_no, compact) in attribute_lines(text, needle) {
                    hits.push(format!("{}:{line_no}: {compact}", path.display()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "the Row::get gate is bypassed at fn granularity only, with `#[expect]` (never `allow`, \
             never an inner `#![…]` attribute — an `allow` is never checked for fulfilment and a \
             file-level attribute leaves the whole file open to new sites): {hits:#?}"
        );
    }

    #[test]
    fn the_gate_is_still_wired() {
        let root = repo_root();
        let clippy_toml = std::fs::read_to_string(root.join("clippy.toml")).expect(
            "clippy.toml at the repo root — the Row::get gate's disallowed-methods entry lives there",
        );
        let clippy_code = toml_code(&clippy_toml);
        assert!(
            clippy_code.contains("disallowed-methods"),
            "clippy.toml no longer has a `disallowed-methods` table"
        );
        assert!(
            clippy_code.contains("tokio_postgres::Row::get"),
            "clippy.toml no longer disallows `tokio_postgres::Row::get`"
        );
        // clippy stops at the FIRST clippy.toml it finds walking up from
        // CARGO_MANIFEST_DIR, so a file beside src-tauri/Cargo.toml would
        // silently shadow the root one — including its disallowed-methods.
        let shadow = manifest_dir().join("clippy.toml");
        assert!(
            !shadow.exists(),
            "{} shadows the repo-root clippy.toml (clippy uses the nearest config only) — \
             move the disallowed-methods entry there or delete this file",
            shadow.display()
        );

        let cargo_toml = std::fs::read_to_string(manifest_dir().join("Cargo.toml"))
            .expect("src-tauri/Cargo.toml");
        let cargo_code = toml_code(&cargo_toml);
        assert!(
            cargo_code.contains("disallowed_methods = { level = \"deny\""),
            "src-tauri/Cargo.toml no longer sets `disallowed_methods = {{ level = \"deny\" …}}` under [lints.clippy]"
        );
        assert!(
            cargo_code.contains("unfulfilled_lint_expectations = \"deny\""),
            "src-tauri/Cargo.toml no longer sets `unfulfilled_lint_expectations = \"deny\"` under [lints.rust] — \
             without it a migrated fn's stale `#[expect]` is silent and the ratchet count lies"
        );
    }
}
