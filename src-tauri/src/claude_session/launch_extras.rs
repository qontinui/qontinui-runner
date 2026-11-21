//! The runner-owned argv tail and env a stream-json [`super::ClaudeSession`]
//! child is spawned with, composed in one place so every arm is assertable
//! without spawning `claude` (plan
//! `2026-11-21-sessions-spawn-takes-the-parent-mcp-json-and-acts-as-the-default-tenant`,
//! Phases 2-3).
//!
//! `ClaudeSession::spawn` is a DIRECT exec: no identity shim sits between the
//! runner and `claude`, so every flag the shim would add for a terminal has to
//! be spelled out here or the session runs without it. That is how this spawn
//! used to run with no coord-mcp of its own (it loaded an ancestor `.mcp.json`
//! and acted as the device's default tenant), no hook carrier and no spawn-time
//! policy body. The tail now carries, in order:
//!
//! 1. the stream-json protocol flags;
//! 2. the session's coord-mcp ([`super::coord_mcp_delivery`]);
//! 3. the blueprint tool policy's allow/deny flags;
//! 4. ONE `--settings` — the hook carrier, merged with the tool policy's
//!    settings block when there is one. Two `--settings` flags do not merge:
//!    only the last applies (finding `a1a66bfe` case d), so passing both would
//!    silently drop the hooks or the deny rules;
//! 5. the system-prompt carrier (briefing, plus the session tenant's cached
//!    policy body when there is one).

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::coord_mcp_delivery::AiSessionCoordMcp;
use crate::coord_mcp::CoordMcpDelivery;
use crate::session::spawn_prompt::SystemPromptCarrier;

/// Everything [`compose_launch_extras`] reads. Every ambient input is a field,
/// so a test can point it at a tempdir.
pub(crate) struct LaunchExtrasInputs<'a> {
    /// The child's cwd.
    pub(crate) working_dir: &'a str,
    /// The session id — the coord-mcp delivery's key, and the id the child
    /// exports for the policy route.
    pub(crate) session_id: &'a str,
    /// The account config dir the child runs under (`None`: ambient default).
    pub(crate) config_dir: Option<&'a str>,
    pub(crate) tool_policy: Option<&'a crate::workflow::dag_schema::ToolPolicy>,
    /// The runner API port a coord-mcp config may point at
    /// ([`crate::coord_mcp::resolve_bound_api_port`]); `None` fails closed.
    pub(crate) bound_port: Option<u16>,
    /// The port the child's hooks call ([`crate::terminal::spawn_seam_api_port`]).
    pub(crate) api_port: u16,
    /// The materialized hook-carrier settings file
    /// ([`crate::session::claude_hook::materialize`]); `None` when it could not
    /// be written (the session then simply has no hooks).
    pub(crate) hook_settings: Option<PathBuf>,
    /// Where a merged settings file is written.
    pub(crate) scratch_dir: &'a Path,
}

/// What a stream-json spawn adds to its launch.
#[derive(Debug)]
pub(crate) struct LaunchExtras {
    /// The caller-required argv tail ([`super::launch_spec::LaunchSpec::extra_required`]).
    pub(crate) extra_required: Vec<String>,
    /// Env for the child: `(name, Some(value))` to set, `(name, None)` to
    /// remove. Applied before `ClaudeSession::finalize_child_env`.
    pub(crate) env: Vec<(&'static str, Option<String>)>,
    /// The system-prompt carrier the tail carries, for the delivered-policy
    /// marker the spawn derives once its final argv is rendered.
    pub(crate) prompt_carrier: Option<SystemPromptCarrier>,
    /// The coord-mcp delivery, whose lease the spawn hands to the child's
    /// waiter. Dropping it (any error before the child exists) retires it.
    pub(crate) coord_mcp: AiSessionCoordMcp,
}

/// Compose the tail and env. `prompt` turns the session's coord-mcp outcome
/// into its system-prompt carrier (production: the runner briefing for that
/// outcome plus the session tenant's policy body —
/// [`crate::session::spawn_prompt::resolve_system_prompt_carrier`]).
///
/// `Err` refuses the spawn before any child exists (see
/// [`super::coord_mcp_delivery::deliver_for_ai_session`], and a tool-policy
/// settings file that could not be written).
pub(crate) fn compose_launch_extras(
    inputs: LaunchExtrasInputs<'_>,
    prompt: impl FnOnce(CoordMcpDelivery) -> Option<SystemPromptCarrier>,
) -> Result<LaunchExtras, String> {
    let mut extra_required = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];

    let coord_mcp = super::coord_mcp_delivery::deliver_for_ai_session(
        inputs.working_dir,
        inputs.session_id,
        inputs.config_dir,
        inputs.bound_port,
    )?;
    extra_required.extend(coord_mcp.argv().iter().cloned());

    // Blueprint tool policy (Phase 3 of the tool-policy plan): allow/deny
    // flags plus, for argument-scoped denies, a settings block — merged into
    // the one `--settings` below rather than passed as a second.
    let tool_policy_settings = match inputs.tool_policy {
        Some(policy) => {
            let (args, settings_json) = super::tool_policy_args::build_tool_policy_cli(policy);
            extra_required.extend(args);
            settings_json
        }
        None => None,
    };
    extra_required.extend(settings_args(
        inputs.hook_settings.as_deref(),
        tool_policy_settings.as_deref(),
        inputs.scratch_dir,
        inputs.session_id,
    )?);

    let prompt_carrier = prompt(coord_mcp.delivery());
    if let Some(carrier) = &prompt_carrier {
        extra_required.extend(carrier.argv());
    }

    let env = coord_mcp.child_env(inputs.session_id, inputs.api_port);
    Ok(LaunchExtras {
        extra_required,
        env,
        prompt_carrier,
        coord_mcp,
    })
}

/// The ONE `--settings` pair: the hook carrier alone, or — when the tool policy
/// has a settings block — a file merging both. Empty when there is neither.
fn settings_args(
    hook_settings: Option<&Path>,
    tool_policy_settings: Option<&str>,
    scratch_dir: &Path,
    session_id: &str,
) -> Result<Vec<String>, String> {
    let Some(tool_policy_settings) = tool_policy_settings else {
        return Ok(hook_settings
            .map(|p| vec!["--settings".to_string(), p.to_string_lossy().into_owned()])
            .unwrap_or_default());
    };
    let tool: serde_json::Value = serde_json::from_str(tool_policy_settings)
        .map_err(|e| format!("tool-policy settings are not JSON: {e}"))?;
    let hooks = hook_settings.and_then(|path| {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()));
        match parsed {
            Ok(doc) => Some(doc),
            Err(e) => {
                warn!(
                    "hook-carrier settings {} unreadable ({e}) — the merged settings carry the \
                     tool policy only, so this session has no SessionStart hook",
                    path.display()
                );
                None
            }
        }
    });
    let mut merged = hooks.unwrap_or_else(|| serde_json::json!({}));
    merge_settings(&mut merged, tool);
    let file = scratch_dir.join(format!("claude_session_settings_{session_id}.json"));
    std::fs::write(&file, merged.to_string())
        .map_err(|e| format!("Failed to write tool-policy settings file: {e}"))?;
    info!(
        "Applied merged hook-carrier + tool-policy settings for interactive session {} at {}",
        session_id,
        file.display()
    );
    Ok(vec![
        "--settings".to_string(),
        file.to_string_lossy().into_owned(),
    ])
}

/// Merge `overlay` into `base`: objects key by key, recursively; arrays are
/// UNIONED (a deny list in both layers keeps every rule); any other collision
/// takes the overlay's value.
fn merge_settings(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base), serde_json::Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_settings(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (serde_json::Value::Array(base), serde_json::Value::Array(overlay)) => {
            for item in overlay {
                if !base.contains(&item) {
                    base.push(item);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

#[cfg(test)]
#[path = "launch_extras_tests.rs"]
mod tests;
