-- Knowledge Graph Operations
-- Covers: workflow_versions, step_finding_links, step_provenance,
-- generation_pipeline_events, rule_influence_log, cross_run_patterns

--: WorkflowVersionRow(parent_version_id?, generation_task_run_id?, diff_summary?, diff_json?, trigger?)
--: StepFindingLinkRow()
--: StepProvenanceRow(workflow_version_id?, ui_bridge_event_ids?)
--: PipelineEventRow(payload?, duration_ms?, token_count?, validation_errors_before?, validation_errors_after?)
--: PhaseStatsRow()
--: RuleInfluenceRow(evidence?)
--: IneffectiveRuleRow()
--: CrossRunPatternRow(workflow_name?, first_seen_task_run_id?, last_seen_task_run_id?, first_seen_at?, last_seen_at?, affected_components?, pattern_data?, resolved_by_fix_id?)
--: RecurringFindingRow(workflow_name?)

-- ============================================================================
-- Workflow Versions
-- ============================================================================

--! insert_workflow_version
INSERT INTO workflow_versions (id, workflow_id, version_number, parent_version_id,
    generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at)
VALUES (:id, :workflow_id, :version_number, :parent_version_id,
    :generation_task_run_id, :workflow_json, :diff_summary, :diff_json, :trigger, NOW());

--! get_workflow_versions : WorkflowVersionRow
SELECT id, workflow_id, version_number, parent_version_id,
       generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at
FROM workflow_versions
WHERE workflow_id = :workflow_id
ORDER BY version_number DESC;

--! get_latest_workflow_version : WorkflowVersionRow
SELECT id, workflow_id, version_number, parent_version_id,
       generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at
FROM workflow_versions
WHERE workflow_id = :workflow_id
ORDER BY version_number DESC
LIMIT 1;

-- ============================================================================
-- Step Finding Links
-- ============================================================================

--! insert_step_finding_link
INSERT INTO step_finding_links (id, task_run_id, step_name, step_index, finding_id,
    link_type, confidence, created_at)
VALUES (:id, :task_run_id, :step_name, :step_index, :finding_id,
    :link_type, :confidence, NOW());

--! get_findings_for_step : StepFindingLinkRow
SELECT id, task_run_id, step_name, step_index, finding_id, link_type, confidence, created_at
FROM step_finding_links
WHERE task_run_id = :task_run_id AND step_name = :step_name;

--! get_steps_for_finding : StepFindingLinkRow
SELECT id, task_run_id, step_name, step_index, finding_id, link_type, confidence, created_at
FROM step_finding_links
WHERE finding_id = :finding_id;

-- ============================================================================
-- Step Provenance
-- ============================================================================

--! insert_step_provenance
INSERT INTO step_provenance (id, workflow_id, workflow_version_id, step_name, step_index,
    phase, generating_agent, generation_iteration, original_step_json, final_step_json,
    ui_bridge_event_ids, created_at)
VALUES (:id, :workflow_id, :workflow_version_id, :step_name, :step_index,
    :phase, :generating_agent, :generation_iteration, :original_step_json, :final_step_json,
    :ui_bridge_event_ids, NOW());

--! get_provenance_for_workflow : StepProvenanceRow
SELECT id, workflow_id, workflow_version_id, step_name, step_index, phase,
       generating_agent, generation_iteration, original_step_json, final_step_json,
       ui_bridge_event_ids, created_at
FROM step_provenance
WHERE workflow_id = :workflow_id
ORDER BY created_at DESC;

--! get_provenance_by_agent : StepProvenanceRow
SELECT id, workflow_id, workflow_version_id, step_name, step_index, phase,
       generating_agent, generation_iteration, original_step_json, final_step_json,
       ui_bridge_event_ids, created_at
FROM step_provenance
WHERE generating_agent = :generating_agent
ORDER BY created_at DESC
LIMIT :limit;

-- ============================================================================
-- Generation Pipeline Events
-- ============================================================================

--! insert_pipeline_event
INSERT INTO generation_pipeline_events (id, task_run_id, workflow_id, event_type, phase,
    iteration, payload, duration_ms, token_count, validation_errors_before,
    validation_errors_after, created_at)
VALUES (:id, :task_run_id, :workflow_id, :event_type, :phase,
    :iteration, :payload, :duration_ms, :token_count, :validation_errors_before,
    :validation_errors_after, NOW());

--! get_pipeline_events : PipelineEventRow
SELECT id, task_run_id, workflow_id, event_type, phase, iteration, payload,
       duration_ms, token_count, validation_errors_before, validation_errors_after, created_at
FROM generation_pipeline_events
WHERE task_run_id = :task_run_id
ORDER BY created_at;

--! get_phase_stats : PhaseStatsRow
SELECT phase,
       COUNT(*) as event_count,
       AVG(duration_ms) as avg_duration_ms,
       SUM(COALESCE(token_count, 0)) as total_token_count,
       AVG(validation_errors_before) as avg_errors_before,
       AVG(validation_errors_after) as avg_errors_after
FROM generation_pipeline_events
WHERE workflow_id = :workflow_id
  AND event_type = 'phase_end'
GROUP BY phase;

-- ============================================================================
-- Rule Influence Log
-- ============================================================================

--! insert_rule_influence
INSERT INTO rule_influence_log (id, rule_id, task_run_id, workflow_id,
    influence_type, evidence, phase, created_at)
VALUES (:id, :rule_id, :task_run_id, :workflow_id,
    :influence_type, :evidence, :phase, NOW());

--! get_rule_influences : RuleInfluenceRow
SELECT id, rule_id, task_run_id, workflow_id, influence_type, evidence, phase, created_at
FROM rule_influence_log
WHERE rule_id = :rule_id
ORDER BY created_at DESC;

--! get_ineffective_rules : IneffectiveRuleRow
SELECT rule_id,
       COUNT(*) FILTER (WHERE influence_type = 'no_effect') as no_effect_count,
       COUNT(*) FILTER (WHERE influence_type = 'prevented_error') as prevented_error_count,
       COUNT(*) as total_loaded_count
FROM rule_influence_log
GROUP BY rule_id
HAVING COUNT(*) FILTER (WHERE influence_type = 'no_effect') >= :min_no_effect_count
   AND COUNT(*) FILTER (WHERE influence_type = 'prevented_error') = 0;

-- ============================================================================
-- Cross-Run Patterns
-- ============================================================================

--! upsert_cross_run_pattern
INSERT INTO cross_run_patterns (id, pattern_type, signature_hash, workflow_name,
    first_seen_task_run_id, last_seen_task_run_id, first_seen_at, last_seen_at,
    affected_components, pattern_data, created_at, updated_at)
VALUES (:id, :pattern_type, :signature_hash, :workflow_name,
    :task_run_id, :task_run_id, NOW(), NOW(),
    :affected_components, :pattern_data, NOW(), NOW())
ON CONFLICT (pattern_type, signature_hash) DO UPDATE SET
    occurrence_count = cross_run_patterns.occurrence_count + 1,
    last_seen_task_run_id = :task_run_id,
    last_seen_at = NOW(),
    affected_components = COALESCE(:affected_components, cross_run_patterns.affected_components),
    pattern_data = COALESCE(:pattern_data, cross_run_patterns.pattern_data),
    updated_at = NOW();

--! get_active_patterns : CrossRunPatternRow
SELECT id, pattern_type, signature_hash, workflow_name, occurrence_count,
       first_seen_task_run_id, last_seen_task_run_id, first_seen_at, last_seen_at,
       affected_components, pattern_data, status, resolved_by_fix_id, created_at, updated_at
FROM cross_run_patterns
WHERE status = 'active'
  AND (workflow_name = :workflow_name OR :workflow_name IS NULL)
ORDER BY occurrence_count DESC;

--! resolve_pattern
UPDATE cross_run_patterns
SET status = 'resolved', resolved_by_fix_id = :fix_id, updated_at = NOW()
WHERE id = :id;

--! detect_recurring_findings : RecurringFindingRow
SELECT :workflow_name as workflow_name, f.signature_hash, COUNT(DISTINCT f.task_run_id) as occurrence_count
FROM task_run_findings f
JOIN task_runs t ON f.task_run_id = t.id
WHERE t.workflow_name = :workflow_name
  AND f.signature_hash IS NOT NULL
GROUP BY f.signature_hash
HAVING COUNT(DISTINCT f.task_run_id) >= :min_occurrences;
