--- Miscellaneous small CRUD tables: shell commands, saved API requests, MCP servers.

--! get_shell_command
SELECT id, name, description, command, working_directory, timeout_seconds,
       fail_on_error, category, tags, enabled, created_at, updated_at
FROM shell_commands WHERE id = :id;

--! create_shell_command (description?, working_directory?, timeout_seconds?)
INSERT INTO shell_commands (id, name, description, command, working_directory,
                            timeout_seconds, fail_on_error, category, tags, enabled)
VALUES (:id, :name, :description, :command, :working_directory,
        :timeout_seconds, :fail_on_error, :category, :tags, :enabled)
RETURNING id;

--! delete_shell_command
DELETE FROM shell_commands WHERE id = :id RETURNING id;

--! list_saved_api_requests
SELECT id, name, description, category, tags, method, url, headers, body,
       body_content_type, timeout_ms, follow_redirects, variable_extractions,
       assertions, credential_id, created_at, updated_at
FROM saved_api_requests ORDER BY updated_at DESC;

--! get_saved_api_request
SELECT id, name, description, category, tags, method, url, headers, body,
       body_content_type, timeout_ms, follow_redirects, variable_extractions,
       assertions, credential_id, created_at, updated_at
FROM saved_api_requests WHERE id = :id;

--! delete_saved_api_request
DELETE FROM saved_api_requests WHERE id = :id RETURNING id;

--! get_saved_api_request_tags
SELECT DISTINCT tags FROM saved_api_requests WHERE tags != '[]';

--! list_mcp_servers
SELECT id, name, description, transport, stdio_config, http_config, enabled,
       auto_start, timeout_seconds, cached_tools, tools_cached_at, created_at, updated_at
FROM mcp_servers ORDER BY name ASC;

--! get_mcp_server
SELECT id, name, description, transport, stdio_config, http_config, enabled,
       auto_start, timeout_seconds, cached_tools, tools_cached_at, created_at, updated_at
FROM mcp_servers WHERE id = :id;
