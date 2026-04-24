//! Prompt Duel Engine — Dueling bandits optimization for prompt selection.
//!
//! Manages a pool of prompt candidates, selects duel pairs via Thompson Sampling
//! (Beta-Bernoulli posteriors), tracks pairwise wins in a matrix, and ranks
//! candidates using Copeland scores.

use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

// ---------------------------------------------------------------------------
// Inline Beta sampling (Marsaglia–Tsang gamma sampler)
// ---------------------------------------------------------------------------

/// Sample from Gamma(shape, 1) using the Marsaglia–Tsang method.
/// Requires `shape >= 1.0`. For `shape < 1.0` we use the boost trick:
/// Gamma(a) = Gamma(a+1) * U^(1/a).
fn sample_gamma_inline(rng: &mut StdRng, shape: f64) -> f64 {
    if shape < 1.0 {
        // Boost: Gamma(a) = Gamma(a+1) * U^(1/a)
        let u: f64 = rng.random();
        return sample_gamma_inline(rng, shape + 1.0) * u.powf(1.0 / shape);
    }

    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();

    loop {
        let x: f64 = {
            // Standard normal via Box-Muller
            let u1: f64 = rng.random();
            let u2: f64 = rng.random();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        };

        let v = (1.0 + c * x).powi(3);
        if v <= 0.0 {
            continue;
        }

        let u: f64 = rng.random();
        // Squeeze + rejection
        if u < 1.0 - 0.0331 * x.powi(4) || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

/// Sample from Beta(alpha, beta) using two independent Gamma samples.
fn sample_beta_inline(rng: &mut StdRng, alpha: f64, beta: f64) -> f64 {
    let x = sample_gamma_inline(rng, alpha);
    let y = sample_gamma_inline(rng, beta);
    if x + y == 0.0 {
        return 0.5;
    }
    x / (x + y)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A candidate in the duel pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuelCandidate {
    pub id: String,
    pub prompt_content: String,
    pub variant_id: Option<String>,
    pub generation: u32,
    pub parent_id: Option<String>,
}

/// Result of a single pairwise duel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuelResult {
    pub duel_id: String,
    pub candidate_a_id: String,
    pub candidate_b_id: String,
    pub winner_id: String,
    pub judge_rationale: String,
    pub position_swapped: bool,
    pub confidence: f64,
}

/// Pairwise win matrix. `wins[i][j]` = number of times candidate i beat candidate j.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinMatrix {
    pub candidate_ids: Vec<String>,
    pub wins: Vec<Vec<u32>>,
}

impl WinMatrix {
    pub fn new(candidate_ids: &[String]) -> Self {
        let n = candidate_ids.len();
        Self {
            candidate_ids: candidate_ids.to_vec(),
            wins: vec![vec![0u32; n]; n],
        }
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.candidate_ids.iter().position(|c| c == id)
    }

    /// Record that `winner` beat `loser` once.
    pub fn record_win(&mut self, winner: &str, loser: &str) {
        if let (Some(wi), Some(li)) = (self.index_of(winner), self.index_of(loser)) {
            self.wins[wi][li] += 1;
        }
    }

    /// Copeland score for each candidate using pairwise dominance.
    ///
    /// For each opponent j, candidate i scores +1 if it won more duels against j
    /// than j won against i, -1 if it lost more, 0 for a tie. The Copeland score
    /// is the sum divided by max(1, K-1) where K = number of candidates.
    /// Returns sorted descending by score.
    pub fn copeland_scores(&self) -> Vec<(String, f64)> {
        let k = self.candidate_ids.len();
        let divisor = if k <= 1 { 1.0 } else { (k - 1) as f64 };

        let mut scores: Vec<(String, f64)> = self
            .candidate_ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut net = 0.0f64;
                for j in 0..k {
                    if j == i {
                        continue;
                    }
                    let w_ij = self.wins[i][j];
                    let w_ji = self.wins[j][i];
                    if w_ij > w_ji {
                        net += 1.0;
                    } else if w_ij < w_ji {
                        net -= 1.0;
                    }
                    // tie: net += 0
                }
                (id.clone(), net / divisor)
            })
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    }

    /// Total wins for a candidate (across all opponents).
    pub fn get_wins(&self, candidate: &str) -> u32 {
        match self.index_of(candidate) {
            Some(i) => self.wins[i].iter().sum(),
            None => 0,
        }
    }

    /// Total losses for a candidate.
    pub fn get_losses(&self, candidate: &str) -> u32 {
        match self.index_of(candidate) {
            Some(i) => self.wins.iter().map(|row| row[i]).sum(),
            None => 0,
        }
    }

    /// Add a new candidate, expanding the matrix.
    pub fn add_candidate(&mut self, id: &str) {
        if self.index_of(id).is_some() {
            return; // already present
        }
        let n = self.candidate_ids.len();
        // Extend each existing row with a 0 for the new column
        for row in &mut self.wins {
            row.push(0);
        }
        // Add a new row of zeros (n+1 wide)
        self.wins.push(vec![0u32; n + 1]);
        self.candidate_ids.push(id.to_string());
    }

    /// Remove a candidate by id, shrinking the matrix.
    fn remove_candidate(&mut self, id: &str) {
        if let Some(idx) = self.index_of(id) {
            self.candidate_ids.remove(idx);
            self.wins.remove(idx);
            for row in &mut self.wins {
                row.remove(idx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CandidatePool
// ---------------------------------------------------------------------------

/// Manages active duel candidates with Bayesian Beta-Bernoulli posteriors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePool {
    pub candidates: Vec<DuelCandidate>,
    pub matrix: WinMatrix,
    /// Beta posterior parameters (alpha, beta) per candidate id.
    pub beta_posteriors: HashMap<String, (f64, f64)>,
    pub generation: u32,
}

impl CandidatePool {
    pub fn new() -> Self {
        Self {
            candidates: Vec::new(),
            matrix: WinMatrix::new(&[]),
            beta_posteriors: HashMap::new(),
            generation: 0,
        }
    }

    /// Add a candidate to the pool. Skips duplicates by ID.
    pub fn add_candidate(&mut self, candidate: DuelCandidate) {
        if self.candidates.iter().any(|c| c.id == candidate.id) {
            return; // already present
        }
        self.matrix.add_candidate(&candidate.id);
        self.beta_posteriors
            .insert(candidate.id.clone(), (1.0, 1.0));
        self.candidates.push(candidate);
    }

    /// Select a duel pair via Thompson Sampling.
    ///
    /// For each candidate, draw a sample from its Beta(alpha, beta) posterior.
    /// Return the two candidates with the highest samples. Returns `None` if
    /// fewer than two candidates are in the pool.
    pub fn select_duel_pair(&self, rng: &mut StdRng) -> Option<(String, String)> {
        if self.candidates.len() < 2 {
            return None;
        }

        let mut samples: Vec<(String, f64)> = self
            .candidates
            .iter()
            .map(|c| {
                let &(alpha, beta) = self.beta_posteriors.get(&c.id).unwrap_or(&(1.0, 1.0));
                let s = sample_beta_inline(rng, alpha, beta);
                (c.id.clone(), s)
            })
            .collect();

        // Sort descending by sample value
        samples.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let a = samples[0].0.clone();
        let b = samples[1].0.clone();
        debug!(candidate_a = %a, candidate_b = %b, "selected duel pair via Thompson Sampling");
        Some((a, b))
    }

    /// Record a duel result: update the win matrix and Beta posteriors.
    pub fn record_duel_result(&mut self, result: &DuelResult) {
        // Validate winner_id is one of the two candidates
        if result.winner_id != result.candidate_a_id && result.winner_id != result.candidate_b_id {
            tracing::warn!(
                winner = %result.winner_id,
                a = %result.candidate_a_id,
                b = %result.candidate_b_id,
                "winner_id is neither candidate — ignoring duel result"
            );
            return;
        }

        let loser_id = if result.winner_id == result.candidate_a_id {
            &result.candidate_b_id
        } else {
            &result.candidate_a_id
        };

        self.matrix.record_win(&result.winner_id, loser_id);

        // Update posteriors: winner alpha += 1, loser beta += 1
        if let Some(posterior) = self.beta_posteriors.get_mut(&result.winner_id) {
            posterior.0 += 1.0; // alpha
        }
        if let Some(posterior) = self.beta_posteriors.get_mut(loser_id) {
            posterior.1 += 1.0; // beta
        }

        debug!(
            winner = %result.winner_id,
            loser = %loser_id,
            "recorded duel result"
        );
    }

    /// Prune the pool to the top `keep_top_k` candidates by Copeland score.
    /// Returns the IDs of removed candidates.
    pub fn prune_losers(&mut self, keep_top_k: usize) -> Vec<String> {
        let scores = self.matrix.copeland_scores();
        if scores.len() <= keep_top_k {
            return Vec::new();
        }

        let to_remove: Vec<String> = scores[keep_top_k..]
            .iter()
            .map(|(id, _)| id.clone())
            .collect();

        for id in &to_remove {
            self.candidates.retain(|c| c.id != *id);
            self.matrix.remove_candidate(id);
            self.beta_posteriors.remove(id);
        }

        debug!(removed = ?to_remove, "pruned losers from candidate pool");
        to_remove
    }

    /// The top candidate by Copeland score.
    pub fn top_candidate(&self) -> Option<&DuelCandidate> {
        let scores = self.matrix.copeland_scores();
        scores
            .first()
            .and_then(|(id, _)| self.candidates.iter().find(|c| c.id == *id))
    }

    /// Rankings: Copeland scores sorted descending.
    pub fn rankings(&self) -> Vec<(String, f64)> {
        self.matrix.copeland_scores()
    }

    /// Number of candidates in the pool.
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(id: &str) -> DuelCandidate {
        DuelCandidate {
            id: id.to_string(),
            prompt_content: format!("prompt for {id}"),
            variant_id: None,
            generation: 0,
            parent_id: None,
        }
    }

    #[test]
    fn test_win_matrix_basic() {
        let ids: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let mut m = WinMatrix::new(&ids);

        // a beats b twice, a beats c once, b beats c once
        m.record_win("a", "b");
        m.record_win("a", "b");
        m.record_win("a", "c");
        m.record_win("b", "c");

        assert_eq!(m.get_wins("a"), 3);
        assert_eq!(m.get_wins("b"), 1);
        assert_eq!(m.get_wins("c"), 0);

        assert_eq!(m.get_losses("a"), 0);
        assert_eq!(m.get_losses("b"), 2);
        assert_eq!(m.get_losses("c"), 2);

        let scores = m.copeland_scores();
        // a should be first (highest), c should be last
        assert_eq!(scores[0].0, "a");
        assert_eq!(scores[2].0, "c");
        // Pairwise Copeland: a wins vs b (+1), a wins vs c (+1) → 2/(K-1=2) = 1.0
        assert!((scores[0].1 - 1.0).abs() < 1e-9);
        // c loses to a (-1), c loses to b (-1) → -2/2 = -1.0
        assert!((scores[2].1 - -1.0).abs() < 1e-9);
    }

    #[test]
    fn test_win_matrix_add_candidate() {
        let ids: Vec<String> = vec!["a".into(), "b".into()];
        let mut m = WinMatrix::new(&ids);
        m.record_win("a", "b");

        m.add_candidate("c");

        assert_eq!(m.candidate_ids.len(), 3);
        assert_eq!(m.wins.len(), 3);
        for row in &m.wins {
            assert_eq!(row.len(), 3);
        }

        // Original win preserved
        assert_eq!(m.get_wins("a"), 1);
        // New candidate starts at 0
        assert_eq!(m.get_wins("c"), 0);
        assert_eq!(m.get_losses("c"), 0);
    }

    #[test]
    fn test_candidate_pool_add_and_count() {
        let mut pool = CandidatePool::new();
        for id in &["x", "y", "z", "w"] {
            pool.add_candidate(make_candidate(id));
        }
        assert_eq!(pool.candidate_count(), 4);
        assert_eq!(pool.matrix.candidate_ids.len(), 4);
        assert_eq!(pool.beta_posteriors.len(), 4);

        // All posteriors should be (1.0, 1.0)
        for (a, b) in pool.beta_posteriors.values() {
            assert!((a - 1.0).abs() < 1e-9);
            assert!((b - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_candidate_pool_record_duel() {
        let mut pool = CandidatePool::new();
        pool.add_candidate(make_candidate("alpha"));
        pool.add_candidate(make_candidate("bravo"));

        let result = DuelResult {
            duel_id: "d1".into(),
            candidate_a_id: "alpha".into(),
            candidate_b_id: "bravo".into(),
            winner_id: "alpha".into(),
            judge_rationale: "alpha was better".into(),
            position_swapped: false,
            confidence: 0.85,
        };

        pool.record_duel_result(&result);

        // Winner alpha: alpha should be 2.0
        let &(a, b) = pool.beta_posteriors.get("alpha").unwrap();
        assert!((a - 2.0).abs() < 1e-9, "winner alpha param: {a}");
        assert!((b - 1.0).abs() < 1e-9, "winner beta param: {b}");

        // Loser bravo: beta should be 2.0
        let &(a, b) = pool.beta_posteriors.get("bravo").unwrap();
        assert!((a - 1.0).abs() < 1e-9, "loser alpha param: {a}");
        assert!((b - 2.0).abs() < 1e-9, "loser beta param: {b}");

        // Win matrix
        assert_eq!(pool.matrix.get_wins("alpha"), 1);
        assert_eq!(pool.matrix.get_losses("bravo"), 1);
    }

    #[test]
    fn test_candidate_pool_prune() {
        let mut pool = CandidatePool::new();
        for id in &["p1", "p2", "p3", "p4", "p5"] {
            pool.add_candidate(make_candidate(id));
        }

        // Make p1 dominate: beats everyone multiple times
        for loser in &["p2", "p3", "p4", "p5"] {
            for _ in 0..3 {
                let result = DuelResult {
                    duel_id: format!("d-p1-{loser}"),
                    candidate_a_id: "p1".into(),
                    candidate_b_id: loser.to_string(),
                    winner_id: "p1".into(),
                    judge_rationale: "p1 wins".into(),
                    position_swapped: false,
                    confidence: 0.9,
                };
                pool.record_duel_result(&result);
            }
        }

        // Give p2 some wins over p4 and p5
        for loser in &["p4", "p5"] {
            let result = DuelResult {
                duel_id: format!("d-p2-{loser}"),
                candidate_a_id: "p2".into(),
                candidate_b_id: loser.to_string(),
                winner_id: "p2".into(),
                judge_rationale: "p2 wins".into(),
                position_swapped: false,
                confidence: 0.8,
            };
            pool.record_duel_result(&result);
        }

        // p3 beats p5
        let result = DuelResult {
            duel_id: "d-p3-p5".into(),
            candidate_a_id: "p3".into(),
            candidate_b_id: "p5".into(),
            winner_id: "p3".into(),
            judge_rationale: "p3 wins".into(),
            position_swapped: false,
            confidence: 0.7,
        };
        pool.record_duel_result(&result);

        let removed = pool.prune_losers(3);
        assert_eq!(removed.len(), 2);
        assert_eq!(pool.candidate_count(), 3);

        // p1 should still be present (top scorer)
        assert!(pool.candidates.iter().any(|c| c.id == "p1"));
        // p5 should be removed (worst)
        assert!(!pool.candidates.iter().any(|c| c.id == "p5"));
    }

    #[test]
    fn test_candidate_pool_rankings() {
        let mut pool = CandidatePool::new();
        pool.add_candidate(make_candidate("r1"));
        pool.add_candidate(make_candidate("r2"));
        pool.add_candidate(make_candidate("r3"));

        // r2 beats r1, r3 beats r1, r2 beats r3
        for (winner, loser) in &[("r2", "r1"), ("r3", "r1"), ("r2", "r3")] {
            let result = DuelResult {
                duel_id: format!("d-{winner}-{loser}"),
                candidate_a_id: winner.to_string(),
                candidate_b_id: loser.to_string(),
                winner_id: winner.to_string(),
                judge_rationale: "wins".into(),
                position_swapped: false,
                confidence: 0.9,
            };
            pool.record_duel_result(&result);
        }

        let rankings = pool.rankings();
        assert_eq!(rankings.len(), 3);
        // r2 best (2 wins, 0 losses), r3 middle (1 win, 1 loss), r1 worst (0 wins, 2 losses)
        assert_eq!(rankings[0].0, "r2");
        assert_eq!(rankings[1].0, "r3");
        assert_eq!(rankings[2].0, "r1");

        // Verify scores: r2 = (2-0)/2 = 1.0, r3 = (1-1)/2 = 0.0, r1 = (0-2)/2 = -1.0
        assert!((rankings[0].1 - 1.0).abs() < 1e-9);
        assert!((rankings[1].1 - 0.0).abs() < 1e-9);
        assert!((rankings[2].1 - (-1.0)).abs() < 1e-9);
    }

    #[test]
    fn test_select_duel_pair_deterministic() {
        let mut pool = CandidatePool::new();
        pool.add_candidate(make_candidate("s1"));
        pool.add_candidate(make_candidate("s2"));
        pool.add_candidate(make_candidate("s3"));

        let mut rng = StdRng::seed_from_u64(42);
        let pair = pool.select_duel_pair(&mut rng);
        assert!(pair.is_some());

        let (a, b) = pair.unwrap();
        assert_ne!(a, b);
        // Both should be valid candidate IDs
        assert!(["s1", "s2", "s3"].contains(&a.as_str()));
        assert!(["s1", "s2", "s3"].contains(&b.as_str()));
    }

    #[test]
    fn test_select_duel_pair_too_few() {
        let mut pool = CandidatePool::new();
        pool.add_candidate(make_candidate("only_one"));

        let mut rng = StdRng::seed_from_u64(0);
        assert!(pool.select_duel_pair(&mut rng).is_none());
    }
}
