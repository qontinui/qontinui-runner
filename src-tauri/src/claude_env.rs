//! Claude Code process-topology env markers, and the strip rule for them.
//!
//! Claude Code sets a small number of env vars that describe a process's place
//! in a session tree. They are inherited by the entire process tree and nothing
//! clears them, so a long-lived daemon that happens to have been launched from
//! a Claude Code session passes them down forever — and every `claude` it
//! eventually spawns claims to be a nested session of something that exited
//! months ago.
//!
//! The rule: **every spawn site strips every marker in
//! [`INHERITED_SESSION_MARKERS`]**, by calling
//! [`StripInheritedClaudeMarkers::strip_inherited_claude_markers`]. This module
//! exists so the spawn-site strips, the startup warning
//! ([`inherited_session_markers`]) and the `coord doctor` check share one
//! spelling of each name — a typo in any one of them is silent, removing
//! nothing while appearing to.
//!
//! ## Why the strip is a shared helper and not a per-site `env_remove`
//!
//! It used to be the latter, and that is precisely how
//! [`CLAUDE_CHILD_SESSION_ENV`] came to be missing from every site for months: a
//! rule enforced by prose plus copy-pasted `env_remove` lines is re-broken by
//! the next spawn site somebody adds by copying an existing one. The first pass
//! at this module (PR #942) kept the per-site form and, in the same change,
//! missed FOUR live `claude` spawn functions —
//! `agent_runtime::spawn_claude_child`,
//! `commands::ai_settings::refresh_claude_cli_auth` (and
//! `test_claude_cli_connection` in the same file),
//! `commands::command_interpreter::command_interpret`, and
//! `fleet::detect_claude_code_now` — which is the same failure repeating
//! inside the fix for it.
//!
//! Those four were not found together. The follow-up's first draft found two;
//! review found a third; the guard test below found the fourth. THREE careful
//! passes over one short rule each left a site behind — which is the whole
//! argument for not enforcing it by reading.
//!
//! So the rule is code now: one helper, one marker list, and three guard tests
//! (`no_spawn_site_open_codes_the_strip`, `every_claude_spawn_site_strips`,
//! `the_shared_strip_is_called_at_every_known_spawn_site`). Adding a third
//! marker is a one-line change that covers every site at once. This mirrors
//! `qontinui-supervisor/src/process/claude_env.rs`, which shipped the enforced
//! form for the supervisor half of the same plan.
//!
//! **What the guards do and do not cover.** `every_claude_spawn_site_strips`
//! triggers on a command constructor whose ARGUMENT is a Claude program (a
//! `"claude…"` literal, or a `claude_program`/`claude_bin`/`claude_path`
//! binding). It therefore does NOT recognise a program passed as an
//! arbitrarily-named variable — `spawn_claude_child`'s `tokio_no_window(&bin)`
//! is invisible to it, and is held instead by the call-count floor in
//! `the_shared_strip_is_called_at_every_known_spawn_site`. The trigger is
//! narrow on purpose: a broader one flagged 19 sites of which 17 were keychain
//! strings and process-list filters, and a guard that cries wolf gets
//! allowlisted into uselessness. These are backstops, not proofs.
//!
//! ## The markers
//!
//! - [`CLAUDECODE_ENV`] — "you are running inside Claude Code". Long-standing;
//!   stripped at seven spawn sites before this module existed. (An earlier
//!   revision of this note said eleven. Counted at `a4576298^`: seven source
//!   files carried `env_remove("CLAUDECODE")`.)
//! - [`CLAUDE_CHILD_SESSION_ENV`] — "you are a child/nested session". Added by
//!   plan `2026-07-28-runner-transcript-persistence-env-leak`, which found it
//!   leaking into every fleet session.
//!
//! ## What the child-session marker does NOT do
//!
//! It was believed to disable transcript persistence. Vetting on 2026-08-03
//! **refuted** that by direct observation: a session with the marker set was
//! writing its JSONL transcript incrementally. So this strip is env hygiene —
//! removing a lie about process topology that the CLI is entitled to act on
//! however it likes — and NOT a fix for lost transcripts. Do not re-justify it
//! as data recovery; see §0 of that plan.

/// "You are running inside Claude Code."
pub const CLAUDECODE_ENV: &str = "CLAUDECODE";

/// "You are a child/nested Claude Code session."
pub const CLAUDE_CHILD_SESSION_ENV: &str = "CLAUDE_CODE_CHILD_SESSION";

/// Every marker a spawn site must strip, and the startup check must report.
///
/// **The single source of truth.** Add a marker here and every spawn site picks
/// it up through [`StripInheritedClaudeMarkers`]; there is no per-site list to
/// keep in sync. It is also the list the startup warning and `coord doctor`
/// iterate.
pub const INHERITED_SESSION_MARKERS: &[&str] = &[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV];

/// Removes every [`INHERITED_SESSION_MARKERS`] entry from a command's child env.
///
/// Call this on **every** spawn of the Claude CLI, and on every spawn of a
/// process that goes on to launch one (a runner instance, a terminal hosting a
/// `claude` pane, a shell running `claude auth login`).
///
/// Implemented for the three command builders the runner spawns with —
/// `std::process::Command`, `tokio::process::Command` and
/// `portable_pty::CommandBuilder` — because they share no upstream trait, which
/// is the reason the strip was previously copy-pasted per site.
///
/// Returns `&mut Self` so it drops into an existing builder chain in place of
/// the `.env_remove(...)` lines it replaces.
pub trait StripInheritedClaudeMarkers {
    /// Strip the inherited markers. Call this on **every** spawn.
    fn strip_inherited_claude_markers(&mut self) -> &mut Self;
}

impl StripInheritedClaudeMarkers for std::process::Command {
    fn strip_inherited_claude_markers(&mut self) -> &mut Self {
        for marker in INHERITED_SESSION_MARKERS {
            self.env_remove(marker);
        }
        self
    }
}

impl StripInheritedClaudeMarkers for tokio::process::Command {
    fn strip_inherited_claude_markers(&mut self) -> &mut Self {
        for marker in INHERITED_SESSION_MARKERS {
            self.env_remove(marker);
        }
        self
    }
}

impl StripInheritedClaudeMarkers for portable_pty::CommandBuilder {
    fn strip_inherited_claude_markers(&mut self) -> &mut Self {
        for marker in INHERITED_SESSION_MARKERS {
            // Unlike the `Command` types this returns `()`, which is exactly the
            // kind of shape difference that kept the PTY path on its own
            // hand-written strip.
            self.env_remove(marker);
        }
        self
    }
}

/// Which of [`INHERITED_SESSION_MARKERS`] this process inherited, in order.
///
/// A marker counts as inherited whenever it is **present**, including when set
/// to the empty string — `env_remove` is what clears one, not assigning `""`.
pub fn inherited_session_markers() -> Vec<&'static str> {
    INHERITED_SESSION_MARKERS
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

/// One-line, operator-readable summary of an inherited-marker set.
///
/// Kept next to the detection so the runner startup warning and the
/// `coord doctor` check render the same sentence.
pub fn inherited_markers_detail(markers: &[&str]) -> String {
    format!(
        "inherited Claude Code session marker(s): {} — this process is mislabelled as a nested \
         session. They are stripped from every terminal/CLI spawn, so panes are unaffected; the \
         markers reach this process from whatever launched it (usually the supervisor, which \
         inherits them from a Claude Code session).",
        markers.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_names_are_the_cli_spellings() {
        // A typo here is silent: a spawn would keep leaking the real marker
        // while `env_remove` deleted a variable nobody sets.
        assert_eq!(CLAUDECODE_ENV, "CLAUDECODE");
        assert_eq!(CLAUDE_CHILD_SESSION_ENV, "CLAUDE_CODE_CHILD_SESSION");
    }

    #[test]
    fn every_marker_is_listed_for_detection() {
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDECODE_ENV));
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDE_CHILD_SESSION_ENV));
        assert_eq!(INHERITED_SESSION_MARKERS.len(), 2);
    }

    #[test]
    fn detection_reports_present_markers_including_empty_values() {
        // `std::env` is process-global — hold the shared lock and restore on
        // drop so a sibling test can neither race this nor inherit its writes.
        let _g = crate::test_env::env_lock();
        let _restore =
            crate::test_env::EnvVarRestore::capture(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);

        for name in INHERITED_SESSION_MARKERS {
            std::env::remove_var(name);
        }
        assert!(
            inherited_session_markers().is_empty(),
            "no markers set → nothing inherited"
        );

        std::env::set_var(CLAUDE_CHILD_SESSION_ENV, "1");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDE_CHILD_SESSION_ENV],
            "a set marker is detected"
        );

        // Empty-but-set is still inherited — this is the case a naive
        // `var() == Ok(non_empty)` check would wrongly clear.
        std::env::set_var(CLAUDECODE_ENV, "");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV],
            "empty-but-set counts, and order follows INHERITED_SESSION_MARKERS"
        );
    }

    #[test]
    fn detail_names_every_marker_it_was_given() {
        let detail = inherited_markers_detail(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);
        assert!(detail.contains(CLAUDECODE_ENV));
        assert!(detail.contains(CLAUDE_CHILD_SESSION_ENV));
    }

    // ---- The shared strip ------------------------------------------------

    /// Assert the builder records a REMOVAL (not a set, not an absence) for
    /// every marker.
    ///
    /// `get_envs` yields `(key, None)` for `env_remove` and `(key, Some(v))`
    /// for `env`. The distinction is the whole point: merely *not setting* a
    /// marker still lets the child inherit this process's copy, which is the
    /// bug. Only an explicit removal blocks inheritance.
    fn assert_all_markers_removed(cmd: &std::process::Command, what: &str) {
        for marker in INHERITED_SESSION_MARKERS {
            let entry = cmd
                .get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(marker));
            match entry {
                Some((_, None)) => {}
                Some((_, Some(v))) => panic!(
                    "{what}: {marker} is SET to {v:?} — a set marker is still inherited by the \
                     child; it must be removed"
                ),
                None => panic!(
                    "{what}: {marker} has no removal entry — the child would inherit this \
                     process's copy"
                ),
            }
        }
    }

    /// Every command flavour the runner spawns with must mark every marker for
    /// removal.
    ///
    /// Inspects the builder rather than spawning: deterministic, no
    /// process-global env mutation (so it cannot race
    /// `detection_reports_present_markers_including_empty_values`, which does
    /// mutate it), and identical on every platform.
    #[test]
    fn strip_marks_every_marker_for_removal_on_every_command_type() {
        let mut std_cmd = std::process::Command::new("echo");
        std_cmd.strip_inherited_claude_markers();
        assert_all_markers_removed(&std_cmd, "std::process::Command");

        let mut tokio_cmd = tokio::process::Command::new("echo");
        tokio_cmd.strip_inherited_claude_markers();
        assert_all_markers_removed(tokio_cmd.as_std(), "tokio::process::Command");
    }

    /// The PTY builder has different removal SEMANTICS from `Command`, so it
    /// gets its own assertion.
    ///
    /// `CommandBuilder::new` seeds its env map from the parent environment
    /// (`get_base_env()`), and `env_remove` deletes the entry outright — so
    /// "absent from the map" IS "not inherited by the pane" here, whereas for
    /// `Command` absence means the opposite (the child inherits ours). Setting
    /// the markers first makes the assertion independent of whether this test
    /// process happens to carry them.
    #[test]
    fn strip_clears_markers_on_the_pty_command_builder() {
        let mut builder = portable_pty::CommandBuilder::new("echo");
        for marker in INHERITED_SESSION_MARKERS {
            builder.env(marker, "1");
        }
        builder.strip_inherited_claude_markers();
        for marker in INHERITED_SESSION_MARKERS {
            assert!(
                builder.get_env(marker).is_none(),
                "{marker} survived the strip on portable_pty::CommandBuilder — the PTY pane \
                 would inherit it"
            );
        }
    }

    /// The strip must OVERRIDE an earlier explicit set on the same builder.
    ///
    /// Guards the ordering contract the spawn sites rely on: the strip sits in
    /// the middle of a builder chain, so it has to win against anything set
    /// before it.
    #[test]
    fn strip_overrides_a_marker_set_earlier_in_the_chain() {
        let mut cmd = std::process::Command::new("echo");
        for marker in INHERITED_SESSION_MARKERS {
            cmd.env(marker, "1");
        }
        cmd.strip_inherited_claude_markers();
        assert_all_markers_removed(&cmd, "marker set before the strip");
    }

    /// Collect every `.rs` file under `src/`.
    fn source_files() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Is this the module that implements the strip? It is the one file where
    /// the banned hand-rolled form legitimately appears.
    ///
    /// Matched on the PATH tail, not the bare file name: a future
    /// `src/something/claude_env.rs` must not inherit the exemption.
    fn is_this_module(path: &std::path::Path) -> bool {
        path.ends_with("src/claude_env.rs") || path.ends_with("src\\claude_env.rs")
    }

    /// Collapse all whitespace so a `rustfmt` line-wrap cannot hide a match.
    fn squash_ws(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// PART 1 of the guard — no site may open-code the strip.
    ///
    /// The `CLAUDE_CODE_CHILD_SESSION` leak existed because the strip rule
    /// lived in prose plus copy-pasted `env_remove` lines. This fails the build
    /// when somebody reintroduces that form.
    ///
    /// Matching runs over a WHITESPACE-COLLAPSED copy of each file, so a
    /// `rustfmt` wrap (which would otherwise split `env_remove(` from its
    /// argument and defeat a per-line scan) still matches. Needles are anchored
    /// to `env_remove(` so that merely READING a marker constant — as
    /// `coord_doctor` and this module's own detection do — is not flagged.
    #[test]
    fn no_spawn_site_open_codes_the_strip() {
        // Built at runtime so this file never contains the banned literals
        // itself. (The exemption in `is_this_module` is the real safety net;
        // this keeps the test honest even if that exemption is narrowed.)
        let mut needles: Vec<String> = INHERITED_SESSION_MARKERS
            .iter()
            .map(|m| format!("env_remove( \"{m}\""))
            .collect();
        for tail in ["CLAUDECODE_ENV", "CLAUDE_CHILD_SESSION_ENV"] {
            needles.push(format!("env_remove( {tail}"));
            needles.push(format!("env_remove( claude_env::{tail}"));
            needles.push(format!("env_remove( crate::claude_env::{tail}"));
            needles.push(format!(
                "env_remove( qontinui_runner_lib::claude_env::{tail}"
            ));
        }
        let mut offenders = Vec::new();
        let mut files_scanned = 0usize;

        for path in source_files() {
            if is_this_module(&path) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            files_scanned += 1;
            // `env_remove(x` and `env_remove( x` both normalise to the latter.
            let squashed = squash_ws(&text).replace("env_remove(", "env_remove( ");
            for needle in &needles {
                if squashed.contains(needle.as_str()) {
                    offenders.push(format!("{} (matched {:?})", path.display(), needle));
                }
            }
            // The loop form — `for m in INHERITED_SESSION_MARKERS { c.env_remove(m) }`.
            // Proximity, not bare presence: READING the list is legitimate (the
            // `coord doctor` check and the startup warning both iterate it), so
            // only a nearby `env_remove(` makes it a hand-rolled strip.
            for (at, _) in squashed.match_indices("INHERITED_SESSION_MARKERS") {
                let tail = &squashed[at..squashed.len().min(at + 200)];
                if tail.contains("env_remove(") {
                    offenders.push(format!("{} (hand-rolled strip loop)", path.display()));
                    break;
                }
            }
        }

        assert!(
            files_scanned > 50,
            "scanned only {files_scanned} sources — the assertion below would be near-vacuous"
        );
        assert!(
            offenders.is_empty(),
            "these sites reach for an inherited Claude marker directly instead of calling \
             `strip_inherited_claude_markers()`, which is how CLAUDE_CODE_CHILD_SESSION came to \
             be missing from every site for months:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// PART 2 of the guard — and the one that actually matters.
    ///
    /// Part 1 only catches the OLD form being reintroduced. It cannot catch the
    /// failure that actually happened twice: **a brand-new `claude` spawn site
    /// that never strips at all**. PR #942 shipped with
    /// `agent_runtime::spawn_claude_child` and
    /// `commands::ai_settings::refresh_claude_cli_auth` unstripped, and the
    /// follow-up's first draft still missed
    /// `commands::command_interpreter::command_interpret` — all three would
    /// have passed a Part-1-only guard, because none of them strip anything.
    ///
    /// So this scans for command CONSTRUCTIONS that look Claude-bound and
    /// requires a strip inside the same statement. "Claude-bound" is either:
    ///
    /// - the program expression mentions claude (`Command::new("claude")`,
    ///   `tokio_no_window(claude_program)`), or
    /// - the enclosing `fn` name mentions claude (`spawn_claude_child`,
    ///   `test_claude_cli_connection`) — which is what catches the
    ///   `tokio_no_window(&bin)` form where the program is a variable.
    ///
    /// A textual heuristic cannot be complete, and this one is deliberately
    /// biased toward the shapes this codebase actually writes. It is a
    /// backstop, not a proof.
    #[test]
    fn every_claude_spawn_site_strips() {
        // Trigger on the CONSTRUCTOR'S OWN ARGUMENT being a Claude program, not
        // on "the line/function mentions claude somewhere". This codebase says
        // "claude" constantly — in probe names, keychain service strings,
        // process-list filters — and a trigger that loose produced 19 hits of
        // which 17 were noise. A guard test that cries wolf gets allowlisted
        // into uselessness, so precision is the point.
        //
        // The narrow form is what caught `fleet::detect_claude_code_now`, a
        // real unstripped `no_window("claude")` that three earlier passes over
        // this exact rule had all walked past.
        fn constructs_claude(line: &str) -> bool {
            const CTORS: [&str; 3] = ["Command::new(", "CommandBuilder::new(", "no_window("];
            for ctor in CTORS {
                let mut from = 0;
                while let Some(at) = line[from..].find(ctor) {
                    let arg = line[from + at + ctor.len()..].trim_start();
                    let arg = arg.strip_prefix('&').unwrap_or(arg).trim_start();
                    if arg.starts_with("\"claude")
                        || arg.starts_with("claude_program")
                        || arg.starts_with("claude_bin")
                        || arg.starts_with("claude_path")
                    {
                        return true;
                    }
                    from += at + ctor.len();
                }
            }
            false
        }

        /// The enclosing `fn` block for `idx`: back to the nearest `fn`, forward
        /// to the next one. Coarse, but a spawn site's strip is always inside
        /// its own function, and this correctly covers the common
        /// `let mut cmd = …;` / `cmd.…strip…();` two-statement shape that a
        /// single-statement window misses.
        fn enclosing_fn(lines: &[&str], idx: usize) -> (String, usize, usize) {
            let is_fn = |s: &str| {
                let s = s.trim_start();
                s.starts_with("fn ")
                    || s.starts_with("pub fn ")
                    || s.starts_with("async fn ")
                    || s.starts_with("pub async fn ")
            };
            let start = (0..=idx).rev().find(|&j| is_fn(lines[j])).unwrap_or(0);
            let end = ((start + 1)..lines.len())
                .find(|&j| is_fn(lines[j]))
                .unwrap_or(lines.len());
            let name = lines[start]
                .rsplit("fn ")
                .next()
                .unwrap_or("")
                .split('(')
                .next()
                .unwrap_or("")
                .to_string();
            (name, start, end)
        }

        let mut offenders = Vec::new();
        let mut claude_sites = 0usize;

        for path in source_files() {
            if is_this_module(&path) {
                continue;
            }
            // The shim IS `claude.exe` on PATH and re-execs the real binary, so
            // a marker it carries came from its own parent. When that parent is
            // a genuine Claude Code session the marker is TRUE, and stripping
            // would replace one topology lie with its opposite. Deliberate.
            if path.ends_with("qontinui_shim.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();

            for (idx, line) in lines.iter().enumerate() {
                // Doc comments quote these shapes verbatim; they spawn nothing.
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with('*') {
                    continue;
                }
                if !constructs_claude(line) {
                    continue;
                }
                let (name, start, end) = enclosing_fn(&lines, idx);
                let body = lines[start..end].join("\n");

                claude_sites += 1;
                // `spawn_and_wait_with_doctor` is the shared funnel every
                // `ai_provider::claude_cli` variant hands its half-built
                // `Command` to, and it strips (`ai_provider/process.rs`). A
                // construction that goes there is covered even though the strip
                // is not lexically in this function.
                if !body.contains("strip_inherited_claude_markers()")
                    && !body.contains("spawn_and_wait_with_doctor(")
                {
                    offenders.push(format!("{}:{} (fn `{}`)", path.display(), idx + 1, name));
                }
            }
        }

        // Vacuity guard: if the matcher stops recognising ANY site it would
        // report PASS while asserting nothing.
        assert!(
            claude_sites >= 4,
            "the scan recognised only {claude_sites} Claude-bound spawn site(s) — it has almost \
             certainly stopped matching this codebase's shapes, so the assertion below is \
             meaningless. Fix the matcher before trusting a green run."
        );

        assert!(
            offenders.is_empty(),
            "these Claude-bound spawn sites do not call `strip_inherited_claude_markers()`, so \
             they hand this process's inherited topology markers to a `claude` that is not \
             actually a nested session:\n  {}\n\nIf a site is a deliberate exception (see the \
             shim), exempt it explicitly in this test with the reason.",
            offenders.join("\n  ")
        );
    }

    /// The helper is actually WIRED, not merely defined.
    ///
    /// Split out from the scans above because it answers a different question,
    /// and kept as an exact-ish floor: a generous floor lets someone delete the
    /// strip from several sites while the test still passes. Counts only real
    /// call lines — a doc comment mentioning the method must not inflate it,
    /// which a naive `text.matches()` over the whole file would allow.
    #[test]
    fn the_shared_strip_is_called_at_every_known_spawn_site() {
        let mut call_sites = 0usize;
        for path in source_files() {
            if is_this_module(&path) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("*") {
                    continue; // prose, not a call
                }
                if t.contains(".strip_inherited_claude_markers()") {
                    call_sites += 1;
                }
            }
        }
        assert!(
            call_sites >= 14,
            "expected the shared strip at every known spawn site, found {call_sites} call(s). \
             Removing a spawn site is fine — lower this floor deliberately and say which site \
             went away. Silently dropping below it means a site stopped stripping."
        );
    }
}
