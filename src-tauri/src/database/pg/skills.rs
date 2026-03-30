//! PostgreSQL user skill operations via Clorinde-generated queries.

use super::PgDb;
use crate::skills::SkillDefinition;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() { T::default() } else { serde_json::from_str(s).unwrap_or_default() }
}

/// Map a Clorinde row to SkillDefinition.
macro_rules! row_to_skill {
    ($r:expr) => {{
        SkillDefinition {
            id: $r.id,
            name: $r.name,
            slug: $r.slug,
            description: $r.description,
            category: if $r.category.is_empty() { "custom".to_string() } else { $r.category },
            tags: json_or_default(&$r.tags),
            icon: if $r.icon.is_empty() { "puzzle".to_string() } else { $r.icon },
            color: if $r.color.is_empty() { "gray".to_string() } else { $r.color },
            allowed_phases: {
                let v: Vec<String> = json_or_default(&$r.allowed_phases);
                if v.is_empty() { vec!["setup".to_string()] } else { v }
            },
            parameters: json_or_default(&$r.parameters),
            template: serde_json::from_str(&$r.template).unwrap_or(
                crate::skills::SkillTemplate::SingleStep {
                    step: std::collections::HashMap::new(),
                },
            ),
            source: if $r.source.is_empty() { "user".to_string() } else { $r.source },
            version: non_empty($r.version),
            author: {
                let s = &$r.author;
                if s.is_empty() { None } else { serde_json::from_str(s).ok() }
            },
            checksum: non_empty($r.checksum),
            depends_on: {
                let s = &$r.depends_on;
                if s.is_empty() { None } else { serde_json::from_str(s).ok() }
            },
            usage_count: if $r.usage_count == 0 { None } else { Some($r.usage_count as u64) },
            approval_status: non_empty($r.approval_status),
            forked_from: non_empty($r.forked_from),
        }
    }};
}

impl PgDb {
    /// List all user skills.
    pub async fn list_user_skills(&self) -> Result<Vec<SkillDefinition>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::skills::list_user_skills()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_user_skills: {}", e))?;
        Ok(rows.into_iter().map(|r| row_to_skill!(r)).collect())
    }

    /// Get a single user skill by ID.
    pub async fn get_user_skill(&self, id: &str) -> Result<Option<SkillDefinition>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::skills::get_user_skill()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_user_skill: {}", e))?;
        Ok(row.map(|r| row_to_skill!(r)))
    }

    /// Delete a user skill by ID.
    pub async fn delete_user_skill(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::skills::delete_user_skill()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_user_skill: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Increment usage count for a skill.
    pub async fn increment_skill_usage(&self, id: &str) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let count = qontinui_db::queries::skills::increment_skill_usage()
            .bind(&conn, &id)
            .one()
            .await
            .map_err(|e| format!("PG increment_skill_usage: {}", e))?;
        Ok(count as u64)
    }

    /// Get approved auto-extracted skills for prompt injection.
    /// Returns (name, slug, description, category) tuples ordered by usage_count DESC, limited to 10.
    pub async fn get_approved_auto_skills(&self) -> Result<Vec<(String, String, String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            r#"SELECT name, slug, description, category
               FROM user_skills
               WHERE approval_status = 'approved'
               AND source = 'auto'
               ORDER BY usage_count DESC
               LIMIT 10"#,
            &[],
        ).await.map_err(|e| format!("PG get_approved_auto_skills: {}", e))?;

        Ok(rows.iter().map(|row| {
            let name: String = row.get(0);
            let slug: String = row.get(1);
            let description: String = row.try_get(2).unwrap_or_default();
            let category: String = row.try_get(3).unwrap_or_else(|_| "custom".to_string());
            (name, slug, description, category)
        }).collect())
    }

    /// Import skills from an export. Sets source to "community" and id to "community:<slug>".
    /// `conflict_mode` is "skip" or "overwrite".
    pub async fn import_skills(
        &self,
        skills: &[crate::skills::SkillDefinition],
        conflict_mode: &str,
    ) -> Result<crate::skills::SkillImportResult, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut overwritten = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for skill in skills {
            let id = format!("community:{}", skill.slug);
            let tags_json = serde_json::to_string(&skill.tags).unwrap_or_else(|_| "[]".to_string());
            let phases_json = serde_json::to_string(&skill.allowed_phases).unwrap_or_else(|_| "[]".to_string());
            let params_json = serde_json::to_string(&skill.parameters).unwrap_or_else(|_| "[]".to_string());
            let template_json = match serde_json::to_string(&skill.template) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!("Failed to serialize template for '{}': {}", skill.slug, e));
                    continue;
                }
            };
            let author_json: Option<String> = skill.author.as_ref().and_then(|a| serde_json::to_string(a).ok());
            let depends_json: Option<String> = skill.depends_on.as_ref().and_then(|d| serde_json::to_string(d).ok());
            let version = skill.version.as_deref().unwrap_or("1.0.0");
            let checksum = skill.checksum.as_deref();
            let usage_count = skill.usage_count.unwrap_or(0) as i64;
            let approval_status = skill.approval_status.as_deref();
            let forked_from = skill.forked_from.as_deref();

            // Check existence before upsert to count overwritten vs imported
            let existed = if conflict_mode == "overwrite" {
                conn.query_one(
                    "SELECT EXISTS(SELECT 1 FROM user_skills WHERE slug = $1)",
                    &[&skill.slug.as_str()],
                ).await.map(|r| r.get::<_, bool>(0)).unwrap_or(false)
            } else {
                false
            };

            let result = if conflict_mode == "overwrite" {
                conn.execute(
                    "INSERT INTO user_skills (id, name, slug, description, category, tags, icon, color,
                        allowed_phases, parameters, template, source, version, author, checksum,
                        depends_on, usage_count, approval_status, forked_from, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'community', $12, $13, $14, $15, $16, $17, $18, NOW(), NOW())
                    ON CONFLICT(slug) DO UPDATE SET
                        id = EXCLUDED.id, name = EXCLUDED.name, description = EXCLUDED.description,
                        category = EXCLUDED.category, tags = EXCLUDED.tags, icon = EXCLUDED.icon,
                        color = EXCLUDED.color, allowed_phases = EXCLUDED.allowed_phases,
                        parameters = EXCLUDED.parameters, template = EXCLUDED.template,
                        source = EXCLUDED.source, version = EXCLUDED.version, author = EXCLUDED.author,
                        checksum = EXCLUDED.checksum, depends_on = EXCLUDED.depends_on,
                        usage_count = EXCLUDED.usage_count, approval_status = EXCLUDED.approval_status,
                        forked_from = EXCLUDED.forked_from, updated_at = NOW()",
                    &[
                        &id as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.name as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.slug as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.description as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.category as &(dyn tokio_postgres::types::ToSql + Sync),
                        &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.icon as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.color as &(dyn tokio_postgres::types::ToSql + Sync),
                        &phases_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &params_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &version as &(dyn tokio_postgres::types::ToSql + Sync),
                        &author_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &checksum as &(dyn tokio_postgres::types::ToSql + Sync),
                        &depends_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &usage_count as &(dyn tokio_postgres::types::ToSql + Sync),
                        &approval_status as &(dyn tokio_postgres::types::ToSql + Sync),
                        &forked_from as &(dyn tokio_postgres::types::ToSql + Sync),
                    ],
                ).await
            } else {
                // "skip" mode
                conn.execute(
                    "INSERT INTO user_skills (id, name, slug, description, category, tags, icon, color,
                        allowed_phases, parameters, template, source, version, author, checksum,
                        depends_on, usage_count, approval_status, forked_from, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'community', $12, $13, $14, $15, $16, $17, $18, NOW(), NOW())
                    ON CONFLICT(slug) DO NOTHING",
                    &[
                        &id as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.name as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.slug as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.description as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.category as &(dyn tokio_postgres::types::ToSql + Sync),
                        &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.icon as &(dyn tokio_postgres::types::ToSql + Sync),
                        &skill.color as &(dyn tokio_postgres::types::ToSql + Sync),
                        &phases_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &params_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &version as &(dyn tokio_postgres::types::ToSql + Sync),
                        &author_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &checksum as &(dyn tokio_postgres::types::ToSql + Sync),
                        &depends_json as &(dyn tokio_postgres::types::ToSql + Sync),
                        &usage_count as &(dyn tokio_postgres::types::ToSql + Sync),
                        &approval_status as &(dyn tokio_postgres::types::ToSql + Sync),
                        &forked_from as &(dyn tokio_postgres::types::ToSql + Sync),
                    ],
                ).await
            };

            match result {
                Ok(rows_affected) => {
                    if conflict_mode == "overwrite" {
                        if existed {
                            overwritten += 1;
                        } else {
                            imported += 1;
                        }
                    } else if rows_affected == 0 {
                        skipped += 1;
                    } else {
                        imported += 1;
                    }
                }
                Err(e) => {
                    errors.push(format!("Failed to import skill '{}': {}", skill.slug, e));
                }
            }
        }

        Ok(crate::skills::SkillImportResult {
            imported,
            skipped,
            overwritten,
            errors,
            warnings: Vec::new(),
        })
    }

    /// Update a user skill (COALESCE pattern for optional fields).
    pub async fn update_user_skill(
        &self,
        id: &str,
        request: &crate::mcp::skills::UpdateSkillRequest,
    ) -> Result<SkillDefinition, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Read existing skill to merge optional fields
        let current = self.get_user_skill(id).await?
            .ok_or_else(|| format!("User skill not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let slug = request.slug.as_ref().unwrap_or(&current.slug);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let icon = request.icon.as_ref().unwrap_or(&current.icon);
        let color = request.color.as_ref().unwrap_or(&current.color);
        let allowed_phases = request.allowed_phases.as_ref().unwrap_or(&current.allowed_phases);
        let parameters = request.parameters.as_ref().unwrap_or(&current.parameters);
        let template = request.template.as_ref().unwrap_or(&current.template);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let phases_json = serde_json::to_string(allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json = serde_json::to_string(parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        let new_id = format!("user:{}", slug);

        conn.execute(
            "UPDATE user_skills SET
                id = $1, name = $2, slug = $3, description = $4, category = $5,
                tags = $6, icon = $7, color = $8, allowed_phases = $9,
                parameters = $10, template = $11, updated_at = NOW()
            WHERE id = $12",
            &[
                &new_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &slug as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &category as &(dyn tokio_postgres::types::ToSql + Sync),
                &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &icon as &(dyn tokio_postgres::types::ToSql + Sync),
                &color as &(dyn tokio_postgres::types::ToSql + Sync),
                &phases_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &params_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG update_user_skill: {}", e))?;

        self.get_user_skill(&new_id).await?
            .ok_or_else(|| "Failed to retrieve updated skill".to_string())
    }

    /// Export user skills. If `ids` is empty, exports all.
    pub async fn export_user_skills(
        &self,
        ids: &[String],
    ) -> Result<Vec<SkillDefinition>, String> {
        if ids.is_empty() {
            return self.list_user_skills().await;
        }

        // Filter from full list for simplicity (PG doesn't have dynamic IN-clause easily)
        let all = self.list_user_skills().await?;
        Ok(all.into_iter().filter(|s| ids.contains(&s.id)).collect())
    }

    /// Update the approval status of a skill.
    pub async fn update_skill_approval(&self, skill_id: &str, status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.execute(
            "UPDATE user_skills SET approval_status = $1, updated_at = NOW() WHERE id = $2",
            &[
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &skill_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG update_skill_approval: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Fork a skill by creating a copy with a new ID.
    pub async fn fork_skill(
        &self,
        skill_id: &str,
        new_name: Option<&str>,
    ) -> Result<SkillDefinition, String> {
        let original = self.get_user_skill(skill_id).await?
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

        let tags_json = serde_json::to_string(&original.tags).unwrap_or_else(|_| "[]".to_string());
        let phases_json = serde_json::to_string(&original.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json = serde_json::to_string(&original.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&original.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;
        let author_json: Option<String> = original.author.as_ref().and_then(|a| serde_json::to_string(a).ok());
        let depends_json: Option<String> = original.depends_on.as_ref().and_then(|d| serde_json::to_string(d).ok());

        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO user_skills (id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source, version, author, checksum,
                depends_on, usage_count, approval_status, forked_from, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'user', '1.0.0', $12, NULL, $13, 0, NULL, $14, NOW(), NOW())",
            &[
                &fork_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &fork_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &fork_slug as &(dyn tokio_postgres::types::ToSql + Sync),
                &original.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &original.category as &(dyn tokio_postgres::types::ToSql + Sync),
                &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &original.icon as &(dyn tokio_postgres::types::ToSql + Sync),
                &original.color as &(dyn tokio_postgres::types::ToSql + Sync),
                &phases_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &params_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &author_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &depends_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &skill_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG fork_skill: {}", e))?;

        self.get_user_skill(&fork_id).await?
            .ok_or_else(|| "Failed to retrieve forked skill".to_string())
    }

    /// Update the version and checksum of a skill.
    pub async fn update_skill_version(
        &self,
        skill_id: &str,
        version: &str,
        checksum: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.execute(
            "UPDATE user_skills SET version = $1, checksum = $2, updated_at = NOW() WHERE id = $3",
            &[
                &version as &(dyn tokio_postgres::types::ToSql + Sync),
                &checksum as &(dyn tokio_postgres::types::ToSql + Sync),
                &skill_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG update_skill_version: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Create a new user skill.
    pub async fn create_user_skill(
        &self,
        request: &crate::mcp::skills::CreateSkillRequest,
    ) -> Result<SkillDefinition, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("user:{}", request.slug);
        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let phases_json = serde_json::to_string(&request.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json = serde_json::to_string(&request.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&request.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;
        let version = "1.0.0";
        let source = "user";
        let no_str: Option<&str> = None;

        qontinui_db::queries::skills::create_user_skill()
            .bind(
                &conn,
                &id.as_str(),
                &request.name.as_str(),
                &request.slug.as_str(),
                &Some(request.description.as_str()),
                &request.category.as_str(),
                &tags_json.as_str(),
                &request.icon.as_str(),
                &request.color.as_str(),
                &phases_json.as_str(),
                &params_json.as_str(),
                &template_json.as_str(),
                &source,
                &version,
                &no_str,     // author
                &no_str,     // checksum
                &no_str,     // depends_on
                &no_str,     // approval_status
                &no_str,     // forked_from
                &no_str,     // source_fix_id
                &no_str,     // source_pattern_id
            )
            .one()
            .await
            .map_err(|e| format!("PG create_user_skill: {}", e))?;

        self.get_user_skill(&id).await?
            .ok_or_else(|| "Failed to retrieve created skill".to_string())
    }
}
