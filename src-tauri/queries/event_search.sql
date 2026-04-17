--- Unified full-text search across the four event tables.
---
--- UNIONs activity_timeline, observations, deferred_questions, and error_events
--- with a source_table discriminator and a ts_rank_cd relevance score, so a
--- single query can surface "what was happening in the system around X?"
---
--- Notes:
---   * Each table has its own GIN FTS index (idx_at_fts / idx_obs_fts /
---     idx_dq_fts / idx_ee_fts). The expression in each UNION arm must match
---     its index's expression exactly for the planner to use the index.
---   * deferred_questions.id is TEXT (not BIGSERIAL); we cast the BIGSERIAL
---     ids to TEXT in the other arms to keep a uniform record_id column.
---   * error_events uses captured_at as its event time (no created_at).
---   * activity_timeline and observations are soft-deletable — filter is_deleted.
---     deferred_questions and error_events have no soft-delete column.
---   * All parameters live in the leading `q` CTE. Clorinde's query parser
---     garbles the `LIMIT :param` tail when a named parameter is referenced
---     across multiple UNION ALL arms, so we keep each parameter used exactly
---     once in the prepared query.

--: SearchEventsRow()

--! search_events : SearchEventsRow
WITH q AS (
    SELECT plainto_tsquery('english', :query_text) AS tsq,
           :since_ts::timestamptz AS since_ts,
           :max_results::bigint AS max_rows
)
SELECT source_table, record_id, snippet, event_ts, score
FROM (
    SELECT
        'activity_timeline'::text AS source_table,
        at.id::text AS record_id,
        LEFT(at.text_content, 400) AS snippet,
        at.created_at AS event_ts,
        ts_rank_cd(to_tsvector('english', at.text_content), q.tsq)::float4 AS score
    FROM activity_timeline at, q
    WHERE to_tsvector('english', at.text_content) @@ q.tsq
      AND at.created_at >= q.since_ts
      AND NOT at.is_deleted

    UNION ALL
    SELECT
        'observations'::text,
        o.id::text,
        LEFT(o.title || ' — ' || o.content, 400),
        o.created_at,
        ts_rank_cd(to_tsvector('english', o.title || ' ' || o.content), q.tsq)::float4
    FROM observations o, q
    WHERE to_tsvector('english', o.title || ' ' || o.content) @@ q.tsq
      AND o.created_at >= q.since_ts
      AND NOT o.is_deleted

    UNION ALL
    SELECT
        'deferred_questions'::text,
        dq.id,
        LEFT(dq.question, 400),
        dq.created_at,
        ts_rank_cd(
            to_tsvector('english', dq.question || ' ' || COALESCE(dq.context_json, '')),
            q.tsq
        )::float4
    FROM deferred_questions dq, q
    WHERE to_tsvector('english', dq.question || ' ' || COALESCE(dq.context_json, '')) @@ q.tsq
      AND dq.created_at >= q.since_ts

    UNION ALL
    SELECT
        'error_events'::text,
        ee.id::text,
        LEFT(ee.message, 400),
        ee.captured_at,
        ts_rank_cd(
            to_tsvector(
                'english',
                ee.message || ' ' || COALESCE(ee.stack_trace, '') || ' ' || COALESCE(ee.context_lines, '')
            ),
            q.tsq
        )::float4
    FROM error_events ee, q
    WHERE to_tsvector(
            'english',
            ee.message || ' ' || COALESCE(ee.stack_trace, '') || ' ' || COALESCE(ee.context_lines, '')
          ) @@ q.tsq
      AND ee.captured_at >= q.since_ts
) sub
ORDER BY score DESC, event_ts DESC
LIMIT (SELECT max_rows FROM q);
