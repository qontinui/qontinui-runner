//! Content fingerprints for workflow-step replay.
//!
//! Both durability journals key a step's cached outcome by POSITION, never by
//! content:
//!
//! | Journal | Replay key |
//! |---|---|
//! | `project.workflow_event_log` (DAG engine) | `(execution_id, node_id)` |
//! | `project.workflow_step_checkpoints` (unified engine) | `(execution_id, phase, iteration, step_index, stage_index)` |
//!
//! Neither key contains one byte of what the step actually *does*, so editing a
//! node's prompt and re-running under the same `execution_id` serves the stale
//! cached output, and a changed step list silently misaligns the positional
//! key. `step_fingerprint` (alembic `wf_resume_fingerprint_01`, both journals,
//! nullable TEXT, NON-KEY) is the content half: the row is still located by the
//! existing key, and the fingerprint is then COMPARED.
//!
//! # The consumer contract
//!
//! **A stored fingerprint that is NULL, empty, or different from the freshly
//! computed one is a cache MISS — the step re-executes.** NULL is never a
//! wildcard. Every row written before this shipped has no fingerprint, so the
//! first resume after upgrade re-executes and rewrites those rows with one;
//! that is the intended, bounded cost.
//!
//! # What is IN the fingerprint, and why
//!
//! Both failure directions are real. A field silently *omitted* is a stale hit
//! — a silently wrong answer served as if it were fresh. A volatile field
//! wrongly *included* makes every lookup miss, which silently disables replay
//! and re-bills the most expensive steps in the product. So the set is fixed
//! and explicit:
//!
//! 1. **Algorithm tag** (`sf1`, the digest prefix). Changing the input set
//!    changes the tag, so old digests become a clean global miss instead of
//!    colliding with new ones under different semantics.
//! 2. **Resolved prompt text** — the exact prompt body the step will send,
//!    after any context resolution the caller has already applied. A prompt
//!    edit is the headline defect this closes.
//! 3. **Model** and **provider**, after override resolution (step-level beats
//!    phase-level, exactly as the executors resolve them). Re-running the same
//!    prompt against a different model is different work.
//! 4. **Step / node definition version** — the canonical JSON of the step's own
//!    `ExecutionStepConfig` (or, for a consolidated AI session, the ordered
//!    list of the configs that compose it). This is deliberately the WHOLE
//!    authored config: command text, working directory, check type, timeouts,
//!    retry policy, UI-bridge target, tool policy, and every field added
//!    later. Hashing the whole struct means a field added next year is covered
//!    by construction rather than by someone remembering to extend a list.
//!    It is scoped to ONE step, which is what makes "editing step 5 does not
//!    invalidate steps 1-4" true.
//! 5. **Upstream input values** — the resolved values of the step's DECLARED
//!    `inputs` map (`name -> value`), canonically ordered by name.
//! 6. **Slice identity** where the journal key is not self-describing — a DAG
//!    loop body's loop id and iteration number.
//!
//! # What is deliberately OUT, and why
//!
//! * **`execution_id` / `task_run_id` / any run or session id, and the
//!   checkpoint's own `id`.** The fingerprint exists to match ACROSS runs.
//!   `StepCheckpoint::id` is a fresh `Uuid::new_v4()` on every construction, so
//!   including it would miss even within a single run.
//! * **Timestamps, durations, cursors, retry attempt counts.** Same reason:
//!   guaranteed to differ, guaranteed to disable replay.
//! * **The step's own recorded output / `result_json`.** That is the value the
//!   fingerprint GATES, not an input to it.
//! * **The verification-failure narrative injected into agentic prompts**
//!   (`failure_context`) and the resume progress-marker context. Verification
//!   is deliberately non-replayable, so a resumed run re-observes the world and
//!   produces a *different* failure list even when nothing about the workflow
//!   changed. Hashing it would miss on essentially every crash-recovery resume
//!   and re-bill the agentic session — the single most expensive step in the
//!   product, and precisely the harm this plan exists to close. It is runtime
//!   narration wrapped around a definition that did not change.
//! * **Transitive upstream outputs not named in `inputs`.** One nondeterministic
//!   upstream step (a shell command echoing a timestamp) would invalidate every
//!   node downstream of it on every resume. `inputs` is the declared data-flow
//!   contract; that is the line.
//!   *Known gap, stated rather than hidden:* a prompt that consumes an upstream
//!   value through a mechanism OTHER than `inputs` (an implicit `$nodeId`
//!   substitution performed inside the executor) is covered only to the extent
//!   the substitution has already been applied to `prompt_content` by the time
//!   the fingerprint is taken.
//! * **Environment variables, credentials, tokens.** The digest is written to
//!   the database; nothing secret is hashed into a value that must never be a
//!   plaintext oracle. (The digest is one-way, but the pre-image set stays
//!   small enough to be guessable, so this is not a theoretical concern.)
//! * **Sibling steps' definitions.** Required for the per-step scoping in (4).
//!
//! # Determinism
//!
//! Every map input is ordered (`BTreeMap`, and JSON objects are re-emitted with
//! sorted keys), so `HashMap` iteration order can never reach the hasher. Every
//! component is fed length-prefixed and field-name-tagged, so no concatenation
//! of two fields can be confused with a different split of the same bytes.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::step_executor::executor_types::ExecutionStepConfig;

/// Algorithm tag prefixed to every digest.
///
/// Bump this whenever the SET of hashed inputs changes. Old rows then miss
/// cleanly (and are rewritten) instead of comparing equal under semantics that
/// no longer hold.
pub const STEP_FINGERPRINT_ALGO: &str = "sf1";

/// Accumulator for the inputs that determine one step's output.
///
/// Build it, then call [`StepFingerprint::digest`]. Producer and consumer MUST
/// build it the same way for the same step — the adapters below
/// ([`config_fingerprint`], [`configs_fingerprint`]) exist so both sides call
/// one function rather than assembling the parts twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepFingerprint {
    prompt: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    /// Canonical JSON of each contributing step/node definition, in definition
    /// order (a consolidated AI session has more than one).
    definitions: Vec<String>,
    /// Declared upstream inputs, `name -> canonical JSON value`.
    upstream: BTreeMap<String, String>,
    /// Extra key identity the journal key does not already carry.
    slice: BTreeMap<String, String>,
}

impl StepFingerprint {
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolved prompt body this step will send.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// The resolved prompt body, when the caller has an `Option`.
    pub fn with_prompt_opt(mut self, prompt: Option<&str>) -> Self {
        self.prompt = prompt.map(str::to_string);
        self
    }

    /// The model id after override resolution.
    pub fn with_model(mut self, model: Option<&str>) -> Self {
        self.model = model.map(str::to_string);
        self
    }

    /// The provider id after override resolution.
    pub fn with_provider(mut self, provider: Option<&str>) -> Self {
        self.provider = provider.map(str::to_string);
        self
    }

    /// Append one step/node definition. Order is significant and is the
    /// definition order, which is itself part of what determines the output.
    ///
    /// A definition that fails to serialize contributes the literal
    /// `"<unserializable>"` rather than being dropped: dropping it would make
    /// two different definitions hash the same, which is the stale-hit
    /// direction. A constant makes them collide with each other only, and any
    /// real change elsewhere in the input set still separates them.
    pub fn with_definition<T: Serialize>(mut self, definition: &T) -> Self {
        let rendered = match serde_json::to_value(definition) {
            Ok(v) => canonical_json(&v),
            Err(_) => "<unserializable>".to_string(),
        };
        self.definitions.push(rendered);
        self
    }

    /// Append several definitions in order (a consolidated AI session).
    pub fn with_definitions<T: Serialize>(mut self, definitions: &[T]) -> Self {
        for d in definitions {
            self = self.with_definition(d);
        }
        self
    }

    /// One resolved upstream input value.
    pub fn with_upstream(mut self, name: impl Into<String>, value: &serde_json::Value) -> Self {
        self.upstream.insert(name.into(), canonical_json(value));
        self
    }

    /// Every resolved upstream input value. Ordering of the source map is
    /// irrelevant — the accumulator is a `BTreeMap`.
    pub fn with_upstream_values<'a, I>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = (&'a String, &'a serde_json::Value)>,
    {
        for (k, v) in values {
            self.upstream.insert(k.clone(), canonical_json(v));
        }
        self
    }

    /// Extra identity the journal key does not carry (loop id, iteration, ...).
    pub fn with_slice(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.slice.insert(name.into(), value.into());
        self
    }

    /// The digest, as stored in `step_fingerprint`: `"sf1:<64 hex chars>"`.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();

        // Every component is tagged and length-prefixed, so no two distinct
        // input sets can produce the same byte stream by concatenation.
        feed_opt(&mut h, "prompt", self.prompt.as_deref());
        feed_opt(&mut h, "model", self.model.as_deref());
        feed_opt(&mut h, "provider", self.provider.as_deref());

        feed(
            &mut h,
            "definitions.len",
            &self.definitions.len().to_string(),
        );
        for (i, d) in self.definitions.iter().enumerate() {
            feed(&mut h, &format!("definition.{}", i), d);
        }

        feed(&mut h, "upstream.len", &self.upstream.len().to_string());
        for (k, v) in &self.upstream {
            feed(&mut h, &format!("upstream.{}", k), v);
        }

        feed(&mut h, "slice.len", &self.slice.len().to_string());
        for (k, v) in &self.slice {
            feed(&mut h, &format!("slice.{}", k), v);
        }

        format!("{}:{}", STEP_FINGERPRINT_ALGO, hex::encode(h.finalize()))
    }
}

fn feed(h: &mut Sha256, tag: &str, value: &str) {
    h.update((tag.len() as u64).to_be_bytes());
    h.update(tag.as_bytes());
    h.update((value.len() as u64).to_be_bytes());
    h.update(value.as_bytes());
}

/// `None` is fed as a distinct marker rather than as an empty string, so
/// "no model configured" and "model configured as the empty string" do not
/// collide.
fn feed_opt(h: &mut Sha256, tag: &str, value: Option<&str>) {
    match value {
        Some(v) => {
            feed(h, tag, "some");
            feed(h, tag, v);
        }
        None => feed(h, tag, "none"),
    }
}

/// Render a JSON value with object keys sorted, so map iteration order can
/// never change the digest.
///
/// `serde_json::Map` is a `BTreeMap` unless the `preserve_order` feature is on
/// somewhere in the dependency graph — this function makes the ordering
/// guarantee ours rather than a transitive feature flag's.
fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => out.push_str(&n.to_string()),
        serde_json::Value::String(s) => {
            // Reuse serde's escaping so control characters and quotes are
            // unambiguous.
            match serde_json::to_string(s) {
                Ok(escaped) => out.push_str(&escaped),
                Err(_) => {
                    out.push('"');
                    out.push_str(s);
                    out.push('"');
                }
            }
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match serde_json::to_string(k) {
                    Ok(escaped) => out.push_str(&escaped),
                    Err(_) => {
                        out.push('"');
                        out.push_str(k);
                        out.push('"');
                    }
                }
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
    }
}

// ============================================================================
// Adapters — the ONLY entry points call sites should use
// ============================================================================

/// Resolve the model for a step exactly as the executors do: a step-level
/// override beats the phase/workflow-level one.
pub fn resolved_model<'a>(
    cfg: &'a ExecutionStepConfig,
    phase_model: Option<&'a str>,
) -> Option<&'a str> {
    cfg.model.as_deref().or(phase_model)
}

/// Resolve the provider for a step, same precedence as [`resolved_model`].
pub fn resolved_provider<'a>(
    cfg: &'a ExecutionStepConfig,
    phase_provider: Option<&'a str>,
) -> Option<&'a str> {
    cfg.provider.as_deref().or(phase_provider)
}

/// Fingerprint for a single-config step (automation step, response-mode prompt,
/// DAG node).
///
/// Producer and consumer must both call THIS function with the same arguments.
///
/// # Collision with [`configs_fingerprint`] — incidental, not designed
///
/// For a step whose `prompt_content` is `None` and which carries no step-level
/// overrides (`model: None`, and likewise `provider: None`),
/// `config_fingerprint(cfg, m, p)` and `configs_fingerprint(&[cfg], m, p)`
/// currently produce BYTE-IDENTICAL digests: `with_prompt_opt(None)` feeds the
/// same `none` marker `configs_fingerprint` gets by never setting a prompt at
/// all, `resolved_model`/`resolved_provider` collapse to exactly the
/// phase-level `m`/`p` that `configs_fingerprint` feeds directly, and
/// `with_definition(cfg)` and `with_definitions(&[cfg])` hash the same single
/// canonicalised definition under the same `definitions.len = 1`.
///
/// (A step-level `model` or `provider` override breaks the collision, because
/// only `config_fingerprint` resolves it — so the property is narrower than
/// "no prompt and no model".)
///
/// This is harmless TODAY only because two other gates keep the callers apart:
/// a journal row is located by its positional key first (execution, phase,
/// iteration, stage, step index), and `journalled_step`'s `step_type` gate
/// rejects a row written by the other caller at the same index. Neither gate
/// exists because of this property.
///
/// It is an incidental consequence of how the builder skips `None` fields, NOT
/// a designed equivalence — nothing here guarantees it, and adding any field to
/// either adapter may silently end it. Do not build a caller that relies on the
/// two digests agreeing (or on their disagreeing); if you need one step and a
/// one-element session to be interchangeable, make that explicit rather than
/// inheriting it from this collision.
pub fn config_fingerprint(
    cfg: &ExecutionStepConfig,
    phase_model: Option<&str>,
    phase_provider: Option<&str>,
) -> String {
    StepFingerprint::new()
        .with_prompt_opt(cfg.prompt_content.as_deref())
        .with_model(resolved_model(cfg, phase_model))
        .with_provider(resolved_provider(cfg, phase_provider))
        .with_definition(cfg)
        .digest()
}

/// Fingerprint for a consolidated AI session, whose prompt is built from an
/// ordered list of prompt-step definitions.
///
/// The consolidated prompt is a pure function of these definitions, so hashing
/// the definitions is equivalent to hashing the built prompt AND is available
/// at the replay-lookup point, which happens before the prompt is assembled.
pub fn configs_fingerprint(
    cfgs: &[ExecutionStepConfig],
    phase_model: Option<&str>,
    phase_provider: Option<&str>,
) -> String {
    StepFingerprint::new()
        .with_model(phase_model)
        .with_provider(phase_provider)
        .with_definitions(cfgs)
        .digest()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn prompt_step(name: &str, prompt: &str, model: Option<&str>) -> ExecutionStepConfig {
        ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some(name.to_string()),
            prompt_content: Some(prompt.to_string()),
            model: model.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn digest_is_tagged_and_hex() {
        let fp = config_fingerprint(&prompt_step("a", "hello", None), None, None);
        assert!(
            fp.starts_with("sf1:"),
            "digest must carry its algo tag: {}",
            fp
        );
        assert_eq!(fp.len(), "sf1:".len() + 64);
        assert!(fp["sf1:".len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The whole point: identical inputs hit.
    #[test]
    fn identical_inputs_produce_the_same_fingerprint() {
        let a = config_fingerprint(&prompt_step("a", "hello", Some("m1")), None, None);
        let b = config_fingerprint(&prompt_step("a", "hello", Some("m1")), None, None);
        assert_eq!(a, b);
    }

    /// The headline defect: an edited prompt must NOT serve the cached output.
    #[test]
    fn changed_prompt_changes_the_fingerprint() {
        let before = config_fingerprint(&prompt_step("a", "hello", None), None, None);
        let after = config_fingerprint(&prompt_step("a", "hello!", None), None, None);
        assert_ne!(before, after, "an edited prompt must miss");
    }

    #[test]
    fn changed_model_changes_the_fingerprint() {
        let before = config_fingerprint(&prompt_step("a", "hello", Some("m1")), None, None);
        let after = config_fingerprint(&prompt_step("a", "hello", Some("m2")), None, None);
        assert_ne!(before, after, "a model swap must miss");
    }

    /// A phase-level model override is part of the resolved model, so changing
    /// it must miss for a step that does not pin its own.
    #[test]
    fn changed_phase_model_override_changes_the_fingerprint() {
        let step = prompt_step("a", "hello", None);
        let before = config_fingerprint(&step, Some("m1"), None);
        let after = config_fingerprint(&step, Some("m2"), None);
        assert_ne!(before, after);
    }

    /// ...but a step that pins its own model ignores the phase override, so the
    /// fingerprint must not move either. (Producer and consumer both resolve
    /// through `resolved_model`; if they disagreed, replay would silently die.)
    #[test]
    fn step_level_model_wins_over_phase_override() {
        let step = prompt_step("a", "hello", Some("pinned"));
        assert_eq!(
            config_fingerprint(&step, Some("m1"), None),
            config_fingerprint(&step, Some("m2"), None)
        );
        assert_eq!(resolved_model(&step, Some("m1")), Some("pinned"));
    }

    #[test]
    fn changed_provider_changes_the_fingerprint() {
        let step = prompt_step("a", "hello", None);
        let before = config_fingerprint(&step, None, Some("p1"));
        let after = config_fingerprint(&step, None, Some("p2"));
        assert_ne!(before, after);
    }

    /// A non-prompt field of the SAME step is still part of that step's
    /// definition version.
    #[test]
    fn changed_command_text_changes_the_fingerprint() {
        let mut a = ExecutionStepConfig {
            step_type: "command".to_string(),
            shell_command: Some("cargo test".to_string()),
            ..Default::default()
        };
        let before = config_fingerprint(&a, None, None);
        a.shell_command = Some("cargo test --release".to_string());
        assert_ne!(before, config_fingerprint(&a, None, None));
    }

    /// Changed upstream input value must miss.
    #[test]
    fn changed_upstream_input_changes_the_fingerprint() {
        let base = StepFingerprint::new().with_prompt("summarise");
        let before = base
            .clone()
            .with_upstream("report", &serde_json::json!("all green"))
            .digest();
        let after = base
            .with_upstream("report", &serde_json::json!("3 failures"))
            .digest();
        assert_ne!(before, after, "a changed upstream value must miss");
    }

    /// An upstream input that APPEARS must miss too — not just one that changes.
    #[test]
    fn added_upstream_input_changes_the_fingerprint() {
        let bare = StepFingerprint::new().with_prompt("summarise").digest();
        let with_input = StepFingerprint::new()
            .with_prompt("summarise")
            .with_upstream("report", &serde_json::json!("all green"))
            .digest();
        assert_ne!(bare, with_input);
    }

    /// Map iteration order must never reach the hasher.
    #[test]
    fn upstream_ordering_is_canonical() {
        let mut forward: HashMap<String, serde_json::Value> = HashMap::new();
        forward.insert("alpha".into(), serde_json::json!(1));
        forward.insert("beta".into(), serde_json::json!(2));
        forward.insert("gamma".into(), serde_json::json!(3));

        // Same logical inputs, inserted in the opposite order.
        let mut backward: HashMap<String, serde_json::Value> = HashMap::new();
        backward.insert("gamma".into(), serde_json::json!(3));
        backward.insert("beta".into(), serde_json::json!(2));
        backward.insert("alpha".into(), serde_json::json!(1));

        let a = StepFingerprint::new()
            .with_upstream_values(forward.iter())
            .digest();
        let b = StepFingerprint::new()
            .with_upstream_values(backward.iter())
            .digest();
        assert_eq!(a, b, "HashMap order must not change the fingerprint");
    }

    /// The same for object keys nested inside a definition: a `HashMap` field
    /// (`inputs`, `extract`, `ref_workflow_inputs`) must serialize canonically.
    #[test]
    fn definition_object_keys_are_canonically_ordered() {
        let mut forward = HashMap::new();
        forward.insert("a".to_string(), "n1.out".to_string());
        forward.insert("b".to_string(), "n2.out".to_string());
        forward.insert("c".to_string(), "n3.out".to_string());

        let mut backward = HashMap::new();
        backward.insert("c".to_string(), "n3.out".to_string());
        backward.insert("b".to_string(), "n2.out".to_string());
        backward.insert("a".to_string(), "n1.out".to_string());

        let one = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            inputs: Some(forward),
            ..Default::default()
        };
        let two = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            inputs: Some(backward),
            ..Default::default()
        };
        assert_eq!(
            config_fingerprint(&one, None, None),
            config_fingerprint(&two, None, None)
        );
    }

    /// Field boundaries must be unambiguous: two fields whose concatenation is
    /// equal must still fingerprint differently.
    #[test]
    fn field_boundaries_are_unambiguous() {
        let a = StepFingerprint::new()
            .with_prompt("ab")
            .with_model(Some("c"))
            .digest();
        let b = StepFingerprint::new()
            .with_prompt("a")
            .with_model(Some("bc"))
            .digest();
        assert_ne!(a, b);
    }

    /// An absent value and an empty one are different states.
    #[test]
    fn absent_and_empty_are_distinguished() {
        let absent = StepFingerprint::new().with_model(None).digest();
        let empty = StepFingerprint::new().with_model(Some("")).digest();
        assert_ne!(absent, empty);
    }

    /// Editing a LATER step must not move an earlier step's fingerprint —
    /// this is the property that keeps an edit's re-billing bounded.
    #[test]
    fn editing_a_later_step_does_not_invalidate_earlier_steps() {
        let steps_v1 = [
            prompt_step("s1", "one", None),
            prompt_step("s2", "two", None),
            prompt_step("s3", "three", None),
        ];
        let mut steps_v2 = steps_v1.clone();
        steps_v2[2] = prompt_step("s3", "three, but edited", None);

        for i in 0..2 {
            assert_eq!(
                config_fingerprint(&steps_v1[i], None, None),
                config_fingerprint(&steps_v2[i], None, None),
                "step {} must still replay after step 3 was edited",
                i
            );
        }
        assert_ne!(
            config_fingerprint(&steps_v1[2], None, None),
            config_fingerprint(&steps_v2[2], None, None),
            "the edited step itself must miss"
        );
    }

    /// A consolidated session's fingerprint covers every contributing step, in
    /// order.
    #[test]
    fn consolidated_session_covers_every_contributing_step() {
        let v1 = [
            prompt_step("s1", "one", None),
            prompt_step("s2", "two", None),
        ];
        let mut v2 = v1.clone();
        v2[1] = prompt_step("s2", "two, edited", None);
        assert_ne!(
            configs_fingerprint(&v1, None, None),
            configs_fingerprint(&v2, None, None)
        );

        // Order is significant: the consolidated prompt concatenates them.
        let reordered = [v1[1].clone(), v1[0].clone()];
        assert_ne!(
            configs_fingerprint(&v1, None, None),
            configs_fingerprint(&reordered, None, None)
        );

        // ...and it is stable for identical input.
        assert_eq!(
            configs_fingerprint(&v1, None, None),
            configs_fingerprint(&v1.clone(), None, None)
        );
    }

    /// Adding a step to a consolidated session must miss.
    #[test]
    fn consolidated_session_misses_when_a_step_is_added() {
        let v1 = [prompt_step("s1", "one", None)];
        let v2 = [
            prompt_step("s1", "one", None),
            prompt_step("s2", "two", None),
        ];
        assert_ne!(
            configs_fingerprint(&v1, None, None),
            configs_fingerprint(&v2, None, None)
        );
    }

    /// Slice identity separates two iterations of the same loop-body config.
    #[test]
    fn slice_identity_separates_loop_iterations() {
        let cfg = prompt_step("body", "do it", None);
        let it0 = StepFingerprint::new()
            .with_definition(&cfg)
            .with_slice("iteration", "0")
            .digest();
        let it1 = StepFingerprint::new()
            .with_definition(&cfg)
            .with_slice("iteration", "1")
            .digest();
        assert_ne!(it0, it1);
    }

    #[test]
    fn canonical_json_sorts_nested_object_keys() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"b":{"z":1,"a":2},"a":[1,{"q":1,"p":2}]}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":[1,{"p":2,"q":1}],"b":{"a":2,"z":1}}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        // Array order, by contrast, is meaningful and must NOT be normalised.
        let c: serde_json::Value = serde_json::from_str(r#"{"a":[2,1]}"#).unwrap();
        let d: serde_json::Value = serde_json::from_str(r#"{"a":[1,2]}"#).unwrap();
        assert_ne!(canonical_json(&c), canonical_json(&d));
    }
}
