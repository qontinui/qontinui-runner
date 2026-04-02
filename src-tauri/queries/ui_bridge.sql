--- UI Bridge analytics: events, elements, stalls, and cross-run analytics.

--: UiBridgeEventRow(task_run_id?, element_id?, state_id?, transition_id?, action?, params?, result?, duration_ms?, error_message?, metadata?)

--! insert_ui_bridge_event (task_run_id?, element_id?, state_id?, transition_id?, action?, params?, result?, duration_ms?, error_message?, metadata?)
INSERT INTO ui_bridge_events
    (task_run_id, timestamp, sequence, event_type, element_id,
     state_id, transition_id, action, params, result,
     duration_ms, success, error_message, metadata)
VALUES (:task_run_id, :timestamp, :sequence, :event_type, :element_id,
        :state_id, :transition_id, :action, :params, :result,
        :duration_ms, :success, :error_message, :metadata)
RETURNING id;

--! get_element_interactions : UiBridgeEventRow
SELECT id, task_run_id, timestamp, sequence, event_type,
       element_id, state_id, transition_id, action, params,
       result, duration_ms, success, error_message, metadata
FROM ui_bridge_events
WHERE task_run_id = :task_run_id
ORDER BY sequence ASC;

--! get_element_history : UiBridgeEventRow
SELECT id, task_run_id, timestamp, sequence, event_type,
       element_id, state_id, transition_id, action, params,
       result, duration_ms, success, error_message, metadata
FROM ui_bridge_events
WHERE element_id = :element_id
ORDER BY timestamp DESC
LIMIT :max_results;

--! get_flaky_elements
SELECT element_id,
       COUNT(*)::bigint AS total,
       COUNT(*) FILTER (WHERE success)::bigint AS successes,
       COUNT(*) FILTER (WHERE success)::double precision / COUNT(*) AS rate
FROM ui_bridge_events
WHERE element_id IS NOT NULL
  AND event_type = 'action_executed'
GROUP BY element_id
HAVING COUNT(*) >= :min_interactions
   AND COUNT(*) FILTER (WHERE success)::double precision / COUNT(*) <= :max_success_rate
ORDER BY rate ASC;

--! get_element_reliability
SELECT element_id,
       COUNT(*)::bigint AS total,
       COUNT(*) FILTER (WHERE success)::bigint AS successes,
       COUNT(*) FILTER (WHERE success)::double precision / GREATEST(COUNT(*), 1) AS rate
FROM ui_bridge_events
WHERE element_id = :element_id
  AND event_type = 'action_executed'
GROUP BY element_id;

--! get_last_failure_reason
SELECT error_message FROM ui_bridge_events
WHERE element_id = :element_id AND NOT success AND error_message IS NOT NULL
ORDER BY timestamp DESC LIMIT 1;

--! insert_stall_event (pattern_details?, action_count?, intervention_action?, intervention_result?)
INSERT INTO stall_events (id, task_run_id, iteration, pattern_type, pattern_details,
                          action_count, intervention_action, intervention_result)
VALUES (:id, :task_run_id, :iteration, :pattern_type, :pattern_details,
        :action_count, :intervention_action, :intervention_result)
RETURNING id;

--! get_stall_events
SELECT id, task_run_id, iteration, pattern_type, COALESCE(pattern_details, '') as pattern_details,
       COALESCE(action_count, 0) as action_count, COALESCE(intervention_action, '') as intervention_action,
       COALESCE(intervention_result, '') as intervention_result, created_at
FROM stall_events
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! get_element_decay_curve
SELECT (timestamp / :window_ms) AS bucket,
       COUNT(*)::bigint AS total,
       COALESCE(COUNT(*) FILTER (WHERE success), 0)::bigint AS successes,
       COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate
FROM ui_bridge_events
WHERE element_id = :element_id AND event_type = 'action_executed'
GROUP BY bucket
ORDER BY bucket DESC
LIMIT :num_windows;

--! get_action_latency_baselines
SELECT action,
       COUNT(*)::bigint AS cnt,
       COALESCE(AVG(duration_ms), 0)::double precision AS avg_dur,
       COALESCE(MIN(duration_ms), 0)::double precision AS min_dur,
       COALESCE(MAX(duration_ms), 0)::double precision AS max_dur,
       COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate
FROM ui_bridge_events
WHERE event_type = 'action_executed'
  AND action IS NOT NULL
  AND timestamp >= :since_epoch_ms
GROUP BY action
ORDER BY cnt DESC
LIMIT 50;

--! get_failure_taxonomy
SELECT error_message,
       COUNT(*)::bigint AS freq,
       STRING_AGG(DISTINCT element_id, ',') AS elements
FROM ui_bridge_events
WHERE NOT success
  AND error_message IS NOT NULL
  AND timestamp >= :since_epoch_ms
GROUP BY error_message
ORDER BY freq DESC
LIMIT :max_results;

--! get_element_fragility_by_region
SELECT ev.element_id,
       el.bounds,
       COUNT(*)::bigint AS cnt,
       COALESCE(COUNT(*) FILTER (WHERE ev.success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate
FROM ui_bridge_events ev
LEFT JOIN ui_bridge_elements el ON ev.element_id = el.element_id
WHERE ev.event_type = 'action_executed'
  AND ev.element_id IS NOT NULL
  AND ev.timestamp >= :since_epoch_ms
GROUP BY ev.element_id, el.bounds
HAVING COUNT(*) >= 3
ORDER BY rate ASC
LIMIT 100;

--! get_automation_regressions
WITH element_splits AS (
    SELECT element_id, action,
           timestamp,
           success,
           NTILE(2) OVER (PARTITION BY element_id, action ORDER BY timestamp) AS half
    FROM ui_bridge_events
    WHERE event_type = 'action_executed'
      AND element_id IS NOT NULL
      AND action IS NOT NULL
      AND timestamp >= :since_epoch_ms
)
SELECT element_id, action,
       SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision /
         GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) AS prior_rate,
       SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision /
         GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1) AS recent_rate
FROM element_splits
GROUP BY element_id, action
HAVING COUNT(*) >= 4
   AND SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision /
       GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1)
     < SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision /
       GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) - 0.1
ORDER BY (
    SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision /
    GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1)
  - SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision /
    GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1)
) DESC
LIMIT :max_results;

--! get_stall_frequency
SELECT pattern_type, COUNT(*)::bigint AS cnt
FROM stall_events
WHERE created_at >= :since_timestamp::timestamptz
GROUP BY pattern_type
ORDER BY cnt DESC;

--! get_intervention_effectiveness
SELECT intervention_action,
       COUNT(*)::bigint AS total,
       COUNT(*) FILTER (WHERE intervention_result = 'success')::bigint AS successes,
       COUNT(*) FILTER (WHERE intervention_result = 'success')::double precision / GREATEST(COUNT(*), 1) AS rate
FROM stall_events
WHERE created_at >= :since_timestamp::timestamptz
  AND intervention_action IS NOT NULL
GROUP BY intervention_action
ORDER BY total DESC;

--! get_state_coverage
SELECT DISTINCT state_id
FROM ui_bridge_events
WHERE task_run_id = :task_run_id
  AND state_id IS NOT NULL;

--! get_unannotated_high_interaction_elements
SELECT element_id,
       COUNT(*)::bigint AS interaction_count,
       COUNT(DISTINCT action)::bigint AS action_types,
       COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS success_rate
FROM ui_bridge_events
WHERE element_id IS NOT NULL
  AND event_type = 'action_executed'
GROUP BY element_id
HAVING COUNT(*) >= :min_interactions
ORDER BY interaction_count DESC;
