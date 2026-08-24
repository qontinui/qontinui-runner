//! **Does this skill reach its own files by a path that survives
//! provisioning?** — the shape rule, applied to a skill bundle before the
//! runner writes it into a session's `.claude/skills/<name>/`.
//!
//! A skill that hardcodes the operator's `qontinui-claude-config` checkout
//! resolves on a fleet device, then fails at the step that matters: the agent
//! reads `SKILL.md`, runs the script by the path it was given, gets "no such
//! file", and improvises. That is the *exact* silent failure the fleet-served
//! skills plan exists to close, and it survives the fix unless something
//! refuses the unit.
//!
//! ## Why this is shape-based and not a substring list
//!
//! [`crate::fleet_commands`]'s `FORBIDDEN` list is a substring floor, and a
//! floor is all it is. Measured 2026-08-24 across the four real hardcode sites
//! in `coord-pr-label/SKILL.md`: widening that list to
//! `qontinui-claude-config/.claude` catches **one** of them. The other three
//! wore an elided spelling — `.../coord-pr-label/set-label.sh` — which expands
//! to the same config-repo path while containing neither
//! `qontinui-claude-config` nor `.claude`. A substring arm keyed on the pattern
//! anyone thinks of first passes them silently.
//!
//! So the rule is about *shape*: an invocation of a script the unit itself
//! ships is a violation whenever it is not spelled skill-dir-relative, however
//! the path is written.
//!
//! ## This is a port, not a second design
//!
//! `qontinui-claude-config/scripts/lint-command-frontmatter.py` check #22
//! already states this rule for the config repo's own tree. This module mirrors
//! its four arms, its literals, and its known-good / known-bad corpus, so a
//! skill the config repo's lint accepts is one this runner will provision and
//! vice versa. When one changes, change both — a divergence here does not fail
//! to compile, it produces a unit the config repo ships and the runner then
//! refuses (or the reverse).
//!
//! | Arm | Rejects |
//! |---|---|
//! | A | a reach into a `qontinui-claude-config` checkout's `.claude/` tree |
//! | B | a **rooted** path token naming a `.claude/skills/` or `.agents/skills/` tree |
//! | C | an interpreter invoking a script this unit ships, with a prefix that is not skill-dir-relative |
//! | D | a shipped script reaching a shipped sibling other than through a substitution derived from its own location |
//!
//! ## The one arm deliberately NOT ported
//!
//! Check #22 also fails a skill that ships scripts but whose `SKILL.md` never
//! invokes one canonically. That arm protects the *lint*: the Python walks
//! files on disk, so a skill that names no script gives arms C and D nothing to
//! match and passes vacuously. Here there is no vacuous pass to protect — arms
//! A–D run over every file of every unit unconditionally, and coverage is
//! structural rather than sampled. Porting it would only make the runner refuse
//! a unit whose script is invoked from a sibling script rather than from prose,
//! which is a documentation shortfall, not a path that breaks on provisioning.

use once_cell::sync::Lazy;
use qontinui_types::agent_text_units::AgentTextUnitFiles;
use regex::Regex;

/// Per-line opt-out for an audited residual, matching check #22's marker
/// exactly. Unused by the shipped corpus; present so the two guards stay
/// interchangeable.
pub const SKILL_SELF_PATH_MARKER: &str = "skill-self-path-ok";

/// The ONE canonical placeholder for "the directory this `SKILL.md` sits in".
/// Pinned as a literal rather than pattern-matched as "something skill
/// relative", because divergent phrasings across skills is exactly how this
/// regresses.
pub const SKILL_DIR_PLACEHOLDER: &str = "<path-to-this-skill-dir>";

/// [`SKILL_DIR_PLACEHOLDER`] as an invocation prefix. Kept as its own literal
/// (Rust cannot `concat!` a `const`); [`tests::the_two_placeholder_literals_agree`]
/// pins them together.
const SKILL_DIR_PREFIX: &str = "<path-to-this-skill-dir>/";

/// Prefixes an invocation may legitimately carry: nothing at all (a usage line,
/// or a caller that already `cd`'d), the explicit cwd-relative form, or the
/// canonical placeholder.
const OK_INVOKE_PREFIXES: &[&str] = &["", "./", SKILL_DIR_PREFIX];

/// Interpreters whose argument is a script path. Shared by arms C and D.
const INTERPRETERS: &str = "bash|sh|pwsh|powershell|python3|python|node";

/// Extensions that make a shipped file a *script* — the thing arms C and D are
/// about. Matched case-sensitively, as check #22 matches them.
const SCRIPT_SUFFIXES: &[&str] = &[".sh", ".ps1", ".py", ".mjs", ".cjs", ".js"];

/// Arm D accepts a sibling path only when its directory part was derived from
/// the running script's own location, which in every supported shell means it
/// carries a variable or a command substitution.
const SUBSTITUTION_SIGIL: char = '$';

/// Where a path token starts. Quotes, backticks and shell/markdown punctuation
/// all end the previous token; `/`, `\` and `.` obviously do not.
///
/// `<` and `>` are deliberately NOT breaks. Every placeholder in this corpus is
/// angle-bracketed, and breaking on them would reduce `<workspace-root>/…` and
/// `<session-workdir>/…` to the same prefix (`/`) — flagging the second, which
/// merely *describes* where a provisioned copy lands.
const TOKEN_BREAK_CHARS: &str = " \t'\"`(){}[]|&;,=*";

/// Arm A: a reach into a `qontinui-claude-config` checkout's `.claude/` tree,
/// in any separator spelling. `qontinui-claude-config/knowledge-base/…` and
/// `qontinui-claude-config/scripts/…` are deliberately NOT matched: the first
/// is a doc pointer and the second is where the non-bundled
/// `coord-acting-bearer.sh` helper genuinely lives.
static CONFIG_REPO_CLAUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"qontinui-claude-config[/\\]+\.claude\b").expect("arm A regex"));

/// Arm B: the skills tree named inside a path token. The token's FIRST
/// character decides rooted-ness, so a bare `.claude/skills/coord-revive` in an
/// orientation comment passes while `<workspace-root>/.claude/skills/…` does
/// not.
static SKILLS_TREE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\.(?:claude|agents)[/\\]+skills[/\\]").expect("arm B regex"));

/// The rooted spellings arm B objects to, anchored at the token start.
static ROOTED_TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"^(?:<workspace-root>",  // the cross-repo idiom — correct for OTHER repos
        r"|\.\.\.",               // an elided prefix, e.g. `.../.claude/skills/`
        r"|~",                    // a home-relative path
        r"|\$\{?[A-Za-z_]\w*\}?", // $QONTINUI_ROOT, ${ROOT}
        r"|%[A-Za-z_]\w*%",       // %QONTINUI_ROOT%
        r"|[A-Za-z]:",            // D:, C:
        r"|/)",                   // POSIX absolute, incl. /d/ and /mnt/d/
    ))
    .expect("arm B rooted-token regex")
});

/// One reason a bundle would not survive provisioning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfPathViolation {
    /// The `files` key the violation was found in.
    pub path: String,
    /// 1-based line number within that file.
    pub line: usize,
    /// Human-readable reason, phrased the way check #22 phrases it.
    pub reason: String,
}

impl std::fmt::Display for SelfPathViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.reason)
    }
}

/// Arms A and B on one line: the reason it fails, or `None`.
fn self_path_hit(line: &str) -> Option<String> {
    if CONFIG_REPO_CLAUDE_RE.is_match(line) {
        return Some(
            "reaches its own tree through a `qontinui-claude-config` checkout".to_string(),
        );
    }
    for m in SKILLS_TREE_RE.find_iter(line) {
        let prefix = token_prefix(line, m.start());
        if !prefix.is_empty() && ROOTED_TOKEN_RE.is_match(&prefix) {
            return Some(format!(
                "rooted path onto the skills tree (`{prefix}{}…`)",
                m.as_str()
            ));
        }
    }
    None
}

/// The path token `line[..start]` ends with — everything back to the nearest
/// [`TOKEN_BREAK_CHARS`] character.
fn token_prefix(line: &str, start: usize) -> String {
    let head = &line[..start];
    let mut taken: Vec<char> = head
        .chars()
        .rev()
        .take_while(|c| !TOKEN_BREAK_CHARS.contains(*c))
        .collect();
    taken.reverse();
    taken.into_iter().collect()
}

/// Every path prefix `line` puts in front of `script_name` when INVOKING it.
///
/// Prose that merely names the script (no interpreter token in front) yields
/// nothing — the arm is about how the agent is told to RUN the file.
fn skill_invocation_prefixes(line: &str, script_name: &str) -> Vec<String> {
    invocation_re(script_name)
        .captures_iter(line)
        .map(|c| c[1].to_string())
        .collect()
}

fn invocation_re(script_name: &str) -> Regex {
    Regex::new(&format!(
        r#"\b(?:{INTERPRETERS})\s+(?:(?:-{{1,2}}[A-Za-z][\w-]*)\s+)*['"]?([^\s'"`|;&]*){}\b"#,
        regex::escape(script_name)
    ))
    .expect("arm C regex")
}

/// Every path prefix `line` puts in front of `sibling` at a USE site.
///
/// A use site is an assignment RHS or an interpreter / `source` / `.`
/// argument. A comment or a doc line that merely names the sibling yields
/// nothing.
///
/// Check #22 spells the `.`-source arm with a lookbehind, which the `regex`
/// crate does not support; the equivalent here is an explicit
/// start-or-separator alternative, which admits and rejects the same lines.
fn sibling_use_prefixes(line: &str, sibling: &str) -> Vec<String> {
    sibling_use_re(sibling)
        .captures_iter(line)
        .map(|c| c[1].to_string())
        .collect()
}

fn sibling_use_re(sibling: &str) -> Regex {
    Regex::new(&format!(
        r#"(?:=|\b(?:{INTERPRETERS}|source)\s+|(?:^|[\s;&|(])\.\s+)['"]?([^\s'"`|;&]{{0,120}}?){}\b"#,
        regex::escape(sibling)
    ))
    .expect("arm D regex")
}

/// The scripts a unit *ships*: top-level `files` keys with a script suffix.
///
/// Top-level only, mirroring check #22's `skill_dir.iterdir()`. A script in a
/// subdirectory is still scanned as a file (arms A and B), it is simply not one
/// of the names arms C and D look for an invocation of.
fn shipped_scripts(files: &AgentTextUnitFiles) -> Vec<&str> {
    let mut scripts: Vec<&str> = files
        .keys()
        .filter(|k| !k.contains('/'))
        .filter(|k| SCRIPT_SUFFIXES.iter().any(|s| k.ends_with(s)))
        .map(String::as_str)
        .collect();
    scripts.sort_unstable();
    scripts
}

/// Every way this bundle would fail to reach its own files once provisioned.
///
/// Empty means the unit is safe to write into `<workdir>/.claude/skills/<name>/`
/// as far as self-reference goes. Pure: no filesystem, no network, so the
/// caller can run it on fetched content before anything touches disk.
pub fn skill_self_path_violations(files: &AgentTextUnitFiles) -> Vec<SelfPathViolation> {
    let scripts = shipped_scripts(files);
    let mut violations = Vec::new();

    for (path, text) in files {
        for (lineno, line) in text.lines().enumerate() {
            let lineno = lineno + 1;
            if line.contains(SKILL_SELF_PATH_MARKER) {
                continue;
            }
            // Arms A and B.
            if let Some(reason) = self_path_hit(line) {
                violations.push(SelfPathViolation {
                    path: path.clone(),
                    line: lineno,
                    reason,
                });
            }
            // Arm C.
            for script in &scripts {
                for prefix in skill_invocation_prefixes(line, script) {
                    if OK_INVOKE_PREFIXES.contains(&prefix.as_str()) {
                        continue;
                    }
                    violations.push(SelfPathViolation {
                        path: path.clone(),
                        line: lineno,
                        reason: format!(
                            "invokes its own `{script}` as `{prefix}{script}` — spell it \
                             `{SKILL_DIR_PREFIX}{script}`"
                        ),
                    });
                }
            }
        }

        // Arm D: a shipped script USING a shipped sibling must reach it through
        // a variable or substitution derived from its own location. Scoped to
        // the scripts, not to SKILL.md, which is prose and has no runtime
        // location.
        if !scripts.contains(&path.as_str()) {
            continue;
        }
        for sibling in scripts.iter().filter(|s| **s != path.as_str()) {
            for (lineno, line) in text.lines().enumerate() {
                let lineno = lineno + 1;
                if line.contains(SKILL_SELF_PATH_MARKER) {
                    continue;
                }
                for prefix in sibling_use_prefixes(line, sibling) {
                    if prefix.contains(SUBSTITUTION_SIGIL) {
                        continue;
                    }
                    violations.push(SelfPathViolation {
                        path: path.clone(),
                        line: lineno,
                        reason: format!(
                            "uses shipped sibling `{sibling}` as `{prefix}{sibling}` — resolve it \
                             from `$(dirname \"${{BASH_SOURCE[0]}}\")` (or $PSScriptRoot / \
                             __file__), never from the cwd or a workspace path"
                        ),
                    });
                }
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(entries: &[(&str, &str)]) -> AgentTextUnitFiles {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn the_two_placeholder_literals_agree() {
        assert_eq!(SKILL_DIR_PREFIX, format!("{SKILL_DIR_PLACEHOLDER}/"));
        assert!(OK_INVOKE_PREFIXES.contains(&SKILL_DIR_PREFIX));
    }

    /// Check #22's known-BAD corpus, verbatim. A pattern that stops matching
    /// turns this guard into a silent no-op that reads as a clean tree — and
    /// this guard's whole subject is a failure that already reads as clean.
    #[test]
    fn arms_a_and_b_flag_every_known_bad_sample() {
        for sample in [
            r"  bash <workspace-root>/qontinui-claude-config/.claude/skills/x/set-label.sh \",
            r"bash ~/qontinui-root/.claude/skills/pr-status/pr-status.sh --mine",
            r#"bash "$QONTINUI_ROOT/.claude/skills/coord-revive/coord-revive.sh""#,
            r"run C:/qontinui-root/.agents/skills/coord/coord-read.ps1",
            r"see /mnt/d/qontinui-root/.claude/skills/preflight/SKILL.md",
        ] {
            assert!(
                self_path_hit(sample).is_some(),
                "known-BAD sample not flagged: {sample}"
            );
        }
    }

    /// Check #22's known-GOOD corpus, verbatim. Each line is one a real skill
    /// ships today, and flagging any of them would make the runner refuse a
    /// working unit.
    #[test]
    fn arms_a_and_b_pass_every_known_good_sample() {
        for sample in [
            r"bash <path-to-this-skill-dir>/coord-revive.sh",
            r"  bash <path-to-this-skill-dir>/set-label.sh \",
            r"# $HERE = .claude/skills/coord-revive, so the repo's scripts/ dir is three",
            r"# $HERE = .claude/skills/pr-status, so the repo root (and its scripts/) is",
            r#"  AB="${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-acting-bearer.sh""#,
            r"`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`",
            r"- `<workspace-root>/qontinui-dev-notes/docs/coord/pr-merge-labels.md` —",
            r"being copied into `<session-workdir>/.claude/skills/coord-pr-label/`, on devices",
        ] {
            assert_eq!(
                self_path_hit(sample),
                None,
                "known-GOOD sample flagged: {sample}"
            );
        }
    }

    /// Arm C's prefix reader, on the shapes that actually shipped — including
    /// the `.../` elision, which names no skills tree at all and is therefore
    /// invisible to arms A and B. Three of the four broken sites in
    /// `coord-pr-label/SKILL.md` were spelled that way, which is precisely why
    /// a substring list is not the gate.
    #[test]
    fn arm_c_reads_and_classifies_invocation_prefixes() {
        let probe = format!("  bash {SKILL_DIR_PREFIX}set-label.sh \\");
        assert_eq!(
            skill_invocation_prefixes(&probe, "set-label.sh"),
            vec![SKILL_DIR_PREFIX.to_string()]
        );

        let elided = skill_invocation_prefixes("bash .../x/set-label.sh", "set-label.sh");
        assert_eq!(elided, vec![".../x/".to_string()]);
        assert!(
            !OK_INVOKE_PREFIXES.contains(&elided[0].as_str()),
            "the elided `.../` prefix must be rejected"
        );

        for ok in OK_INVOKE_PREFIXES {
            let probe_ok = format!("bash {ok}coord-revive.sh");
            assert_eq!(
                skill_invocation_prefixes(&probe_ok, "coord-revive.sh"),
                vec![(*ok).to_string()],
                "an accepted prefix must round-trip: {ok:?}"
            );
        }

        assert!(
            skill_invocation_prefixes("`set-label.sh` pre-flights the ceiling", "set-label.sh")
                .is_empty(),
            "prose naming a script must not read as an invocation"
        );
    }

    /// Arm D. The known-BAD cases include the exact mutation that defeated an
    /// earlier whole-file `${BASH_SOURCE[0]}`-presence test, so this pins the
    /// fix rather than the intent.
    #[test]
    fn arm_d_classifies_sibling_use_sites() {
        for (label, line, want_flagged) in [
            (
                "a cwd-relative sibling assignment",
                r#"SCRIPT="set-label.sh""#,
                true,
            ),
            (
                "a literal-directory sibling",
                r#"SCRIPT="../coord-pr-label/set-label.sh""#,
                true,
            ),
            (
                "an interpreter on a bare sibling",
                "bash set-label.sh --dry-run",
                true,
            ),
            (
                "the shipped self-located form",
                r#"SCRIPT="$HERE/set-label.sh""#,
                false,
            ),
            (
                "the inline substitution form",
                r#"SCRIPT="$(dirname "${BASH_SOURCE[0]}")/set-label.sh""#,
                false,
            ),
            (
                "prose naming a sibling",
                "# mirrors set-label.sh's validator",
                false,
            ),
        ] {
            let prefixes = sibling_use_prefixes(line, "set-label.sh");
            let flagged = prefixes.iter().any(|p| !p.contains(SUBSTITUTION_SIGIL));
            assert_eq!(flagged, want_flagged, "arm D: {label}: {line}");
        }
    }

    /// End to end over a bundle: the four hardcode spellings that shipped in
    /// `coord-pr-label/SKILL.md` are all refused, and the corrected bundle is
    /// clean.
    #[test]
    fn the_real_regression_bundle_is_refused_and_its_fix_accepted() {
        let broken = files(&[
            (
                "SKILL.md",
                "# coord-pr-label\n\
                 bash <workspace-root>/qontinui-claude-config/.claude/skills/coord-pr-label/set-label.sh\n\
                 bash .../coord-pr-label/set-label.sh\n",
            ),
            ("set-label.sh", "#!/usr/bin/env bash\nset -euo pipefail\n"),
        ]);
        let hits = skill_self_path_violations(&broken);
        assert!(
            hits.len() >= 3,
            "every hardcode spelling must be refused, got {hits:?}"
        );
        assert!(
            hits.iter()
                .any(|v| v.reason.contains("qontinui-claude-config")),
            "arm A must fire: {hits:?}"
        );
        assert!(
            hits.iter().any(|v| v.reason.contains("spell it")),
            "arm C must fire on the elided spelling — the case a substring list misses: {hits:?}"
        );

        let fixed = files(&[
            (
                "SKILL.md",
                "# coord-pr-label\n\
                 It sits next to this SKILL.md; run it as\n\
                 bash <path-to-this-skill-dir>/set-label.sh\n",
            ),
            (
                "set-label.sh",
                "#!/usr/bin/env bash\nHERE=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")\" && pwd)\"\n",
            ),
        ]);
        assert_eq!(skill_self_path_violations(&fixed), Vec::new());
    }

    /// A script reaching a shipped sibling by a bare name is refused even
    /// though no `.claude/skills/` string appears anywhere — arm D is the arm
    /// no substring list can express.
    #[test]
    fn arm_d_fires_inside_a_bundle_with_no_skills_path_anywhere() {
        let bundle = files(&[
            ("SKILL.md", "# x\nbash <path-to-this-skill-dir>/run.sh\n"),
            ("run.sh", "#!/usr/bin/env bash\nSELFTEST=\"selftest.sh\"\n"),
            ("selftest.sh", "#!/usr/bin/env bash\necho ok\n"),
        ]);
        let hits = skill_self_path_violations(&bundle);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].path, "run.sh");
        assert!(hits[0].reason.contains("shipped sibling `selftest.sh`"));
    }

    /// The audited-residual marker suppresses a line, and nothing else.
    #[test]
    fn the_marker_suppresses_only_its_own_line() {
        let bundle = files(&[(
            "SKILL.md",
            "bash ~/qontinui-root/.claude/skills/x/y.sh  # skill-self-path-ok\n\
             bash ~/qontinui-root/.claude/skills/x/y.sh\n",
        )]);
        let hits = skill_self_path_violations(&bundle);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 2);
    }

    /// A bundle with no scripts and no rooted paths is clean — the common case
    /// (6 of the 9 shipped skills carry only `SKILL.md`).
    #[test]
    fn a_prose_only_bundle_is_clean() {
        let bundle = files(&[(
            "SKILL.md",
            "---\nname: preflight\n---\n\nRun the repo's own checks before pushing.\n",
        )]);
        assert_eq!(skill_self_path_violations(&bundle), Vec::new());
    }
}
