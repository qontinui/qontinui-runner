//! Bridge between the comparison system and the meta-optimizer.
//!
//! Converts comparison run winners into pending recommendations, and allows
//! triggering comparison runs to validate recommendations before applying them.

use std::sync::Arc;
use tracing::info;

use crate::comparison::{
    axis_adjusted_confidence, axis_facts_from_entries_json, recommendation_from_entries_json,
    ComparisonStatus, BRIDGE_MIN_CONFIDENCE,
};
use crate::database::pg::PgDb;

/// Convert a completed comparison run's winner into a meta-optimizer recommendation.
///
/// Returns the recommendation ID if one was created, None otherwise.
///
/// ## What this reads, and what it no longer pretends to read
///
/// This function used to load a `ComparisonRun` carrying a `comparison_report`,
/// a `recommendation_json` and a `workflow_name` — three columns
/// `project.comparison_runs` has never had in any alembic revision, so the very
/// first statement failed `42703` and the Tauri command
/// `convert_comparison_to_recommendation` could never succeed. It also wrote a
/// `recommendation_id` / `source` pair back to two more columns that do not
/// exist. Plan
/// `2026-08-22-comparison-to-recommendation-bridge-references-columns-that-never-existed`.
///
/// The repair is *drop and derive*, not *add columns*:
///
/// * the report is the `report` column that does exist;
/// * the workflow name is a join, not a column;
/// * the recommendation is **derived from the arms the run actually stored**
///   ([`recommendation_from_entries_json`]) rather than read from a column
///   nothing in the tree would ever have written;
/// * the comparison to recommendation link lives on the RECOMMENDATION row,
///   where `create_recommendation` already records `source: "comparison"` and
///   the `comparison_id`. A second copy on the comparison side was a second
///   source of truth, so the write is deleted rather than repointed.
pub fn comparison_to_recommendation(
    pg_db: &Arc<PgDb>,
    comparison_id: &str,
) -> Result<Option<String>, String> {
    let comp_id = comparison_id.to_string();

    let (entries_json, report, status, workflow_name, variation_type) =
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(pg_db.get_comparison_run_for_bridge(&comp_id))
        })?
        .ok_or_else(|| format!("Comparison not found: {}", comp_id))?;

    let status = match status.as_str() {
        "completed" => ComparisonStatus::Completed,
        "failed" => ComparisonStatus::Failed,
        "comparing" => ComparisonStatus::Comparing,
        _ => ComparisonStatus::Running,
    };
    if status != ComparisonStatus::Completed {
        return Ok(None);
    }

    // The axis facts, derived from the arms as they were actually STORED by the
    // SAME function every writer persists them with — so what this reads can
    // never disagree with the row's own `computed_axis` / `axis_drift_class`.
    let axis_facts = axis_facts_from_entries_json(&variation_type, &entries_json);

    let Some(rec) = recommendation_from_entries_json(&entries_json) else {
        info!(
            "Comparison {} produced no derivable winner (fewer than two completed arms, \
             or no arm won more metrics than every other)",
            comp_id
        );
        return Ok(None);
    };
    if rec.confidence < BRIDGE_MIN_CONFIDENCE {
        return Ok(None);
    }

    // ---- A non-clean treatment axis may not underwrite a rollout ----
    //
    // `variation_type` is what the run DECLARED would vary; the arms are what
    // actually did. When they disagree, this comparison cannot support an
    // autonomous promotion, so its confidence is clamped below the threshold
    // that `meta_optimizer::parser::auto_apply_high_confidence` sweeps at. A
    // human can still apply it deliberately; it just no longer applies itself.
    //
    // This composes with the cap `recommendation_from_entries_json` already
    // applied: that one says "a three-metric heuristic is not an AI judgement",
    // this one says "and the arms did not vary as declared". The lower wins.
    let axis_class = axis_facts.drift_class;
    let (effective_confidence, axis_note) = axis_adjusted_confidence(rec.confidence, axis_class);
    if let Some(note) = axis_note.as_deref() {
        info!(
            "Comparison {} treatment axis: {} — {}",
            comp_id,
            axis_class.as_wire_str(),
            note
        );
    }

    let title = format!(
        "Comparison winner: {} (workflow: {})",
        rec.branch_name, workflow_name
    );
    let mut description = format!(
        "Comparison run {} identified '{}' as the winner with {:.0}% confidence.\n\nReasoning: {}",
        comp_id,
        rec.branch_name,
        rec.confidence * 100.0,
        rec.reasoning
    );
    // Record the discrepancy as a fact in the recommendation itself, so a human
    // reading it sees WHY the confidence differs from the comparison's own.
    if let Some(note) = &axis_note {
        description.push_str(&format!("\n\nTreatment-axis check: {}", note));
    }

    // `report` is the column that exists. Nothing in the tree writes it today,
    // so this is the fallback on every current row — see the plan's closing
    // risk, which keeps building a producer OUT of this defect fix.
    let evidence = report.as_deref().unwrap_or("No detailed report available");

    let recommendation = super::recommendations::create_recommendation(
        pg_db,
        "comparison",
        "config_change",
        None,
        &title,
        &description,
        None,
        Some(
            &serde_json::json!({
                "source": "comparison",
                "comparison_id": comp_id,
                "winner_branch": rec.branch_name,
                "confidence": effective_confidence,
                "declared_confidence": rec.confidence,
                "declared_variation_type": variation_type,
                // Null, not `[]`, when no axis could be computed — the same
                // absence-is-not-zero distinction the column carries.
                "computed_axis": axis_facts.computed_axis_json(),
                "axis_drift_class": axis_class.as_wire_str(),
            })
            .to_string(),
        ),
        Some(evidence),
        effective_confidence,
        None,
    )?;

    info!(
        "Created recommendation {} from comparison {} (winner: {}, confidence: {:.0}%)",
        recommendation.id,
        comp_id,
        rec.branch_name,
        effective_confidence * 100.0
    );

    Ok(Some(recommendation.id))
}

/// Build the comparison that would VALIDATE a pending recommendation.
///
/// Two arms — the workflow's current configuration, and the same workflow with
/// the recommendation applied — so a `config_change` can be measured before a
/// human promotes it rather than only after a canary has already shipped it.
///
/// ## What the candidate arm has to carry, and what it used to carry
///
/// The arms are launched through `POST /unified-workflows/{id}/run`, which — on
/// its prompt-steps path — applies the keys in
/// [`crate::mcp::unified_workflows::RUNTIME_OVERRIDE_KEYS`] out of each arm's
/// `overrides` blob and ignores everything else. This function used to put the
/// recommendation under `config_override`, which is not one of them — so the
/// candidate ran with the *baseline's* configuration, the comparison measured
/// the same thing twice, and `label` plus the inert key were all that separated
/// the arms. It never surfaced because nothing could reach this function at
/// all: its only caller was a zero-caller `#[allow(dead_code)]` wrapper.
///
/// So the recommendation's payload is TRANSLATED into a per-run override by
/// [`runtime_override_from_recommended_value`] — see there for why a
/// translation, and not a rename, is what this needs.
///
/// ## Both arms are explicit, and the run declares the axis it is moving
///
/// The baseline carries the recommendation's `current_value` as the SAME
/// override key, rather than being left bare. A bare baseline runs "whatever
/// the workflow already resolves to", which is unrecorded — and when the
/// recommended value happens to equal it (a roll-back to `traditional`, say,
/// against a workflow that is already traditional) the two arms execute
/// identical configuration while the axis classifier, seeing the key present in
/// one blob and absent from the other, reports a treatment axis anyway. Two
/// arms that differ only in a key one of them omits is the same false claim as
/// two arms that differ only in an inert key.
///
/// So: the baseline states its value, an equal pair is refused outright, and
/// the run DECLARES `architecture` rather than `custom`. That declaration is
/// what gives `classify_axis_drift` teeth — `custom` is
/// `DeclaredAxes::Unconstrained` and can never disagree with the arms, whereas
/// `architecture` pins the expected axis to `workflow_architecture` and clamps
/// the confidence of any run whose arms did not actually move it.
///
/// Returns `Ok(None)`, honestly, whenever the recommendation cannot be
/// validated this way: it is not a `config_change`; it is no longer actionable
/// (only `pending` and `canary` are); its payload does not translate to a
/// per-run override (the common case — most `config_change` recommendations
/// name a global setting, which two arms running concurrently against that same
/// setting cannot vary, so they are a canary's job); the recommended value
/// equals the current one, so there is nothing to compare; or there is no
/// workflow to benchmark against.
///
/// ## Known limits, named rather than implied
///
/// Three, all recorded as follow-ups on the plan rather than half-built here.
///
/// **The benchmark workflow may ignore the overrides entirely.**
/// `get_most_recent_workflow_id` is `ORDER BY updated_at DESC LIMIT 1` with no
/// predicate, and `run_unified_workflow` applies `overrides` only on its
/// prompt-steps path — a DAG-routed or automation-only workflow returns before
/// the apply block. Land on one of those and both arms run identical
/// configuration while the row still records a computed axis, which is the
/// measured-the-same-thing-twice shape this function otherwise closes. Fixing
/// it needs the `has_prompt_steps` predicate to be SHARED rather than
/// recomputed here: it is derived from normalized stage steps
/// (`unified_workflows.rs`), not from a column, and a second copy of that
/// derivation would be exactly the divergence this module's history is made of.
///
/// **The benchmark ignores the recommendation's own `workflow_category`.** It
/// is the most recent workflow, not one drawn from the category the
/// recommendation is scoped to, because there is no by-category lookup — so a
/// recommendation about `testing` workflows is measured on whatever was
/// updated last.
///
/// **Nothing records that a comparison validates a recommendation.** There is
/// no column for the link and therefore no dedup, so two invocations spawn two
/// independent pairs of real workflow runs. The `pending`/`canary` status guard
/// above is the only bound on that.
pub fn build_validation_comparison(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Option<crate::comparison::ComparisonConfig>, String> {
    // Look up the recommendation from PG
    let rec = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_recommendation(recommendation_id))
    })?
    .ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))?;

    // Only config_change recommendations can be validated via comparison
    if rec.recommendation_type != "config_change" {
        return Ok(None);
    }

    // Validating a recommendation nobody can act on any more spends two real
    // workflow runs for nothing. `pending` and `canary` are the two actionable
    // states — the same predicate `recommendations::apply_recommendation` uses.
    if rec.status != "pending" && rec.status != "canary" {
        info!(
            "Recommendation {} is `{}`, not actionable — no validation comparison",
            recommendation_id, rec.status
        );
        return Ok(None);
    }

    let Some(candidate) = rec
        .recommended_value
        .as_deref()
        .and_then(runtime_override_from_recommended_value)
    else {
        return Ok(None);
    };
    let Some(baseline) = rec
        .current_value
        .as_deref()
        .and_then(runtime_override_from_recommended_value)
    else {
        // Without a stated baseline the control arm would be "whatever the
        // workflow resolves to", which nothing records and nothing can check.
        return Ok(None);
    };
    // The PRE-translation keys, not the translated ones: every `architecture.*`
    // key translates to the same override, so comparing those would pass an
    // `architecture.build` baseline against an `architecture.testing` candidate
    // and launch it as though the two arms varied one thing.
    if baseline.settings_key != candidate.settings_key {
        info!(
            "Recommendation {} states `{}` and recommends `{}` — two different settings, \
             not a comparison",
            recommendation_id, baseline.settings_key, candidate.settings_key
        );
        return Ok(None);
    }
    if baseline.value == candidate.value {
        info!(
            "Recommendation {} recommends the value already in use ({}={}) — nothing to compare",
            recommendation_id, candidate.override_key, candidate.value
        );
        return Ok(None);
    }

    // Find a recent workflow to use as benchmark
    let workflow_id: Option<String> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_most_recent_workflow_id())
    })
    .ok()
    .flatten();

    let workflow_id = match workflow_id {
        Some(id) => id,
        None => return Ok(None),
    };

    // Both arms state their value for the SAME key, so the axis the classifier
    // computes is exactly that key and its values genuinely differ.
    let arm = |label: &str, value: &serde_json::Value| {
        let mut map = serde_json::Map::new();
        map.insert("label".to_string(), serde_json::json!(label));
        map.insert(candidate.override_key.clone(), value.clone());
        serde_json::Value::Object(map)
    };
    let overrides = vec![
        arm("baseline", &baseline.value),
        arm("candidate", &candidate.value),
    ];

    Ok(Some(crate::comparison::ComparisonConfig {
        workflow_id,
        run_count: 2,
        variation: crate::comparison::ComparisonVariation::Custom { overrides },
        declared_variation_type: VALIDATION_DECLARED_VARIATION.to_string(),
    }))
}

/// The `variation_type` a validation comparison DECLARES.
///
/// Not `custom`: `declared_axes("custom")` is `Unconstrained`, so the drift
/// classifier could never contradict the arms. `architecture` declares
/// [`ARCHITECTURE_OVERRIDE_KEY`] as the expected axis, which is exactly what
/// these arms move — and if a future edit stops them moving it, the run is
/// classified non-clean and its confidence is clamped instead of passing.
const VALIDATION_DECLARED_VARIATION: &str = "architecture";

/// The per-run override key an `architecture.*` recommendation translates to.
///
/// Asserted to be on [`RUNTIME_OVERRIDE_KEYS`] by
/// [`tests::the_translated_key_is_one_the_run_endpoint_applies`].
const ARCHITECTURE_OVERRIDE_KEY: &str = "workflow_architecture";

/// Translate a recommendation's `recommended_value` into the per-run override
/// that would apply it to ONE comparison arm.
///
/// ## Why this is a translation and not a pass-through
///
/// A `config_change` recommendation is authored for the CANARY path, which
/// applies it with `PgDb::set_setting(key, value)` — a global settings write.
/// Its `key` is therefore a dotted *settings* name, and its `value` is always a
/// JSON string, because both come from an AI-authored `[CONFIG_RECOMMENDATION]`
/// / `[ARCH_RECOMMENDATION]` block parsed with `get_str`
/// (`meta_optimizer::parser`). The two producers emit:
///
/// * `{"key": "architecture.<workflow_category>", "value": "<architecture>"}`
/// * `{"key": "<architecture>.<parameter>",       "value": "<value>"}`
///
/// A comparison arm is a different namespace: `POST /unified-workflows/{id}/run`
/// applies only [`RUNTIME_OVERRIDE_KEYS`], each with its own typed accessor.
/// Neither producer's key is in it, so passing `key` through verbatim would
/// return `None` for every recommendation the system actually makes — and
/// stripping the prefix hopefully would be worse: `<architecture>.<parameter>`
/// carries an OPEN, AI-authored parameter vocabulary (the parser's own fixture
/// is `max_total_iterations`, which is not `max_iterations`), and the values are
/// strings that the typed accessors would silently drop, leaving two arms
/// identical but for their label while the axis classifier called the run clean.
///
/// So exactly one form is translated, and it is translated by PARSING rather
/// than by renaming:
///
/// * `architecture.*` -> [`ARCHITECTURE_OVERRIDE_KEY`], with the value round-
///   tripped through [`WorkflowArchitecture`]. A value that does not name an
///   architecture is refused, so an unapplicable override can never reach an arm.
///
/// Everything else returns `None`: a recommendation that only a global setting
/// can express cannot be validated by two arms running concurrently against
/// that same global setting, and saying so is the honest answer. Widening this
/// needs the per-run override namespace to be able to express what
/// `set_setting` applies — a feature, not a rename.
fn runtime_override_from_recommended_value(recommended_value: &str) -> Option<TranslatedOverride> {
    let payload: serde_json::Value = serde_json::from_str(recommended_value).ok()?;
    let key = payload.get("key")?.as_str()?;
    let value = payload.get("value")?;

    let category = key.strip_prefix("architecture.")?;
    if category.is_empty() {
        return None;
    }
    // Round-trip through the target type. This is what makes the arm's override
    // APPLICABLE rather than merely well-named: the run endpoint parses the same
    // value with the same `from_value`, so anything that survives here survives
    // there, and anything that would be silently dropped there is refused here.
    let architecture: crate::agentic_verification::WorkflowArchitecture =
        serde_json::from_value(value.clone()).ok()?;
    Some(TranslatedOverride {
        settings_key: key.to_string(),
        override_key: ARCHITECTURE_OVERRIDE_KEY.to_string(),
        value: serde_json::to_value(architecture).ok()?,
    })
}

/// One half of a recommendation, translated into a per-run override.
///
/// `settings_key` is retained because the two halves of a recommendation must
/// name the SAME setting to be a comparison of one thing. Every
/// `architecture.*` key translates to the one `override_key`, so comparing the
/// TRANSLATED keys is a tautology — `architecture.build` against
/// `architecture.testing` would pass it — and only the pre-translation key can
/// catch that.
#[derive(Debug, Clone, PartialEq)]
struct TranslatedOverride {
    settings_key: String,
    override_key: String,
    value: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::{runtime_override_from_recommended_value, ARCHITECTURE_OVERRIDE_KEY};
    use crate::mcp::unified_workflows::RUNTIME_OVERRIDE_KEYS;
    use serde_json::json;

    /// The translation is only honest if the key it produces is one the run
    /// endpoint actually applies.
    #[test]
    fn the_translated_key_is_one_the_run_endpoint_applies() {
        assert!(
            RUNTIME_OVERRIDE_KEYS.contains(&ARCHITECTURE_OVERRIDE_KEY),
            "`{}` is not a per-run override, so a translated arm would carry an inert key",
            ARCHITECTURE_OVERRIDE_KEY
        );
    }

    /// The exact payload `meta_optimizer::parser` writes for an architecture
    /// recommendation — `{"key": "architecture.<category>", "value":
    /// "<architecture>"}` with the three tokens `architecture_optimizer`'s
    /// prompt names.
    #[test]
    fn a_real_architecture_recommendation_translates() {
        for architecture in [
            "traditional",
            "agentic_verification",
            "multi_agent_pipeline",
        ] {
            let payload = json!({"key": "architecture.testing", "value": architecture}).to_string();
            let translated = runtime_override_from_recommended_value(&payload)
                .unwrap_or_else(|| panic!("`{}` should translate", architecture));
            assert_eq!(translated.override_key, ARCHITECTURE_OVERRIDE_KEY);
            assert_eq!(translated.value, json!(architecture));
            assert_eq!(
                translated.settings_key, "architecture.testing",
                "the pre-translation key is what distinguishes two categories"
            );
        }
    }

    /// The OTHER producer in `parser.rs` writes `{"key":
    /// "<architecture>.<parameter>"}` over an open, AI-authored parameter
    /// vocabulary, with a string value the run endpoint's typed accessors would
    /// silently drop. It is refused rather than renamed — the parser's own
    /// fixture parameter is `max_total_iterations`, which is not the override
    /// key `max_iterations`, and guessing would produce two identical arms.
    #[test]
    fn a_settings_only_recommendation_is_not_validatable() {
        for key in [
            "traditional.max_total_iterations",
            "traditional.max_iterations",
            "agentic_verification.some_knob",
        ] {
            let payload = json!({"key": key, "value": "12"}).to_string();
            assert!(
                runtime_override_from_recommended_value(&payload).is_none(),
                "`{}` names a global setting, which two concurrent arms cannot vary",
                key
            );
        }
    }

    /// A value that does not name an architecture is refused HERE, because the
    /// run endpoint would drop it silently THERE — leaving a candidate arm that
    /// differs from the baseline only by its label.
    #[test]
    fn an_unparseable_architecture_value_is_refused_not_passed_through() {
        for value in [
            json!("Traditional"),
            json!("no_such_arch"),
            json!(3),
            json!(null),
        ] {
            let payload = json!({"key": "architecture.testing", "value": value}).to_string();
            assert!(
                runtime_override_from_recommended_value(&payload).is_none(),
                "{:?} does not deserialize into WorkflowArchitecture",
                value
            );
        }
    }

    /// Both halves of a recommendation go through the same translation, so a
    /// `current_value` and a `recommended_value` for the same recommendation
    /// yield the same key and different values — which is what makes the two
    /// arms a comparison rather than a pair of labels.
    #[test]
    fn current_and_recommended_translate_to_one_key_and_two_values() {
        let current = json!({"key": "architecture.testing", "value": "traditional"}).to_string();
        let recommended =
            json!({"key": "architecture.testing", "value": "multi_agent_pipeline"}).to_string();
        let base = runtime_override_from_recommended_value(&current).expect("baseline");
        let cand = runtime_override_from_recommended_value(&recommended).expect("candidate");
        assert_eq!(base.override_key, cand.override_key);
        assert_eq!(base.settings_key, cand.settings_key);
        assert_ne!(
            base.value, cand.value,
            "equal values are refused by build_validation_comparison — there is nothing to compare"
        );

        // Two DIFFERENT categories translate to the same override key, so only
        // the settings key can tell them apart — which is why
        // `build_validation_comparison` compares that one and not the other.
        let other_category =
            json!({"key": "architecture.build", "value": "multi_agent_pipeline"}).to_string();
        let other = runtime_override_from_recommended_value(&other_category).expect("other");
        assert_eq!(
            other.override_key, cand.override_key,
            "comparing translated keys would call these one axis"
        );
        assert_ne!(
            other.settings_key, cand.settings_key,
            "comparing settings keys correctly calls them two"
        );
    }

    /// The declared `variation_type` must be one the drift classifier can
    /// CONTRADICT. `custom` is `Unconstrained` and never disagrees with the
    /// arms, so declaring it would leave the classifier no way to catch a
    /// validation comparison whose arms stopped moving the architecture.
    #[test]
    fn the_declared_variation_pins_the_axis_these_arms_move() {
        match crate::comparison::declared_axes(super::VALIDATION_DECLARED_VARIATION) {
            crate::comparison::DeclaredAxes::Exact(paths) => assert!(
                paths.contains(ARCHITECTURE_OVERRIDE_KEY),
                "`{}` declares {:?}, which does not include the key these arms move",
                super::VALIDATION_DECLARED_VARIATION,
                paths
            ),
            other => panic!(
                "`{}` declares {:?}, which the classifier cannot contradict",
                super::VALIDATION_DECLARED_VARIATION,
                other
            ),
        }
    }

    /// The comparison-sourced recommendation `comparison_to_recommendation`
    /// itself writes carries no `key` at all, so the other direction of this
    /// bridge cannot feed back into this one.
    #[test]
    fn a_payload_outside_the_grammar_is_not_validatable() {
        assert!(runtime_override_from_recommended_value("not json").is_none());
        assert!(runtime_override_from_recommended_value(
            &json!({"source": "comparison", "comparison_id": "cmp-1", "winner_branch": "b"})
                .to_string()
        )
        .is_none());
        assert!(
            runtime_override_from_recommended_value(
                &json!({"key": "architecture.testing"}).to_string()
            )
            .is_none(),
            "a key with no value is not an override"
        );
        assert!(
            runtime_override_from_recommended_value(
                &json!({"key": "architecture.", "value": "traditional"}).to_string()
            )
            .is_none(),
            "an empty category is not the `architecture.<category>` grammar"
        );
    }
}
