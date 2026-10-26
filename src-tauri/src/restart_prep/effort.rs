//! The effort a session runs at, and the baseline its `held` flag compares
//! against (plan `2026-10-21-operator-prepare-for-restart-and-progressive-restore`,
//! D5).
//!
//! ## Where the effort is read from
//!
//! Claude Code paints the session's effort on a status row just above its
//! input box, right-aligned, as `<glyph> <level> · /effort` — `● high · /effort`
//! in the 2.1.270 capture `terminal/graceful_exit.rs` keeps as a fixture,
//! `◐ medium · /effort` in the resumed-session first paint
//! `src/components/terminal/resumeVerification.test.ts` keeps. [`parse_screen`]
//! reads that row from the rendered VT grid — the same grid read
//! `terminal/usage_limit.rs` scans for limit messages, so a full-screen repaint
//! cannot hide it the way it hides text from a byte stream.
//!
//! **The screen row is transient on Claude Code 2.1.310**: the CLI paints it
//! when `/effort` runs and removes it on the next repaint, so an end-to-end
//! prepare read `effort: unknown` on every row. The DURABLE source is the
//! session's own transcript ([`transcript_effort_level`]): an `/effort` command
//! leaves a local-command record whose stdout reads
//! `Set effort level to <level> (…)`, in the session's JSONL under the account
//! it ran on — and `claude --resume` appends to that same file, so the record
//! survives a restore too. That is read FIRST ([`EffortSource::Transcript`]),
//! then the screen row, then the account's own `settings.json` `effortLevel`
//! ([`EffortSource::AccountDefault`]). When none answers, the effort is
//! [`EffortSource::Unknown`] with no level: an unread effort is never rendered
//! as a guessed default.
//!
//! Not read: the `--effort` flag on the live `claude` argv. Finding the pane's
//! `claude` and its argv is an async process-table walk the cohort's sync probe
//! does not have; the transcript covers every session whose effort was set
//! with `/effort`, including one this runner resumed with `--effort` (its
//! original record is still in the file).
//!
//! ## The baseline
//!
//! A row is `held` for a hand restore when an EXPLICITLY SET effort
//! ([`EffortSource::is_explicit`] — a transcript `/effort` record) is ABOVE the
//! effort a session on that account gets anyway. A level that may only be the
//! CLI's or the model's default — the screen row, which a fresh session paints
//! too, or the account default — never holds: Claude Code 2.1.310 starts a
//! Sonnet session at `xhigh` while [`CLI_DEFAULT_EFFORT`] is `high`, so a
//! default read off the screen would have held every Sonnet session. That is the account's `effortLevel`
//! when its `settings.json` carries one, else the CLI's own default as
//! observed ([`CLI_DEFAULT_EFFORT`]) — none of the twelve account
//! `settings.json` files on the vet box carried `effortLevel`, so a baseline
//! of "`effortLevel` or unknown" would never hold a session.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Every effort level `claude --effort` accepts, lowest first. Claude Code
/// 2.1.305 (vet, 2026-10-21) and 2.1.306 (implementation, 2026-10-21) both list
/// `--effort <level>` with exactly these five. The order IS the comparison
/// [`is_above`] makes.
pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The effort a freshly started session shows when nothing sets one.
///
/// **Observed, not assumed.** 2026-10-21 on merytshost: Claude Code 2.1.306
/// started in a PTY (120×40, `--safe-mode`, a trusted directory, no prompt
/// sent) under an account whose `settings.json` carries no `effortLevel`
/// painted `Opus 5.5 (1M context) with high effort · Claude Max` in its banner
/// and `● high · /effort` on the status row. The default can move with a CLI
/// release or a model change; re-observe it when either moves, and record the
/// version beside the value.
pub const CLI_DEFAULT_EFFORT: Option<&str> = Some("high");

/// The CLI version [`CLI_DEFAULT_EFFORT`] was observed on.
pub const CLI_DEFAULT_OBSERVED_ON: &str = "Claude Code 2.1.306 (2026-10-21)";

/// The row's trailing token: what makes a line the effort indicator.
const INDICATOR_SUFFIX: &str = "/effort";

/// Separators Claude Code draws between the level and `/effort`: the middle
/// dot, and the bullet operator other status rows use.
const SEPARATORS: &[char] = &['·', '∙'];

/// Where an effort reading came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortSource {
    /// An `/effort` command's record in the session's transcript: a level the
    /// operator (or the session) SET, durable across repaints and resumes.
    Transcript,
    /// The pane's own status row (transient on 2.1.310, and painted for a
    /// default level too — so not proof of an explicit choice).
    Screen,
    /// The account's `settings.json` `effortLevel`.
    AccountDefault,
    /// Neither answered. The level is absent, never guessed.
    Unknown,
}

/// A session's effort: `{level, source}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effort {
    /// One of [`EFFORT_LEVELS`]; `None` exactly when `source` is `unknown`.
    pub level: Option<String>,
    pub source: EffortSource,
}

impl EffortSource {
    /// Whether a level from this source was explicitly SET for the session
    /// (only the transcript's `/effort` record), rather than possibly being a
    /// CLI, model or account default. Only an explicit level can hold a row.
    pub fn is_explicit(self) -> bool {
        matches!(self, EffortSource::Transcript)
    }
}

impl Effort {
    /// The unread effort.
    pub fn unknown() -> Self {
        Self {
            level: None,
            source: EffortSource::Unknown,
        }
    }
}

/// Where a baseline came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// The account's `settings.json` `effortLevel`.
    Settings,
    /// [`CLI_DEFAULT_EFFORT`], the observed CLI default.
    CliDefault,
    /// Neither is known.
    Unknown,
}

/// The effort a session on this account gets without asking for one:
/// `{baseline, default_source}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub baseline: Option<String>,
    pub default_source: DefaultSource,
}

/// The position of `level` in [`EFFORT_LEVELS`], case-insensitively. `None`
/// for anything that is not a level.
pub fn rank(level: &str) -> Option<usize> {
    let lower = level.trim().to_ascii_lowercase();
    EFFORT_LEVELS.iter().position(|l| *l == lower)
}

/// The canonical spelling of `level`, when it is one.
fn canonical(level: &str) -> Option<&'static str> {
    rank(level).map(|i| EFFORT_LEVELS[i])
}

/// The level on one rendered row, when the row IS the effort indicator.
///
/// The row must END with `/effort` (after trimming whitespace and a box edge),
/// be preceded by a separator, and the word before the separator must be a
/// level. A half-rendered row (`● high · /eff`, `● hi`) is no indicator at
/// all, and an unrecognised word is no level — both read `None`.
fn parse_row(row: &str) -> Option<&'static str> {
    let row = row.trim().trim_end_matches('│').trim_end();
    let before = row.strip_suffix(INDICATOR_SUFFIX)?.trim_end();
    let before = before.strip_suffix(SEPARATORS)?.trim_end();
    let word = before.split_whitespace().next_back()?;
    canonical(word)
}

/// The effort level the rendered screen shows, if any.
///
/// Scans bottom-up and takes the LOWEST indicator row: the live status row sits
/// just above the input box, below any transcript text that happens to quote
/// one.
pub fn parse_screen(lines: &[String]) -> Option<&'static str> {
    lines.iter().rev().find_map(|l| parse_row(l))
}

/// The stdout Claude Code records for an `/effort` command (2.1.306–2.1.310,
/// observed in the fleet's transcripts): `Set effort level to max (this
/// session only): …` or `Set effort level to xhigh (saved as your default for
/// new sessions): …`.
const TRANSCRIPT_EFFORT_PREFIX: &str = "<local-command-stdout>Set effort level to ";

/// The level the LAST `/effort` record in a transcript set, from the JSONL's
/// text. Only a real record counts: a `type: "user"` line whose
/// `message.content` (a string, or a text part) STARTS with the local-command
/// stdout — so the phrase quoted inside a tool result, an assistant message or
/// a pasted log is never mistaken for one. A later record whose level is not
/// one of [`EFFORT_LEVELS`] (a reset, a value a newer CLI adds) clears the
/// earlier one: the session's effort is then UNKNOWN, never the stale level.
pub fn transcript_effort_level_in(jsonl: &str) -> Option<&'static str> {
    let mut level: Option<&'static str> = None;
    for line in jsonl.lines() {
        // Cheap pre-filter: a transcript can be tens of MB.
        if !line.contains("Set effort level to ") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let text = match content {
            serde_json::Value::String(s) => Some(s.as_str()),
            serde_json::Value::Array(parts) => parts.iter().find_map(|p| {
                (p.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .then(|| p.get("text").and_then(|t| t.as_str()))
                    .flatten()
            }),
            _ => None,
        };
        let Some(rest) = text.and_then(|t| t.strip_prefix(TRANSCRIPT_EFFORT_PREFIX)) else {
            continue;
        };
        let word: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        level = canonical(&word);
    }
    level
}

/// [`transcript_effort_level_in`] over the transcript file at `path`. An
/// unreadable file reads `None` (UNKNOWN).
pub fn transcript_effort_level(path: &Path) -> Option<&'static str> {
    let raw = std::fs::read_to_string(path).ok()?;
    transcript_effort_level_in(&raw)
}

/// `<config_dir>/settings.json` `effortLevel`, when it holds a level. An
/// unreadable file, an absent key or a value that is not a level all read
/// `None`.
pub fn settings_effort_level(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(config_dir.join("settings.json")).ok()?;
    settings_effort_level_in(&raw)
}

/// [`settings_effort_level`] over the file's text.
fn settings_effort_level_in(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let level = v.get("effortLevel")?.as_str()?;
    canonical(level).map(str::to_string)
}

/// The session's effort: its transcript's last `/effort` record first, then
/// the screen, then the account default, else unknown.
pub fn effort_from_sources(
    transcript_level: Option<&str>,
    screen_level: Option<&str>,
    settings_level: Option<&str>,
) -> Effort {
    if let Some(level) = transcript_level.and_then(canonical) {
        return Effort {
            level: Some(level.to_string()),
            source: EffortSource::Transcript,
        };
    }
    effort_from(screen_level, settings_level)
}

/// The session's effort with no transcript reading: the screen first, then
/// the account default, else unknown.
pub fn effort_from(screen_level: Option<&str>, settings_level: Option<&str>) -> Effort {
    if let Some(level) = screen_level.and_then(canonical) {
        return Effort {
            level: Some(level.to_string()),
            source: EffortSource::Screen,
        };
    }
    if let Some(level) = settings_level.and_then(canonical) {
        return Effort {
            level: Some(level.to_string()),
            source: EffortSource::AccountDefault,
        };
    }
    Effort::unknown()
}

/// The account's baseline: its `effortLevel`, else the observed CLI default
/// (`cli_default`, which production passes as [`CLI_DEFAULT_EFFORT`]), else
/// unknown.
pub fn baseline_from(settings_level: Option<&str>, cli_default: Option<&str>) -> Baseline {
    if let Some(level) = settings_level.and_then(canonical) {
        return Baseline {
            baseline: Some(level.to_string()),
            default_source: DefaultSource::Settings,
        };
    }
    if let Some(level) = cli_default.and_then(canonical) {
        return Baseline {
            baseline: Some(level.to_string()),
            default_source: DefaultSource::CliDefault,
        };
    }
    Baseline {
        baseline: None,
        default_source: DefaultSource::Unknown,
    }
}

/// Is an EXPLICITLY set `effort` strictly above `baseline`? `false` when either
/// side is unknown — an unread effort holds nothing — and `false` for a level
/// that may only be a default ([`EffortSource::is_explicit`]): a Sonnet
/// session painting its `xhigh` default must not be held against the `high`
/// CLI default.
pub fn is_above(effort: &Effort, baseline: &Baseline) -> bool {
    if !effort.source.is_explicit() {
        return false;
    }
    match (
        effort.level.as_deref().and_then(rank),
        baseline.baseline.as_deref().and_then(rank),
    ) {
        (Some(e), Some(b)) => e > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rs: &[&str]) -> Vec<String> {
        rs.iter().map(|s| s.to_string()).collect()
    }

    /// Rows 35-39 of the 120x40 Claude Code 2.1.270 capture
    /// `terminal/graceful_exit.rs` keeps, with the level swapped in.
    fn prompt_screen(indicator: &str) -> Vec<String> {
        vec![
            format!("{}{indicator}", " ".repeat(102)),
            "─".repeat(120),
            "❯\u{a0}Try \"write a test for <filepath>\"".to_string(),
            "─".repeat(120),
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents".to_string(),
        ]
    }

    #[test]
    fn every_level_parses_off_the_status_row() {
        for (glyph, level) in [
            ("○", "low"),
            ("◐", "medium"),
            ("●", "high"),
            ("◉", "xhigh"),
            ("◉", "max"),
        ] {
            let s = prompt_screen(&format!("{glyph} {level} · /effort"));
            assert_eq!(parse_screen(&s), Some(level), "level {level}");
        }
    }

    #[test]
    fn the_two_repo_fixtures_parse() {
        // terminal/graceful_exit.rs:695
        assert_eq!(
            parse_screen(&prompt_screen("● high · /effort")),
            Some("high")
        );
        // src/components/terminal/resumeVerification.test.ts:110, after the
        // VT parse (the `\x1b[28C` cursor move becomes leading spaces).
        let resumed = rows(&[
            "✻ Churned for 1m 31s · done 7:49 AM",
            "                            ◐ medium · /effort",
            "────────────────────────────────────────────────",
            "❯\u{a0}",
        ]);
        assert_eq!(parse_screen(&resumed), Some("medium"));
    }

    #[test]
    fn an_absent_indicator_is_none() {
        let s = rows(&[
            "● Done.",
            "─".repeat(40).as_str(),
            "❯ ",
            "─".repeat(40).as_str(),
        ]);
        assert_eq!(parse_screen(&s), None);
        assert_eq!(parse_screen(&[]), None);
    }

    #[test]
    fn a_half_rendered_row_is_none() {
        for half in [
            "● high · /eff",
            "● high ·",
            "● hi",
            "● high /effort",
            "● · /effort",
            "● hig · /effort",
            "● ultra · /effort",
        ] {
            assert_eq!(parse_screen(&prompt_screen(half)), None, "{half:?}");
        }
    }

    #[test]
    fn the_lowest_indicator_row_wins_over_a_quoted_one() {
        // Transcript text above quotes an indicator; the live row is lower.
        let mut s = prompt_screen("◐ medium · /effort");
        s.insert(0, "  the fixture reads `● max · /effort`".to_string());
        s.insert(0, "● max · /effort".to_string());
        assert_eq!(parse_screen(&s), Some("medium"));
    }

    #[test]
    fn settings_effort_level_reads_only_a_real_level() {
        assert_eq!(
            settings_effort_level_in(r#"{"effortLevel":"xhigh"}"#).as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            settings_effort_level_in(r#"{"effortLevel":"HIGH"}"#).as_deref(),
            Some("high")
        );
        assert_eq!(settings_effort_level_in(r#"{"effortLevel":"turbo"}"#), None);
        assert_eq!(settings_effort_level_in(r#"{"model":"opus"}"#), None);
        assert_eq!(settings_effort_level_in("not json"), None);
        assert_eq!(
            settings_effort_level(Path::new("Z:/no/such/config/dir")),
            None
        );
    }

    #[test]
    fn effort_prefers_the_screen_then_the_account_default_else_unknown() {
        let e = effort_from(Some("max"), Some("low"));
        assert_eq!(e.level.as_deref(), Some("max"));
        assert_eq!(e.source, EffortSource::Screen);
        let e = effort_from(None, Some("low"));
        assert_eq!(e.level.as_deref(), Some("low"));
        assert_eq!(e.source, EffortSource::AccountDefault);
        let e = effort_from(None, None);
        assert_eq!(e, Effort::unknown());
        assert_eq!(e.level, None);
    }

    #[test]
    fn the_baseline_is_settings_then_the_cli_default_else_unknown() {
        let b = baseline_from(Some("medium"), CLI_DEFAULT_EFFORT);
        assert_eq!(b.baseline.as_deref(), Some("medium"));
        assert_eq!(b.default_source, DefaultSource::Settings);
        let b = baseline_from(None, CLI_DEFAULT_EFFORT);
        assert_eq!(b.baseline.as_deref(), Some("high"));
        assert_eq!(b.default_source, DefaultSource::CliDefault);
        let b = baseline_from(None, None);
        assert_eq!(b.baseline, None);
        assert_eq!(b.default_source, DefaultSource::Unknown);
    }

    /// An explicit (transcript) level.
    fn set(level: &str) -> Effort {
        effort_from_sources(Some(level), None, None)
    }

    #[test]
    fn is_above_is_strict_and_unknown_holds_nothing() {
        let cli = baseline_from(None, Some("high"));
        assert!(is_above(&set("xhigh"), &cli));
        assert!(is_above(&set("max"), &cli));
        assert!(!is_above(&set("high"), &cli));
        assert!(!is_above(&set("low"), &cli));
        // Either side unknown: never held.
        assert!(!is_above(&Effort::unknown(), &cli));
        assert!(!is_above(&set("max"), &baseline_from(None, None)));
    }

    #[test]
    fn only_an_explicitly_set_level_holds() {
        let cli = baseline_from(None, CLI_DEFAULT_EFFORT);
        // A Sonnet session on 2.1.310 paints its `xhigh` DEFAULT on the status
        // row: a screen reading may be a default, so it never holds.
        assert!(!is_above(&effort_from(Some("xhigh"), None), &cli));
        assert!(!is_above(&effort_from(Some("max"), None), &cli));
        // An account default is the baseline itself, never above it.
        assert!(!is_above(
            &effort_from(None, Some("max")),
            &baseline_from(Some("max"), CLI_DEFAULT_EFFORT)
        ));
        // `/effort max` recorded in the transcript: held.
        assert!(is_above(&set("max"), &cli));
        assert!(EffortSource::Transcript.is_explicit());
        assert!(!EffortSource::Screen.is_explicit());
        assert!(!EffortSource::AccountDefault.is_explicit());
        assert!(!EffortSource::Unknown.is_explicit());
    }

    /// Real-shaped records, copied from Claude Code 2.1.306 transcripts on the
    /// fleet (`~/.claude-*/projects/<slug>/<session>.jsonl`, read-only): the
    /// caveat, the command and its stdout are three `type: "user"` lines.
    const EFFORT_RECORD: &str = r#"{"parentUuid":"6ef1","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-caveat>The command below was run directly in Claude Code, not sent to you as a request, and its output goes straight to the user. It's recorded here as context for later messages.</local-command-caveat>"},"isMeta":true,"uuid":"fb79","timestamp":"2026-10-21T22:14:59.055Z","sessionId":"1445af04","version":"2.1.306"}
{"parentUuid":"fb79","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args></command-args>"},"uuid":"b795","timestamp":"2026-10-21T22:14:59.055Z","sessionId":"1445af04","version":"2.1.306"}
{"parentUuid":"b795","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-stdout>Set effort level to xhigh (saved as your default for new sessions): Deeper reasoning than high, just below maximum (on supported models)</local-command-stdout>"},"uuid":"34b0","timestamp":"2026-10-21T22:14:59.055Z","sessionId":"1445af04","version":"2.1.306"}
{"type":"last-prompt","leafUuid":"34b0","sessionId":"1445af04"}"#;

    const MAX_RECORD: &str = r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Set effort level to max (this session only): Maximum capability with deepest reasoning. May use excessive tokens resulting in slower responses.</local-command-stdout>"},"uuid":"9a1c","timestamp":"2026-10-22T08:00:00.000Z","sessionId":"1445af04","version":"2.1.310"}"#;

    #[test]
    fn the_transcript_effort_record_parses() {
        assert_eq!(transcript_effort_level_in(EFFORT_RECORD), Some("xhigh"));
        assert_eq!(transcript_effort_level_in(MAX_RECORD), Some("max"));
        assert_eq!(transcript_effort_level_in(""), None);
    }

    #[test]
    fn the_last_effort_record_wins_and_an_unknown_one_clears_it() {
        let both = format!("{EFFORT_RECORD}\n{MAX_RECORD}");
        assert_eq!(transcript_effort_level_in(&both), Some("max"));
        let back = format!("{MAX_RECORD}\n{EFFORT_RECORD}");
        assert_eq!(transcript_effort_level_in(&back), Some("xhigh"));
        let reset = MAX_RECORD.replace("to max (", "to auto (");
        assert_eq!(
            transcript_effort_level_in(&format!("{MAX_RECORD}\n{reset}")),
            None,
            "a later level that is not one of ours is UNKNOWN, not the stale max"
        );
    }

    #[test]
    fn a_quoted_effort_line_is_not_a_record() {
        // The phrase inside an assistant message, a tool result, or mid-text.
        let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"<local-command-stdout>Set effort level to max (this session only)</local-command-stdout>"}]}}"#;
        let tool_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"<local-command-stdout>Set effort level to max (this session only)"}]}}"#;
        let quoted = r#"{"type":"user","message":{"role":"user","content":"the log said <local-command-stdout>Set effort level to max"}}"#;
        for not_a_record in [
            assistant,
            tool_result,
            quoted,
            "not json Set effort level to max",
        ] {
            assert_eq!(
                transcript_effort_level_in(not_a_record),
                None,
                "{not_a_record}"
            );
        }
        // A text-part array that IS the stdout counts.
        let parts = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<local-command-stdout>Set effort level to low (this session only): Quick</local-command-stdout>"}]}}"#;
        assert_eq!(transcript_effort_level_in(parts), Some("low"));
    }

    #[test]
    fn the_transcript_file_is_read_and_a_missing_one_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, MAX_RECORD).unwrap();
        assert_eq!(transcript_effort_level(&path), Some("max"));
        assert_eq!(
            transcript_effort_level(&dir.path().join("none.jsonl")),
            None
        );
    }

    #[test]
    fn the_transcript_comes_before_the_screen_and_the_account() {
        let e = effort_from_sources(Some("max"), Some("high"), Some("low"));
        assert_eq!(e.level.as_deref(), Some("max"));
        assert_eq!(e.source, EffortSource::Transcript);
        let e = effort_from_sources(None, Some("high"), Some("low"));
        assert_eq!(e.source, EffortSource::Screen);
        let e = effort_from_sources(Some("turbo"), None, Some("low"));
        assert_eq!(e.source, EffortSource::AccountDefault);
        assert_eq!(
            serde_json::to_value(EffortSource::Transcript).unwrap(),
            serde_json::json!("transcript")
        );
    }

    #[test]
    fn the_wire_spells_the_plan_vocabulary() {
        let e = serde_json::to_value(effort_from(None, Some("low"))).unwrap();
        assert_eq!(
            e,
            serde_json::json!({"level": "low", "source": "account_default"})
        );
        let b = serde_json::to_value(baseline_from(None, Some("high"))).unwrap();
        assert_eq!(
            b,
            serde_json::json!({"baseline": "high", "default_source": "cli_default"})
        );
    }
}
