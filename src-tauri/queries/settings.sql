--- Settings and config storage operations.

--! get_setting
SELECT value FROM settings WHERE key = :key;

--! set_setting
INSERT INTO settings (key, value, updated_at)
VALUES (:key, :value, NOW())
ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
RETURNING key;

--! delete_setting
DELETE FROM settings WHERE key = :key RETURNING key;

--! get_all_settings
SELECT key, value FROM settings ORDER BY key;

--! save_config (source_path?)
INSERT INTO configs (id, name, config_json, source_type, source_path, created_at, updated_at)
VALUES (:id, :name, :config_json, :source_type, :source_path, NOW(), NOW())
ON CONFLICT(id) DO UPDATE SET
    name = EXCLUDED.name,
    config_json = EXCLUDED.config_json,
    source_type = EXCLUDED.source_type,
    source_path = EXCLUDED.source_path,
    updated_at = NOW()
RETURNING id;

--! get_config
SELECT config_json FROM configs WHERE id = :id;

--! list_configs
SELECT id, name, source_type, source_path, created_at, updated_at
FROM configs
ORDER BY updated_at DESC;

--! delete_config
DELETE FROM configs WHERE id = :id RETURNING id;
