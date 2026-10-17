//! Statistics and KPI values, with the conventions of Phase 1's `stats.mjs`
//! and `kpi-catalog.mjs`, so a number the probe reports means what the catalog
//! says it means:
//!
//! - nearest-rank percentiles (never interpolated: every reported value was
//!   observed), and a p-th percentile needs `ceil(100 / (100 - p))` samples;
//! - every value is `{state: measured | unknown | not_applicable, ...}` with
//!   `n`, and an unmeasured value carries a `reason` and NO number — UNKNOWN is
//!   never rendered as 0 (policy verification-and-evidence/unknown-must-not-render-as-a-default).

use serde::Serialize;
use std::collections::BTreeMap;

/// A catalog KPI the probe computes: its id, unit and minimum sample count,
/// copied from `scripts/termhost-bench/kpi-catalog.mjs` (a test reads that file
/// and fails on drift).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KpiSpec {
    pub id: &'static str,
    pub unit: &'static str,
    pub min_n: u64,
}

pub const OUTPUT_LATENCY: KpiSpec = KpiSpec {
    id: "output_latency_ms",
    unit: "ms",
    min_n: 100,
};
pub const INPUT_LATENCY: KpiSpec = KpiSpec {
    id: "input_latency_ms",
    unit: "ms",
    min_n: 100,
};
pub const FRAME_INTERVAL: KpiSpec = KpiSpec {
    id: "frame_interval_ms",
    unit: "ms",
    min_n: 100,
};
pub const JANK_SHARE: KpiSpec = KpiSpec {
    id: "jank_share",
    unit: "%",
    min_n: 100,
};
pub const HANGS: KpiSpec = KpiSpec {
    id: "hangs",
    unit: "count",
    min_n: 60,
};
pub const MARKER_LOSS: KpiSpec = KpiSpec {
    id: "marker_loss",
    unit: "%",
    min_n: 100,
};
pub const MARKER_SKIPPED: KpiSpec = KpiSpec {
    id: "marker_skipped",
    unit: "%",
    min_n: 100,
};
pub const CPU_PCT: KpiSpec = KpiSpec {
    id: "cpu_pct",
    unit: "% of one logical core",
    min_n: 20,
};
pub const GPU_PCT: KpiSpec = KpiSpec {
    id: "gpu_pct",
    unit: "%",
    min_n: 20,
};

/// Every catalog KPI this crate restates.
pub const RESTATED: [KpiSpec; 9] = [
    OUTPUT_LATENCY,
    INPUT_LATENCY,
    FRAME_INTERVAL,
    JANK_SHARE,
    HANGS,
    MARKER_LOSS,
    MARKER_SKIPPED,
    CPU_PCT,
    GPU_PCT,
];

/// The 1-based nearest rank of the p-th percentile among n samples:
/// `ceil(p * n / 100)`, clamped to `1..=n`.
pub fn nearest_rank(p: u32, n: usize) -> usize {
    assert!((1..=100).contains(&p) && n >= 1, "nearest_rank({p}, {n})");
    let rank = (p as usize * n).div_ceil(100);
    rank.clamp(1, n)
}

/// The fewest samples a p-th percentile needs to differ from the maximum:
/// `ceil(100 / (100 - p))`.
pub fn percentile_floor(p: u32) -> usize {
    if p >= 100 {
        1
    } else {
        100usize.div_ceil(100 - p as usize)
    }
}

/// Nearest-rank percentile of `values` (any order); `None` below the floor or
/// for an empty slice.
pub fn percentile(values: &[f64], p: u32) -> Option<f64> {
    if values.is_empty() || values.len() < percentile_floor(p) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[nearest_rank(p, sorted.len()) - 1])
}

pub fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// A KPI value's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KpiState {
    Measured,
    Unknown,
    NotApplicable,
}

/// One KPI value in the catalog's shape.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KpiValue {
    pub state: KpiState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<BTreeMap<String, f64>>,
    pub unit: &'static str,
    pub n: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl KpiValue {
    pub fn measured_value(spec: KpiSpec, value: f64, n: u64) -> Self {
        KpiValue {
            state: KpiState::Measured,
            value: Some(value),
            stats: None,
            unit: spec.unit,
            n,
            reason: None,
            detail: None,
        }
    }

    pub fn unknown(
        spec: KpiSpec,
        reason: &'static str,
        n: u64,
        detail: impl Into<Option<String>>,
    ) -> Self {
        KpiValue {
            state: KpiState::Unknown,
            value: None,
            stats: None,
            unit: spec.unit,
            n,
            reason: Some(reason),
            detail: detail.into(),
        }
    }

    pub fn is_measured(&self) -> bool {
        self.state == KpiState::Measured
    }

    /// Nearest-rank percentiles of `samples` as a `stats` value, or
    /// unknown(insufficient_samples) below the KPI's minimum (and each
    /// percentile's floor).
    pub fn percentiles(spec: KpiSpec, samples: &[f64], ps: &[u32]) -> Self {
        let n = samples.len() as u64;
        let floor = ps
            .iter()
            .map(|&p| percentile_floor(p) as u64)
            .max()
            .unwrap_or(1);
        if n < spec.min_n.max(floor) {
            return KpiValue::unknown(spec, "insufficient_samples", n, None);
        }
        let mut stats = BTreeMap::new();
        for &p in ps {
            if let Some(v) = percentile(samples, p) {
                stats.insert(format!("p{p}"), v);
            }
        }
        KpiValue {
            state: KpiState::Measured,
            value: None,
            stats: Some(stats),
            unit: spec.unit,
            n,
            reason: None,
            detail: None,
        }
    }

    /// Mean and p95 (the catalog's statistics for cpu/gpu percentages).
    pub fn mean_p95(spec: KpiSpec, samples: &[f64]) -> Self {
        let n = samples.len() as u64;
        if n < spec.min_n.max(percentile_floor(95) as u64) {
            return KpiValue::unknown(spec, "insufficient_samples", n, None);
        }
        let mut stats = BTreeMap::new();
        stats.insert("mean".to_string(), mean(samples).unwrap_or_default());
        stats.insert(
            "p95".to_string(),
            percentile(samples, 95).unwrap_or_default(),
        );
        KpiValue {
            state: KpiState::Measured,
            value: None,
            stats: Some(stats),
            unit: spec.unit,
            n,
            reason: None,
            detail: None,
        }
    }

    /// A percentage of `hits` among `n`, or unknown below the minimum.
    pub fn share(spec: KpiSpec, hits: u64, n: u64) -> Self {
        if n < spec.min_n.max(1) {
            return KpiValue::unknown(spec, "insufficient_samples", n, None);
        }
        KpiValue::measured_value(spec, 100.0 * hits as f64 / n as f64, n)
    }

    /// The `p`-th percentile when measured.
    pub fn stat(&self, key: &str) -> Option<f64> {
        self.stats.as_ref().and_then(|s| s.get(key).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_stats_mjs() {
        assert_eq!(nearest_rank(95, 20), 19);
        assert_eq!(nearest_rank(50, 1), 1);
        assert_eq!(nearest_rank(99, 100), 99);
        assert_eq!(nearest_rank(50, 3), 2);
        assert_eq!(percentile_floor(95), 20);
        assert_eq!(percentile_floor(99), 100);
        assert_eq!(percentile_floor(50), 2);
    }

    #[test]
    fn percentiles_never_interpolate() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 95), Some(19.0));
        assert_eq!(percentile(&v, 50), Some(10.0));
        assert_eq!(percentile(&[3.0], 95), None);
    }

    #[test]
    fn under_sampled_kpis_are_unknown_with_no_number() {
        let k = KpiValue::percentiles(OUTPUT_LATENCY, &[1.0; 99], &[50, 95, 99]);
        assert_eq!(k.state, KpiState::Unknown);
        assert_eq!(k.reason, Some("insufficient_samples"));
        assert!(k.stats.is_none() && k.value.is_none());
        let k = KpiValue::percentiles(OUTPUT_LATENCY, &[1.0; 100], &[50, 95, 99]);
        assert!(k.is_measured());
        assert_eq!(k.stat("p99"), Some(1.0));
    }

    #[test]
    fn shares_are_percentages() {
        assert_eq!(KpiValue::share(MARKER_LOSS, 5, 200).value, Some(2.5));
        assert_eq!(KpiValue::share(MARKER_LOSS, 0, 99).state, KpiState::Unknown);
    }

    #[test]
    fn restated_minimums_match_the_kpi_catalog() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/termhost-bench/kpi-catalog.mjs");
        let text = std::fs::read_to_string(path).expect("kpi-catalog.mjs is readable");
        for spec in RESTATED {
            // Each catalog row opens with `id: "<id>",` and closes with `    },`.
            let opener = format!("{}\",", spec.id);
            let row = text
                .split("id: \"")
                .find(|chunk| chunk.starts_with(&opener))
                .and_then(|chunk| chunk.split("\n    },").next())
                .unwrap_or_else(|| panic!("{} not in the catalog", spec.id));
            let unit_ok = row.contains(&format!("unit: \"{}\"", spec.unit))
                || (spec.unit == "% of one logical core" && row.contains("unit: CORE_PCT"));
            assert!(
                unit_ok,
                "{}: unit {} not in its catalog row",
                spec.id, spec.unit
            );
            assert!(
                row.contains(&format!("min_n: {},", spec.min_n)),
                "{}: min_n {} not in its catalog row",
                spec.id,
                spec.min_n
            );
        }
    }
}
