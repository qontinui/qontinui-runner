//! Ratchets for the deny lints that grandfather existing sites with a fn-level
//! `#[expect]` — one table, one walk, one set of tests.
//!
//! Plan `2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor`,
//! Phase 3 step 3 (`tokio_postgres::Row::get`, source
//! dossier:row-get-panic-kills-spawned-loop), made table-driven by plan
//! `2026-09-14-runner-str-byte-slice-class-has-no-lint-gate`, which added
//! `clippy::string_slice` and replaced the single-lint `row_get_ratchet.rs`.
//! The design is qontinui-coord's `crates/coord/src/expect_ratchet.rs`
//! (qontinui-coord#2164), with this repo's walk (`src-tauri/src` +
//! `src-tauri/tests` + `src-tauri/build.rs`, `clorinde` excluded) and its
//! `clippy.toml` shadow check.
//!
//! The mechanism is the same for every entry of [`RATCHETS`]. A panic class
//! (`Row::get` on a NULL / type / index mismatch; a `&str` byte slice on a
//! non-char-boundary) is DENIED in `src-tauri/Cargo.toml` `[lints.clippy]`
//! (and for `Row::get` the repo-root `clippy.toml` `disallowed-methods` entry
//! names the method), so a NEW site cannot land. The sites that existed when
//! each gate landed are grandfathered one fn at a time with
//! `#[expect(<lint>, reason = …)]`, inserted by
//! `scripts/row-get-expect-sweep.py --lint <lint>` (run once per required
//! clippy leg — ubuntu and `x86_64-pc-windows-msvc`), and this module pins each
//! attribute count as a CEILING: it only falls. A fn that migrates (to
//! `try_get`; to `str::get` / `char_indices` / `str_utils::truncate_str`) must
//! drop its attribute — `unfulfilled_lint_expectations`, also `deny`, reds the
//! build otherwise — and then lower that entry's `baseline` here.
//!
//! Three source-scan tests, DB-free, over the same directories the sweep
//! counts, each iterating the table:
//!
//! 1. the attribute count is `<= baseline`, and non-vacuously `> 0`;
//! 2. no `#[allow(<lint>` and no inner (`#![…]`) expectation anywhere — an
//!    `allow` never fires the unfulfilled-expectation check, and a crate- or
//!    module-level attribute leaves a whole file open to new sites;
//! 3. the gate itself is still wired: every `gate_wiring` needle is present in
//!    its file's non-comment lines, and no `src-tauri/clippy.toml` shadows the
//!    repo-root one (clippy stops at the FIRST config it finds walking up from
//!    `CARGO_MANIFEST_DIR`) — so a gate cannot be removed silently.
//!
//! The scan is attribute-aware, not line-aware: attributes are FLATTENED
//! (continuation lines joined, all whitespace removed) before any needle is
//! applied, so the rustfmt four-line spelling, a hand-wrapped one and a
//! GROUPED one (`#[expect(clippy::foo, clippy::string_slice)]`) all count.
//!
//! This file is excluded from its own walk: it spells the needles as literals.
//! Declared `#[cfg(test)]` in `main.rs` beside the other test-only modules.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// One grandfathered deny lint.
    struct Ratchet {
        /// The lint, as clippy names it (`clippy::…`).
        lint: &'static str,
        /// The attribute the sweep inserts, as it reads once FLATTENED (the
        /// prefix; the `reason` follows). Kept for the failure messages and the
        /// sweep-script cross-reference — the classifier matches the lint name
        /// anywhere in the attribute's lint list, deliberately.
        expect_needle: &'static str,
        /// The ceiling — the number of fn-level `#[expect(<lint>, …)]`
        /// attributes the sweep placed when the gate landed. **Lower it when
        /// you migrate a fn; never raise it.** A new site is not grandfathered
        /// — it is a deny error until it is rewritten.
        baseline: usize,
        /// `(file relative to the repo root, needle)` pairs that must each
        /// appear in that file's non-comment lines for the gate to count as
        /// wired.
        gate_wiring: &'static [(&'static str, &'static str)],
    }

    const RATCHETS: [Ratchet; 3] = [
        // Plan 2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor,
        // Phase 3: carried over from `row_get_ratchet.rs`, whose BASELINE had
        // stayed at 562 while migrations took the real count down to 537 —
        // re-measured here and pinned at the measured count, since the ceiling
        // only falls.
        Ratchet {
            lint: "clippy::disallowed_methods",
            expect_needle: "#[expect(clippy::disallowed_methods",
            baseline: 537,
            gate_wiring: &[
                ("clippy.toml", "disallowed-methods"),
                ("clippy.toml", "tokio_postgres::Row::get"),
                (
                    "src-tauri/Cargo.toml",
                    "disallowed_methods = { level = \"deny\"",
                ),
                (
                    "src-tauri/Cargo.toml",
                    "unfulfilled_lint_expectations = \"deny\"",
                ),
            ],
        },
        // Plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate: the
        // count the sweep left once BOTH required clippy legs were clean —
        // ubuntu (`--all-targets`) and `x86_64-pc-windows-msvc`, the latter
        // completed from the `Clippy (windows)` job's own log in the SAME PR
        // that landed the gate, so it is the initial count, not a raise —
        // after the arbitrary-text truncations migrated to
        // `str_utils::truncate_str`. Only sites that
        // PREDATE the gate are ever grandfathered — a site that lands after it
        // migrates instead (the lesson of qontinui-coord plan
        // 2026-09-21-coord-string-slice-sites-are-re-swept-instead-of-migrated-and-red-main-blocks-nobody).
        Ratchet {
            lint: "clippy::string_slice",
            expect_needle: "#[expect(clippy::string_slice",
            baseline: 435,
            gate_wiring: &[
                ("src-tauri/Cargo.toml", "string_slice = { level = \"deny\""),
                (
                    "src-tauri/Cargo.toml",
                    "unfulfilled_lint_expectations = \"deny\"",
                ),
            ],
        },
        // Plan 2026-09-12-residual-work-from-the-april-2026-plan-audit, Phase 1:
        // the regression guard plan runner-arc-runtime-root-cause (deliverable
        // #5) never landed. An owned `tokio::runtime::Runtime` dropped from an
        // async context panics (`Cannot drop a runtime in a context where
        // blocking is not allowed`), so naming the type is denied via the
        // repo-root `clippy.toml` `disallowed-types`. Grandfathered: the
        // `&Runtime` params in `main.rs`, the two process-lived `OnceLock`
        // statics (`APP_RUNTIME`, `API_RUNTIME` — item-level expects), and the
        // `#[cfg(test)]` runtimes that test a sync path outside any runtime.
        // Measured on the ubuntu `--all-targets` leg only; the windows leg
        // cannot run on the authoring box.
        Ratchet {
            lint: "clippy::disallowed_types",
            expect_needle: "#[expect(clippy::disallowed_types",
            baseline: 10,
            gate_wiring: &[
                ("clippy.toml", "disallowed-types"),
                ("clippy.toml", "tokio::runtime::Runtime"),
                (
                    "src-tauri/Cargo.toml",
                    "disallowed_types = { level = \"deny\"",
                ),
                (
                    "src-tauri/Cargo.toml",
                    "unfulfilled_lint_expectations = \"deny\"",
                ),
            ],
        },
    ];

    impl Ratchet {
        /// The lint's bare name (`disallowed_methods`), matched inside a
        /// flattened attribute's lint list so a grouped spelling counts.
        fn bare_lint(&self) -> &'static str {
            self.lint.strip_prefix("clippy::").unwrap_or(self.lint)
        }
    }

    /// The directories the sweep script counts over — kept in lockstep with
    /// `RATCHET_DIRS` in `scripts/row-get-expect-sweep.py`.
    const DIRS: [&str; 2] = ["src-tauri/src", "src-tauri/tests"];
    /// Directory names skipped anywhere in the walk — lockstep with
    /// `EXCLUDED_DIR_NAMES` in the sweep script.
    const EXCLUDED_DIR_NAMES: [&str; 1] = ["clorinde"];
    /// Single files outside [`DIRS`] that the lints reach — lockstep with
    /// `RATCHET_FILES` in the sweep script. The build script is linted by
    /// `[lints]` like any other target.
    const FILES: [&str; 1] = ["src-tauri/build.rs"];

    /// A floor on the number of `.rs` files the walk must visit. `src-tauri/src`
    /// holds ~1500; a walker that lost the tree (a moved `CARGO_MANIFEST_DIR`,
    /// a renamed directory) must fail loudly rather than report a clean zero.
    const MIN_FILES_WALKED: usize = 1000;

    /// `src-tauri` — where `Cargo.toml` with the `[lints]` tables lives.
    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The repo root — where `clippy.toml` lives.
    fn repo_root() -> PathBuf {
        manifest_dir().join("..")
    }

    /// Every `.rs` file under [`DIRS`], as `(path, contents)`, this file
    /// excluded.
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
                if path.file_name().and_then(|f| f.to_str()) == Some("expect_ratchet.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read source");
                out.push((path, text));
            }
        }
        for rel in FILES {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} — listed in FILES: {e}", path.display()));
            out.push((path, text));
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

    /// One attribute in the source, flattened to a single whitespace-free
    /// string — `#[expect(\n  clippy::disallowed_methods,\n  reason = "…"\n)]`
    /// and `#[expect(clippy::disallowed_methods, reason = "…")]` both arrive
    /// here as the same bytes.
    #[derive(Debug, Clone)]
    struct Attribute {
        /// 1-based line of the attribute's OPENING `#`.
        line_no: usize,
        /// The whole attribute, whitespace removed.
        flat: String,
        /// `true` for an inner attribute (`#![…]`) — file or crate
        /// granularity, which leaves everything below it open.
        inner: bool,
    }

    impl Attribute {
        /// The attribute's LINT LIST: everything before a `reason=` key, so a
        /// lint name quoted inside some other attribute's reason text is not
        /// mistaken for a lint the attribute governs.
        fn lint_list(&self) -> &str {
            self.flat
                .split_once("reason=")
                .map_or(self.flat.as_str(), |(list, _)| list)
        }
        /// Does the lint list name `lint` (bare name) as a whole path segment?
        fn names_lint(&self, lint: &str) -> bool {
            self.lint_list()
                .split(|c: char| c == ',' || c == '(' || c == ')')
                .any(|item| item == format!("clippy::{lint}") || item == lint)
        }
        /// An `#[expect(…)]` / `#![expect(…)]` grandfathering `lint`.
        fn is_expect_of_lint(&self, lint: &str) -> bool {
            (self.flat.starts_with("#[expect(") || self.flat.starts_with("#![expect("))
                && self.names_lint(lint)
        }
        /// An `#[allow(…)]` / `#![allow(…)]` of `lint`. An `allow` is NEVER
        /// checked for fulfilment, so it hides a migrated site forever.
        fn is_allow_of_lint(&self, lint: &str) -> bool {
            (self.flat.starts_with("#[allow(") || self.flat.starts_with("#![allow("))
                && self.names_lint(lint)
        }
    }

    /// Every attribute in `text`, flattened.
    ///
    /// A line whose stripped form opens with `#[` or `#![` starts an
    /// attribute; continuation lines are appended until the square brackets
    /// balance (bounded, so a malformed file cannot run away). Prose quoting
    /// an attribute inside a `//!` or `///` comment never opens with `#[`, so
    /// doc comments are skipped.
    fn flatten_attributes(text: &str) -> Vec<Attribute> {
        const MAX_CONTINUATION_LINES: usize = 32;
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < lines.len() {
            let trimmed = lines[i].trim_start();
            let inner = trimmed.starts_with("#![");
            if !inner && !trimmed.starts_with("#[") {
                i += 1;
                continue;
            }
            let mut flat: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
            let mut j = i;
            let balanced = |s: &str| s.matches('[').count() == s.matches(']').count();
            while !balanced(&flat) && j + 1 < lines.len() && j - i < MAX_CONTINUATION_LINES {
                j += 1;
                flat.extend(lines[j].chars().filter(|c| !c.is_whitespace()));
            }
            out.push(Attribute {
                line_no: i + 1,
                flat,
                inner,
            });
            i = j + 1;
        }
        out
    }

    /// A file's non-comment lines, joined — so a needle quoted in a comment
    /// does not count as wiring.
    fn code_lines(root: &Path, rel: &str) -> String {
        let text = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel} — a gate's wiring lives there: {e}"));
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_expect_count_only_falls() {
        let sources = walk_sources();
        for ratchet in &RATCHETS {
            let mut per_file: Vec<(String, usize)> = Vec::new();
            let mut total = 0usize;
            for (path, text) in &sources {
                let n = flatten_attributes(text)
                    .iter()
                    .filter(|a| a.is_expect_of_lint(ratchet.bare_lint()))
                    .count();
                if n > 0 {
                    per_file.push((path.display().to_string(), n));
                    total += n;
                }
            }
            let (lint, baseline, needle) = (ratchet.lint, ratchet.baseline, ratchet.expect_needle);
            // Non-vacuity: while the baseline is above zero the walk must find
            // attributes, or the ceiling is being compared against a walker
            // that read nothing.
            if baseline > 0 {
                assert!(
                    total > 0,
                    "{lint}: baseline is {baseline} but the walk found no `{needle}` attribute — \
                     the walker or the needle is wrong"
                );
            }
            assert!(
                total <= baseline,
                "{lint}: the count only falls — lower the baseline when you migrate a fn. \
                 Found {total} `{needle}` attribute(s), baseline is {baseline}. A count ABOVE \
                 the baseline means a new site was grandfathered with an `#[expect]` instead \
                 of being rewritten; rewrite it. Per file: {per_file:?}"
            );
            if total < baseline {
                eprintln!(
                    "expect_ratchet: {lint}: {total} attribute(s) < baseline {baseline} — lower \
                     that entry's baseline in src-tauri/src/expect_ratchet.rs to {total}"
                );
            }
        }
    }

    #[test]
    fn no_allow_and_no_inner_attribute_bypasses_the_gate() {
        let sources = walk_sources();
        for ratchet in &RATCHETS {
            let mut hits: Vec<String> = Vec::new();
            for (path, text) in &sources {
                for attr in flatten_attributes(text) {
                    // Two bypasses, both fatal:
                    //  - an `allow` of the lint at ANY granularity (never
                    //    checked for fulfilment), and
                    //  - an INNER `expect` (`#![expect(…)]`), which is file- or
                    //    crate-wide whether or not it names this lint.
                    let forbidden = attr.is_allow_of_lint(ratchet.bare_lint())
                        || (attr.inner && attr.flat.starts_with("#![expect("));
                    if forbidden {
                        hits.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            attr.line_no,
                            attr.flat
                        ));
                    }
                }
            }
            assert!(
                hits.is_empty(),
                "{}: the gate is bypassed at fn granularity only, with `#[expect]` (never \
                 `allow`, never an inner `#![…]` attribute — an `allow` is never checked for \
                 fulfilment and a file-level attribute leaves the whole file open to new \
                 sites): {hits:#?}",
                ratchet.lint
            );
        }
    }

    #[test]
    fn the_gate_is_still_wired() {
        let root = repo_root();
        for ratchet in &RATCHETS {
            for (rel, needle) in ratchet.gate_wiring {
                assert!(
                    code_lines(&root, rel).contains(needle),
                    "{}: `{rel}` no longer carries `{needle}` outside a comment — the gate \
                     was unwired, and without it the grandfathered `#[expect]` count says \
                     nothing",
                    ratchet.lint
                );
            }
        }
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
    }

    /// The scanner's OWN blind spot, pinned: a wrapped attribute — the shape
    /// rustfmt produces and the sweep writes — must count exactly like the
    /// one-line spelling, and prose / an unrelated lint must not.
    #[test]
    fn flatten_attributes_sees_a_multi_line_attribute() {
        let src = "\
struct A;

    #[expect(
        clippy::disallowed_methods,
        reason = \"legacy Row::get\"
    )]
    fn wrapped() {}

    #[expect(clippy::disallowed_methods, reason = \"legacy Row::get\")]
    fn inline() {}

    #[expect(clippy::needless_borrow)]
    fn unrelated() {}
";
        let attrs = flatten_attributes(src);
        let counted: Vec<usize> = attrs
            .iter()
            .filter(|a| a.is_expect_of_lint("disallowed_methods"))
            .map(|a| a.line_no)
            .collect();
        assert_eq!(
            counted,
            vec![3, 9],
            "both spellings must count, and the unrelated lint must not. Flattened: {:?}",
            attrs.iter().map(|a| a.flat.as_str()).collect::<Vec<_>>()
        );
    }

    /// Grouping the lint with another one inside ONE attribute is also a
    /// grandfathering, and must be counted as one.
    #[test]
    fn a_grouped_expect_counts_once() {
        let src = "    #[expect(clippy::needless_borrow, clippy::string_slice)]\n    fn g() {}\n";
        let n = flatten_attributes(src)
            .iter()
            .filter(|a| a.is_expect_of_lint("string_slice"))
            .count();
        assert_eq!(n, 1, "a grouped attribute still grandfathers the lint");
    }

    /// A lint name that appears only inside the REASON text of an attribute
    /// for another lint is not a grandfathering of it.
    #[test]
    fn a_lint_named_only_in_a_reason_is_not_counted() {
        let src = "    #[expect(\n        clippy::disallowed_methods,\n        reason = \"unlike clippy::string_slice, this is Row::get\"\n    )]\n    fn f() {}\n";
        let attrs = flatten_attributes(src);
        assert!(attrs
            .iter()
            .any(|a| a.is_expect_of_lint("disallowed_methods")));
        assert!(
            !attrs.iter().any(|a| a.is_expect_of_lint("string_slice")),
            "a lint quoted in the reason must not count: {attrs:?}"
        );
    }

    /// The bypass test must see the wrapped spellings too — otherwise an
    /// `allow` simply needs a line break to become invisible.
    #[test]
    fn a_multi_line_allow_and_a_multi_line_inner_expect_are_both_caught() {
        let wrapped_allow = "    #[allow(\n        clippy::string_slice\n    )]\n    fn a() {}\n";
        assert!(
            flatten_attributes(wrapped_allow)
                .iter()
                .any(|a| a.is_allow_of_lint("string_slice")),
            "a wrapped `allow` of the lint must be caught"
        );
        let wrapped_inner = "#![expect(\n    clippy::disallowed_methods\n)]\n";
        let attrs = flatten_attributes(wrapped_inner);
        assert!(
            attrs
                .iter()
                .any(|a| a.inner && a.flat.starts_with("#![expect(")),
            "a wrapped INNER expect must be caught: {attrs:?}"
        );
    }

    /// Prose that quotes an attribute must not be counted. The walk already
    /// excludes this file; the classifier must not need that exclusion.
    #[test]
    fn prose_quoting_an_attribute_is_not_an_attribute() {
        let src = "/// The sweep inserts `#[expect(clippy::disallowed_methods, reason = \"…\")]`.\n//! and `#[allow(clippy::disallowed_methods)]` is forbidden.\nfn f() {}\n";
        let attrs = flatten_attributes(src);
        assert!(
            attrs.is_empty(),
            "a doc comment opening with `///` or `//!` is not an attribute: {attrs:?}"
        );
    }

    /// Each entry's sweep spelling and the classifier must agree, or the sweep
    /// inserts attributes the ratchet cannot see.
    #[test]
    fn the_sweep_scripts_own_spelling_is_classified_as_an_expect_of_the_lint() {
        for ratchet in &RATCHETS {
            let needle = ratchet.expect_needle;
            let src = format!(
                "    #[expect(\n        {},\n        reason = \"legacy\"\n    )]\n    fn f() {{}}\n    {needle}, reason = \"legacy\")]\n    fn g() {{}}\n",
                ratchet.lint
            );
            assert_eq!(
                flatten_attributes(&src)
                    .iter()
                    .filter(|a| a.is_expect_of_lint(ratchet.bare_lint()))
                    .count(),
                2,
                "`{needle}` (both the four-line spelling scripts/row-get-expect-sweep.py writes \
                 and the one-line one) must be counted for {}",
                ratchet.lint
            );
        }
    }
}
