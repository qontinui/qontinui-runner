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
//! When the screen shows no indicator, the account's own `settings.json`
//! `effortLevel` is the fallback ([`EffortSource::AccountDefault`]). When
//! neither answers, the effort is [`EffortSource::Unknown`] with no level: an
//! unread effort is never rendered as a guessed default.
//!
//! ## The baseline
//!
//! A row is `held` for a hand restore when its effort is ABOVE the effort a
//! session on that account gets anyway. That is the account's `effortLevel`
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
    /// The pane's own status row.
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

/// The session's effort: the screen first, then the account default, else
/// unknown.
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

/// Is `effort` strictly above `baseline`? `false` when either side is unknown
/// — an unread effort holds nothing.
pub fn is_above(effort: &Effort, baseline: &Baseline) -> bool {
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

    #[test]
    fn is_above_is_strict_and_unknown_holds_nothing() {
        let cli = baseline_from(None, Some("high"));
        assert!(is_above(&effort_from(Some("xhigh"), None), &cli));
        assert!(is_above(&effort_from(Some("max"), None), &cli));
        assert!(!is_above(&effort_from(Some("high"), None), &cli));
        assert!(!is_above(&effort_from(Some("low"), None), &cli));
        // Either side unknown: never held.
        assert!(!is_above(&Effort::unknown(), &cli));
        assert!(!is_above(
            &effort_from(Some("max"), None),
            &baseline_from(None, None)
        ));
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
