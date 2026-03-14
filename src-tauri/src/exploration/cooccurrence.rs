//! Co-occurrence export builder.
//!
//! Builds a `CooccurrenceExport` from captured element data across
//! multiple page visits. Used by the exploration engine to accumulate
//! data that the discovery algorithm consumes.

use std::collections::{HashMap, HashSet};

use super::types::*;

/// Builder that accumulates captures and produces a CooccurrenceExport.
pub struct CooccurrenceBuilder {
    session_id: String,
    captures: Vec<CaptureRecord>,
    fingerprint_catalog: HashMap<String, ElementFingerprint>,
    transitions: Vec<TransitionRecord>,
}

impl CooccurrenceBuilder {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            captures: Vec::new(),
            fingerprint_catalog: HashMap::new(),
            transitions: Vec::new(),
        }
    }

    /// Record a capture with its fingerprints.
    pub fn add_capture(
        &mut self,
        capture_id: String,
        url: String,
        title: String,
        fingerprint_hashes: Vec<String>,
        catalog: &HashMap<String, ElementFingerprint>,
    ) {
        let timestamp = chrono::Utc::now().timestamp_millis();

        self.captures.push(CaptureRecord {
            capture_id,
            url,
            title,
            timestamp,
            fingerprint_hashes,
            triggered_by: None,
        });

        // Merge new fingerprints into catalog
        for (hash, fp) in catalog {
            self.fingerprint_catalog
                .entry(hash.clone())
                .or_insert_with(|| fp.clone());
        }
    }

    /// Record a transition (before/after capture).
    pub fn add_transition(
        &mut self,
        action_type: String,
        target_fingerprint: Option<String>,
        before_capture_id: String,
        after_capture_id: String,
        before_hashes: &HashSet<String>,
        after_hashes: &HashSet<String>,
    ) {
        let appeared: Vec<String> = after_hashes.difference(before_hashes).cloned().collect();
        let disappeared: Vec<String> = before_hashes.difference(after_hashes).cloned().collect();

        self.transitions.push(TransitionRecord {
            action_id: format!(
                "t-{}-{}",
                chrono::Utc::now().timestamp_millis(),
                self.transitions.len()
            ),
            action_type,
            target_fingerprint,
            before_capture_id,
            after_capture_id,
            appeared_fingerprints: appeared,
            disappeared_fingerprints: disappeared,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });

        // Tag the after capture with triggered_by (lookup by ID, not position)
        let transition = self.transitions.last().unwrap();
        let after_id = transition.after_capture_id.clone();
        let trigger = TriggeredBy {
            action_type: transition.action_type.clone(),
            target_fingerprint: transition.target_fingerprint.clone().unwrap_or_default(),
            previous_capture_id: transition.before_capture_id.clone(),
        };
        if let Some(after_cap) = self.captures.iter_mut().rfind(|c| c.capture_id == after_id) {
            after_cap.triggered_by = Some(trigger);
        }
    }

    /// Get number of captures so far.
    pub fn capture_count(&self) -> usize {
        self.captures.len()
    }

    /// Get number of unique fingerprints.
    pub fn unique_fingerprint_count(&self) -> usize {
        self.fingerprint_catalog.len()
    }

    /// Get the last capture ID.
    pub fn last_capture_id(&self) -> Option<&str> {
        self.captures.last().map(|c| c.capture_id.as_str())
    }

    /// Build the final CooccurrenceExport.
    pub fn build(self) -> CooccurrenceExport {
        let all_fingerprints: Vec<String> = {
            let mut fps: Vec<String> = self.fingerprint_catalog.keys().cloned().collect();
            fps.sort();
            fps
        };

        // Build presence matrix
        let presence_matrix: Vec<PresenceMatrixEntry> = self
            .captures
            .iter()
            .enumerate()
            .map(|(i, cap)| PresenceMatrixEntry {
                capture_id: cap.capture_id.clone(),
                capture_index: i,
                timestamp: cap.timestamp,
                url: cap.url.clone(),
                title: cap.title.clone(),
                fingerprints: cap.fingerprint_hashes.clone(),
            })
            .collect();

        // Build co-occurrence counts (pairwise)
        let mut cooccurrence_counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for cap in &self.captures {
            let fps = &cap.fingerprint_hashes;
            for (i, fp1) in fps.iter().enumerate() {
                for fp2 in fps.iter().skip(i + 1) {
                    *cooccurrence_counts
                        .entry(fp1.clone())
                        .or_default()
                        .entry(fp2.clone())
                        .or_insert(0) += 1;
                    *cooccurrence_counts
                        .entry(fp2.clone())
                        .or_default()
                        .entry(fp1.clone())
                        .or_insert(0) += 1;
                }
            }
        }

        // Build fingerprint stats
        let mut fingerprint_stats: HashMap<String, FingerprintStats> = HashMap::new();
        for cap in &self.captures {
            for fp in &cap.fingerprint_hashes {
                let stats =
                    fingerprint_stats
                        .entry(fp.clone())
                        .or_insert_with(|| FingerprintStats {
                            total_appearances: 0,
                            capture_ids: Vec::new(),
                            first_seen: cap.timestamp,
                            last_seen: cap.timestamp,
                        });
                stats.total_appearances += 1;
                stats.capture_ids.push(cap.capture_id.clone());
                stats.first_seen = stats.first_seen.min(cap.timestamp);
                stats.last_seen = stats.last_seen.max(cap.timestamp);
            }
        }

        // Build state candidates (groups with 100% co-occurrence)
        let state_candidates = compute_state_candidates(&self.captures);

        CooccurrenceExport {
            session_id: self.session_id,
            exported_at: chrono::Utc::now().timestamp_millis(),
            all_fingerprints,
            fingerprint_details: self.fingerprint_catalog,
            presence_matrix,
            cooccurrence_counts,
            fingerprint_stats,
            transitions: self.transitions,
            state_candidates,
        }
    }
}

/// Compute state candidates: groups of fingerprints that always appear together.
fn compute_state_candidates(captures: &[CaptureRecord]) -> Vec<StateCandidate> {
    if captures.is_empty() {
        return Vec::new();
    }

    // Group fingerprints by their capture signature (which captures they appear in)
    let mut signature_groups: HashMap<Vec<String>, Vec<String>> = HashMap::new();

    // Build per-fingerprint capture sets
    let mut fp_to_captures: HashMap<String, HashSet<String>> = HashMap::new();
    for cap in captures {
        for fp in &cap.fingerprint_hashes {
            fp_to_captures
                .entry(fp.clone())
                .or_default()
                .insert(cap.capture_id.clone());
        }
    }

    // Group by signature (sorted list of capture IDs)
    for (fp, cap_ids) in &fp_to_captures {
        let mut sig: Vec<String> = cap_ids.iter().cloned().collect();
        sig.sort();
        signature_groups.entry(sig).or_default().push(fp.clone());
    }

    // Convert to state candidates
    signature_groups
        .into_values()
        .filter(|fps| !fps.is_empty())
        .map(|fps| StateCandidate {
            fingerprints: fps,
            cooccurrence_rate: 1.0,
            position_zone: None,
            landmark_context: None,
        })
        .collect()
}
