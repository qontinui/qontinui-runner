//! Common race condition pattern detection
//!
//! Detects patterns like TOCTOU, double-checked locking, and unprotected compound operations.

use super::types::{AccessType, AnalysisContext, RaceIssue, RacePattern, RaceSeverity, StateAccess};

/// Detect TOCTOU (Time-of-Check to Time-of-Use) patterns
///
/// This pattern occurs when code checks a condition and then acts on it,
/// but the condition could change between the check and the action.
///
/// Examples:
/// - `if key not in dict: dict[key] = value`
/// - `if file.exists(): file.read()`
/// - `if lock.available(): lock.acquire()`
pub fn detect_toctou(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        // Look for read-then-write patterns on same state within close proximity
        let accesses = &state.accesses;

        for i in 0..accesses.len() {
            if accesses[i].access_type != AccessType::Read {
                continue;
            }

            // Look for a write within 5 lines after a read
            for j in (i + 1)..accesses.len() {
                if accesses[j].access_type == AccessType::Write
                    && accesses[j].line <= accesses[i].line + 5
                    && !accesses[i].is_protected
                    && !accesses[j].is_protected
                {
                    issues.push(RaceIssue {
                        pattern: RacePattern::Toctou,
                        severity: RaceSeverity::High,
                        file: file.to_string(),
                        line: accesses[i].line,
                        column: accesses[i].column,
                        state_name: state.name.clone(),
                        message: format!(
                            "Potential TOCTOU: '{}' is checked on line {} and modified on line {} without protection",
                            state.name, accesses[i].line, accesses[j].line
                        ),
                        related_locations: vec![(file.to_string(), accesses[j].line)],
                        confidence: 0.7,
                    });
                    break; // Only report once per read
                }
            }
        }
    }

    issues
}

/// Detect unprotected compound operations (+=, -=, *=, etc.)
///
/// These operations are read-modify-write sequences that are not atomic
/// and can cause lost updates when accessed concurrently.
pub fn detect_unprotected_compound(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        for access in &state.accesses {
            if access.access_type == AccessType::Compound && !access.is_protected {
                // Check if this state has multiple accesses (indicating potential concurrency)
                let total_accesses = state.accesses.len();
                if total_accesses > 1 {
                    issues.push(RaceIssue {
                        pattern: RacePattern::UnprotectedCompound,
                        severity: RaceSeverity::High,
                        file: file.to_string(),
                        line: access.line,
                        column: access.column,
                        state_name: state.name.clone(),
                        message: format!(
                            "Unprotected compound operation on '{}' - this is not atomic and can cause lost updates",
                            state.name
                        ),
                        related_locations: state
                            .accesses
                            .iter()
                            .filter(|a| a.line != access.line)
                            .map(|a| (file.to_string(), a.line))
                            .collect(),
                        confidence: 0.8,
                    });
                }
            }
        }
    }

    issues
}

/// Detect double-checked locking pattern
///
/// This pattern checks a condition, acquires a lock, then checks again.
/// It can be incorrect in languages without proper memory barriers.
pub fn detect_double_checked_locking(
    accesses: &[StateAccess],
    state_name: &str,
    file: &str,
) -> Option<RaceIssue> {
    // Look for pattern: read -> (lock acquired) -> read -> write
    // This is a simplified detection

    let reads: Vec<_> = accesses
        .iter()
        .filter(|a| a.access_type == AccessType::Read)
        .collect();
    let writes: Vec<_> = accesses
        .iter()
        .filter(|a| a.access_type == AccessType::Write)
        .collect();

    // Need at least 2 reads and 1 write
    if reads.len() < 2 || writes.is_empty() {
        return None;
    }

    // Check if there's an unprotected read followed by a protected read
    for i in 0..reads.len() {
        if reads[i].is_protected {
            continue;
        }

        for j in (i + 1)..reads.len() {
            if reads[j].is_protected && reads[j].line > reads[i].line {
                // Found pattern: unprotected read followed by protected read
                // Check if there's a write after the protected read
                for write in &writes {
                    if write.is_protected
                        && write.line > reads[j].line
                        && write.line < reads[j].line + 10
                    {
                        return Some(RaceIssue {
                            pattern: RacePattern::DoubleCheckedLocking,
                            severity: RaceSeverity::Medium,
                            file: file.to_string(),
                            line: reads[i].line,
                            column: reads[i].column,
                            state_name: state_name.to_string(),
                            message: format!(
                                "Potential double-checked locking on '{}' - this pattern may be incorrect without proper memory barriers",
                                state_name
                            ),
                            related_locations: vec![
                                (file.to_string(), reads[j].line),
                                (file.to_string(), write.line),
                            ],
                            confidence: 0.6,
                        });
                    }
                }
            }
        }
    }

    None
}

/// Detect write-write races (multiple unprotected writes)
pub fn detect_write_write_race(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        let unprotected_writes: Vec<_> = state
            .accesses
            .iter()
            .filter(|a| a.access_type == AccessType::Write && !a.is_protected)
            .collect();

        if unprotected_writes.len() > 1 {
            let first_write = unprotected_writes[0];
            issues.push(RaceIssue {
                pattern: RacePattern::WriteWriteRace,
                severity: RaceSeverity::Critical,
                file: file.to_string(),
                line: first_write.line,
                column: first_write.column,
                state_name: state.name.clone(),
                message: format!(
                    "Multiple unprotected writes to '{}' detected ({} locations) - potential write-write race",
                    state.name,
                    unprotected_writes.len()
                ),
                related_locations: unprotected_writes
                    .iter()
                    .skip(1)
                    .map(|w| (file.to_string(), w.line))
                    .collect(),
                confidence: 0.85,
            });
        }
    }

    issues
}

/// Detect read-write races (unprotected writes with reads)
pub fn detect_read_write_race(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        let unprotected_writes: Vec<_> = state
            .accesses
            .iter()
            .filter(|a| a.access_type == AccessType::Write && !a.is_protected)
            .collect();

        let reads: Vec<_> = state
            .accesses
            .iter()
            .filter(|a| a.access_type == AccessType::Read)
            .collect();

        // If there are unprotected writes and multiple reads
        if !unprotected_writes.is_empty() && reads.len() > 1 {
            let first_write = unprotected_writes[0];
            issues.push(RaceIssue {
                pattern: RacePattern::ReadWriteRace,
                severity: RaceSeverity::Critical,
                file: file.to_string(),
                line: first_write.line,
                column: first_write.column,
                state_name: state.name.clone(),
                message: format!(
                    "Unprotected write to '{}' with {} concurrent reads - potential read-write race",
                    state.name,
                    reads.len()
                ),
                related_locations: reads.iter().map(|r| (file.to_string(), r.line)).collect(),
                confidence: 0.75,
            });
        }
    }

    issues
}

/// Detect partially protected state (some accesses protected, some not)
pub fn detect_partially_protected(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        let protected_count = state.accesses.iter().filter(|a| a.is_protected).count();
        let total_count = state.accesses.len();

        // If some but not all accesses are protected
        if protected_count > 0 && protected_count < total_count && total_count > 2 {
            let unprotected: Vec<_> = state
                .accesses
                .iter()
                .filter(|a| !a.is_protected)
                .collect();

            if let Some(first_unprotected) = unprotected.first() {
                issues.push(RaceIssue {
                    pattern: RacePattern::PartiallyProtected,
                    severity: RaceSeverity::Medium,
                    file: file.to_string(),
                    line: first_unprotected.line,
                    column: first_unprotected.column,
                    state_name: state.name.clone(),
                    message: format!(
                        "'{}' has inconsistent protection: {} of {} accesses are protected",
                        state.name, protected_count, total_count
                    ),
                    related_locations: unprotected
                        .iter()
                        .skip(1)
                        .map(|a| (file.to_string(), a.line))
                        .collect(),
                    confidence: 0.65,
                });
            }
        }
    }

    issues
}

/// Run all pattern detectors on the analysis context
pub fn detect_all_patterns(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    issues.extend(detect_write_write_race(ctx, file));
    issues.extend(detect_read_write_race(ctx, file));
    issues.extend(detect_toctou(ctx, file));
    issues.extend(detect_unprotected_compound(ctx, file));
    issues.extend(detect_partially_protected(ctx, file));

    // Check for double-checked locking on each state
    for state in &ctx.shared_states {
        if let Some(issue) = detect_double_checked_locking(&state.accesses, &state.name, file) {
            issues.push(issue);
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_executor::analyzers::concurrency::types::{SharedState, SharedStateType};

    fn create_access(line: u32, access_type: AccessType, protected: bool) -> StateAccess {
        StateAccess {
            state_name: "test".to_string(),
            access_type,
            line,
            column: None,
            is_protected: protected,
            lock_name: if protected {
                Some("lock".to_string())
            } else {
                None
            },
            context: None,
        }
    }

    #[test]
    fn test_detect_write_write_race() {
        let mut ctx = AnalysisContext::new();
        ctx.shared_states.push(SharedState {
            name: "counter".to_string(),
            state_type: SharedStateType::ModuleGlobal,
            definition_line: 1,
            file: "test.py".to_string(),
            accesses: vec![
                create_access(10, AccessType::Write, false),
                create_access(20, AccessType::Write, false),
            ],
            appears_thread_safe: false,
            thread_safe_reason: None,
        });

        let issues = detect_write_write_race(&ctx, "test.py");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].pattern, RacePattern::WriteWriteRace);
    }

    #[test]
    fn test_detect_toctou() {
        let mut ctx = AnalysisContext::new();
        ctx.shared_states.push(SharedState {
            name: "cache".to_string(),
            state_type: SharedStateType::ModuleGlobal,
            definition_line: 1,
            file: "test.py".to_string(),
            accesses: vec![
                create_access(10, AccessType::Read, false),
                create_access(11, AccessType::Write, false),
            ],
            appears_thread_safe: false,
            thread_safe_reason: None,
        });

        let issues = detect_toctou(&ctx, "test.py");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].pattern, RacePattern::Toctou);
    }

    #[test]
    fn test_no_race_when_protected() {
        let mut ctx = AnalysisContext::new();
        ctx.shared_states.push(SharedState {
            name: "counter".to_string(),
            state_type: SharedStateType::ModuleGlobal,
            definition_line: 1,
            file: "test.py".to_string(),
            accesses: vec![
                create_access(10, AccessType::Write, true),
                create_access(20, AccessType::Write, true),
            ],
            appears_thread_safe: false,
            thread_safe_reason: None,
        });

        let issues = detect_write_write_race(&ctx, "test.py");
        assert!(issues.is_empty());
    }
}
