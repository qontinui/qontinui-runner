//! Baseline learning and adaptive scoring for agentic metrics.
//!
//! Historical baseline computation and persistence is currently unimplemented;
//! the previous SQLite-backed implementation has been removed pending a
//! PostgreSQL replacement. The deterministic scorers in sibling modules
//! continue to operate without learned baselines.

#[cfg(test)]
mod tests {
    /// Compute the 25th percentile of a sorted list (kept for future use).
    fn percentile_25(data: &[f64]) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        let mut sorted = data.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let idx = (sorted.len() as f64 * 0.25).floor() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    #[test]
    fn test_percentile_25_empty() {
        assert_eq!(percentile_25(&[]), 0.0);
    }

    #[test]
    fn test_percentile_25_single() {
        assert_eq!(percentile_25(&[5.0]), 5.0);
    }

    #[test]
    fn test_percentile_25_four_elements() {
        assert_eq!(percentile_25(&[3.0, 1.0, 4.0, 2.0]), 2.0);
    }

    #[test]
    fn test_percentile_25_twenty_elements() {
        let data: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        assert_eq!(percentile_25(&data), 6.0);
    }
}
