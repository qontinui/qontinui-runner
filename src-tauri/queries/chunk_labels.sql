--- Chunk labels — per-(config_id, chunk_id) user-chosen overrides for the
--- auto-derived chunk name shown in the chunked state-machine graph view.

--! list_chunk_labels
SELECT chunk_id, label, updated_at::TEXT AS updated_at
FROM chunk_labels
WHERE config_id = :config_id;

--! upsert_chunk_label
INSERT INTO chunk_labels (config_id, chunk_id, label, updated_at)
VALUES (:config_id, :chunk_id, :label, NOW())
ON CONFLICT (config_id, chunk_id) DO UPDATE
SET label = EXCLUDED.label,
    updated_at = NOW();

--! delete_chunk_label
DELETE FROM chunk_labels
WHERE config_id = :config_id AND chunk_id = :chunk_id;
