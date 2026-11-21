//! Classify a headless child's exit as an account-limit hit, and cool the
//! account it ran under until that limit actually resets.
//!
//! Plan `2026-11-13-devops-scheduled-remote-agent-relaunches-into-an-exhausted-account`
//! (Design 1-2). One helper for every headless path that reads a child's own
//! output after it exits: the scheduler's `RemoteAgent` child today, and the
//! `agent_runtime` headless children of plan
//! `2026-11-05-an-ai-provider-call-runs-under-its-own-units-account-and-cools-the-account-it-ran-under`
//! Phase 3. That plan and this one name the same two entry points,
//! [`classify_limit_tail`] and [`cool_until_reset`].
//!
//! ## A match is a hint
//!
//! The pattern set is the terminal watcher's
//! ([`crate::terminal::usage_limit::window_indicates_usage_limit`]), extended
//! rather than duplicated. A match says the child's output CONTAINS a limit
//! phrase. How long to keep the account out of rotation is decided by
//! [`cooling_for`], which prefers the usage probe's exact reset epoch over the
//! banner's wall-clock text, and the banner over a fixed floor.
//!
//! ## Why no clamp
//!
//! The terminal migration path clamps its probe-derived cooldown to 300 s-6 h.
//! A weekly limit resets days away, so a 6 h ceiling would hand an exhausted
//! account back to the picker long before it can serve a request. The cooldown
//! here runs to the reset the probe or the banner states, unclamped.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::commands::ai_settings::AccountUsageInfo;

/// The cooldown an account gets when its limit is confirmed but no reset time
/// is known: no exhausted probe window and no parseable banner. One hour, the
/// trust window of the probe's `exhausted` flag (`EXHAUSTION_STALE_TTL`). The
/// ONE shared floor: the terminal migration path uses it too.
pub const EXHAUSTED_ACCOUNT_COOLDOWN: Duration = Duration::from_secs(3600);

/// Utilization at or above which a probe window counts as exhausted. Mirrors
/// `commands::ai_settings::EXHAUSTION_UTILIZATION`, which `probe_result_exhausted`
/// reads for the picker's `exhausted` flag.
const WINDOW_EXHAUSTED: f64 = crate::commands::ai_settings::EXHAUSTION_UTILIZATION;

/// How much of a child's log end [`classify_limit_tail`]'s callers read. A
/// limit answer is the LAST thing a `claude -p` child prints before it exits,
/// so a short tail holds it. Reading less of a long transcript also keeps a
/// limit phrase the session merely QUOTED earlier out of the match.
pub const LIMIT_TAIL_BYTES: u64 = 8 * 1024;

/// A child's output carried an account-limit answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitHit {
    /// The matched pattern, from `USAGE_LIMIT_PATTERNS`.
    pub pattern: &'static str,
    /// The reset the banner states (`resets Nov 18, 10pm (Europe/Berlin)`),
    /// when one is present and parses. `None` is UNKNOWN, never "now".
    pub banner_reset: Option<DateTime<Utc>>,
}

/// The generic catch-all phrases of `USAGE_LIMIT_PATTERNS`. They are there so
/// the terminal watcher never MISSES a limit (a false hint there costs one
/// probe), but in a headless child's stderr they can just as well be an
/// unrelated failure's text — so on their own they confirm nothing here.
///
/// `"spend limit"` is here too: it is a bare two-word phrase an unrelated
/// error or a report can carry, and the usage probe cannot see a spend cap, so
/// only the banner's own stated `resets …` confirms it.
const CATCH_ALL_PATTERNS: &[&str] = &[
    "hit your limit",
    "limit reached",
    "out of usage",
    "spend limit",
];

/// The 5-hour-window banners. For these the probe's 5 h reset is
/// authoritative; for every other banner a LATER stated reset outlasts the
/// probe window [`cooling_for`] picked (a spend cap or a per-model cap does not
/// clear when an unrelated 5 h window does).
const FIVE_HOUR_PATTERNS: &[&str] = &["hit your 5-hour limit", "5-hour limit reached"];

impl LimitHit {
    /// A SPECIFIC limit banner (weekly, 5-hour, session, spend, usage, a
    /// per-model cap) rather than one of [`CATCH_ALL_PATTERNS`].
    pub fn is_specific(&self) -> bool {
        !CATCH_ALL_PATTERNS.contains(&self.pattern)
    }

    fn is_five_hour(&self) -> bool {
        FIVE_HOUR_PATTERNS.contains(&self.pattern)
    }
}

/// Classify a log tail. `None` when it carries no limit phrase.
pub fn classify_limit_tail(tail: &str) -> Option<LimitHit> {
    classify_limit_tail_at(tail, Utc::now())
}

/// [`classify_limit_tail`] against an explicit `now`, which anchors a banner
/// that names no year (or no date).
///
/// Only the LAST [`LIMIT_BANNER_LINES`] non-empty lines are read. A child
/// refused by a limit ends on the limit answer; a child that failed for some
/// other reason may still have QUOTED a limit banner earlier in its output (a
/// dev-ops steward reporting on exactly this incident does), and reading the
/// whole tail would cool a healthy account and relaunch a run that already
/// did its work.
pub fn classify_limit_tail_at(tail: &str, now: DateTime<Utc>) -> Option<LimitHit> {
    let ending = final_lines(tail, LIMIT_BANNER_LINES);
    let pattern = crate::terminal::usage_limit::window_indicates_usage_limit(
        &ending,
        &qontinui_runner_lib::cli_profile::claude::PROFILE.usage_limit_phrases,
    )?;
    Some(LimitHit {
        pattern,
        banner_reset: parse_banner_reset(&ending, now),
    })
}

/// How many trailing non-empty lines of a child's log may carry its limit
/// answer (the banner, plus a line or two of `[stderr]` framing after it).
pub const LIMIT_BANNER_LINES: usize = 4;

/// The last `n` non-empty lines of `text`, in order, newline-joined.
fn final_lines(text: &str, n: usize) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect();
    lines.reverse();
    lines.join("\n")
}

/// Which source decided a cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetSource {
    /// The usage probe's seven-day window is exhausted: its `resets_at`.
    SevenDayProbe,
    /// The usage probe's 5-hour window is exhausted: its `session_resets_at`.
    FiveHourProbe,
    /// No exhausted probe window (a spend cap, a per-model cap): the banner.
    Banner,
    /// Neither: [`EXHAUSTED_ACCOUNT_COOLDOWN`].
    Floor,
}

/// The cooldown decided for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cooling {
    pub duration: Duration,
    /// When the account comes back. For [`ResetSource::Floor`] it is
    /// `now + floor`, which is NOT a known reset — render it as UNKNOWN.
    pub until: DateTime<Utc>,
    pub source: ResetSource,
}

impl Cooling {
    /// The reset this cooldown KNOWS — `None` for the floor, whose `until` is
    /// a guess.
    pub fn known_reset(&self) -> Option<DateTime<Utc>> {
        (self.source != ResetSource::Floor).then_some(self.until)
    }
}

/// What [`cool_until_reset`] did, plus the probe it read, so a caller deciding
/// what to do next (relaunch elsewhere, or wait for the soonest reset) reads
/// the same numbers rather than probing again.
#[derive(Debug, Clone)]
pub struct CoolOutcome {
    /// `None` when nothing confirmed the hit — a catch-all phrase with no
    /// exhausted probe window and no banner reset. Nothing was cooled then,
    /// and the caller must treat the exit as a plain failure.
    pub cooling: Option<Cooling>,
    pub probe: Vec<AccountUsageInfo>,
}

/// The reset of whichever probe window of `info` is exhausted, seven-day
/// first. `None` when no window is exhausted, or its reset is unknown or past.
pub fn exhausted_window_reset(
    info: &AccountUsageInfo,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, ResetSource)> {
    // A failed probe reports `utilization: 1.0` with an `error`: that is
    // UNKNOWN, not an exhausted window.
    if info.error.is_some() {
        return None;
    }
    let future = |epoch: Option<u64>| {
        epoch
            .and_then(|e| Utc.timestamp_opt(e as i64, 0).single())
            .filter(|t| *t > now)
    };
    if info.utilization >= WINDOW_EXHAUSTED {
        if let Some(t) = future(info.resets_at) {
            return Some((t, ResetSource::SevenDayProbe));
        }
    }
    if info
        .session_utilization
        .is_some_and(|u| u >= WINDOW_EXHAUSTED)
    {
        if let Some(t) = future(info.session_resets_at) {
            return Some((t, ResetSource::FiveHourProbe));
        }
    }
    None
}

/// Whether a successful probe reads any window of `info` as exhausted, with
/// or without a known reset. A probe ERROR is not exhausted: it is UNKNOWN.
pub fn probe_reads_exhausted(info: &AccountUsageInfo) -> bool {
    if info.error.is_some() {
        return false;
    }
    info.utilization >= WINDOW_EXHAUSTED
        || info
            .session_utilization
            .is_some_and(|u| u >= WINDOW_EXHAUSTED)
        || info.status.as_deref().is_some_and(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("reject") || s.contains("block") || s.contains("exceed")
        })
}

/// Pure cooldown decision for `dir`: the probe's exhausted window first, the
/// banner's reset second, the floor last — or `None`, nothing to cool.
///
/// - A weekly banner stating a LATER reset than the probe window picked wins:
///   the account is not back before both have cleared.
/// - The floor applies only to a [`LimitHit::is_specific`] banner. A
///   catch-all phrase that neither the probe nor a banner reset confirms is
///   `None`: cooling on it would let one unrelated failure's stderr walk the
///   whole pool into cooldown.
pub fn cooling_for(
    dir: &str,
    hit: &LimitHit,
    probe: &[AccountUsageInfo],
    now: DateTime<Utc>,
) -> Option<Cooling> {
    let from_probe = probe
        .iter()
        .find(|i| i.config_dir == dir)
        .and_then(|i| exhausted_window_reset(i, now));
    let banner = hit.banner_reset.filter(|t| *t > now);
    let (until, source) = match (from_probe, banner) {
        (Some((p, _)), Some(b)) if !hit.is_five_hour() && b > p => (b, ResetSource::Banner),
        (Some((t, src)), _) => (t, src),
        (None, Some(t)) => (t, ResetSource::Banner),
        (None, None) if hit.is_specific() => {
            let floor = chrono::Duration::from_std(EXHAUSTED_ACCOUNT_COOLDOWN)
                .expect("an hour fits a chrono duration");
            (now + floor, ResetSource::Floor)
        }
        (None, None) => return None,
    };
    let duration = (until - now).to_std().unwrap_or(EXHAUSTED_ACCOUNT_COOLDOWN);
    Some(Cooling {
        duration,
        until,
        source,
    })
}

/// Re-probe every account, decide `dir`'s cooldown with [`cooling_for`], and
/// mark it via [`super::mark_account_rate_limited_at_least`] — the cooldown
/// the picker already treats as an exclusion — never SHORTENING a longer
/// cooldown the account already carries. An unconfirmed hit cools nothing.
pub async fn cool_until_reset(dir: &str, hit: &LimitHit) -> CoolOutcome {
    let probe = crate::commands::ai_settings::refresh_account_usage_snapshot().await;
    let cooling = cooling_for(dir, hit, &probe, Utc::now());
    match &cooling {
        Some(c) => {
            tracing::info!(
                account = dir,
                pattern = hit.pattern,
                source = ?c.source,
                until = %c.until,
                "account limit hit: cooling the account it ran under until its reset"
            );
            super::mark_account_rate_limited_at_least(dir, c.duration);
        }
        None => tracing::warn!(
            account = dir,
            pattern = hit.pattern,
            "a catch-all limit phrase that neither the usage probe nor a banner reset \
             confirms: not cooling the account"
        ),
    }
    CoolOutcome { cooling, probe }
}

/// The reset a limit banner states, e.g. `resets Nov 18, 10pm (Europe/Berlin)`,
/// `resets 3am`, `resets Dec 1, 12:30am (UTC)`. The LAST such phrase in the
/// text wins (the newest answer). A banner without a zone is in the machine's
/// local zone, which is the zone the CLI renders in. A date without a year is
/// the next occurrence; a time without a date is the next occurrence of that
/// time. `None` when nothing parses — never a guess.
pub fn parse_banner_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\bresets\s+(?:at\s+)?(?:(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2}),?\s+(?:at\s+)?)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)(?:\s*\(([^)\s]+)\))?",
        )
        .expect("banner reset regex compiles")
    });
    let caps = re.captures_iter(text).last()?;
    let month = caps.get(1).and_then(|m| month_number(m.as_str()));
    let day: Option<u32> = caps.get(2).and_then(|d| d.as_str().parse().ok());
    let hour12: u32 = caps.get(3)?.as_str().parse().ok()?;
    let minute: u32 = caps.get(4).map_or(Some(0), |m| m.as_str().parse().ok())?;
    if !(1..=12).contains(&hour12) || minute > 59 {
        return None;
    }
    let pm = caps.get(5)?.as_str().eq_ignore_ascii_case("pm");
    let hour = match (hour12, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, false) => h,
        (h, true) => h + 12,
    };
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    match caps.get(6) {
        Some(tz) => {
            let tz = chrono_tz::Tz::from_str(tz.as_str()).ok()?;
            resolve_reset(&tz, month, day, time, now)
        }
        None => resolve_reset(&Local, month, day, time, now),
    }
}

fn month_number(name: &str) -> Option<u32> {
    let m = name.get(..3)?.to_ascii_lowercase();
    [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|x| *x == m)
    .map(|i| i as u32 + 1)
}

/// Anchor a banner's wall-clock reset to an instant in `tz`.
fn resolve_reset<Tz: TimeZone>(
    tz: &Tz,
    month: Option<u32>,
    day: Option<u32>,
    time: NaiveTime,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(tz);
    let at = |date: NaiveDate| {
        tz.from_local_datetime(&date.and_time(time))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    };
    let resolved = match (month, day) {
        (Some(m), Some(d)) => {
            // This year's date, or next year's when this year's is already
            // more than a day behind (a banner rendered just before a reset
            // that has since passed still reads as this year).
            let this_year = at(NaiveDate::from_ymd_opt(local_now.year(), m, d)?)?;
            if this_year + chrono::Duration::days(1) < now {
                at(NaiveDate::from_ymd_opt(local_now.year() + 1, m, d)?)
            } else {
                Some(this_year)
            }
        }
        _ => {
            let today = at(local_now.date_naive())?;
            if today > now {
                Some(today)
            } else {
                at(local_now.date_naive().succ_opt()?)
            }
        }
    }?;
    // No usage window (5 h, weekly, monthly spend) resets further out than a
    // month or so. A banner date that resolves beyond that is a STALE banner
    // whose date rolled into next year, and with no clamp downstream it would
    // cool the account for most of a year: UNKNOWN instead.
    (resolved - now <= chrono::Duration::days(MAX_BANNER_RESET_DAYS)).then_some(resolved)
}

/// The furthest a stated banner reset may lie ahead of `now` and be believed.
const MAX_BANNER_RESET_DAYS: i64 = 62;

/// `when`, rendered for a history row: RFC 3339 UTC, or `UNKNOWN`.
pub fn render_reset(when: Option<DateTime<Utc>>) -> String {
    when.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        // 2026-11-13T17:27:10Z — nawa's third failure in the plan's evidence.
        Utc.with_ymd_and_hms(2026, 11, 13, 17, 27, 10).unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn weekly_banner_classifies_with_its_zoned_reset() {
        let tail = "some earlier output\nYou've hit your weekly limit \u{b7} resets Nov 18, 10pm (Europe/Berlin)\n";
        let hit = classify_limit_tail_at(tail, now()).expect("weekly banner is a hit");
        assert_eq!(hit.pattern, "hit your weekly limit");
        // 22:00 CET (UTC+1 in November) = 21:00Z.
        assert_eq!(hit.banner_reset, Some(utc(2026, 11, 18, 21, 0)));
    }

    #[test]
    fn five_hour_banner_with_time_only_is_the_next_occurrence() {
        let tail = "You've hit your 5-hour limit \u{b7} resets 3am (UTC)";
        let hit = classify_limit_tail_at(tail, now()).expect("5h banner is a hit");
        assert_eq!(hit.pattern, "hit your 5-hour limit");
        // 03:00Z today is past at 17:27Z, so tomorrow's.
        assert_eq!(hit.banner_reset, Some(utc(2026, 11, 14, 3, 0)));
    }

    #[test]
    fn spend_limit_banner_classifies() {
        let tail = "You've hit your org's monthly spend limit \u{b7} resets Dec 1, 12:30am (UTC)";
        let hit = classify_limit_tail_at(tail, now()).expect("spend banner is a hit");
        assert_eq!(hit.pattern, "spend limit");
        assert_eq!(hit.banner_reset, Some(utc(2026, 12, 1, 0, 30)));
    }

    /// The three wordings a scheduled RemoteAgent child on nilan actually ended
    /// its log on, copied byte-for-byte from `~/.qontinui/scheduler-runs/*.log`
    /// (2026-11-16): a dated weekly banner, a time-only weekly banner, and the
    /// org spend-cap banner whose only stated reset is the WEEKLY one. Each is
    /// the whole log, as it is for a child refused at its first turn.
    #[test]
    fn the_observed_scheduler_run_wordings_all_classify_with_a_reset() {
        let dated = "You've hit your weekly limit \u{b7} resets Nov 18, 10pm (Europe/Berlin)\n";
        let hit = classify_limit_tail_at(dated, now()).expect("dated weekly banner");
        assert_eq!(hit.pattern, "hit your weekly limit");
        assert!(hit.is_specific());
        assert_eq!(hit.banner_reset, Some(utc(2026, 11, 18, 21, 0)));

        let time_only = "You've hit your weekly limit \u{b7} resets 1pm (Europe/Berlin)\n";
        let hit = classify_limit_tail_at(time_only, now()).expect("time-only weekly banner");
        assert_eq!(hit.pattern, "hit your weekly limit");
        // 13:00 CET = 12:00Z, already past at 17:27Z: tomorrow's.
        assert_eq!(hit.banner_reset, Some(utc(2026, 11, 14, 12, 0)));

        let spend = "You've hit your org's monthly spend limit \u{b7} ask your admin to raise it \
                     at https://claude.ai/admin-settings/usage \u{b7} your weekly limit resets \
                     Nov 18, 6pm (Europe/Berlin)\n";
        let hit = classify_limit_tail_at(spend, now()).expect("spend-cap banner");
        assert_eq!(hit.pattern, "spend limit");
        // A catch-all that its stated reset confirms: it cools, not ignored.
        assert_eq!(hit.banner_reset, Some(utc(2026, 11, 18, 17, 0)));
        assert!(cooling_for("/a", &hit, &[], now()).is_some());
    }

    #[test]
    fn a_banner_quoted_earlier_in_the_output_is_not_a_hit() {
        // A steward whose report quotes the incident it investigated, then
        // fails for an unrelated reason: not a limit hit.
        let tail = "## Evidence\n\
                    session 5b0863ca: \"You've hit your weekly limit \u{b7} resets Nov 18, 10pm (Europe/Berlin)\"\n\
                    step 2 ok\nstep 3 ok\nstep 4 ok\n\n\
                    error: coord write refused (401)\nexiting\n";
        assert_eq!(classify_limit_tail_at(tail, now()), None);
        // The same banner as the run's final answer is.
        let tail = "[stderr] starting\nYou've hit your weekly limit \u{b7} resets Nov 18, 10pm (Europe/Berlin)\n\n";
        assert!(classify_limit_tail_at(tail, now()).is_some());
    }

    #[test]
    fn a_stale_banner_date_is_unknown_not_a_year_away() {
        // now() is Nov 13: "Nov 11" would roll to 2027-11-11, ~363 days out.
        assert_eq!(parse_banner_reset("resets Nov 11, 10pm (UTC)", now()), None);
    }

    #[test]
    fn a_spend_cap_banner_outlasts_an_unrelated_exhausted_five_hour_window() {
        let five_hour_reset = utc(2026, 11, 13, 20, 0);
        let probe = vec![info(
            "/a",
            0.3,
            None,
            Some(1.0),
            Some(five_hour_reset.timestamp() as u64),
        )];
        let banner = utc(2026, 12, 1, 0, 0);
        let spend = catch_all("spend limit", Some(banner));
        let c = cooling_for("/a", &spend, &probe, now()).unwrap();
        assert_eq!((c.until, c.source), (banner, ResetSource::Banner));
    }

    #[test]
    fn a_spend_limit_phrase_with_no_stated_reset_cools_nothing() {
        let quiet = vec![info("/a", 0.2, None, Some(0.1), None)];
        assert_eq!(
            cooling_for("/a", &catch_all("spend limit", None), &quiet, now()),
            None
        );
    }

    #[test]
    fn a_banner_date_already_past_rolls_into_next_year() {
        let at = parse_banner_reset("resets Jan 2, 9pm (UTC)", now());
        assert_eq!(at, Some(utc(2027, 1, 2, 21, 0)));
    }

    #[test]
    fn plain_failure_text_is_not_a_hit_and_an_unparseable_reset_is_unknown() {
        assert_eq!(
            classify_limit_tail_at("error: cargo build failed\nexit 1", now()),
            None
        );
        let hit = classify_limit_tail_at("Claude usage limit reached", now()).unwrap();
        assert_eq!(hit.banner_reset, None);
        assert_eq!(render_reset(hit.banner_reset), "UNKNOWN");
        assert_eq!(
            parse_banner_reset("resets soon (Not/AZone)", now()),
            None,
            "an unknown zone is UNKNOWN, not a guess"
        );
    }

    fn info(
        dir: &str,
        util7: f64,
        reset7: Option<u64>,
        util5: Option<f64>,
        reset5: Option<u64>,
    ) -> AccountUsageInfo {
        AccountUsageInfo {
            config_dir: dir.to_string(),
            utilization: util7,
            resets_at: reset7,
            session_utilization: util5,
            session_resets_at: reset5,
            ..Default::default()
        }
    }

    fn hit(banner: Option<DateTime<Utc>>) -> LimitHit {
        LimitHit {
            pattern: "hit your weekly limit",
            banner_reset: banner,
        }
    }

    #[test]
    fn a_seven_day_exhausted_probe_cools_until_its_reset_unclamped() {
        let reset = utc(2026, 11, 18, 21, 0);
        let probe = vec![info(
            "/a",
            1.0,
            Some(reset.timestamp() as u64),
            Some(0.0),
            None,
        )];
        let c = cooling_for("/a", &hit(Some(utc(2026, 11, 14, 0, 0))), &probe, now())
            .expect("a specific banner always cools");
        assert_eq!(c.source, ResetSource::SevenDayProbe);
        assert_eq!(c.until, reset);
        // Days, not the terminal path's 6 h ceiling.
        assert!(c.duration > Duration::from_secs(4 * 24 * 3600));
        assert_eq!(c.known_reset(), Some(reset));
    }

    #[test]
    fn a_five_hour_exhausted_probe_cools_until_the_session_reset() {
        let reset = utc(2026, 11, 13, 20, 0);
        let probe = vec![info(
            "/a",
            0.4,
            Some(utc(2026, 11, 18, 0, 0).timestamp() as u64),
            Some(1.0),
            Some(reset.timestamp() as u64),
        )];
        let c =
            cooling_for("/a", &hit(None), &probe, now()).expect("a specific banner always cools");
        assert_eq!(c.source, ResetSource::FiveHourProbe);
        assert_eq!(c.until, reset);
    }

    #[test]
    fn no_exhausted_window_falls_to_the_banner_then_the_floor() {
        let probe = vec![info("/a", 0.2, None, Some(0.1), None)];
        let banner = utc(2026, 12, 1, 0, 0);
        let c = cooling_for("/a", &hit(Some(banner)), &probe, now())
            .expect("a specific banner always cools");
        assert_eq!(c.source, ResetSource::Banner);
        assert_eq!(c.until, banner);

        let c =
            cooling_for("/a", &hit(None), &probe, now()).expect("a specific banner always cools");
        assert_eq!(c.source, ResetSource::Floor);
        assert_eq!(c.duration, EXHAUSTED_ACCOUNT_COOLDOWN);
        assert_eq!(c.known_reset(), None);
    }

    #[test]
    fn the_probe_of_another_account_is_not_read() {
        let probe = vec![info(
            "/other",
            1.0,
            Some(utc(2026, 11, 20, 0, 0).timestamp() as u64),
            None,
            None,
        )];
        let c =
            cooling_for("/a", &hit(None), &probe, now()).expect("a specific banner always cools");
        assert_eq!(c.source, ResetSource::Floor);
    }

    fn catch_all(pattern: &'static str, banner: Option<DateTime<Utc>>) -> LimitHit {
        LimitHit {
            pattern,
            banner_reset: banner,
        }
    }

    /// A1: a catch-all phrase the probe does not confirm and that states no
    /// reset cools NOTHING — the floor is for a specific banner only.
    #[test]
    fn an_unconfirmed_catch_all_phrase_is_not_cooled() {
        let quiet = vec![info("/a", 0.2, None, Some(0.1), None)];
        for p in ["limit reached", "out of usage", "hit your limit"] {
            assert!(!catch_all(p, None).is_specific(), "{p}");
            assert_eq!(
                cooling_for("/a", &catch_all(p, None), &quiet, now()),
                None,
                "{p}"
            );
        }
        // The same phrase IS cooled when the probe confirms it...
        let reset = utc(2026, 11, 18, 21, 0);
        let full = vec![info("/a", 1.0, Some(reset.timestamp() as u64), None, None)];
        assert_eq!(
            cooling_for("/a", &catch_all("limit reached", None), &full, now()).map(|c| c.until),
            Some(reset)
        );
        // ...or states its own reset.
        let banner = utc(2026, 11, 14, 3, 0);
        assert_eq!(
            cooling_for(
                "/a",
                &catch_all("limit reached", Some(banner)),
                &quiet,
                now()
            )
            .map(|c| c.source),
            Some(ResetSource::Banner)
        );
        // Specific banners keep the floor.
        for p in [
            "hit your weekly limit",
            "hit your 5-hour limit",
            "hit your session limit",
        ] {
            assert_eq!(
                cooling_for("/a", &catch_all(p, None), &quiet, now()).map(|c| c.source),
                Some(ResetSource::Floor),
                "{p}"
            );
        }
    }

    /// A9: a banner stating a LATER reset than the probe window picked wins;
    /// a 5-hour banner does not override the probe.
    #[test]
    fn a_later_weekly_banner_reset_outlasts_the_probe_window() {
        let probe_reset = utc(2026, 11, 13, 20, 0);
        let probe = vec![info(
            "/a",
            0.5,
            None,
            Some(1.0),
            Some(probe_reset.timestamp() as u64),
        )];
        let banner = utc(2026, 11, 18, 21, 0);
        let c = cooling_for("/a", &hit(Some(banner)), &probe, now()).unwrap();
        assert_eq!((c.until, c.source), (banner, ResetSource::Banner));
        assert_eq!(c.known_reset(), Some(banner));

        let five_hour = catch_all("hit your 5-hour limit", Some(banner));
        let c = cooling_for("/a", &five_hour, &probe, now()).unwrap();
        assert_eq!(
            (c.until, c.source),
            (probe_reset, ResetSource::FiveHourProbe)
        );
    }

    /// A5 (classifier side): a failed probe reads `utilization: 1.0` with an
    /// error; that is not an exhausted window.
    #[test]
    fn a_probe_error_is_not_an_exhausted_window() {
        let mut errored = info(
            "/a",
            1.0,
            Some(utc(2026, 11, 18, 0, 0).timestamp() as u64),
            None,
            None,
        );
        errored.error = Some("HTTP 401".into());
        assert_eq!(exhausted_window_reset(&errored, now()), None);
        assert!(!probe_reads_exhausted(&errored));
        assert_eq!(
            cooling_for("/a", &catch_all("limit reached", None), &[errored], now()),
            None
        );
    }

    /// A4: marking never SHORTENS a longer cooldown the account carries.
    #[test]
    fn a_new_cooldown_never_shortens_a_longer_one() {
        let dir = format!("/tmp/limit-hit-test-{}", uuid::Uuid::new_v4());
        let week = Duration::from_secs(5 * 24 * 3600);
        assert_eq!(
            super::super::mark_account_rate_limited_at_least(&dir, week),
            week
        );
        let kept =
            super::super::mark_account_rate_limited_at_least(&dir, EXHAUSTED_ACCOUNT_COOLDOWN);
        assert!(kept > Duration::from_secs(4 * 24 * 3600), "{kept:?}");
        let remaining = super::super::time_until_cooled_down(&dir).unwrap();
        assert!(
            remaining > Duration::from_secs(4 * 24 * 3600),
            "{remaining:?}"
        );
        // A LONGER one does replace a shorter one.
        let dir2 = format!("/tmp/limit-hit-test-{}", uuid::Uuid::new_v4());
        super::super::mark_account_rate_limited_at_least(&dir2, EXHAUSTED_ACCOUNT_COOLDOWN);
        super::super::mark_account_rate_limited_at_least(&dir2, week);
        assert!(
            super::super::time_until_cooled_down(&dir2).unwrap()
                > Duration::from_secs(4 * 24 * 3600)
        );
    }
}
