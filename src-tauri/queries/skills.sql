--- User skill CRUD operations.

--! list_user_skills
SELECT id, name, slug, description, category, tags, icon, color,
       allowed_phases, parameters, template, source,
       version, author, checksum, depends_on, usage_count, approval_status, forked_from,
       created_at, updated_at
FROM user_skills
ORDER BY updated_at DESC;

--! get_user_skill
SELECT id, name, slug, description, category, tags, icon, color,
       allowed_phases, parameters, template, source,
       version, author, checksum, depends_on, usage_count, approval_status, forked_from,
       created_at, updated_at
FROM user_skills
WHERE id = :id;

--! create_user_skill (description?, author?, checksum?, depends_on?, approval_status?, forked_from?, source_fix_id?, source_pattern_id?)
INSERT INTO user_skills (
    id, name, slug, description, category, tags, icon, color,
    allowed_phases, parameters, template, source,
    version, author, checksum, depends_on, approval_status, forked_from,
    source_fix_id, source_pattern_id
) VALUES (
    :id, :name, :slug, :description, :category, :tags, :icon, :color,
    :allowed_phases, :parameters, :template, :source,
    :version, :author, :checksum, :depends_on, :approval_status, :forked_from,
    :source_fix_id, :source_pattern_id
)
RETURNING id;

--! delete_user_skill
DELETE FROM user_skills WHERE id = :id RETURNING id;

--! increment_skill_usage
UPDATE user_skills SET usage_count = usage_count + 1, updated_at = NOW()
WHERE id = :id
RETURNING usage_count;
