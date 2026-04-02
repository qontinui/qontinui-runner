--- Check and check group CRUD operations.

--! list_checks
SELECT id, name, COALESCE(description, '') as description, check_type, tool, COALESCE(command, '') as command,
       COALESCE(working_directory, '') as working_directory, COALESCE(config_path, '') as config_path,
       auto_fix, fail_on_warning, COALESCE(timeout_seconds, 30) as timeout_seconds, is_critical, enabled, ai_generated,
       COALESCE(ai_generation_prompt, '') as ai_generation_prompt, tags, created_at, updated_at
FROM checks
ORDER BY updated_at DESC;

--! get_check
SELECT id, name, COALESCE(description, '') as description, check_type, tool, COALESCE(command, '') as command,
       COALESCE(working_directory, '') as working_directory, COALESCE(config_path, '') as config_path,
       auto_fix, fail_on_warning, COALESCE(timeout_seconds, 30) as timeout_seconds, is_critical, enabled, ai_generated,
       COALESCE(ai_generation_prompt, '') as ai_generation_prompt, tags, created_at, updated_at
FROM checks
WHERE id = :id;

--! create_check (description?, command?, working_directory?, config_path?, timeout_seconds?, ai_generation_prompt?)
INSERT INTO checks (id, name, description, check_type, tool, command, working_directory,
                    config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical,
                    enabled, ai_generated, ai_generation_prompt, tags)
VALUES (:id, :name, :description, :check_type, :tool, :command, :working_directory,
        :config_path, :auto_fix, :fail_on_warning, :timeout_seconds, :is_critical,
        :enabled, :ai_generated, :ai_generation_prompt, :tags)
RETURNING id;

--! update_check (name?, description?, check_type?, tool?, command?, working_directory?, config_path?, auto_fix?, fail_on_warning?, timeout_seconds?, is_critical?, enabled?, ai_generation_prompt?, tags?)
UPDATE checks SET
    name = COALESCE(:name, name),
    description = COALESCE(:description, description),
    check_type = COALESCE(:check_type, check_type),
    tool = COALESCE(:tool, tool),
    command = COALESCE(:command, command),
    working_directory = COALESCE(:working_directory, working_directory),
    config_path = COALESCE(:config_path, config_path),
    auto_fix = COALESCE(:auto_fix, auto_fix),
    fail_on_warning = COALESCE(:fail_on_warning, fail_on_warning),
    timeout_seconds = COALESCE(:timeout_seconds, timeout_seconds),
    is_critical = COALESCE(:is_critical, is_critical),
    enabled = COALESCE(:enabled, enabled),
    ai_generation_prompt = COALESCE(:ai_generation_prompt, ai_generation_prompt),
    tags = COALESCE(:tags, tags),
    updated_at = NOW()
WHERE id = :id
RETURNING id;

--! delete_check
DELETE FROM checks WHERE id = :id RETURNING id;

--! get_check_group
SELECT id, name, COALESCE(description, '') as description, COALESCE(color, '') as color,
       enabled, run_in_parallel, stop_on_failure, tags,
       created_at, updated_at
FROM check_groups
WHERE id = :id;

--! create_check_group (description?, color?)
INSERT INTO check_groups (id, name, description, color, enabled, run_in_parallel, stop_on_failure, tags)
VALUES (:id, :name, :description, :color, :enabled, :run_in_parallel, :stop_on_failure, :tags)
RETURNING id;

--! delete_check_group
DELETE FROM check_groups WHERE id = :id RETURNING id;

--! get_checks_in_group
SELECT c.id, c.name, COALESCE(c.description, '') as description, c.check_type, c.tool, COALESCE(c.command, '') as command,
       COALESCE(c.working_directory, '') as working_directory, COALESCE(c.config_path, '') as config_path,
       c.auto_fix, c.fail_on_warning, COALESCE(c.timeout_seconds, 30) as timeout_seconds, c.is_critical,
       c.enabled, c.ai_generated, COALESCE(c.ai_generation_prompt, '') as ai_generation_prompt, c.tags, c.created_at, c.updated_at
FROM checks c
JOIN check_group_members cgm ON c.id = cgm.check_id
WHERE cgm.group_id = :group_id
ORDER BY cgm.sort_order ASC;
