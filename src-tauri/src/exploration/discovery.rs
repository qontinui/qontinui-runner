//! Fingerprint-enhanced state discovery algorithm.
//!
//! Ports the Python `FingerprintStateDiscovery` class to Rust.
//! Groups element fingerprints into UI states using co-occurrence analysis,
//! position zone classification, and repeat pattern deduplication.

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use super::types::*;

// =============================================================================
// Configuration
// =============================================================================

/// Default size weights for element importance scoring.
fn default_size_weights() -> HashMap<String, f64> {
    [
        ("icon", 0.1),
        ("button", 0.3),
        ("small", 0.5),
        ("medium", 0.7),
        ("large", 0.9),
        ("fullwidth", 1.0),
        ("panel", 1.0),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscoveryConfig {
    pub min_cooccurrence_rate: f64,
    pub treat_header_footer_as_global: bool,
    pub auto_detect_modal_states: bool,
    pub dedupe_repeating_elements: bool,
    pub max_repeat_representatives: usize,
    pub use_size_weighting: bool,
    pub size_weights: HashMap<String, f64>,
    pub min_state_elements: usize,
    pub max_state_elements: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            min_cooccurrence_rate: 0.95,
            treat_header_footer_as_global: true,
            auto_detect_modal_states: true,
            dedupe_repeating_elements: true,
            max_repeat_representatives: 3,
            use_size_weighting: true,
            size_weights: default_size_weights(),
            min_state_elements: 1,
            max_state_elements: 100,
        }
    }
}

// =============================================================================
// FingerprintStateDiscovery
// =============================================================================

pub struct FingerprintStateDiscovery {
    config: DiscoveryConfig,
    fingerprints: HashMap<String, ElementFingerprint>,
    captures: Vec<CaptureRecord>,
    capture_fingerprints: HashMap<String, HashSet<String>>,
    transitions: Vec<TransitionRecord>,
    cooccurrence_counts: HashMap<String, HashMap<String, usize>>,
    fingerprint_appearance_count: HashMap<String, usize>,
    state_candidates: Vec<StateCandidate>,
    discovered_states: Vec<DiscoveredState>,
    global_fingerprints: HashSet<String>,
}

impl FingerprintStateDiscovery {
    pub fn new(config: DiscoveryConfig) -> Self {
        Self {
            config,
            fingerprints: HashMap::new(),
            captures: Vec::new(),
            capture_fingerprints: HashMap::new(),
            transitions: Vec::new(),
            cooccurrence_counts: HashMap::new(),
            fingerprint_appearance_count: HashMap::new(),
            state_candidates: Vec::new(),
            discovered_states: Vec::new(),
            global_fingerprints: HashSet::new(),
        }
    }

    /// Load data from a CooccurrenceExport.
    pub fn load_cooccurrence_export(&mut self, export: &CooccurrenceExport) {
        self.fingerprints = export.fingerprint_details.clone();
        self.cooccurrence_counts = export.cooccurrence_counts.clone();
        self.transitions = export.transitions.clone();
        self.state_candidates = export.state_candidates.clone();

        // Build appearance counts from stats
        for (fp_hash, stats) in &export.fingerprint_stats {
            self.fingerprint_appearance_count
                .insert(fp_hash.clone(), stats.total_appearances);
        }

        // Parse presence matrix into captures
        for entry in &export.presence_matrix {
            let capture = CaptureRecord {
                capture_id: entry.capture_id.clone(),
                url: entry.url.clone(),
                title: entry.title.clone(),
                timestamp: entry.timestamp,
                fingerprint_hashes: entry.fingerprints.clone(),
                triggered_by: None,
            };
            let fps: HashSet<String> = entry.fingerprints.iter().cloned().collect();
            self.capture_fingerprints
                .insert(entry.capture_id.clone(), fps);
            self.captures.push(capture);
        }

        // Identify global fingerprints
        if self.config.treat_header_footer_as_global {
            self.identify_global_fingerprints();
        }

        info!(
            "Loaded co-occurrence export: {} fingerprints, {} captures, {} candidates",
            self.fingerprints.len(),
            self.captures.len(),
            self.state_candidates.len()
        );
    }

    /// Run state discovery.
    pub fn discover_states(&mut self) -> &[DiscoveredState] {
        if self.fingerprints.is_empty() {
            warn!("No fingerprints to discover states from");
            return &[];
        }

        if !self.state_candidates.is_empty() {
            self.process_state_candidates();
        } else {
            self.compute_state_candidates();
        }

        self.refine_states();

        info!("Discovered {} states", self.discovered_states.len());
        &self.discovered_states
    }

    /// Get discovery results.
    pub fn into_result(self) -> DiscoveryResult {
        let transitions = self.build_transitions();
        let stats = DiscoveryStatistics {
            total_captures: self.captures.len(),
            total_transitions: transitions.len(),
            unique_fingerprints: self.fingerprints.len(),
            discovered_states: self.discovered_states.len(),
            global_states: self
                .discovered_states
                .iter()
                .filter(|s| s.is_global)
                .count(),
            modal_states: self.discovered_states.iter().filter(|s| s.is_modal).count(),
            discovered_transitions: transitions.len(),
        };

        DiscoveryResult {
            states: self.discovered_states,
            transitions,
            statistics: stats,
        }
    }

    // =========================================================================
    // Position Zone Handling
    // =========================================================================

    fn identify_global_fingerprints(&mut self) {
        self.global_fingerprints.clear();
        for (fp_hash, fp) in &self.fingerprints {
            if GLOBAL_POSITION_ZONES.contains(&fp.position_zone.as_str()) {
                self.global_fingerprints.insert(fp_hash.clone());
            }
        }
    }

    fn filter_global_elements(&self, fps: &[String]) -> (Vec<String>, Vec<String>) {
        let mut global = Vec::new();
        let mut specific = Vec::new();
        for fp in fps {
            if self.global_fingerprints.contains(fp) {
                global.push(fp.clone());
            } else {
                specific.push(fp.clone());
            }
        }
        (global, specific)
    }

    fn get_dominant_zone(&self, fps: &[String]) -> String {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for fp_hash in fps {
            if let Some(fp) = self.fingerprints.get(fp_hash) {
                *counts.entry(&fp.position_zone).or_insert(0) += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(z, _)| z.to_string())
            .unwrap_or_else(|| "main".to_string())
    }

    fn get_dominant_landmark(&self, fps: &[String]) -> String {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for fp_hash in fps {
            if let Some(fp) = self.fingerprints.get(fp_hash) {
                if !fp.landmark_context.is_empty() {
                    *counts.entry(&fp.landmark_context).or_insert(0) += 1;
                }
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(l, _)| l.to_string())
            .unwrap_or_default()
    }

    // =========================================================================
    // Repeat Pattern Handling
    // =========================================================================

    fn dedupe_repeating_elements(&self, fps: &[String]) -> Vec<String> {
        if !self.config.dedupe_repeating_elements {
            return fps.to_vec();
        }

        let mut seen_patterns: HashMap<String, Vec<String>> = HashMap::new();
        let mut non_repeating = Vec::new();

        for fp_hash in fps {
            if let Some(fp) = self.fingerprints.get(fp_hash) {
                if fp.is_repeating {
                    if let Some(ref pattern) = fp.repeat_pattern {
                        // Group by container + item selector to avoid merging unrelated lists
                        let key =
                            format!("{}:{}", pattern.container_selector, pattern.item_selector);
                        seen_patterns.entry(key).or_default().push(fp_hash.clone());
                        continue;
                    }
                }
            }
            non_repeating.push(fp_hash.clone());
        }

        let mut result = non_repeating;
        for pattern_fps in seen_patterns.values() {
            result.extend(
                pattern_fps
                    .iter()
                    .take(self.config.max_repeat_representatives)
                    .cloned(),
            );
        }
        result
    }

    fn count_repeat_patterns(&self, fps: &[String]) -> usize {
        fps.iter()
            .filter(|fp_hash| {
                self.fingerprints
                    .get(*fp_hash)
                    .map(|fp| fp.is_repeating)
                    .unwrap_or(false)
            })
            .count()
    }

    // =========================================================================
    // State Candidate Processing
    // =========================================================================

    fn process_state_candidates(&mut self) {
        let candidates = self.state_candidates.clone();
        for candidate in &candidates {
            if candidate.fingerprints.is_empty() {
                continue;
            }

            let deduped = self.dedupe_repeating_elements(&candidate.fingerprints);

            if deduped.len() < self.config.min_state_elements
                || deduped.len() > self.config.max_state_elements
            {
                continue;
            }

            let (global_fps, specific_fps) = self.filter_global_elements(&deduped);

            if self.config.treat_header_footer_as_global && !global_fps.is_empty() {
                self.create_state_from_fingerprints(&global_fps, true, false);
            }

            if !specific_fps.is_empty() {
                let dominant_zone = self.get_dominant_zone(&specific_fps);
                let is_modal = self.config.auto_detect_modal_states
                    && BLOCKING_POSITION_ZONES.contains(&dominant_zone.as_str());
                self.create_state_from_fingerprints(&specific_fps, false, is_modal);
            }
        }
    }

    fn compute_state_candidates(&mut self) {
        if self.captures.is_empty() {
            return;
        }

        // Group fingerprints by exact capture signature
        let mut signature_groups: HashMap<Vec<String>, HashSet<String>> = HashMap::new();

        for fp_hash in self.fingerprints.keys() {
            let mut sig: Vec<String> = self
                .capture_fingerprints
                .iter()
                .filter(|(_, fps)| fps.contains(fp_hash))
                .map(|(cap_id, _)| cap_id.clone())
                .collect();
            sig.sort();

            if !sig.is_empty() {
                signature_groups
                    .entry(sig)
                    .or_default()
                    .insert(fp_hash.clone());
            }
        }

        self.state_candidates = signature_groups
            .into_values()
            .filter(|fps| fps.len() >= self.config.min_state_elements)
            .map(|fps| StateCandidate {
                fingerprints: fps.into_iter().collect(),
                cooccurrence_rate: 1.0,
                position_zone: None,
                landmark_context: None,
            })
            .collect();

        self.process_state_candidates();
    }

    // =========================================================================
    // State Creation
    // =========================================================================

    fn create_state_from_fingerprints(
        &mut self,
        fps: &[String],
        is_global: bool,
        is_modal: bool,
    ) -> String {
        let state_id = generate_state_id(fps);

        // Check for existing state
        if let Some(existing) = self
            .discovered_states
            .iter_mut()
            .find(|s| s.state_id == state_id)
        {
            existing.observation_count += 1;
            return state_id;
        }

        let name = self.generate_state_name(fps, is_global, is_modal);
        let position_zone = self.get_dominant_zone(fps);
        let landmark_context = self.get_dominant_landmark(fps);
        let repeat_count = self.count_repeat_patterns(fps);
        let confidence = self.calculate_confidence(fps);

        // Use accessible names as element labels for display
        let element_ids: Vec<String> = fps
            .iter()
            .filter_map(|fp_hash| {
                self.fingerprints.get(fp_hash).map(|fp| {
                    fp.accessible_name
                        .clone()
                        .unwrap_or_else(|| fp.tag_name.clone())
                })
            })
            .collect();

        let state = DiscoveredState {
            state_id: state_id.clone(),
            name: name.clone(),
            fingerprint_hashes: {
                let mut sorted = fps.to_vec();
                sorted.sort();
                sorted
            },
            element_ids,
            position_zone,
            landmark_context,
            is_global,
            is_modal,
            repeat_pattern_count: repeat_count,
            confidence,
            observation_count: 1,
        };

        debug!("Created state: {} ({} fingerprints)", name, fps.len());
        self.discovered_states.push(state);
        state_id
    }

    fn generate_state_name(&self, fps: &[String], is_global: bool, is_modal: bool) -> String {
        let position_zone = self.get_dominant_zone(fps);
        let landmark = self.get_dominant_landmark(fps);

        let mut accessible_names: Vec<String> = fps
            .iter()
            .take(3)
            .filter_map(|fp_hash| {
                self.fingerprints
                    .get(fp_hash)
                    .and_then(|fp| fp.accessible_name.clone())
            })
            .collect();
        accessible_names.truncate(3);

        let mut parts = Vec::new();

        if is_modal {
            parts.push("Modal".to_string());
        } else if is_global {
            parts.push("Global".to_string());
        }

        if !landmark.is_empty() {
            parts.push(
                landmark
                    .replace('-', " ")
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        } else if position_zone != "main" {
            parts.push(
                position_zone
                    .replace('-', " ")
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }

        if let Some(first_name) = accessible_names.first() {
            let truncated: String = first_name.chars().take(30).collect();
            parts.push(truncated);
        }

        if parts.is_empty() {
            parts.push("State".to_string());
        }

        let mut name = parts.join(" ");
        if fps.len() > 1 {
            name.push_str(&format!(" ({} elements)", fps.len()));
        }
        name
    }

    fn calculate_confidence(&self, fps: &[String]) -> f64 {
        if fps.is_empty() {
            return 0.0;
        }

        let observation_counts: Vec<usize> = fps
            .iter()
            .map(|fp| *self.fingerprint_appearance_count.get(fp).unwrap_or(&0))
            .collect();

        let min_observations = observation_counts.iter().copied().min().unwrap_or(0);

        let observation_score = ((min_observations as f64 + 1.0).log10() / 1.0).min(1.0);
        let size_score = (fps.len() as f64 / 10.0).min(1.0);

        let weights: Vec<f64> = fps
            .iter()
            .filter_map(|fp_hash| self.fingerprints.get(fp_hash))
            .map(|fp| {
                self.config
                    .size_weights
                    .get(&fp.size_category)
                    .copied()
                    .unwrap_or(0.5)
            })
            .collect();

        let weight_score = if weights.is_empty() {
            0.5
        } else {
            weights.iter().sum::<f64>() / weights.len() as f64
        };

        observation_score * 0.5 + size_score * 0.3 + weight_score * 0.2
    }

    // =========================================================================
    // State Refinement
    // =========================================================================

    fn refine_states(&mut self) {
        self.merge_similar_states();
        self.discovered_states.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    fn merge_similar_states(&mut self) {
        if self.discovered_states.len() <= 1 {
            return;
        }

        let threshold = self.config.min_cooccurrence_rate;
        let mut merged = true;

        while merged {
            merged = false;
            let mut new_states: Vec<DiscoveredState> = Vec::new();

            for state in self.discovered_states.drain(..) {
                let state_fps: HashSet<String> = state.fingerprint_hashes.iter().cloned().collect();
                let mut merged_with_existing = false;

                for existing in &mut new_states {
                    let existing_fps: HashSet<String> =
                        existing.fingerprint_hashes.iter().cloned().collect();
                    let intersection = state_fps.intersection(&existing_fps).count();
                    let union = state_fps.union(&existing_fps).count();
                    let similarity = if union > 0 {
                        intersection as f64 / union as f64
                    } else {
                        0.0
                    };

                    if similarity >= threshold {
                        let merged_fps: HashSet<String> =
                            state_fps.union(&existing_fps).cloned().collect();
                        existing.fingerprint_hashes = {
                            let mut v: Vec<String> = merged_fps.into_iter().collect();
                            v.sort();
                            v
                        };
                        existing.observation_count += state.observation_count;
                        existing.confidence = existing.confidence.max(state.confidence);
                        merged_with_existing = true;
                        merged = true;
                        break;
                    }
                }

                if !merged_with_existing {
                    new_states.push(state);
                }
            }

            self.discovered_states = new_states;
        }
    }

    // =========================================================================
    // Transition Building
    // =========================================================================

    fn build_transitions(&self) -> Vec<DiscoveredTransition> {
        let mut transition_counts: HashMap<(String, String), (String, usize)> = HashMap::new();

        for record in &self.transitions {
            let before_state = self.find_state_for_fingerprints(&record.disappeared_fingerprints);
            let after_state = self.find_state_for_fingerprints(&record.appeared_fingerprints);

            if let (Some(before), Some(after)) = (before_state, after_state) {
                if before != after {
                    let key = (before.to_string(), after.to_string());
                    let entry = transition_counts
                        .entry(key)
                        .or_insert_with(|| (record.action_type.clone(), 0));
                    entry.1 += 1;
                }
            }
        }

        transition_counts
            .into_iter()
            .map(|((from, to), (action_type, count))| DiscoveredTransition {
                from_state_id: from,
                to_state_id: to,
                action_type,
                count,
            })
            .collect()
    }

    fn find_state_for_fingerprints(&self, fps: &[String]) -> Option<&str> {
        if fps.is_empty() {
            return None;
        }

        let target: HashSet<&String> = fps.iter().collect();
        let mut best_match = None;
        let mut best_overlap = 0.0;

        for state in &self.discovered_states {
            let state_set: HashSet<&String> = state.fingerprint_hashes.iter().collect();
            let intersection = target.intersection(&state_set).count();
            let union = target.union(&state_set).count();
            let overlap = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            if overlap > best_overlap {
                best_overlap = overlap;
                best_match = Some(state.state_id.as_str());
            }
        }

        if best_overlap > 0.5 {
            best_match
        } else {
            None
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Generate deterministic state ID from fingerprint hashes.
fn generate_state_id(fps: &[String]) -> String {
    let mut sorted: Vec<&str> = fps.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let input = sorted.join("|");

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash.iter().take(6).map(|b| format!("{:02x}", b)).collect();

    format!("fp_state_{}", hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_state_id() {
        let fps = vec!["abc".to_string(), "def".to_string()];
        let id = generate_state_id(&fps);
        assert!(id.starts_with("fp_state_"));
        assert_eq!(id.len(), "fp_state_".len() + 12);

        // Deterministic
        assert_eq!(id, generate_state_id(&fps));

        // Order-independent
        let fps_rev = vec!["def".to_string(), "abc".to_string()];
        assert_eq!(id, generate_state_id(&fps_rev));
    }
}
