//! The runner's LOCAL `osHealth` verdict (plan
//! `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`,
//! Phase 5, D4/D5/D6).
//!
//! Coord grades S7, S8 and S9 from the published samples; an end user has no
//! coord console, so the runner grades the same three axes itself, from the
//! `os-health` cell, against the SAME numbers: the tenant controls the
//! fleet-policy poller caches ([`set_fleet_controls`]), falling back field by
//! field to compiled defaults equal to coord's ([`Thresholds::DEFAULT`]).
//!
//! | Axis | Rule | Warn | Breach |
//! |---|---|---|---|
//! | S7 | Token objects | ≥ 250,000 | ≥ 1,000,000 |
//! | S7 | Token slope (same boot, ≤ 3 h, ≥ 1 h after boot) | ≥ 10,000 / h | — |
//! | S8 | paged + nonpaged pool (Windows only in v1) | ≥ 8 GiB | ≥ 16 GiB |
//! | S8 | nonpaged pool (Windows only in v1) | ≥ 4 GiB | — |
//! | S9 | spawn probe p50 | ≥ 250 ms | ≥ 1,000 ms |
//! | S9 | failures with zero successes | — | always (`spawn_failed`) |
//!
//! ## UNKNOWN vs NOT APPLICABLE (D6)
//!
//! An APPLICABLE input that is `None` grades `unknown` — never `ok`. An axis
//! whose instrument does not exist on this OS (S7 on Linux; S8 on Linux in v1;
//! everything on an unsupported OS) is SKIPPED: reported `not_applicable` and
//! never counted as unknown, or every Linux runner would read `unknown`
//! forever. The S7 slope is the one input whose UNKNOWN is NOT an unknown
//! verdict: D4 says it "cannot fire" until it is computable, so in the first
//! hour after boot (or before the runner has watched for long enough) it simply
//! contributes nothing.
//!
//! ## Combining
//!
//! `breach` if any rule breaches (a breach is certain whatever else is
//! unknown); else `unknown` if any applicable input is unknown (a warn beside
//! an unknown could be a breach); else `warn` if any rule warns; else `ok`.
//! A snapshot older than [`STALE_AFTER_SECS`] is `unknown` outright — a dead
//! collector thread must not keep serving its last verdict as current. (The
//! same bound stops the resource sample republishing it; one constant, so the
//! local verdict and the published readings go UNKNOWN together.)
//!
//! The verdict does NOT feed `/health` `derived_status` (D5): that says whether
//! THIS RUNNER is healthy, and a runner restart cannot fix a kernel leak.

use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{json, Value};

use super::{is_stale, OsHealthSnapshot, OsPlatform, STALE_AFTER_SECS};

const GIB: i64 = 1024 * 1024 * 1024;

/// The S7 slope window (D4).
pub(crate) const SLOPE_WINDOW: ChronoDuration = ChronoDuration::hours(3);
/// The S7 slope ignores the first hour after boot (D4).
pub(crate) const SLOPE_MIN_SINCE_BOOT: ChronoDuration = ChronoDuration::hours(1);
/// The shortest span the runner will difference over. Not a plan number: a
/// LOCAL guard so a runner that started 2 h after boot does not extrapolate an
/// hourly slope from two readings 30 s apart. Coord differences over its own
/// stored rows and needs no such guard.
pub(crate) const SLOPE_MIN_SPAN: ChronoDuration = ChronoDuration::minutes(15);
/// Ring capacity: 3 h of 30 s ticks, plus headroom.
const HISTORY_CAP: usize = 400;

/// The eight D4 controls as coord's `controls` object carries them. `None`
/// means the tenant set nothing (or coord has not answered) and the compiled
/// default applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OsHealthControls {
    pub(crate) kernel_token_objects_warn: Option<i64>,
    pub(crate) kernel_token_objects_breach: Option<i64>,
    pub(crate) kernel_token_slope_warn_per_hour: Option<i64>,
    pub(crate) kernel_pool_warn_bytes: Option<i64>,
    pub(crate) kernel_pool_breach_bytes: Option<i64>,
    pub(crate) kernel_nonpaged_warn_bytes: Option<i64>,
    pub(crate) spawn_p50_warn_ms: Option<i64>,
    pub(crate) spawn_p50_breach_ms: Option<i64>,
}

/// The effective thresholds the verdict grades against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Thresholds {
    pub(crate) token_warn: i64,
    pub(crate) token_breach: i64,
    pub(crate) token_slope_warn_per_hour: i64,
    pub(crate) pool_warn_bytes: i64,
    pub(crate) pool_breach_bytes: i64,
    pub(crate) nonpaged_warn_bytes: i64,
    pub(crate) spawn_p50_warn_ms: i64,
    pub(crate) spawn_p50_breach_ms: i64,
}

impl Thresholds {
    /// Equal to coord's compiled defaults (plan D4).
    pub(crate) const DEFAULT: Self = Self {
        token_warn: 250_000,
        token_breach: 1_000_000,
        token_slope_warn_per_hour: 10_000,
        pool_warn_bytes: 8 * GIB,
        pool_breach_bytes: 16 * GIB,
        nonpaged_warn_bytes: 4 * GIB,
        spawn_p50_warn_ms: 250,
        spawn_p50_breach_ms: 1_000,
    };

    /// The defaults with every control the tenant set laid over them. A
    /// negative control is impossible (coord's `validate` refuses it) and is
    /// treated as unset rather than as a threshold that always fires.
    pub(crate) fn with_controls(c: &OsHealthControls) -> Self {
        let d = Self::DEFAULT;
        let pick = |v: Option<i64>, default: i64| v.filter(|n| *n >= 0).unwrap_or(default);
        Self {
            token_warn: pick(c.kernel_token_objects_warn, d.token_warn),
            token_breach: pick(c.kernel_token_objects_breach, d.token_breach),
            token_slope_warn_per_hour: pick(
                c.kernel_token_slope_warn_per_hour,
                d.token_slope_warn_per_hour,
            ),
            pool_warn_bytes: pick(c.kernel_pool_warn_bytes, d.pool_warn_bytes),
            pool_breach_bytes: pick(c.kernel_pool_breach_bytes, d.pool_breach_bytes),
            nonpaged_warn_bytes: pick(c.kernel_nonpaged_warn_bytes, d.nonpaged_warn_bytes),
            spawn_p50_warn_ms: pick(c.spawn_p50_warn_ms, d.spawn_p50_warn_ms),
            spawn_p50_breach_ms: pick(c.spawn_p50_breach_ms, d.spawn_p50_breach_ms),
        }
    }
}

static FLEET_CONTROLS: RwLock<OsHealthControls> = RwLock::new(OsHealthControls {
    kernel_token_objects_warn: None,
    kernel_token_objects_breach: None,
    kernel_token_slope_warn_per_hour: None,
    kernel_pool_warn_bytes: None,
    kernel_pool_breach_bytes: None,
    kernel_nonpaged_warn_bytes: None,
    spawn_p50_warn_ms: None,
    spawn_p50_breach_ms: None,
});

/// Replace the cached tenant controls. Only the fleet-policy poller calls this
/// (and tests).
pub(crate) fn set_fleet_controls(c: OsHealthControls) {
    *FLEET_CONTROLS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = c;
}

/// The cached tenant controls; all `None` until coord answers.
pub(crate) fn fleet_controls() -> OsHealthControls {
    *FLEET_CONTROLS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One rule's or one axis's grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Band {
    Ok,
    Warn,
    Unknown,
    Breach,
}

impl Band {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Unknown => "unknown",
            Self::Breach => "breach",
        }
    }
}

/// An axis's grade, or the fact that it does not apply here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AxisGrade {
    Graded(Band),
    NotApplicable,
}

impl AxisGrade {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Graded(b) => b.as_str(),
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// One rule that did not grade `ok`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reason {
    /// `S7` | `S8` | `S9`, or `all` for staleness.
    pub(crate) axis: &'static str,
    /// `token_objects` | `token_slope` | `pool_total` | `pool_nonpaged` |
    /// `spawn_p50` | `spawn_failed` | `stale` | `no_snapshot`.
    pub(crate) rule: &'static str,
    pub(crate) band: Band,
    /// The reading graded (for the slope, tokens per hour, rounded).
    pub(crate) value: Option<i64>,
    /// The threshold crossed: the breach one for a breach, else the warn one.
    pub(crate) threshold: Option<i64>,
    pub(crate) message: String,
}

impl Reason {
    fn to_json(&self) -> Value {
        json!({
            "axis": self.axis,
            "rule": self.rule,
            "band": self.band.as_str(),
            "value": self.value,
            "threshold": self.threshold,
            "message": self.message,
        })
    }
}

/// The overall verdict and why.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OsVerdict {
    pub(crate) verdict: Band,
    pub(crate) s7: AxisGrade,
    pub(crate) s8: AxisGrade,
    pub(crate) s9: AxisGrade,
    /// Every rule that graded warn, breach or unknown.
    pub(crate) reasons: Vec<Reason>,
    /// The S7 slope used, tokens per hour, when it was computable.
    pub(crate) token_slope_per_hour: Option<i64>,
    pub(crate) thresholds: Thresholds,
}

impl OsVerdict {
    /// The `/health` `osHealth` fields this verdict adds.
    pub(crate) fn json_fields(&self) -> Value {
        json!({
            "verdict": self.verdict.as_str(),
            "reasons": self.reasons.iter().map(Reason::to_json).collect::<Vec<_>>(),
            "axes": {
                "S7": self.s7.as_str(),
                "S8": self.s8.as_str(),
                "S9": self.s9.as_str(),
            },
            "tokenSlopePerHour": self.token_slope_per_hour,
            "thresholds": {
                "tokenObjectsWarn": self.thresholds.token_warn,
                "tokenObjectsBreach": self.thresholds.token_breach,
                "tokenSlopeWarnPerHour": self.thresholds.token_slope_warn_per_hour,
                "poolWarnBytes": self.thresholds.pool_warn_bytes,
                "poolBreachBytes": self.thresholds.pool_breach_bytes,
                "nonpagedWarnBytes": self.thresholds.nonpaged_warn_bytes,
                "spawnP50WarnMs": self.thresholds.spawn_p50_warn_ms,
                "spawnP50BreachMs": self.thresholds.spawn_p50_breach_ms,
            },
        })
    }
}

/// The S7 slope, or why it cannot be computed (and therefore cannot fire).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Slope {
    PerHour(f64),
    Unknown(&'static str),
}

/// One remembered Token reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenSample {
    pub(crate) at: DateTime<Utc>,
    pub(crate) tokens: i64,
    pub(crate) booted_at: DateTime<Utc>,
}

/// The small local ring the S7 slope differences over.
#[derive(Debug, Clone, Default)]
pub(crate) struct TokenHistory {
    samples: VecDeque<TokenSample>,
}

impl TokenHistory {
    /// Remember `snap`'s Token reading. A snapshot without a Token count or a
    /// boot instant contributes nothing; a re-publish of the same tick (the
    /// collector publishes again after a probe) is not a second sample.
    pub(crate) fn record(&mut self, snap: &OsHealthSnapshot) {
        let (Some(tokens), Some(booted_at)) = (snap.kernel_token_objects, snap.os_booted_at) else {
            return;
        };
        let s = TokenSample {
            at: snap.read_at,
            tokens,
            booted_at,
        };
        if let Some(last) = self.samples.back_mut() {
            if last.at == s.at {
                *last = s;
                return;
            }
        }
        self.samples.push_back(s);
        while self.samples.len() > HISTORY_CAP {
            self.samples.pop_front();
        }
        // Nothing older than the window is ever read again.
        while let Some(front) = self.samples.front() {
            if s.at - front.at > SLOPE_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// The slope over the samples that share the LATEST boot, within the last
    /// [`SLOPE_WINDOW`], counting only samples at least [`SLOPE_MIN_SINCE_BOOT`]
    /// after that boot. Never differences across a reboot.
    pub(crate) fn slope(&self) -> Slope {
        let Some(latest) = self.samples.back().copied() else {
            return Slope::Unknown("no Token readings remembered yet");
        };
        let settled = latest.booted_at + SLOPE_MIN_SINCE_BOOT;
        if latest.at < settled {
            return Slope::Unknown("less than 1 h since boot");
        }
        let window_start = latest.at - SLOPE_WINDOW;
        let Some(oldest) = self
            .samples
            .iter()
            .find(|s| s.booted_at == latest.booted_at && s.at >= window_start && s.at >= settled)
            .copied()
        else {
            return Slope::Unknown("no reading in the window");
        };
        let span = latest.at - oldest.at;
        if span < SLOPE_MIN_SPAN {
            return Slope::Unknown("less than 15 min of readings since the boot settled");
        }
        #[allow(clippy::cast_precision_loss)]
        let hours = span.num_milliseconds() as f64 / 3_600_000.0;
        #[allow(clippy::cast_precision_loss)]
        let delta = (latest.tokens - oldest.tokens) as f64;
        Slope::PerHour(delta / hours)
    }
}

static HISTORY: Mutex<TokenHistory> = Mutex::new(TokenHistory {
    samples: VecDeque::new(),
});

/// Remember a stored snapshot for the slope. Called by the cell's writer.
pub(crate) fn record_history(snap: &OsHealthSnapshot) {
    HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .record(snap);
}

/// The slope over the process-global history.
pub(crate) fn current_slope() -> Slope {
    HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .slope()
}

/// Reset the process-global history. Tests only.
#[cfg(test)]
pub(crate) fn clear_history_for_tests() {
    *HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = TokenHistory::default();
}

/// Grade one value against a warn and an optional breach threshold.
fn band_of(v: i64, warn: i64, breach: Option<i64>) -> Band {
    match breach {
        Some(b) if v >= b => Band::Breach,
        _ if v >= warn => Band::Warn,
        _ => Band::Ok,
    }
}

fn millions(n: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n >= 1_000_000 {
        format!("{:.1}M", f / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", f / 1_000.0)
    } else {
        n.to_string()
    }
}

fn gib(n: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64 / GIB as f64;
    format!("{f:.1} GiB")
}

/// Collects the rules into axis grades and reasons.
struct Grader {
    reasons: Vec<Reason>,
}

impl Grader {
    /// Grade one rule; returns its band and records a reason unless `ok`.
    #[allow(clippy::too_many_arguments)]
    fn rule(
        &mut self,
        axis: &'static str,
        rule: &'static str,
        value: Option<i64>,
        warn: i64,
        breach: Option<i64>,
        what: &str,
        fmt: fn(i64) -> String,
    ) -> Band {
        let Some(v) = value else {
            self.reasons.push(Reason {
                axis,
                rule,
                band: Band::Unknown,
                value: None,
                threshold: None,
                message: format!("{what}: no reading (UNKNOWN)"),
            });
            return Band::Unknown;
        };
        let band = band_of(v, warn, breach);
        if band != Band::Ok {
            let threshold = if band == Band::Breach {
                breach
            } else {
                Some(warn)
            };
            self.reasons.push(Reason {
                axis,
                rule,
                band,
                value: Some(v),
                threshold,
                message: format!(
                    "{what} {} (≥ {} {})",
                    fmt(v),
                    threshold.map(fmt).unwrap_or_default(),
                    band.as_str()
                ),
            });
        }
        band
    }
}

/// Grade `snap` (the PURE core). `slope` is the S7 slope from the history;
/// `now` is the clock the staleness check reads.
pub(crate) fn grade(
    platform: OsPlatform,
    snap: Option<&OsHealthSnapshot>,
    slope: Slope,
    thresholds: Thresholds,
    now: DateTime<Utc>,
) -> OsVerdict {
    let t = thresholds;
    let s7_applies = platform.has_kernel_objects();
    // S8 grades `windows:` sources only in v1 (D2: a Linux reading is
    // reporting-only), which on this runner is the Windows platform.
    let s8_applies = platform == OsPlatform::Windows;
    let s9_applies = platform.has_spawn_probe();
    let na = |applies: bool| {
        if applies {
            AxisGrade::Graded(Band::Unknown)
        } else {
            AxisGrade::NotApplicable
        }
    };
    let any_applies = s7_applies || s8_applies || s9_applies;

    let Some(s) = snap else {
        let reasons = if any_applies {
            vec![Reason {
                axis: "all",
                rule: "no_snapshot",
                band: Band::Unknown,
                value: None,
                threshold: None,
                message: "no os-health tick has completed yet (UNKNOWN)".into(),
            }]
        } else {
            Vec::new()
        };
        return OsVerdict {
            verdict: if any_applies { Band::Unknown } else { Band::Ok },
            s7: na(s7_applies),
            s8: na(s8_applies),
            s9: na(s9_applies),
            reasons,
            token_slope_per_hour: None,
            thresholds: t,
        };
    };

    if any_applies && is_stale(s, now) {
        return OsVerdict {
            verdict: Band::Unknown,
            s7: na(s7_applies),
            s8: na(s8_applies),
            s9: na(s9_applies),
            reasons: vec![Reason {
                axis: "all",
                rule: "stale",
                band: Band::Unknown,
                value: Some((now - s.read_at).num_seconds()),
                threshold: Some(STALE_AFTER_SECS),
                message: format!(
                    "the last os-health reading is {} s old — the collector is not ticking (UNKNOWN)",
                    (now - s.read_at).num_seconds()
                ),
            }],
            token_slope_per_hour: None,
            thresholds: t,
        };
    }

    let mut g = Grader {
        reasons: Vec::new(),
    };
    let mut slope_per_hour = None;

    let s7 = if s7_applies {
        let level = g.rule(
            "S7",
            "token_objects",
            s.kernel_token_objects,
            t.token_warn,
            Some(t.token_breach),
            "kernel token objects",
            millions,
        );
        // The slope cannot fire while it is UNKNOWN (D4) — and its UNKNOWN is
        // not an unknown verdict, or the first hour after every boot would be.
        let slope_band = match slope {
            Slope::PerHour(per_hour) => {
                #[allow(clippy::cast_possible_truncation)]
                let rounded = per_hour.round() as i64;
                slope_per_hour = Some(rounded);
                g.rule(
                    "S7",
                    "token_slope",
                    Some(rounded),
                    t.token_slope_warn_per_hour,
                    None,
                    "kernel token objects growing per hour",
                    millions,
                )
            }
            Slope::Unknown(_) => Band::Ok,
        };
        AxisGrade::Graded(level.max(slope_band))
    } else {
        AxisGrade::NotApplicable
    };

    let s8 = if s8_applies {
        let total = g.rule(
            "S8",
            "pool_total",
            s.kernel_pool_bytes(),
            t.pool_warn_bytes,
            Some(t.pool_breach_bytes),
            "kernel pool (paged + nonpaged)",
            gib,
        );
        let nonpaged = g.rule(
            "S8",
            "pool_nonpaged",
            s.kernel_nonpaged_bytes,
            t.nonpaged_warn_bytes,
            None,
            "nonpaged kernel pool",
            gib,
        );
        AxisGrade::Graded(total.max(nonpaged))
    } else {
        AxisGrade::NotApplicable
    };

    let s9 = if s9_applies {
        let band = match s.spawn_probe {
            None => {
                g.reasons.push(Reason {
                    axis: "S9",
                    rule: "spawn_p50",
                    band: Band::Unknown,
                    value: None,
                    threshold: None,
                    message: "process-creation probe: no probe has run yet (UNKNOWN)".into(),
                });
                Band::Unknown
            }
            // A failed probe is a measurement, not an unknown (D3).
            Some(p) if p.count > 0 && p.successes() == 0 => {
                g.reasons.push(Reason {
                    axis: "S9",
                    rule: "spawn_failed",
                    band: Band::Breach,
                    value: Some(i64::from(p.count)),
                    threshold: None,
                    message: format!(
                        "process creation failed on every probe in the window ({} timed out, {} \
                         could not start{})",
                        p.timeouts,
                        p.errors,
                        p.error_code
                            .map(|c| format!(", os error {c}"))
                            .unwrap_or_default()
                    ),
                });
                Band::Breach
            }
            Some(p) => g.rule(
                "S9",
                "spawn_p50",
                p.p50_ms.map(i64::from),
                t.spawn_p50_warn_ms,
                Some(t.spawn_p50_breach_ms),
                "process creation p50",
                |ms| format!("{ms} ms"),
            ),
        };
        AxisGrade::Graded(band)
    } else {
        AxisGrade::NotApplicable
    };

    let verdict = [s7, s8, s9]
        .iter()
        .filter_map(|a| match a {
            AxisGrade::Graded(b) => Some(*b),
            AxisGrade::NotApplicable => None,
        })
        .max()
        .unwrap_or(Band::Ok);

    OsVerdict {
        verdict,
        s7,
        s8,
        s9,
        reasons: g.reasons,
        token_slope_per_hour: slope_per_hour,
        thresholds: t,
    }
}

/// The verdict over the live cell, history and cached controls, now.
pub(crate) fn current_verdict(platform: OsPlatform, snap: Option<&OsHealthSnapshot>) -> OsVerdict {
    current_verdict_at(platform, snap, Utc::now())
}

/// [`current_verdict`] at an explicit clock, so `/health` grades and renders
/// `ageSecs` against one instant.
pub(crate) fn current_verdict_at(
    platform: OsPlatform,
    snap: Option<&OsHealthSnapshot>,
    now: DateTime<Utc>,
) -> OsVerdict {
    grade(
        platform,
        snap,
        current_slope(),
        Thresholds::with_controls(&fleet_controls()),
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{incident_snapshot_for_tests, SpawnProbeStats};
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A healthy Windows snapshot read at `read_at`.
    fn healthy() -> OsHealthSnapshot {
        let mut s = incident_snapshot_for_tests();
        s.kernel_paged_bytes = Some(1_750_000_000);
        s.kernel_nonpaged_bytes = Some(1_200_000_000);
        s.kernel_token_objects = Some(5_580);
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: Some(30),
            max_ms: Some(60),
            count: 10,
            timeouts: 0,
            errors: 0,
            error_code: None,
        });
        s
    }

    fn now_of(s: &OsHealthSnapshot) -> DateTime<Utc> {
        s.read_at + ChronoDuration::seconds(5)
    }

    fn grade_default(s: &OsHealthSnapshot, slope: Slope) -> OsVerdict {
        grade(s.platform, Some(s), slope, Thresholds::DEFAULT, now_of(s))
    }

    fn no_slope() -> Slope {
        Slope::Unknown("test")
    }

    fn rules(v: &OsVerdict) -> Vec<(&'static str, &'static str, &'static str)> {
        v.reasons
            .iter()
            .map(|r| (r.axis, r.rule, r.band.as_str()))
            .collect()
    }

    #[test]
    fn the_compiled_defaults_are_the_plans_d4_numbers() {
        let d = Thresholds::DEFAULT;
        assert_eq!(d.token_warn, 250_000);
        assert_eq!(d.token_breach, 1_000_000);
        assert_eq!(d.token_slope_warn_per_hour, 10_000);
        assert_eq!(d.pool_warn_bytes, 8 * 1024 * 1024 * 1024);
        assert_eq!(d.pool_breach_bytes, 16 * 1024 * 1024 * 1024);
        assert_eq!(d.nonpaged_warn_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(d.spawn_p50_warn_ms, 250);
        assert_eq!(d.spawn_p50_breach_ms, 1_000);
        assert_eq!(Thresholds::with_controls(&OsHealthControls::default()), d);
    }

    #[test]
    fn a_healthy_windows_snapshot_is_ok_with_no_reasons() {
        let v = grade_default(&healthy(), no_slope());
        assert_eq!(v.verdict, Band::Ok);
        assert_eq!(v.s7, AxisGrade::Graded(Band::Ok));
        assert_eq!(v.s8, AxisGrade::Graded(Band::Ok));
        assert_eq!(v.s9, AxisGrade::Graded(Band::Ok));
        assert!(v.reasons.is_empty(), "{:?}", v.reasons);
    }

    #[test]
    fn s7_token_objects_bands() {
        for (tokens, band) in [
            (249_999, Band::Ok),
            (250_000, Band::Warn),
            (999_999, Band::Warn),
            (1_000_000, Band::Breach),
        ] {
            let mut s = healthy();
            s.kernel_token_objects = Some(tokens);
            let v = grade_default(&s, no_slope());
            assert_eq!(v.s7, AxisGrade::Graded(band), "tokens {tokens}");
            assert_eq!(v.verdict, band, "tokens {tokens}");
        }
        let mut s = healthy();
        s.kernel_token_objects = Some(1_000_000);
        let v = grade_default(&s, no_slope());
        assert_eq!(rules(&v), vec![("S7", "token_objects", "breach")]);
        assert_eq!(v.reasons[0].threshold, Some(1_000_000));
    }

    #[test]
    fn s7_slope_warns_and_cannot_breach() {
        let s = healthy();
        let v = grade_default(&s, Slope::PerHour(9_999.0));
        assert_eq!(v.s7, AxisGrade::Graded(Band::Ok));
        assert_eq!(v.token_slope_per_hour, Some(9_999));
        let v = grade_default(&s, Slope::PerHour(10_000.0));
        assert_eq!(v.s7, AxisGrade::Graded(Band::Warn));
        assert_eq!(rules(&v), vec![("S7", "token_slope", "warn")]);
        let v = grade_default(&s, Slope::PerHour(90_000_000.0));
        assert_eq!(v.s7, AxisGrade::Graded(Band::Warn), "the slope only warns");
    }

    #[test]
    fn an_unknown_slope_does_not_make_the_verdict_unknown() {
        let v = grade_default(&healthy(), Slope::Unknown("less than 1 h since boot"));
        assert_eq!(v.verdict, Band::Ok);
        assert_eq!(v.token_slope_per_hour, None);
    }

    #[test]
    fn s8_pool_bands() {
        for (paged, nonpaged, band) in [
            (4 * GIB - 1, 4 * GIB - 1, Band::Ok),
            (4 * GIB, 4 * GIB - 1, Band::Ok),
            (5 * GIB, 3 * GIB, Band::Warn),
            (12 * GIB, 3 * GIB, Band::Warn),
            (13 * GIB, 3 * GIB, Band::Breach),
            (GIB, 4 * GIB, Band::Warn),
        ] {
            let mut s = healthy();
            s.kernel_paged_bytes = Some(paged);
            s.kernel_nonpaged_bytes = Some(nonpaged);
            let v = grade_default(&s, no_slope());
            assert_eq!(
                v.s8,
                AxisGrade::Graded(band),
                "paged {paged} nonpaged {nonpaged}"
            );
        }
        let mut s = healthy();
        s.kernel_paged_bytes = Some(GIB);
        s.kernel_nonpaged_bytes = Some(4 * GIB);
        assert_eq!(
            rules(&grade_default(&s, no_slope())),
            vec![("S8", "pool_nonpaged", "warn")]
        );
    }

    #[test]
    fn s9_spawn_p50_bands() {
        for (p50, band) in [
            (249, Band::Ok),
            (250, Band::Warn),
            (999, Band::Warn),
            (1_000, Band::Breach),
        ] {
            let mut s = healthy();
            s.spawn_probe = Some(SpawnProbeStats {
                p50_ms: Some(p50),
                ..s.spawn_probe.unwrap()
            });
            let v = grade_default(&s, no_slope());
            assert_eq!(v.s9, AxisGrade::Graded(band), "p50 {p50}");
        }
    }

    #[test]
    fn s9_failures_with_zero_successes_breach_as_spawn_failed() {
        let mut s = healthy();
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: Some(30_000),
            max_ms: Some(30_000),
            count: 4,
            timeouts: 2,
            errors: 2,
            error_code: Some(1450),
        });
        let v = grade_default(&s, no_slope());
        assert_eq!(v.verdict, Band::Breach);
        assert_eq!(rules(&v), vec![("S9", "spawn_failed", "breach")]);
        assert!(v.reasons[0].message.contains("1450"));

        // Only errors: p50 is None, but it is still a measurement, not UNKNOWN.
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: None,
            max_ms: None,
            count: 3,
            timeouts: 0,
            errors: 3,
            error_code: Some(1450),
        });
        let v = grade_default(&s, no_slope());
        assert_eq!(rules(&v), vec![("S9", "spawn_failed", "breach")]);

        // One success among failures grades the p50 instead.
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: Some(40),
            max_ms: Some(30_000),
            count: 3,
            timeouts: 1,
            errors: 1,
            error_code: Some(1450),
        });
        assert_eq!(
            grade_default(&s, no_slope()).s9,
            AxisGrade::Graded(Band::Ok)
        );
    }

    #[test]
    fn the_incident_snapshot_breaches_on_tokens_pool_and_spawn_latency() {
        let s = incident_snapshot_for_tests();
        let v = grade_default(&s, no_slope());
        assert_eq!(v.verdict, Band::Breach);
        let r = rules(&v);
        assert!(r.contains(&("S7", "token_objects", "breach")), "{r:?}");
        assert!(r.contains(&("S8", "pool_total", "breach")), "{r:?}");
        assert!(r.contains(&("S8", "pool_nonpaged", "warn")), "{r:?}");
        assert!(r.contains(&("S9", "spawn_p50", "breach")), "{r:?}");
        let tokens = v
            .reasons
            .iter()
            .find(|r| r.rule == "token_objects")
            .unwrap();
        assert_eq!(tokens.message, "kernel token objects 4.4M (≥ 1.0M breach)");
    }

    #[test]
    fn all_null_applicable_readings_are_unknown_never_ok() {
        let mut s = healthy();
        s.kernel_paged_bytes = None;
        s.kernel_nonpaged_bytes = None;
        s.kernel_handle_count = None;
        s.kernel_token_objects = None;
        s.kernel_process_objects = None;
        s.os_process_count = None;
        s.os_booted_at = None;
        s.kernel_pool_top_tags = None;
        s.spawn_probe = None;
        s.kernel_source = None;
        let v = grade_default(&s, no_slope());
        assert_eq!(v.verdict, Band::Unknown);
        assert_eq!(v.s7, AxisGrade::Graded(Band::Unknown));
        assert_eq!(v.s8, AxisGrade::Graded(Band::Unknown));
        assert_eq!(v.s9, AxisGrade::Graded(Band::Unknown));
        assert!(v.reasons.iter().all(|r| r.band == Band::Unknown));
        assert_eq!(v.reasons.len(), 4, "{:?}", v.reasons);
    }

    #[test]
    fn one_null_applicable_reading_is_unknown_but_a_breach_still_wins() {
        let mut s = healthy();
        s.kernel_token_objects = None;
        assert_eq!(grade_default(&s, no_slope()).verdict, Band::Unknown);
        // A warn beside an unknown could be a breach: unknown.
        s.kernel_nonpaged_bytes = Some(4 * GIB);
        assert_eq!(grade_default(&s, no_slope()).verdict, Band::Unknown);
        // A breach is certain whatever else is unknown.
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: Some(2_400),
            ..s.spawn_probe.unwrap()
        });
        assert_eq!(grade_default(&s, no_slope()).verdict, Band::Breach);
    }

    #[test]
    fn linux_skips_s7_and_s8_and_is_not_unknown() {
        let mut s = healthy();
        s.platform = OsPlatform::Linux;
        s.kernel_token_objects = None;
        s.kernel_process_objects = None;
        s.kernel_pool_top_tags = None;
        s.kernel_pool_tags_at = None;
        // A huge Linux "pool" is reporting-only in v1 and must not grade.
        s.kernel_paged_bytes = Some(40 * GIB);
        s.kernel_nonpaged_bytes = Some(40 * GIB);
        s.kernel_source = Some("linux:proc-meminfo+file-nr".into());
        let v = grade_default(&s, Slope::PerHour(1e9));
        assert_eq!(v.verdict, Band::Ok, "{:?}", v.reasons);
        assert_eq!(v.s7, AxisGrade::NotApplicable);
        assert_eq!(v.s8, AxisGrade::NotApplicable);
        assert_eq!(v.s9, AxisGrade::Graded(Band::Ok));
        assert!(v.reasons.is_empty());
        // S9 still grades on Linux.
        s.spawn_probe = Some(SpawnProbeStats {
            p50_ms: Some(2_000),
            ..s.spawn_probe.unwrap()
        });
        assert_eq!(grade_default(&s, no_slope()).verdict, Band::Breach);
    }

    #[test]
    fn an_unsupported_os_grades_nothing_and_is_not_unknown() {
        let mut s = healthy();
        s.platform = OsPlatform::Unsupported;
        let v = grade(
            OsPlatform::Unsupported,
            None,
            no_slope(),
            Thresholds::DEFAULT,
            Utc::now(),
        );
        assert_eq!(v.verdict, Band::Ok);
        assert!(v.reasons.is_empty());
        assert_eq!(v.s9, AxisGrade::NotApplicable);
        s.kernel_token_objects = None;
        assert_eq!(grade_default(&s, no_slope()).verdict, Band::Ok);
    }

    #[test]
    fn no_snapshot_is_unknown() {
        let v = grade(
            OsPlatform::Windows,
            None,
            no_slope(),
            Thresholds::DEFAULT,
            Utc::now(),
        );
        assert_eq!(v.verdict, Band::Unknown);
        assert_eq!(rules(&v), vec![("all", "no_snapshot", "unknown")]);
    }

    #[test]
    fn a_stale_snapshot_is_unknown_even_when_it_breached() {
        let s = incident_snapshot_for_tests();
        let fresh = s.read_at + ChronoDuration::seconds(STALE_AFTER_SECS);
        assert_eq!(
            grade(s.platform, Some(&s), no_slope(), Thresholds::DEFAULT, fresh).verdict,
            Band::Breach
        );
        let stale = fresh + ChronoDuration::seconds(1);
        let v = grade(s.platform, Some(&s), no_slope(), Thresholds::DEFAULT, stale);
        assert_eq!(v.verdict, Band::Unknown);
        assert_eq!(rules(&v), vec![("all", "stale", "unknown")]);
    }

    #[test]
    fn controls_override_the_defaults_field_by_field() {
        let c = OsHealthControls {
            kernel_token_objects_warn: Some(1_000),
            kernel_token_objects_breach: Some(5_000),
            kernel_token_slope_warn_per_hour: Some(100),
            kernel_pool_warn_bytes: Some(GIB),
            kernel_pool_breach_bytes: Some(2 * GIB),
            kernel_nonpaged_warn_bytes: Some(GIB / 2),
            spawn_p50_warn_ms: Some(10),
            spawn_p50_breach_ms: Some(20),
        };
        let t = Thresholds::with_controls(&c);
        assert_eq!(
            t,
            Thresholds {
                token_warn: 1_000,
                token_breach: 5_000,
                token_slope_warn_per_hour: 100,
                pool_warn_bytes: GIB,
                pool_breach_bytes: 2 * GIB,
                nonpaged_warn_bytes: GIB / 2,
                spawn_p50_warn_ms: 10,
                spawn_p50_breach_ms: 20,
            }
        );
        let s = healthy();
        let v = grade(s.platform, Some(&s), Slope::PerHour(150.0), t, now_of(&s));
        assert_eq!(v.verdict, Band::Breach);
        let r = rules(&v);
        assert!(r.contains(&("S7", "token_objects", "breach")), "{r:?}");
        assert!(r.contains(&("S7", "token_slope", "warn")), "{r:?}");
        assert!(r.contains(&("S8", "pool_total", "breach")), "{r:?}");
        assert!(r.contains(&("S8", "pool_nonpaged", "warn")), "{r:?}");
        assert!(r.contains(&("S9", "spawn_p50", "breach")), "{r:?}");

        // One control set; the rest stay default. A negative one is unset.
        let partial = OsHealthControls {
            spawn_p50_breach_ms: Some(500),
            kernel_token_objects_warn: Some(-1),
            ..OsHealthControls::default()
        };
        let t = Thresholds::with_controls(&partial);
        assert_eq!(t.spawn_p50_breach_ms, 500);
        assert_eq!(t.token_warn, 250_000);
        assert_eq!(t.pool_breach_bytes, 16 * GIB);
    }

    #[test]
    fn current_verdict_reads_the_cached_controls() {
        let _guard = super::super::test_cell_lock();
        let s = healthy();
        set_fleet_controls(OsHealthControls {
            spawn_p50_warn_ms: Some(1),
            spawn_p50_breach_ms: Some(2),
            ..OsHealthControls::default()
        });
        let mut fresh = s.clone();
        fresh.read_at = Utc::now();
        let v = current_verdict(OsPlatform::Windows, Some(&fresh));
        set_fleet_controls(OsHealthControls::default());
        assert_eq!(v.s9, AxisGrade::Graded(Band::Breach));
        assert_eq!(v.thresholds.spawn_p50_breach_ms, 2);
    }

    /// D5: `osHealth` must NOT feed `/health` `derived_status`. That status
    /// says whether THIS RUNNER is healthy, and its consumers act on the
    /// runner; a kernel leak is fixed by rebooting Windows, never by
    /// restarting the runner. So neither `HealthInputs` nor the one place
    /// `/health` builds it may name the os-health verdict.
    #[test]
    fn the_os_health_verdict_does_not_feed_derived_status() {
        const UI_ERROR: &str = include_str!("../ui_error.rs");
        const MCP_API: &str = include_str!("../mcp_api.rs");
        let inputs = UI_ERROR
            .split_once("pub struct HealthInputs {")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("HealthInputs exists");
        let built = MCP_API
            .split_once("compute_derived_status(&crate::ui_error::HealthInputs {")
            .and_then(|(_, rest)| rest.split_once("});"))
            .map(|(body, _)| body)
            .expect("/health builds HealthInputs");
        for (what, src) in [("HealthInputs", inputs), ("/health's HealthInputs", built)] {
            for needle in [
                "os_health",
                "osHealth",
                "OsVerdict",
                "kernel_",
                "spawn_probe",
            ] {
                assert!(
                    !src.contains(needle),
                    "{what} names `{needle}`: osHealth must not feed derived_status"
                );
            }
        }
        // And osHealth IS served on /health, beside derived_status.
        assert!(MCP_API.contains("\"osHealth\": crate::os_health::health_json()"));
    }

    // ---- the slope ring ------------------------------------------------

    fn sample_at(boot: DateTime<Utc>, after_boot_min: i64, tokens: i64) -> OsHealthSnapshot {
        let mut s = healthy();
        s.os_booted_at = Some(boot);
        s.read_at = boot + ChronoDuration::minutes(after_boot_min);
        s.kernel_token_objects = Some(tokens);
        s
    }

    #[test]
    fn the_slope_is_unknown_in_the_first_hour_after_boot() {
        let boot = at("2026-10-15T06:00:00Z");
        let mut h = TokenHistory::default();
        for m in (0..60).step_by(1) {
            h.record(&sample_at(boot, m, 5_000 + m * 10_000));
        }
        assert_eq!(h.slope(), Slope::Unknown("less than 1 h since boot"));
    }

    #[test]
    fn the_slope_counts_only_readings_after_the_boot_settled() {
        let boot = at("2026-10-15T06:00:00Z");
        let mut h = TokenHistory::default();
        // A boot transient of 1M tokens in the first hour must not count.
        h.record(&sample_at(boot, 1, 0));
        h.record(&sample_at(boot, 59, 1_000_000));
        h.record(&sample_at(boot, 60, 1_000_000));
        h.record(&sample_at(boot, 70, 1_000_000));
        assert!(matches!(h.slope(), Slope::Unknown(_)), "span < 15 min");
        h.record(&sample_at(boot, 120, 1_020_000));
        assert_eq!(h.slope(), Slope::PerHour(20_000.0));
    }

    #[test]
    fn the_slope_covers_at_most_the_last_three_hours() {
        let boot = at("2026-10-15T06:00:00Z");
        let mut h = TokenHistory::default();
        // Flat for hours, then +30,000 over the last 3 h.
        h.record(&sample_at(boot, 60, 100_000));
        h.record(&sample_at(boot, 300, 100_000));
        h.record(&sample_at(boot, 480, 130_000));
        // Window start = 480 - 180 = 300.
        assert_eq!(h.slope(), Slope::PerHour(10_000.0));
    }

    #[test]
    fn the_slope_never_differences_across_a_reboot() {
        let boot1 = at("2026-10-15T06:00:00Z");
        let boot2 = at("2026-10-15T09:00:00Z");
        let mut h = TokenHistory::default();
        h.record(&sample_at(boot1, 61, 4_000_000));
        h.record(&sample_at(boot1, 150, 4_100_000));
        // Rebooted: the first reading of the new boot is in its first hour.
        h.record(&sample_at(boot2, 30, 5_000));
        assert_eq!(h.slope(), Slope::Unknown("less than 1 h since boot"));
        h.record(&sample_at(boot2, 65, 5_100));
        assert!(
            matches!(h.slope(), Slope::Unknown(_)),
            "only one reading this boot"
        );
        h.record(&sample_at(boot2, 125, 5_700));
        // (5_700 - 5_100) over 1 h — the old boot's 4M never enters it.
        assert_eq!(h.slope(), Slope::PerHour(600.0));
    }

    #[test]
    fn a_republished_tick_is_not_a_second_sample() {
        let boot = at("2026-10-15T06:00:00Z");
        let mut h = TokenHistory::default();
        h.record(&sample_at(boot, 61, 100));
        h.record(&sample_at(boot, 61, 200));
        assert_eq!(h.samples.len(), 1);
        assert_eq!(h.samples[0].tokens, 200);
        // No Token count or no boot: nothing remembered.
        let mut s = sample_at(boot, 62, 1);
        s.kernel_token_objects = None;
        h.record(&s);
        let mut s = sample_at(boot, 63, 1);
        s.os_booted_at = None;
        h.record(&s);
        assert_eq!(h.samples.len(), 1);
    }
}
