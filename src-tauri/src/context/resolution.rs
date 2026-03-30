//! Context resolution, auto-include evaluation, formatting, and injection.

#![allow(dead_code)]

use super::builtins::get_builtin_contexts;
use super::metadata::record_context_use;
use super::project_contexts::get_project_contexts;
use super::storage::load_user_context_library;
use super::types::{AutoDetectReason, Context, ResolvedContexts};
use super::user_contexts::get_all_user_contexts;

/// Evaluate if a context should be auto-included based on its rules.
///
/// Returns true if any of the auto-include rules match.
pub fn should_auto_include(
    context: &Context,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> bool {
    let Some(ref rules) = context.auto_include else {
        return false;
    };

    let task_lower = task_prompt.to_lowercase();

    // Check task mentions
    if let Some(ref mentions) = rules.task_mentions {
        if mentions
            .iter()
            .any(|m| task_lower.contains(&m.to_lowercase()))
        {
            return true;
        }
    }

    // Check action types
    if let Some(ref types) = rules.action_types {
        if types
            .iter()
            .any(|t| action_types.iter().any(|a| a.eq_ignore_ascii_case(t)))
        {
            return true;
        }
    }

    // Check error patterns (regex)
    if let Some(ref patterns) = rules.error_patterns {
        for pattern in patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if recent_errors.iter().any(|e| re.is_match(e)) {
                    return true;
                }
            }
        }
    }

    false
}

/// Resolve which contexts should be included in a prompt.
///
/// This function:
/// 1. Looks up explicit context IDs from user, project, and builtin sources
/// 2. If auto_detect is true, evaluates all enabled contexts against the task/config
/// 3. Merges and deduplicates the results
pub fn resolve_contexts(
    explicit_ids: &[String],
    auto_detect: bool,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> ResolvedContexts {
    let mut result = ResolvedContexts::default();
    let mut included_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Gather all available contexts (user + project + builtin)
    let user_contexts = get_all_user_contexts();
    let project_contexts = get_project_contexts();
    let builtin_contexts = get_builtin_contexts();
    let library = load_user_context_library();

    // Create a lookup map
    let all_contexts: Vec<&Context> = user_contexts
        .iter()
        .chain(project_contexts.iter())
        .chain(builtin_contexts.iter())
        .collect();

    // Step 1: Resolve explicit IDs
    for id in explicit_ids {
        if included_ids.contains(id) {
            continue;
        }

        if let Some(ctx) = all_contexts.iter().find(|c| c.id == *id) {
            // Check if enabled (for user contexts)
            let is_enabled = library
                .metadata
                .iter()
                .find(|m| m.context_id == *id)
                .map(|m| m.enabled)
                .unwrap_or(true); // Builtin contexts are always enabled

            if is_enabled {
                result.explicit.push((*ctx).clone());
                included_ids.insert(id.clone());
            }
        }
    }

    // Step 2: Auto-detect if enabled
    if auto_detect {
        for ctx in &all_contexts {
            if included_ids.contains(&ctx.id) {
                continue;
            }

            // Check if enabled
            let is_enabled = library
                .metadata
                .iter()
                .find(|m| m.context_id == ctx.id)
                .map(|m| m.enabled)
                .unwrap_or(true);

            if !is_enabled {
                continue;
            }

            // Check auto-include rules
            if let Some(ref rules) = ctx.auto_include {
                let task_lower = task_prompt.to_lowercase();

                // Check task mentions
                if let Some(ref mentions) = rules.task_mentions {
                    for mention in mentions {
                        if task_lower.contains(&mention.to_lowercase()) {
                            result.auto_detected.push((*ctx).clone());
                            result.auto_detect_reasons.push(AutoDetectReason {
                                context_id: ctx.id.clone(),
                                reason: "taskMention".to_string(),
                                matched_trigger: mention.clone(),
                            });
                            included_ids.insert(ctx.id.clone());
                            break;
                        }
                    }
                }

                // Skip if already included
                if included_ids.contains(&ctx.id) {
                    continue;
                }

                // Check action types
                if let Some(ref types) = rules.action_types {
                    for action_type in types {
                        if action_types
                            .iter()
                            .any(|a| a.eq_ignore_ascii_case(action_type))
                        {
                            result.auto_detected.push((*ctx).clone());
                            result.auto_detect_reasons.push(AutoDetectReason {
                                context_id: ctx.id.clone(),
                                reason: "actionType".to_string(),
                                matched_trigger: action_type.clone(),
                            });
                            included_ids.insert(ctx.id.clone());
                            break;
                        }
                    }
                }

                // Skip if already included
                if included_ids.contains(&ctx.id) {
                    continue;
                }

                // Check error patterns (regex)
                if let Some(ref patterns) = rules.error_patterns {
                    for pattern in patterns {
                        if let Ok(re) = regex::Regex::new(pattern) {
                            for error in recent_errors {
                                if re.is_match(error) {
                                    result.auto_detected.push((*ctx).clone());
                                    result.auto_detect_reasons.push(AutoDetectReason {
                                        context_id: ctx.id.clone(),
                                        reason: "errorPattern".to_string(),
                                        matched_trigger: pattern.clone(),
                                    });
                                    included_ids.insert(ctx.id.clone());
                                    break;
                                }
                            }
                        }
                        if included_ids.contains(&ctx.id) {
                            break;
                        }
                    }
                }
            }
        }
    }

    result
}

/// Format a single context for injection into a prompt.
fn format_context(ctx: &Context) -> String {
    let category_attr = ctx
        .category
        .as_ref()
        .map(|c| format!(" category=\"{}\"", c))
        .unwrap_or_default();

    format!(
        "<context name=\"{}\"{}>\n{}\n</context>",
        ctx.name, category_attr, ctx.content
    )
}

/// Format resolved contexts into a prompt section for injection.
///
/// Returns None if there are no contexts to inject.
/// Returns Some(String) with formatted contexts if there are any.
pub fn format_contexts_for_prompt(resolved: &ResolvedContexts) -> Option<String> {
    let all_contexts: Vec<&Context> = resolved
        .explicit
        .iter()
        .chain(resolved.auto_detected.iter())
        .collect();

    if all_contexts.is_empty() {
        return None;
    }

    let mut output = String::new();
    output.push_str("## Relevant Context\n\n");
    output.push_str("The following context has been provided to guide your response:\n\n");

    for ctx in &all_contexts {
        output.push_str(&format_context(ctx));
        output.push_str("\n\n");
    }

    output.push_str("---\n\n");

    Some(output)
}

/// Inject contexts into a prompt.
///
/// This is the main entry point for prompt enhancement. It:
/// 1. Resolves which contexts to include (explicit + auto-detected)
/// 2. Formats the contexts
/// 3. Prepends them to the original prompt
/// 4. Returns the enhanced prompt and the list of context IDs that were used
pub fn inject_contexts(
    base_prompt: &str,
    context_ids: &[String],
    auto_detect: bool,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> (String, Vec<String>) {
    let resolved = resolve_contexts(
        context_ids,
        auto_detect,
        task_prompt,
        action_types,
        recent_errors,
    );

    // Collect all context IDs that were used
    let used_ids: Vec<String> = resolved
        .explicit
        .iter()
        .chain(resolved.auto_detected.iter())
        .map(|c| c.id.clone())
        .collect();

    // Format contexts for injection
    let context_section = format_contexts_for_prompt(&resolved);

    // Build the enhanced prompt
    let enhanced = match context_section {
        Some(section) => format!("{}{}", section, base_prompt),
        None => base_prompt.to_string(),
    };

    (enhanced, used_ids)
}

/// Format observation memory as a prompt section.
///
/// Fetches relevant observations from PostgreSQL and formats them as a
/// markdown section for injection into AI prompts. Returns None if PG
/// is unavailable or no observations found.
///
/// Accepts either a project_id (for project-scoped retrieval) or a
/// search_query (e.g. workflow name) for relevance-based retrieval.
/// Uses progressive disclosure: 300-char previews with IDs.
pub async fn format_observation_memory_for_prompt(
    pg_db: &crate::database::pg::PgDb,
    project_id: Option<&str>,
    search_query: Option<&str>,
) -> Option<String> {
    let pg = pg_db;

    let observations = if let Some(pid) = project_id.filter(|s| !s.is_empty()) {
        pg.get_project_context(pid, None, 15)
            .await
            .unwrap_or_default()
    } else if let Some(query) = search_query.filter(|s| !s.is_empty()) {
        pg.search_observations(query, None, 10)
            .await
            .unwrap_or_default()
    } else {
        return None;
    };

    if observations.is_empty() {
        return None;
    }

    let mut output = String::new();
    output.push_str("## Memory Context (from past sessions)\n\n");
    output.push_str(&format!(
        "{} relevant observation(s) from previous work.\n\n",
        observations.len()
    ));

    for obs in &observations {
        let rev = if obs.revision_count > 1 {
            format!(" (rev {})", obs.revision_count)
        } else {
            String::new()
        };
        let topic = obs
            .topic_key
            .as_deref()
            .map(|k| format!(" [{}]", k))
            .unwrap_or_default();
        output.push_str(&format!(
            "- **{}**{}{} (id: {}, type: {})\n  {}\n\n",
            obs.title, rev, topic, obs.id, obs.observation_type, obs.content_preview
        ));
    }

    output.push_str("---\n\n");
    Some(output)
}

/// Record that multiple contexts were used in a task.
///
/// This updates use_count and last_used_at for each context.
pub fn record_contexts_used(context_ids: &[String]) {
    for id in context_ids {
        if let Err(e) = record_context_use(id) {
            tracing::warn!("Failed to record context use for {}: {}", id, e);
        }
    }
}

/// Format a context for injection into a prompt (used for special contexts).
pub fn format_single_context(ctx: &Context) -> String {
    let category_attr = ctx
        .category
        .as_ref()
        .map(|c| format!(" category=\"{}\"", c))
        .unwrap_or_default();

    format!(
        "<context name=\"{}\"{}>\n{}\n</context>",
        ctx.name, category_attr, ctx.content
    )
}
