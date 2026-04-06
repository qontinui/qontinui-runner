//! Trace adapters: convert raw span events and pipeline traces into
//! algorithm-ready formats for prompt optimization.
//!
//! Inspired by agent-lightning's TraceToMessages and TraceToTriplet adapters.
//! These converters bridge the gap between raw execution data and the structured
//! inputs that optimization algorithms (APO, PDO) require.

use serde::{Deserialize, Serialize};

use crate::meta_optimizer::span_instrumentation::SpanEvent;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// A (input, output, reward) triple extracted from execution traces.
///
/// Used by APO-style algorithms for textual gradient computation:
/// the algorithm critiques why the output scored poorly given the input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTriplet {
    pub input: String,
    pub output: String,
    pub reward: f64,
}

/// A contrastive (prompt, good_output, bad_output) triple.
///
/// Used for preference-based learning: given the same prompt context,
/// one output scored well and another scored poorly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContrastiveTriplet {
    pub prompt: String,
    pub good_output: String,
    pub bad_output: String,
    pub reward_delta: f64,
}

// ---------------------------------------------------------------------------
// TraceToMessages adapter
// ---------------------------------------------------------------------------

/// Converts span events into MessageTriplet sequences.
///
/// Pairs Message events (role=user → input, role=assistant → output) with
/// the closest Reward event at the same or nearest step_index.
pub struct TraceToMessages;

impl TraceToMessages {
    /// Convert span events into message triplets.
    ///
    /// Groups events by step_index, looking for (user message, assistant message, reward)
    /// triples. Steps without all three components are skipped.
    pub fn convert(events: &[SpanEvent]) -> Vec<MessageTriplet> {
        let mut triplets = Vec::new();

        // Group events by step_index
        let max_step = events.iter().map(|e| e.step_index()).max().unwrap_or(0);

        // Collect all rewards for fallback matching
        let rewards: Vec<(usize, f64)> = events
            .iter()
            .filter_map(|e| match e {
                SpanEvent::Reward {
                    value, step_index, ..
                } => Some((*step_index, *value)),
                _ => None,
            })
            .collect();

        // Default reward if none found
        let default_reward = rewards
            .iter()
            .map(|(_, v)| *v)
            .last()
            .unwrap_or(0.0);

        for step in 0..=max_step {
            let mut input = None;
            let mut output = None;
            let mut reward = None;

            for event in events {
                if event.step_index() != step {
                    continue;
                }
                match event {
                    SpanEvent::Message { role, content, .. } => {
                        if role == "user" || role == "system" {
                            input = Some(content.clone());
                        } else if role == "assistant" {
                            output = Some(content.clone());
                        }
                    }
                    SpanEvent::Reward { value, .. } => {
                        reward = Some(*value);
                    }
                    _ => {}
                }
            }

            if let (Some(inp), Some(out)) = (input, output) {
                // Use step reward, or nearest reward, or default
                let r = reward.unwrap_or_else(|| {
                    nearest_reward(&rewards, step).unwrap_or(default_reward)
                });
                triplets.push(MessageTriplet {
                    input: inp,
                    output: out,
                    reward: r,
                });
            }
        }

        triplets
    }
}

/// Find the reward at the nearest step_index.
fn nearest_reward(rewards: &[(usize, f64)], target_step: usize) -> Option<f64> {
    rewards
        .iter()
        .min_by_key(|(step, _)| (*step as i64 - target_step as i64).unsigned_abs())
        .map(|(_, v)| *v)
}

// ---------------------------------------------------------------------------
// TraceToTriplet adapter
// ---------------------------------------------------------------------------

/// Converts span events into contrastive triplets by pairing high-reward
/// and low-reward outputs from the same or similar prompts.
pub struct TraceToTriplet;

impl TraceToTriplet {
    /// Convert message triplets into contrastive pairs.
    ///
    /// Takes the output of `TraceToMessages::convert` and pairs the highest-reward
    /// outputs with the lowest-reward outputs. Requires at least 2 triplets with
    /// different reward values.
    pub fn convert(triplets: &[MessageTriplet]) -> Vec<ContrastiveTriplet> {
        if triplets.len() < 2 {
            return Vec::new();
        }

        let mut sorted = triplets.to_vec();
        sorted.sort_by(|a, b| b.reward.partial_cmp(&a.reward).unwrap_or(std::cmp::Ordering::Equal));

        let mut contrastive = Vec::new();

        // Pair top half with bottom half
        let mid = sorted.len() / 2;
        let good_half = &sorted[..mid];
        let bad_half = &sorted[mid..];

        for (good, bad) in good_half.iter().zip(bad_half.iter().rev()) {
            if (good.reward - bad.reward).abs() < f64::EPSILON {
                continue; // Skip if rewards are identical
            }
            contrastive.push(ContrastiveTriplet {
                prompt: good.input.clone(),
                good_output: good.output.clone(),
                bad_output: bad.output.clone(),
                reward_delta: good.reward - bad.reward,
            });
        }

        contrastive
    }
}

// ---------------------------------------------------------------------------
// Algorithm trait
// ---------------------------------------------------------------------------

/// Pluggable optimization algorithm abstraction.
///
/// Implementations receive trace data and produce improved prompt variants.
/// This trait allows future algorithms beyond APO and PDO to be plugged in
/// without changing the pipeline orchestrator.
pub trait OptimizationAlgorithm: Send + Sync {
    /// Algorithm name for observability.
    fn name(&self) -> &'static str;

    /// Run the algorithm on the given traces and produce improved prompts.
    ///
    /// Returns a list of candidate prompt strings.
    fn run(
        &self,
        agent_type: &str,
        triplets: &[MessageTriplet],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, String>> + Send + '_>,
    >;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_events() -> Vec<SpanEvent> {
        vec![
            SpanEvent::Message {
                role: "user".to_string(),
                content: "Fix the login bug".to_string(),
                step_index: 0,
            },
            SpanEvent::Message {
                role: "assistant".to_string(),
                content: "I'll update auth.rs".to_string(),
                step_index: 0,
            },
            SpanEvent::Reward {
                value: 0.8,
                metric: "task_completion".to_string(),
                step_index: 0,
            },
            SpanEvent::Message {
                role: "user".to_string(),
                content: "Now add tests".to_string(),
                step_index: 1,
            },
            SpanEvent::Message {
                role: "assistant".to_string(),
                content: "Added test_auth.rs".to_string(),
                step_index: 1,
            },
            SpanEvent::Reward {
                value: 0.3,
                metric: "task_completion".to_string(),
                step_index: 1,
            },
        ]
    }

    #[test]
    fn test_trace_to_messages_basic() {
        let events = make_events();
        let triplets = TraceToMessages::convert(&events);
        assert_eq!(triplets.len(), 2);
        assert_eq!(triplets[0].input, "Fix the login bug");
        assert_eq!(triplets[0].output, "I'll update auth.rs");
        assert!((triplets[0].reward - 0.8).abs() < f64::EPSILON);
        assert!((triplets[1].reward - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trace_to_messages_empty() {
        let triplets = TraceToMessages::convert(&[]);
        assert!(triplets.is_empty());
    }

    #[test]
    fn test_trace_to_messages_missing_reward() {
        let events = vec![
            SpanEvent::Message {
                role: "user".to_string(),
                content: "Do something".to_string(),
                step_index: 0,
            },
            SpanEvent::Message {
                role: "assistant".to_string(),
                content: "Done".to_string(),
                step_index: 0,
            },
            // No reward at step 0 — should use default (0.0)
        ];
        let triplets = TraceToMessages::convert(&events);
        assert_eq!(triplets.len(), 1);
        assert!((triplets[0].reward - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trace_to_messages_nearest_reward() {
        let events = vec![
            SpanEvent::Message {
                role: "user".to_string(),
                content: "Task".to_string(),
                step_index: 0,
            },
            SpanEvent::Message {
                role: "assistant".to_string(),
                content: "Response".to_string(),
                step_index: 0,
            },
            // Reward only at step 2 — should be used as nearest
            SpanEvent::Reward {
                value: 0.95,
                metric: "accuracy".to_string(),
                step_index: 2,
            },
        ];
        let triplets = TraceToMessages::convert(&events);
        assert_eq!(triplets.len(), 1);
        assert!((triplets[0].reward - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trace_to_triplet_basic() {
        let triplets = vec![
            MessageTriplet {
                input: "Fix bug".to_string(),
                output: "Great fix".to_string(),
                reward: 0.9,
            },
            MessageTriplet {
                input: "Fix bug".to_string(),
                output: "Bad fix".to_string(),
                reward: 0.2,
            },
        ];
        let contrastive = TraceToTriplet::convert(&triplets);
        assert_eq!(contrastive.len(), 1);
        assert_eq!(contrastive[0].good_output, "Great fix");
        assert_eq!(contrastive[0].bad_output, "Bad fix");
        assert!((contrastive[0].reward_delta - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trace_to_triplet_insufficient_data() {
        let triplets = vec![MessageTriplet {
            input: "Task".to_string(),
            output: "Response".to_string(),
            reward: 0.5,
        }];
        assert!(TraceToTriplet::convert(&triplets).is_empty());
    }

    #[test]
    fn test_trace_to_triplet_identical_rewards_skipped() {
        let triplets = vec![
            MessageTriplet {
                input: "A".to_string(),
                output: "B".to_string(),
                reward: 0.5,
            },
            MessageTriplet {
                input: "C".to_string(),
                output: "D".to_string(),
                reward: 0.5,
            },
        ];
        assert!(TraceToTriplet::convert(&triplets).is_empty());
    }

    #[test]
    fn test_nearest_reward() {
        let rewards = vec![(0, 0.1), (3, 0.5), (7, 0.9)];
        assert!((nearest_reward(&rewards, 2).unwrap() - 0.5).abs() < f64::EPSILON);
        assert!((nearest_reward(&rewards, 6).unwrap() - 0.9).abs() < f64::EPSILON);
        assert!((nearest_reward(&rewards, 0).unwrap() - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_message_triplet_serialization() {
        let t = MessageTriplet {
            input: "in".to_string(),
            output: "out".to_string(),
            reward: 0.75,
        };
        let json = serde_json::to_string(&t).unwrap();
        let deserialized: MessageTriplet = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input, "in");
        assert!((deserialized.reward - 0.75).abs() < f64::EPSILON);
    }
}
