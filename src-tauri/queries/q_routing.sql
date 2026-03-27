--- Q-routing table operations: Q-learning state-action values for architecture routing.

--! upsert_q_entry
INSERT INTO q_routing_table (state_key, action, q_value, visit_count, last_updated)
VALUES (:state_key, :action, :q_value, :visit_count, NOW())
ON CONFLICT(state_key, action) DO UPDATE SET
    q_value = EXCLUDED.q_value,
    visit_count = EXCLUDED.visit_count,
    last_updated = NOW()
RETURNING state_key;

--! get_all_q_entries
SELECT state_key, action, q_value, visit_count, last_updated
FROM q_routing_table
ORDER BY state_key, action;

--! get_q_entries_for_state
SELECT state_key, action, q_value, visit_count, last_updated
FROM q_routing_table
WHERE state_key = :state_key
ORDER BY action;

--! delete_q_entry
DELETE FROM q_routing_table
WHERE state_key = :state_key AND action = :action;

--! clear_q_table
DELETE FROM q_routing_table;
