//! User skills CRUD operations.
//!
//! Contains all CheckpointDb methods related to user-created skills.

use chrono::Utc;
use rusqlite::params;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // User Skills CRUD
    // ========================================================================

    /// List all user-created skills.
    pub fn list_user_skills(&self) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, slug, description, category, tags, icon, color,
                       allowed_phases, parameters, template, source,
                       version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                       created_at, updated_at
                FROM user_skills
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare user_skills query: {}", e))?;

        let skills = stmt
            .query_map([], |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to query user skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Get a single user skill by ID.
    pub fn get_user_skill(
        &self,
        id: &str,
    ) -> Result<Option<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, slug, description, category, tags, icon, color,
                   allowed_phases, parameters, template, source,
                   version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                   created_at, updated_at
            FROM user_skills
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            },
        );

        match result {
            Ok(skill) => Ok(Some(skill)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get user skill: {}", e)),
        }
    }

    /// Create a new user skill.
    pub fn create_user_skill(
        &self,
        request: &crate::mcp::skills::CreateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let id = format!("user:{}", request.slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&request.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&request.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&request.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                request.name,
                request.slug,
                request.description,
                request.category,
                tags_json,
                request.icon,
                request.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",                    // version
                Option::<String>::None,      // author
                Option::<String>::None,      // checksum
                "[]",                        // depends_on
                0i64,                        // usage_count
                Option::<String>::None,      // approval_status
                Option::<String>::None,      // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create user skill: {}", e))?;

        self.get_user_skill(&id)?
            .ok_or_else(|| "Failed to retrieve created skill".to_string())
    }

    /// Update a user skill.
    pub fn update_user_skill(
        &self,
        id: &str,
        request: &crate::mcp::skills::UpdateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let current = self
            .get_user_skill(id)?
            .ok_or_else(|| format!("User skill not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let slug = request.slug.as_ref().unwrap_or(&current.slug);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let icon = request.icon.as_ref().unwrap_or(&current.icon);
        let color = request.color.as_ref().unwrap_or(&current.color);
        let allowed_phases = request
            .allowed_phases
            .as_ref()
            .unwrap_or(&current.allowed_phases);
        let parameters = request.parameters.as_ref().unwrap_or(&current.parameters);
        let template = request.template.as_ref().unwrap_or(&current.template);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        // Update the ID if slug changed
        let new_id = format!("user:{}", slug);

        conn.execute(
            r#"
            UPDATE user_skills SET
                id = ?1, name = ?2, slug = ?3, description = ?4, category = ?5,
                tags = ?6, icon = ?7, color = ?8, allowed_phases = ?9,
                parameters = ?10, template = ?11, updated_at = ?12
            WHERE id = ?13
            "#,
            params![
                new_id,
                name,
                slug,
                description,
                category,
                tags_json,
                icon,
                color,
                allowed_phases_json,
                parameters_json,
                template_json,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update user skill: {}", e))?;

        self.get_user_skill(&new_id)?
            .ok_or_else(|| "Failed to retrieve updated skill".to_string())
    }

    /// Export user skills for sharing.
    /// If `ids` is empty, exports all non-builtin skills.
    pub fn export_user_skills(
        &self,
        ids: &[String],
    ) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        if ids.is_empty() {
            return self.list_user_skills();
        }

        let conn = self.get_conn()?;
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
        let query = format!(
            r#"SELECT id, name, slug, description, category, tags, icon, color,
                      allowed_phases, parameters, template, source,
                      version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                      created_at, updated_at
               FROM user_skills
               WHERE id IN ({})
               ORDER BY updated_at DESC"#,
            placeholders.join(", ")
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare export query: {}", e))?;

        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let skills = stmt
            .query_map(params.as_slice(), |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to export skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Import skills from an export. Sets source to "community" and id to "community:<slug>".
    /// `conflict_mode` is "skip" or "overwrite".
    pub fn import_skills(
        &self,
        skills: &[crate::skills::SkillDefinition],
        conflict_mode: &str,
    ) -> Result<crate::skills::SkillImportResult, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut overwritten = 0usize;
        let mut errors = Vec::new();

        for skill in skills {
            let slug = &skill.slug;
            let id = format!("community:{}", slug);

            let tags_json = serde_json::to_string(&skill.tags).unwrap_or_else(|_| "[]".to_string());
            let allowed_phases_json =
                serde_json::to_string(&skill.allowed_phases).unwrap_or_else(|_| "[]".to_string());
            let parameters_json =
                serde_json::to_string(&skill.parameters).unwrap_or_else(|_| "[]".to_string());
            let template_json = match serde_json::to_string(&skill.template) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!(
                        "Failed to serialize template for '{}': {}",
                        slug, e
                    ));
                    continue;
                }
            };

            // Check if slug already exists
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM user_skills WHERE slug = ?1",
                    params![slug],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if exists {
                if conflict_mode == "overwrite" {
                    let overwrite_author_json = skill
                        .author
                        .as_ref()
                        .map(|a| serde_json::to_string(a).unwrap_or_default());
                    let overwrite_depends_on_json =
                        serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                            .unwrap_or_else(|_| "[]".to_string());

                    match conn.execute(
                        r#"UPDATE user_skills SET
                            id = ?1, name = ?2, description = ?3, category = ?4,
                            tags = ?5, icon = ?6, color = ?7, allowed_phases = ?8,
                            parameters = ?9, template = ?10, source = ?11, updated_at = ?12,
                            version = ?13, author = ?14, checksum = ?15, depends_on = ?16,
                            usage_count = ?17, approval_status = ?18, forked_from = ?19
                        WHERE slug = ?20"#,
                        params![
                            id,
                            skill.name,
                            skill.description,
                            skill.category,
                            tags_json,
                            skill.icon,
                            skill.color,
                            allowed_phases_json,
                            parameters_json,
                            template_json,
                            "community",
                            now,
                            skill.version.as_deref().unwrap_or("1.0.0"),
                            overwrite_author_json,
                            skill.checksum.as_deref(),
                            overwrite_depends_on_json,
                            skill.usage_count.unwrap_or(0) as i64,
                            skill.approval_status.as_deref(),
                            skill.forked_from.as_deref(),
                            slug,
                        ],
                    ) {
                        Ok(_) => overwritten += 1,
                        Err(e) => errors.push(format!("Failed to overwrite '{}': {}", slug, e)),
                    }
                } else {
                    skipped += 1;
                }
            } else {
                let author_json = skill
                    .author
                    .as_ref()
                    .map(|a| serde_json::to_string(a).unwrap_or_default());
                let depends_on_json =
                    serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                        .unwrap_or_else(|_| "[]".to_string());

                match conn.execute(
                    r#"INSERT INTO user_skills (
                        id, name, slug, description, category, tags, icon, color,
                        allowed_phases, parameters, template, source,
                        version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                        created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
                    params![
                        id,
                        skill.name,
                        slug,
                        skill.description,
                        skill.category,
                        tags_json,
                        skill.icon,
                        skill.color,
                        allowed_phases_json,
                        parameters_json,
                        template_json,
                        "community",
                        skill.version.as_deref().unwrap_or("1.0.0"),
                        author_json,
                        skill.checksum.as_deref(),
                        depends_on_json,
                        skill.usage_count.unwrap_or(0) as i64,
                        skill.approval_status.as_deref(),
                        skill.forked_from.as_deref(),
                        now,
                        now,
                    ],
                ) {
                    Ok(_) => imported += 1,
                    Err(e) => errors.push(format!("Failed to import '{}': {}", slug, e)),
                }
            }
        }

        Ok(crate::skills::SkillImportResult {
            imported,
            skipped,
            overwritten,
            errors,
            warnings: vec![],
        })
    }

    /// Delete a user skill by ID.
    pub fn delete_user_skill(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute("DELETE FROM user_skills WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete user skill: {}", e))?;

        Ok(deleted > 0)
    }

    /// Update the approval status of a skill.
    pub fn update_skill_approval(&self, skill_id: &str, status: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET approval_status = ?1 WHERE id = ?2",
                params![status, skill_id],
            )
            .map_err(|e| format!("Failed to update approval status: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Update the version and checksum of a skill.
    pub fn update_skill_version(
        &self,
        skill_id: &str,
        version: &str,
        checksum: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET version = ?1, checksum = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![version, checksum, skill_id],
            )
            .map_err(|e| format!("Failed to update skill version: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Fork a skill by creating a copy with a new ID.
    pub fn fork_skill(
        &self,
        skill_id: &str,
        new_name: Option<&str>,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let original = self
            .get_user_skill(skill_id)?
            .ok_or_else(|| format!("Skill not found: {}", skill_id))?;

        let fork_name = new_name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("{} (fork)", original.name));
        let fork_slug = format!(
            "{}-fork-{}",
            original.slug,
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let fork_id = format!("user:{}", fork_slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&original.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&original.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&original.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&original.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;
        let author_json = original
            .author
            .as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let depends_on_json = serde_json::to_string(&original.depends_on.as_deref().unwrap_or(&[]))
            .unwrap_or_else(|_| "[]".to_string());

        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
            params![
                fork_id,
                fork_name,
                fork_slug,
                original.description,
                original.category,
                tags_json,
                original.icon,
                original.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",
                author_json,
                Option::<String>::None, // checksum
                depends_on_json,
                0i64,                   // usage_count
                Option::<String>::None, // approval_status
                skill_id,               // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create forked skill: {}", e))?;

        self.get_user_skill(&fork_id)?
            .ok_or_else(|| "Failed to retrieve forked skill".to_string())
    }

    /// Increment the usage count of a skill and return the new count.
    pub fn increment_skill_usage(&self, skill_id: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET usage_count = COALESCE(usage_count, 0) + 1 WHERE id = ?1",
                params![skill_id],
            )
            .map_err(|e| format!("Failed to increment usage count: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }

        let count: u64 = conn
            .query_row(
                "SELECT COALESCE(usage_count, 0) FROM user_skills WHERE id = ?1",
                params![skill_id],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .map_err(|e| format!("Failed to read usage count: {}", e))?;

        Ok(count)
    }
}
