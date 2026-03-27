--- Decision Trail CRUD and search operations (architectural decision history).

--: GetDecision(triggered_by?, inspiration_json?, superseded_by?, created_by?)
--: DecisionPreview(triggered_by?, created_by?)
--: DecisionSearchRow(triggered_by?, created_by?, rank?)

--! save_decision (triggered_by?, inspiration_json?, created_by?, superseded_by?)
INSERT INTO decisions
    (id, timestamp, scale, category, status, title, summary, rationale,
     alternatives_json, tradeoffs_json, triggered_by, inspiration_json,
     related_decisions_json, affected_files_json, affected_endpoints_json,
     affected_tables_json, created_by, superseded_by, tags_json)
VALUES (:id, NOW(), :scale, :category, :status, :title, :summary, :rationale,
        :alternatives_json, :tradeoffs_json, :triggered_by, :inspiration_json,
        :related_decisions_json, :affected_files_json, :affected_endpoints_json,
        :affected_tables_json, :created_by, :superseded_by, :tags_json)
RETURNING id;

--! get_decision : GetDecision
SELECT id, timestamp, scale, category, status, title, summary, rationale,
       alternatives_json, tradeoffs_json, triggered_by, inspiration_json,
       related_decisions_json, affected_files_json, affected_endpoints_json,
       affected_tables_json, created_by, superseded_by, tags_json,
       is_deleted, created_at, updated_at
FROM decisions
WHERE id = :id AND NOT is_deleted;

--! list_decisions : DecisionPreview
SELECT id, timestamp, scale, category, status, title,
       LEFT(summary, 300) as summary_preview,
       triggered_by, tags_json, created_by, created_at, updated_at
FROM decisions
WHERE NOT is_deleted
ORDER BY timestamp DESC
LIMIT :max_results;

--! list_decisions_by_category : DecisionPreview
SELECT id, timestamp, scale, category, status, title,
       LEFT(summary, 300) as summary_preview,
       triggered_by, tags_json, created_by, created_at, updated_at
FROM decisions
WHERE NOT is_deleted AND category = :category
ORDER BY timestamp DESC
LIMIT :max_results;

--! list_decisions_by_scale : DecisionPreview
SELECT id, timestamp, scale, category, status, title,
       LEFT(summary, 300) as summary_preview,
       triggered_by, tags_json, created_by, created_at, updated_at
FROM decisions
WHERE NOT is_deleted AND scale = :scale
ORDER BY timestamp DESC
LIMIT :max_results;

--! search_decisions : DecisionSearchRow
SELECT id, timestamp, scale, category, status, title,
       LEFT(summary, 300) as summary_preview,
       triggered_by, tags_json, created_by, created_at, updated_at,
       ts_rank(to_tsvector('english', title || ' ' || summary || ' ' || rationale),
               plainto_tsquery('english', :query)) as rank
FROM decisions
WHERE NOT is_deleted
  AND to_tsvector('english', title || ' ' || summary || ' ' || rationale) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! update_decision_status
UPDATE decisions SET status = :status, updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! supersede_decision
UPDATE decisions SET status = 'superseded', superseded_by = :superseded_by, updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! update_decision (title?, summary?, rationale?, alternatives_json?, tradeoffs_json?, tags_json?, triggered_by?, inspiration_json?, related_decisions_json?, affected_files_json?, affected_endpoints_json?, affected_tables_json?)
UPDATE decisions SET
    title = COALESCE(:title, title),
    summary = COALESCE(:summary, summary),
    rationale = COALESCE(:rationale, rationale),
    alternatives_json = COALESCE(:alternatives_json, alternatives_json),
    tradeoffs_json = COALESCE(:tradeoffs_json, tradeoffs_json),
    tags_json = COALESCE(:tags_json, tags_json),
    triggered_by = COALESCE(:triggered_by, triggered_by),
    inspiration_json = COALESCE(:inspiration_json, inspiration_json),
    related_decisions_json = COALESCE(:related_decisions_json, related_decisions_json),
    affected_files_json = COALESCE(:affected_files_json, affected_files_json),
    affected_endpoints_json = COALESCE(:affected_endpoints_json, affected_endpoints_json),
    affected_tables_json = COALESCE(:affected_tables_json, affected_tables_json),
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! soft_delete_decision
UPDATE decisions SET is_deleted = true, updated_at = NOW()
WHERE id = :id
RETURNING id;

--- Concept Summary CRUD and search operations.

--: GetConceptSummary(inspiration_json?, metrics_json?)
--: ConceptSummaryPreview(inspiration_json?, metrics_json?)
--: ConceptSummarySearchRow(inspiration_json?, metrics_json?, rank?)

--! save_concept_summary (inspiration_json?, metrics_json?)
INSERT INTO concept_summaries
    (id, name, tagline, description, inspiration_json,
     benefits_json, components_json, related_decisions_json, metrics_json)
VALUES (:id, :name, :tagline, :description, :inspiration_json,
        :benefits_json, :components_json, :related_decisions_json, :metrics_json)
RETURNING id;

--! get_concept_summary : GetConceptSummary
SELECT id, name, tagline, description, inspiration_json,
       benefits_json, components_json, related_decisions_json, metrics_json,
       is_deleted, created_at, updated_at
FROM concept_summaries
WHERE id = :id AND NOT is_deleted;

--! list_concept_summaries : ConceptSummaryPreview
SELECT id, name, tagline, LEFT(description, 500) as description_preview,
       inspiration_json, benefits_json, components_json,
       related_decisions_json, metrics_json, created_at, updated_at
FROM concept_summaries
WHERE NOT is_deleted
ORDER BY updated_at DESC
LIMIT :max_results;

--! search_concept_summaries : ConceptSummarySearchRow
SELECT id, name, tagline, LEFT(description, 500) as description_preview,
       inspiration_json, benefits_json, components_json,
       related_decisions_json, metrics_json, created_at, updated_at,
       ts_rank(to_tsvector('english', name || ' ' || tagline || ' ' || description),
               plainto_tsquery('english', :query)) as rank
FROM concept_summaries
WHERE NOT is_deleted
  AND to_tsvector('english', name || ' ' || tagline || ' ' || description) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! update_concept_summary (name?, tagline?, description?, inspiration_json?, benefits_json?, components_json?, related_decisions_json?, metrics_json?)
UPDATE concept_summaries SET
    name = COALESCE(:name, name),
    tagline = COALESCE(:tagline, tagline),
    description = COALESCE(:description, description),
    inspiration_json = COALESCE(:inspiration_json, inspiration_json),
    benefits_json = COALESCE(:benefits_json, benefits_json),
    components_json = COALESCE(:components_json, components_json),
    related_decisions_json = COALESCE(:related_decisions_json, related_decisions_json),
    metrics_json = COALESCE(:metrics_json, metrics_json),
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! soft_delete_concept_summary
UPDATE concept_summaries SET is_deleted = true, updated_at = NOW()
WHERE id = :id
RETURNING id;
