//! PostgreSQL user skill operations via Clorinde-generated queries.

use super::PgDb;
use crate::skills::{SkillDefinition, SkillTemplate};
use tracing::warn;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() {
        T::default()
    } else {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// Parse a row's stored `template` column.
fn parse_stored_template(raw: &str) -> Result<SkillTemplate, serde_json::Error> {
    serde_json::from_str(raw)
}

/// The template a READ serves for a stored row.
///
/// A stored template that does not parse (a `playbook` kind stored before that
/// variant was deleted, or any malformed value) is served as an empty
/// `single_step`, so one bad row never fails a whole list. That fallback used
/// to be silent; it now WARNs with the skill id and the parse error, because a
/// served template that is not the stored one has to say so.
fn stored_template_or_fallback(skill_id: &str, raw: &str) -> SkillTemplate {
    parse_stored_template(raw).unwrap_or_else(|e| {
        warn!(
            skill_id,
            error = %e,
            "user skill: stored template does not parse; serving an empty single_step template in its place"
        );
        SkillTemplate::SingleStep {
            step: std::collections::HashMap::new(),
        }
    })
}

/// Why a rewrite of a stored skill ([`PgDb::update_user_skill`],
/// [`PgDb::fork_skill`], a version bump) wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum SkillRewriteError {
    /// No user skill has the id.
    #[error("user skill not found: {0}")]
    NotFound(String),
    /// The row's stored template does not parse. A read serves it as the empty
    /// `single_step` fallback ([`stored_template_or_fallback`]), so a rewrite
    /// built from that read would store the fallback in place of the original.
    #[error(
        "user skill {skill_id}: its stored template does not parse ({error}); refusing to \
         rewrite it, because it is served as an empty single_step template and a rewrite \
         would store that in place of the original. Delete the skill and create it again."
    )]
    UnparseableStoredTemplate { skill_id: String, error: String },
    /// A pool, query or serialization failure.
    #[error("{0}")]
    Failed(String),
}

/// Refuse to rewrite a row whose stored template does not parse.
fn require_parseable_stored_template(skill_id: &str, raw: &str) -> Result<(), SkillRewriteError> {
    parse_stored_template(raw).map(|_| ()).map_err(|e| {
        SkillRewriteError::UnparseableStoredTemplate {
            skill_id: skill_id.to_string(),
            error: e.to_string(),
        }
    })
}

/// What an outbound door (export, sync push) may send: the stored skills
/// whose templates parse, and the ones withheld because theirs does not.
#[derive(Debug, Default)]
pub struct ExportableUserSkills {
    pub skills: Vec<SkillDefinition>,
    pub withheld: Vec<crate::skills::WithheldSkill>,
}

/// Map a Clorinde row to SkillDefinition.
macro_rules! row_to_skill {
    ($r:expr) => {{
        // Before the struct literal: `id: $r.id` below moves the id.
        let template = stored_template_or_fallback(&$r.id, &$r.template);
        SkillDefinition {
            id: $r.id,
            name: $r.name,
            slug: $r.slug,
            description: $r.description,
            category: if $r.category.is_empty() {
                "custom".to_string()
            } else {
                $r.category
            },
            tags: json_or_default(&$r.tags),
            icon: if $r.icon.is_empty() {
                "puzzle".to_string()
            } else {
                $r.icon
            },
            color: if $r.color.is_empty() {
                "gray".to_string()
            } else {
                $r.color
            },
            allowed_phases: {
                let v: Vec<String> = json_or_default(&$r.allowed_phases);
                if v.is_empty() {
                    vec!["setup".to_string()]
                } else {
                    v
                }
            },
            parameters: json_or_default(&$r.parameters),
            template,
            // Typed at the boundary (`crate::skills::SkillSource`): an empty
            // column keeps its historical "user" default, and any other value
            // — including one written by a newer schema — is preserved
            // verbatim as `Other` rather than coerced or rejected.
            source: if $r.source.is_empty() {
                crate::skills::SkillSource::User
            } else {
                crate::skills::SkillSource::from_wire(&$r.source)
            },
            version: non_empty($r.version),
            author: {
                let s = &$r.author;
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str(s).ok()
                }
            },
            checksum: non_empty($r.checksum),
            depends_on: {
                let s = &$r.depends_on;
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str(s).ok()
                }
            },
            usage_count: if $r.usage_count == 0 {
                None
            } else {
                Some($r.usage_count as u64)
            },
            approval_status: non_empty($r.approval_status),
            forked_from: non_empty($r.forked_from),
        }
    }};
}

/// Partition stored rows for an outbound door, keeping only the ids asked for
/// (every row when `ids` is empty). A row whose stored template does not parse
/// is WITHHELD rather than served: the read fallback is an empty
/// `single_step`, and sending that on would hand an empty skill to the next
/// runner as if it were the stored one.
fn partition_for_export(
    rows: Vec<qontinui_db::queries::skills::ListUserSkills>,
    ids: &[String],
) -> ExportableUserSkills {
    let mut out = ExportableUserSkills::default();
    for r in rows {
        if !ids.is_empty() && !ids.contains(&r.id) {
            continue;
        }
        match parse_stored_template(&r.template) {
            Ok(_) => out.skills.push(row_to_skill!(r)),
            Err(e) => out
                .withheld
                .push(crate::skills::WithheldSkill::unparseable(&r.id, e)),
        }
    }
    out
}

impl PgDb {
    /// List all user skills.
    pub async fn list_user_skills(&self) -> Result<Vec<SkillDefinition>, String> {
        Ok(self
            .list_user_skill_rows()
            .await?
            .into_iter()
            .map(|r| row_to_skill!(r))
            .collect())
    }

    /// Every stored user-skill row, as stored.
    async fn list_user_skill_rows(
        &self,
    ) -> Result<Vec<qontinui_db::queries::skills::ListUserSkills>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::skills::list_user_skills()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_user_skills", &e))
    }

    /// Get a single user skill by ID.
    pub async fn get_user_skill(&self, id: &str) -> Result<Option<SkillDefinition>, String> {
        Ok(self.get_user_skill_row(id).await?.map(|r| row_to_skill!(r)))
    }

    /// One stored user-skill row by id, as stored.
    async fn get_user_skill_row(
        &self,
        id: &str,
    ) -> Result<Option<qontinui_db::queries::skills::GetUserSkill>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::skills::get_user_skill()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_user_skill", &e))
    }

    /// Read one user skill for a REWRITE ([`Self::update_user_skill`],
    /// [`Self::fork_skill`], the version bump): `Ok(None)` when no row has the
    /// id, and [`SkillRewriteError::UnparseableStoredTemplate`] when the row's
    /// stored template does not parse, because the definition
    /// [`Self::get_user_skill`] would hand back carries the fallback rather
    /// than what is stored.
    pub(crate) async fn get_user_skill_for_rewrite(
        &self,
        id: &str,
    ) -> Result<Option<SkillDefinition>, SkillRewriteError> {
        let Some(r) = self
            .get_user_skill_row(id)
            .await
            .map_err(SkillRewriteError::Failed)?
        else {
            return Ok(None);
        };
        require_parseable_stored_template(&r.id, &r.template)?;
        Ok(Some(row_to_skill!(r)))
    }

    /// Delete a user skill by ID.
    pub async fn delete_user_skill(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::skills::delete_user_skill()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG delete_user_skill", &e))?;
        Ok(deleted.is_some())
    }

    /// Increment usage count for a skill.
    pub async fn increment_skill_usage(&self, id: &str) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let count = qontinui_db::queries::skills::increment_skill_usage()
            .bind(&conn, &id)
            .one()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG increment_skill_usage", &e))?;
        Ok(count as u64)
    }

    /// Get approved auto-extracted skills for prompt injection.
    /// Returns (name, slug, description, category) tuples ordered by usage_count DESC, limited to 10.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_approved_auto_skills(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT name, slug, description, category
               FROM user_skills
               WHERE approval_status = 'approved'
               AND source = 'auto'
               ORDER BY usage_count DESC
               LIMIT 10"#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_approved_auto_skills", &e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let slug: String = row.get(1);
                let description: String = row.try_get(2).unwrap_or_default();
                let category: String = row.try_get(3).unwrap_or_else(|_| "custom".to_string());
                (name, slug, description, category)
            })
            .collect())
    }

    /// Import skills from an export. Sets source to "community" and id to "community:<slug>".
    /// `conflict_mode` is "skip" or "overwrite".
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn import_skills(
        &self,
        skills: &[crate::skills::SkillDefinition],
        conflict_mode: &str,
    ) -> Result<crate::skills::SkillImportResult, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut overwritten = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for skill in skills {
            let id = format!("community:{}", skill.slug);
            let tags_json = serde_json::to_string(&skill.tags).unwrap_or_else(|_| "[]".to_string());
            let phases_json =
                serde_json::to_string(&skill.allowed_phases).unwrap_or_else(|_| "[]".to_string());
            let params_json =
                serde_json::to_string(&skill.parameters).unwrap_or_else(|_| "[]".to_string());
            let template_json = match serde_json::to_string(&skill.template) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!(
                        "Failed to serialize template for '{}': {}",
                        skill.slug, e
                    ));
                    continue;
                }
            };
            let author_json: Option<String> = skill
                .author
                .as_ref()
                .and_then(|a| serde_json::to_string(a).ok());
            let depends_json: Option<String> = skill
                .depends_on
                .as_ref()
                .and_then(|d| serde_json::to_string(d).ok());
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
                )
                .await
                .map(|r| r.get::<_, bool>(0))
                .unwrap_or(false)
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
            refused: Vec::new(),
        })
    }

    /// Update a user skill (COALESCE pattern for optional fields).
    ///
    /// Refused ([`SkillRewriteError::UnparseableStoredTemplate`]) when the
    /// row's stored template does not parse, whether or not the request carries
    /// a template: the merge below would write the served fallback back over
    /// the stored one, and a client that round-trips the template it was served
    /// would send that same fallback.
    pub async fn update_user_skill(
        &self,
        id: &str,
        request: &crate::mcp::skills::UpdateSkillRequest,
    ) -> Result<SkillDefinition, SkillRewriteError> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| SkillRewriteError::Failed(format!("PG pool error: {}", e)))?;

        // Read existing skill to merge optional fields
        let current = self
            .get_user_skill_for_rewrite(id)
            .await?
            .ok_or_else(|| SkillRewriteError::NotFound(id.to_string()))?;

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
        let phases_json =
            serde_json::to_string(allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json = serde_json::to_string(parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(template).map_err(|e| {
            SkillRewriteError::Failed(format!("Failed to serialize template: {}", e))
        })?;

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
        )
        .await
        .map_err(|e| {
            SkillRewriteError::Failed(crate::database::pg::pg_err("PG update_user_skill", &e))
        })?;

        self.get_user_skill(&new_id)
            .await
            .map_err(SkillRewriteError::Failed)?
            .ok_or_else(|| {
                SkillRewriteError::Failed("Failed to retrieve updated skill".to_string())
            })
    }

    /// The user skills an outbound door (export, sync push) may send; every
    /// one when `ids` is empty. A skill whose stored template does not parse
    /// is withheld, not sent ([`partition_for_export`]).
    pub async fn export_user_skills(&self, ids: &[String]) -> Result<ExportableUserSkills, String> {
        // Filtered in Rust: the generated query has no dynamic IN clause.
        Ok(partition_for_export(
            self.list_user_skill_rows().await?,
            ids,
        ))
    }

    /// Update the approval status of a skill.
    pub async fn update_skill_approval(&self, skill_id: &str, status: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET approval_status = $1, updated_at = NOW() WHERE id = $2",
                &[
                    &status as &(dyn tokio_postgres::types::ToSql + Sync),
                    &skill_id as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG update_skill_approval", &e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Fork a skill by creating a copy with a new ID.
    ///
    /// Refused ([`SkillRewriteError::UnparseableStoredTemplate`]) when the
    /// original's stored template does not parse: the copy would carry the
    /// served fallback as if it were the original's template.
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    pub async fn fork_skill(
        &self,
        skill_id: &str,
        new_name: Option<&str>,
    ) -> Result<SkillDefinition, SkillRewriteError> {
        let original = self
            .get_user_skill_for_rewrite(skill_id)
            .await?
            .ok_or_else(|| SkillRewriteError::NotFound(skill_id.to_string()))?;

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
        let phases_json =
            serde_json::to_string(&original.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json =
            serde_json::to_string(&original.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&original.template).map_err(|e| {
            SkillRewriteError::Failed(format!("Failed to serialize template: {}", e))
        })?;
        let author_json: Option<String> = original
            .author
            .as_ref()
            .and_then(|a| serde_json::to_string(a).ok());
        let depends_json: Option<String> = original
            .depends_on
            .as_ref()
            .and_then(|d| serde_json::to_string(d).ok());

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| SkillRewriteError::Failed(format!("PG pool error: {}", e)))?;
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
        ).await.map_err(|e| SkillRewriteError::Failed(crate::database::pg::pg_err("PG fork_skill", &e)))?;

        self.get_user_skill(&fork_id)
            .await
            .map_err(SkillRewriteError::Failed)?
            .ok_or_else(|| SkillRewriteError::Failed("Failed to retrieve forked skill".to_string()))
    }

    /// Update the version and checksum of a skill.
    pub async fn update_skill_version(
        &self,
        skill_id: &str,
        version: &str,
        checksum: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.execute(
            "UPDATE user_skills SET version = $1, checksum = $2, updated_at = NOW() WHERE id = $3",
            &[
                &version as &(dyn tokio_postgres::types::ToSql + Sync),
                &checksum as &(dyn tokio_postgres::types::ToSql + Sync),
                &skill_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| crate::database::pg::pg_err("PG update_skill_version", &e))?;

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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("user:{}", request.slug);
        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let phases_json =
            serde_json::to_string(&request.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let params_json =
            serde_json::to_string(&request.parameters).unwrap_or_else(|_| "[]".to_string());
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
                &no_str, // author
                &no_str, // checksum
                &no_str, // depends_on
                &no_str, // approval_status
                &no_str, // forked_from
                &no_str, // source_fix_id
                &no_str, // source_pattern_id
            )
            .one()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG create_user_skill", &e))?;

        self.get_user_skill(&id)
            .await?
            .ok_or_else(|| "Failed to retrieve created skill".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_guard::capture_logs;

    /// A stored row as Clorinde returns it, with `template` as stored.
    fn stored_row(id: &str, template: &str) -> qontinui_db::queries::skills::ListUserSkills {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-07T00:00:00Z")
            .expect("fixed timestamp parses");
        qontinui_db::queries::skills::ListUserSkills {
            id: id.to_string(),
            name: "Salesforce login".to_string(),
            slug: "sf-login".to_string(),
            description: String::new(),
            category: String::new(),
            tags: String::new(),
            icon: String::new(),
            color: String::new(),
            allowed_phases: String::new(),
            parameters: String::new(),
            template: template.to_string(),
            source: String::new(),
            version: String::new(),
            author: String::new(),
            checksum: String::new(),
            depends_on: String::new(),
            usage_count: 0,
            approval_status: String::new(),
            forked_from: String::new(),
            created_at: at,
            updated_at: at,
        }
    }

    /// The plan's case: a stored `{"kind":"playbook","content":"x"}` row is
    /// served as the empty `single_step` fallback, and the read WARNs naming
    /// the skill and the parse error. Control, through the same capture: a
    /// parseable row is served as stored and logs nothing, so the warning is
    /// the unparseable template's and not the capture's.
    #[test]
    fn a_stored_playbook_row_is_served_as_the_fallback_with_a_warning_naming_the_skill() {
        let row = stored_row("user:sf-login", r#"{"kind":"playbook","content":"x"}"#);
        let (skill, logged) = capture_logs(|| row_to_skill!(row));
        assert_eq!(
            serde_json::to_value(&skill.template).expect("a served template serializes"),
            serde_json::json!({"kind": "single_step", "step": {}}),
            "served as the empty single_step fallback"
        );
        assert!(
            logged.contains("stored template does not parse"),
            "{logged}"
        );
        assert!(
            logged.contains("user:sf-login"),
            "names the skill: {logged}"
        );
        assert!(
            logged.contains("unknown variant `playbook`"),
            "carries the parse error: {logged}"
        );

        let row = stored_row(
            "user:lint",
            r#"{"kind":"single_step","step":{"type":"command"}}"#,
        );
        let (skill, logged) = capture_logs(|| row_to_skill!(row));
        assert_eq!(
            serde_json::to_value(&skill.template).expect("a served template serializes"),
            serde_json::json!({"kind": "single_step", "step": {"type": "command"}}),
            "a parseable row is served as stored"
        );
        assert!(logged.is_empty(), "a parseable row logs nothing: {logged}");
    }

    /// Every arm of the rewrite gate that update and fork read through. A
    /// stored template that does not parse (the retired kind, a template
    /// missing its required field, empty, not JSON) is refused with the skill
    /// id; every live kind passes.
    #[test]
    fn a_rewrite_refuses_a_stored_template_that_does_not_parse() {
        for raw in [
            r#"{"kind":"playbook","content":"x"}"#,
            r#"{"kind":"single_step"}"#,
            "",
            "not json",
        ] {
            match require_parseable_stored_template("user:sf-login", raw) {
                Err(SkillRewriteError::UnparseableStoredTemplate { skill_id, error }) => {
                    assert_eq!(skill_id, "user:sf-login", "{raw:?}");
                    assert!(!error.is_empty(), "{raw:?}");
                }
                other => panic!("{raw:?} must be refused, got {other:?}"),
            }
        }
        for raw in [
            r#"{"kind":"single_step","step":{}}"#,
            r#"{"kind":"multi_step","steps":[]}"#,
            r#"{"kind":"composition","skill_refs":[]}"#,
        ] {
            assert!(
                require_parseable_stored_template("user:x", raw).is_ok(),
                "{raw} must pass"
            );
        }
    }

    /// The refusal's message names the skill, the cause and the remedy.
    #[test]
    fn the_rewrite_refusal_names_the_skill_the_cause_and_the_remedy() {
        let refusal = require_parseable_stored_template(
            "user:sf-login",
            r#"{"kind":"playbook","content":"x"}"#,
        )
        .expect_err("a playbook template is refused");
        let message = refusal.to_string();
        assert!(message.contains("user:sf-login"), "{message}");
        assert!(message.contains("unknown variant `playbook`"), "{message}");
        assert!(message.contains("Delete the skill"), "{message}");
    }

    /// An outbound door (export, sync push) WITHHOLDS a row whose stored
    /// template does not parse, naming it with its code and parse error,
    /// rather than sending the served fallback; parseable rows go out as
    /// stored, and a non-empty id list keeps only the ids asked for.
    #[test]
    fn an_outbound_door_withholds_a_stored_template_that_does_not_parse() {
        let rows = || {
            vec![
                stored_row(
                    "user:lint",
                    r#"{"kind":"single_step","step":{"type":"command"}}"#,
                ),
                stored_row("user:sf-login", r#"{"kind":"playbook","content":"x"}"#),
                stored_row("user:notes", r#"{"kind":"multi_step","steps":[]}"#),
            ]
        };

        let all = partition_for_export(rows(), &[]);
        let sent: Vec<&str> = all.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(sent, ["user:lint", "user:notes"]);
        assert_eq!(all.withheld.len(), 1, "{:?}", all.withheld);
        assert_eq!(all.withheld[0].skill_id, "user:sf-login");
        assert_eq!(all.withheld[0].code, "STORED_SKILL_TEMPLATE_UNPARSEABLE");
        assert!(
            all.withheld[0].error.contains("unknown variant `playbook`"),
            "{:?}",
            all.withheld
        );

        let asked = partition_for_export(rows(), &["user:sf-login".to_string()]);
        assert!(asked.skills.is_empty(), "{:?}", asked.skills);
        assert_eq!(asked.withheld.len(), 1);

        let asked = partition_for_export(rows(), &["user:lint".to_string()]);
        assert_eq!(asked.skills.len(), 1);
        assert!(asked.withheld.is_empty());
    }

    /// The rewrite gate at its call sites, against a live Postgres: update
    /// (with and without a template in the request), fork, and the version
    /// bump's read all refuse a stored `playbook` row with
    /// `UnparseableStoredTemplate`, and the stored template column is
    /// byte-identical afterwards. Reverting any call site to the plain
    /// `get_user_skill` turns this red.
    ///
    /// `#[ignore]` to match the `database/pg/*` convention. Run with:
    /// `cargo test -p qontinui-runner database::pg::skills::tests::every_rewrite_refuses_a_stored_playbook_row_and_leaves_it_as_stored -- --ignored`
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); user_skills table"]
    async fn every_rewrite_refuses_a_stored_playbook_row_and_leaves_it_as_stored() {
        let pg = PgDb::new_for_test().await;
        let slug = format!("p7-playbook-{}", uuid::Uuid::new_v4());
        let id = format!("user:{slug}");
        let stored = r#"{"kind":"playbook","content":"x"}"#;
        let conn = pg.pool.get().await.expect("pool");
        conn.execute(
            "INSERT INTO user_skills (id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source, version, usage_count,
                created_at, updated_at)
            VALUES ($1, 'Salesforce login', $2, '', 'custom', '[]', 'puzzle', 'gray',
                '[\"setup\"]', '[]', $3, 'user', '1.0.0', 0, NOW(), NOW())",
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &slug,
                &stored,
            ],
        )
        .await
        .expect("insert the stored playbook row");

        let refused = |r: Result<SkillDefinition, SkillRewriteError>, what: &str| match r {
            Err(SkillRewriteError::UnparseableStoredTemplate { skill_id, .. }) => {
                assert_eq!(skill_id, id, "{what}")
            }
            other => panic!("{what} must be refused, got {other:?}"),
        };
        let no_template: crate::mcp::skills::UpdateSkillRequest =
            serde_json::from_value(serde_json::json!({"name": "renamed"})).expect("request");
        refused(
            pg.update_user_skill(&id, &no_template).await,
            "update without a template",
        );
        let with_template: crate::mcp::skills::UpdateSkillRequest =
            serde_json::from_value(serde_json::json!({
                "template": {"kind": "single_step", "step": {}}
            }))
            .expect("request");
        refused(
            pg.update_user_skill(&id, &with_template).await,
            "update carrying the served fallback",
        );
        refused(pg.fork_skill(&id, None).await, "fork");
        assert!(
            matches!(
                pg.get_user_skill_for_rewrite(&id).await,
                Err(SkillRewriteError::UnparseableStoredTemplate { .. })
            ),
            "the version bump's read"
        );

        let row = conn
            .query_one("SELECT template FROM user_skills WHERE id = $1", &[&id])
            .await
            .expect("the row is still there");
        let after: String = row.try_get(0).expect("template column");
        let forks = conn
            .query_one(
                "SELECT count(*) FROM user_skills WHERE forked_from = $1",
                &[&id],
            )
            .await
            .expect("count forks");
        let fork_count: i64 = forks.try_get(0).expect("count");
        conn.execute("DELETE FROM user_skills WHERE id = $1", &[&id])
            .await
            .expect("clean up");
        assert_eq!(after, stored, "the stored template is untouched");
        assert_eq!(fork_count, 0, "fork wrote no copy");
    }
}
