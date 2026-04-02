--- Miscellaneous small CRUD tables: shell commands, saved API requests, MCP servers.

--! get_shell_command
SELECT id, name, COALESCE(description, '') as description, command,
       COALESCE(working_directory, '') as working_directory, COALESCE(timeout_seconds, 30) as timeout_seconds,
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
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, '') as category, COALESCE(tags, '[]') as tags,
       method, url, COALESCE(headers, '{}') as headers, COALESCE(body, '') as body,
       COALESCE(body_content_type, '') as body_content_type, COALESCE(timeout_ms, 30000) as timeout_ms,
       COALESCE(follow_redirects, true) as follow_redirects, COALESCE(variable_extractions, '[]') as variable_extractions,
       COALESCE(assertions, '[]') as assertions, COALESCE(credential_id, '') as credential_id, created_at, updated_at
FROM saved_api_requests ORDER BY updated_at DESC;

--! get_saved_api_request
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, '') as category, COALESCE(tags, '[]') as tags,
       method, url, COALESCE(headers, '{}') as headers, COALESCE(body, '') as body,
       COALESCE(body_content_type, '') as body_content_type, COALESCE(timeout_ms, 30000) as timeout_ms,
       COALESCE(follow_redirects, true) as follow_redirects, COALESCE(variable_extractions, '[]') as variable_extractions,
       COALESCE(assertions, '[]') as assertions, COALESCE(credential_id, '') as credential_id, created_at, updated_at
FROM saved_api_requests WHERE id = :id;

--! delete_saved_api_request
DELETE FROM saved_api_requests WHERE id = :id RETURNING id;

--! get_saved_api_request_tags
SELECT DISTINCT tags FROM saved_api_requests WHERE tags != '[]';

--! list_mcp_servers
SELECT id, name, COALESCE(description, '') as description, transport,
       COALESCE(stdio_config, '{}') as stdio_config, COALESCE(http_config, '{}') as http_config, enabled,
       auto_start, COALESCE(timeout_seconds, 30) as timeout_seconds,
       COALESCE(cached_tools, '[]') as cached_tools, COALESCE(tools_cached_at::TEXT, '') as tools_cached_at,
       created_at, updated_at
FROM mcp_servers ORDER BY name ASC;

--! get_mcp_server
SELECT id, name, COALESCE(description, '') as description, transport,
       COALESCE(stdio_config, '{}') as stdio_config, COALESCE(http_config, '{}') as http_config, enabled,
       auto_start, COALESCE(timeout_seconds, 30) as timeout_seconds,
       COALESCE(cached_tools, '[]') as cached_tools, COALESCE(tools_cached_at::TEXT, '') as tools_cached_at,
       created_at, updated_at
FROM mcp_servers WHERE id = :id;
