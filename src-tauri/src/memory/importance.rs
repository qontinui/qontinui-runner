//! Importance scoring for observations.
//!
//! Computes a 0.0–1.0 importance score based on observation type, confirmation
//! signals (duplicates, revisions, access), and task linkage.

/// Base importance by observation type.
fn base_importance(observation_type: &str) -> f64 {
    match observation_type {
        "architecture" => 0.8,
        "decision" => 0.7,
        "bugfix" => 0.6,
        "pattern" => 0.5,
        "learning" => 0.5,
        "discovery" => 0.4,
        _ => 0.5,
    }
}

/// Compute the importance score for an observation.
///
/// Factors:
/// - Base importance by type
/// - Confirmation boost: +0.05 per duplicate (capped at +0.2)
/// - Revision boost: +0.05 per revision beyond 1 (capped at +0.2)
/// - Access boost: +0.02 per access (capped at +0.1)
/// - Task-linked boost: +0.1 if linked to a task_run
pub fn compute_importance(
    observation_type: &str,
    duplicate_count: i32,
    revision_count: i32,
    access_count: i32,
    has_task_run: bool,
) -> f64 {
    let base = base_importance(observation_type);

    let confirmation_boost = (duplicate_count as f64 * 0.05).min(0.2);
    let revision_boost = ((revision_count.saturating_sub(1)) as f64 * 0.05).min(0.2);
    let access_boost = (access_count as f64 * 0.02).min(0.1);
    let task_boost = if has_task_run { 0.1 } else { 0.0 };

    (base + confirmation_boost + revision_boost + access_boost + task_boost).min(1.0)
}

/// Determine the appropriate decay rate for an observation.
///
/// - Mental models: 0.02/day (slow — consolidated knowledge)
/// - High-importance (≥0.8): 0.05/day (intermediate)
/// - Regular observations: 0.1/day (default)
pub fn compute_decay_rate(importance: f64, is_mental_model: bool) -> f64 {
    if is_mental_model {
        0.02
    } else if importance >= 0.8 {
        0.05
    } else {
        0.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_importance_by_type() {
        assert!((base_importance("architecture") - 0.8).abs() < f64::EPSILON);
        assert!((base_importance("decision") - 0.7).abs() < f64::EPSILON);
        assert!((base_importance("bugfix") - 0.6).abs() < f64::EPSILON);
        assert!((base_importance("pattern") - 0.5).abs() < f64::EPSILON);
        assert!((base_importance("learning") - 0.5).abs() < f64::EPSILON);
        assert!((base_importance("discovery") - 0.4).abs() < f64::EPSILON);
        assert!((base_importance("unknown") - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_importance_base_only() {
        let score = compute_importance("architecture", 0, 1, 0, false);
        assert!((score - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_importance_with_duplicates() {
        // 4 duplicates = +0.2 (capped)
        let score = compute_importance("discovery", 4, 1, 0, false);
        assert!((score - 0.6).abs() < f64::EPSILON); // 0.4 + 0.2
    }

    #[test]
    fn test_compute_importance_duplicate_cap() {
        // 10 duplicates = +0.5 raw, but capped at +0.2
        let score = compute_importance("discovery", 10, 1, 0, false);
        assert!((score - 0.6).abs() < f64::EPSILON); // 0.4 + 0.2
    }

    #[test]
    fn test_compute_importance_with_revisions() {
        // 3 revisions = 2 beyond first = +0.1
        let score = compute_importance("pattern", 0, 3, 0, false);
        assert!((score - 0.6).abs() < f64::EPSILON); // 0.5 + 0.1
    }

    #[test]
    fn test_compute_importance_with_access() {
        // 5 accesses = +0.1 (capped)
        let score = compute_importance("pattern", 0, 1, 5, false);
        assert!((score - 0.6).abs() < f64::EPSILON); // 0.5 + 0.1
    }

    #[test]
    fn test_compute_importance_task_linked() {
        let score = compute_importance("pattern", 0, 1, 0, true);
        assert!((score - 0.6).abs() < f64::EPSILON); // 0.5 + 0.1
    }

    #[test]
    fn test_compute_importance_all_boosts() {
        // architecture(0.8) + dup(0.2) + rev(0.2) + access(0.1) + task(0.1) = 1.4 → capped at 1.0
        let score = compute_importance("architecture", 10, 10, 10, true);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_importance_capped_at_one() {
        let score = compute_importance("architecture", 5, 5, 5, true);
        assert!(score <= 1.0);
    }

    #[test]
    fn test_decay_rate_mental_model() {
        assert!((compute_decay_rate(0.5, true) - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn test_decay_rate_high_importance() {
        assert!((compute_decay_rate(0.8, false) - 0.05).abs() < f64::EPSILON);
        assert!((compute_decay_rate(0.9, false) - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_decay_rate_regular() {
        assert!((compute_decay_rate(0.5, false) - 0.1).abs() < f64::EPSILON);
        assert!((compute_decay_rate(0.79, false) - 0.1).abs() < f64::EPSILON);
    }
}
