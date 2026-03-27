--- Prompt registry operations: variant CRUD, activation, versioning.

--! get_active_prompt
SELECT id, agent_type, variant_name, prompt_content, version, is_active,
       source_recommendation_id, performance_metrics, created_at, updated_at
FROM prompt_registry
WHERE agent_type = :agent_type AND is_active = true
LIMIT 1;

--! get_prompt_by_version
SELECT id, agent_type, variant_name, prompt_content, version, is_active,
       source_recommendation_id, performance_metrics, created_at, updated_at
FROM prompt_registry
WHERE agent_type = :agent_type AND version = :version
LIMIT 1;

--! create_variant (source_recommendation_id?, performance_metrics?)
INSERT INTO prompt_registry (
    id, agent_type, variant_name, prompt_content, version,
    is_active, source_recommendation_id, performance_metrics
) VALUES (
    :id, :agent_type, :variant_name, :prompt_content, :version,
    :is_active, :source_recommendation_id, :performance_metrics
)
RETURNING id, agent_type, variant_name, version, is_active, created_at;

--! activate_variant
UPDATE prompt_registry
SET is_active = true, updated_at = NOW()
WHERE id = :id
RETURNING id, agent_type, variant_name, version, is_active;

--! deactivate_variants_for_agent
UPDATE prompt_registry
SET is_active = false, updated_at = NOW()
WHERE agent_type = :agent_type
RETURNING id;

--! list_variants
SELECT id, agent_type, variant_name, prompt_content, version, is_active,
       source_recommendation_id, performance_metrics, created_at, updated_at
FROM prompt_registry
ORDER BY agent_type, version DESC;

--! list_variants_by_agent
SELECT id, agent_type, variant_name, prompt_content, version, is_active,
       source_recommendation_id, performance_metrics, created_at, updated_at
FROM prompt_registry
WHERE agent_type = :agent_type
ORDER BY version DESC;

--! update_performance_metrics
UPDATE prompt_registry
SET performance_metrics = :performance_metrics, updated_at = NOW()
WHERE id = :id
RETURNING id, agent_type, variant_name, version, performance_metrics;

--! get_next_version
SELECT COALESCE(MAX(version), 0) + 1 as next_version
FROM prompt_registry
WHERE agent_type = :agent_type;
