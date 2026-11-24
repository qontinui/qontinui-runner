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
    /// The operator launch config the spawn renders with. Its template's own
    /// `--settings` and append-prompt flags are dropped from the argv because
    /// the spawn owns those flags (`OWNED_TEMPLATE_FLAGS`), so they are
    /// FOLDED IN here instead of lost: a
    /// template settings file into the one merged settings file (runner keys
    /// win), a template prompt after the runner's in the one carrier. A
    /// template `--mcp-config` stays dropped — the runner's coord-mcp is
    /// authoritative (plan D1).
    pub(crate) launch_cfg: &'a super::launch_spec::LaunchConfig,
    /// The permission mode the spawn will RENDER — the spec's site default
    /// after the operator's posture ([`super::launch_spec::SessionPermissionSetting::apply`]).
    /// Under an allow-list no template settings are folded: the launch seam
    /// drops a template `--settings` there because its `permissions.allow` and
    /// hooks can pre-approve tools past the list, and folding it would undo
    /// that.
    pub(crate) permission: &'a super::launch_spec::PermissionMode,
    /// The session id the template's `{sessionId}` is substituted with (the
    /// resume id, else the new-session id) — the same pin the launch seam uses.
    pub(crate) template_pin: Option<&'a str>,
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
    /// Template flags the spawn owns on every arm
    /// ([`super::launch_spec::LaunchSpec::owned_flags`]): `--mcp-config`,
    /// `--strict-mcp-config`, `--settings`, `--append-system-prompt` and
    /// `--append-system-prompt-file`. A template copy of any of them never
    /// reaches the argv, whatever the coord-mcp arm; the settings and prompts
    /// reach the session only through the fold.
    pub(crate) owned_flags: Vec<String>,
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
    let template_settings = template_settings_to_fold(&inputs);
    extra_required.extend(settings_args(
        inputs.hook_settings.as_deref(),
        tool_policy_settings.as_deref(),
        &template_settings,
        Path::new(inputs.working_dir),
        inputs.scratch_dir,
        inputs.session_id,
    )?);

    let template_prompts = super::launch_spec::template_flag_values(
        inputs.launch_cfg,
        inputs.template_pin,
        &[
            crate::session::spawn_prompt::APPEND_SYSTEM_PROMPT_FLAG,
            crate::session::spawn_prompt::APPEND_SYSTEM_PROMPT_FILE_FLAG,
        ],
    );
    let prompt_carrier = fold_template_prompts(
        prompt(coord_mcp.delivery()),
        &template_prompts,
        Path::new(inputs.working_dir),
    );
    if let Some(carrier) = &prompt_carrier {
        extra_required.extend(carrier.argv());
    }

    let env = coord_mcp.child_env(inputs.session_id, inputs.api_port);
    Ok(LaunchExtras {
        extra_required,
        env,
        prompt_carrier,
        coord_mcp,
        owned_flags: OWNED_TEMPLATE_FLAGS.iter().map(|f| f.to_string()).collect(),
    })
}

/// Every template flag the stream-json spawn owns on every arm, so the fold is
/// the ONLY route a template's settings, prompt or MCP config can take: a
/// template copy never reaches the argv, even when the fold consumed nothing
/// (an unreadable settings file, an empty prompt) and the tail supplies no flag
/// of that name for the name-only dedup to match.
const OWNED_TEMPLATE_FLAGS: [&str; 5] = [
    super::coord_mcp_delivery::OWNED_MCP_TEMPLATE_FLAGS[0],
    super::coord_mcp_delivery::OWNED_MCP_TEMPLATE_FLAGS[1],
    "--settings",
    crate::session::spawn_prompt::APPEND_SYSTEM_PROMPT_FLAG,
    crate::session::spawn_prompt::APPEND_SYSTEM_PROMPT_FILE_FLAG,
];

/// The template `--settings` values to fold into the one settings file —
/// none under an allow-list (see [`LaunchExtrasInputs::permission`]).
fn template_settings_to_fold(inputs: &LaunchExtrasInputs<'_>) -> Vec<String> {
    let values: Vec<String> = super::launch_spec::template_flag_values(
        inputs.launch_cfg,
        inputs.template_pin,
        &["--settings"],
    )
    .into_iter()
    .map(|(_, value)| value)
    .collect();
    if !values.is_empty()
        && matches!(
            inputs.permission,
            super::launch_spec::PermissionMode::AllowList { .. }
        )
    {
        warn!(
            "launch template --settings not merged: the session runs under the allow-list \
             permission mode, which a template's settings layer (permissions, hooks) could widen"
        );
        return Vec::new();
    }
    values
}

/// `value` as a path, resolved against the session's cwd when relative — the
/// directory Claude Code itself resolves it from.
fn resolve_against(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The ONE `--settings` pair: the hook carrier alone, or — when the tool policy
/// has a settings block or the operator template carries its own `--settings`
/// — a file merging them. Empty when there is none of the three.
///
/// Merge order is lowest precedence first: the template's settings, then the
/// hook carrier, then the tool policy — so on a key the runner sets, the
/// runner's value wins, while arrays (hook lists, deny rules) are unioned and
/// keep every layer's entries.
fn settings_args(
    hook_settings: Option<&Path>,
    tool_policy_settings: Option<&str>,
    template_settings: &[String],
    working_dir: &Path,
    scratch_dir: &Path,
    session_id: &str,
) -> Result<Vec<String>, String> {
    let template_docs: Vec<(String, serde_json::Value)> = template_settings
        .iter()
        .filter_map(|value| match read_settings_value(value, working_dir) {
            Ok(doc) => Some((settings_source(value), strip_widening_template_keys(doc))),
            Err(e) => {
                warn!("{}", template_settings_unreadable_line(value, &e));
                None
            }
        })
        .collect();
    if tool_policy_settings.is_none() && template_docs.is_empty() {
        return Ok(hook_settings
            .map(|p| vec!["--settings".to_string(), p.to_string_lossy().into_owned()])
            .unwrap_or_default());
    }
    let tool: Option<serde_json::Value> = tool_policy_settings
        .map(|t| {
            serde_json::from_str(t).map_err(|e| format!("tool-policy settings are not JSON: {e}"))
        })
        .transpose()?;
    let hooks = hook_settings.and_then(|path| {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()));
        match parsed {
            Ok(doc) => Some(doc),
            Err(e) => {
                warn!(
                    "hook-carrier settings {} unreadable ({e}) — the merged settings carry the \
                     other layers only, so this session has no SessionStart hook",
                    path.display()
                );
                None
            }
        }
    });
    let mut merged = serde_json::json!({});
    for (source, doc) in template_docs {
        info!(
            "{}",
            template_settings_merge_line(&source, &doc, session_id)
        );
        merge_settings(&mut merged, doc);
    }
    if let Some(hooks) = hooks {
        merge_settings(&mut merged, hooks);
    }
    if let Some(tool) = tool {
        merge_settings(&mut merged, tool);
    }
    let file = scratch_dir.join(format!("claude_session_settings_{session_id}.json"));
    std::fs::write(&file, merged.to_string())
        .map_err(|e| format!("Failed to write the merged settings file: {e}"))?;
    info!(
        "Applied merged settings for interactive session {} at {}",
        session_id,
        file.display()
    );
    Ok(vec![
        "--settings".to_string(),
        file.to_string_lossy().into_owned(),
    ])
}

/// The `permissions` keys that WIDEN what a session may do: an `allow` rule
/// pre-approves a tool, `defaultMode` can switch the posture, and
/// `additionalDirectories` grants paths outside the cwd. A template's `deny`
/// and `ask` rules are kept because they only narrow the session.
const WIDENING_PERMISSION_KEYS: [&str; 3] = ["allow", "defaultMode", "additionalDirectories"];

/// A template settings layer with exactly four keys removed:
/// `permissions.allow`, `permissions.defaultMode` and
/// `permissions.additionalDirectories` (the [`WIDENING_PERMISSION_KEYS`]; a
/// `permissions` object left empty is dropped), and the top-level
/// `disableAllHooks`, which would disable the runner's hook carrier — its
/// SessionStart policy hook included. Everything else in the layer is kept.
///
/// The fold only runs under the bypass modes: under an allow-list no template
/// settings are folded at all ([`template_settings_to_fold`]).
fn strip_widening_template_keys(mut doc: serde_json::Value) -> serde_json::Value {
    if let Some(object) = doc.as_object_mut() {
        object.remove("disableAllHooks");
        if let Some(permissions) = object
            .get_mut("permissions")
            .and_then(|p| p.as_object_mut())
        {
            for key in WIDENING_PERMISSION_KEYS {
                permissions.remove(key);
            }
            if permissions.is_empty() {
                object.remove("permissions");
            }
        }
    }
    doc
}

/// The info line for folding one template settings layer: its source (never
/// an inline value) and the keys it contributes.
fn template_settings_merge_line(source: &str, doc: &serde_json::Value, session_id: &str) -> String {
    let keys: Vec<&String> = doc
        .as_object()
        .map(|o| o.keys().collect())
        .unwrap_or_default();
    format!(
        "merging launch template --settings {source} (keys {keys:?}) into interactive session \
         {session_id}'s settings; the runner's own keys win on a conflict, and a template's \
         widening permissions and disableAllHooks are never merged"
    )
}

/// The warn line for a template settings value that could not be read. The
/// error text is NOT included for an inline value: a JSON parse error can quote
/// the input it failed on.
fn template_settings_unreadable_line(value: &str, error: &str) -> String {
    let source = settings_source(value);
    if is_inline_settings(value) {
        format!("launch template --settings {source} is not a JSON object — not merged")
    } else {
        format!("launch template --settings {source} unreadable ({error}) — not merged")
    }
}

fn is_inline_settings(value: &str) -> bool {
    value.trim_start().starts_with('{')
}

/// How a template `--settings` value is named in a log line: its path, or
/// `<inline JSON>` — an inline object may carry a secret and is never logged.
fn settings_source(value: &str) -> String {
    if is_inline_settings(value) {
        "<inline JSON>".to_string()
    } else {
        value.to_string()
    }
}

/// A `--settings` value as Claude Code reads it: an inline JSON object, or a
/// path to a JSON file (relative to the session's cwd).
fn read_settings_value(value: &str, working_dir: &Path) -> Result<serde_json::Value, String> {
    let text = if is_inline_settings(value) {
        value.to_string()
    } else {
        std::fs::read_to_string(resolve_against(working_dir, value)).map_err(|e| e.to_string())?
    };
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if doc.is_object() {
        Ok(doc)
    } else {
        Err("not a JSON object".to_string())
    }
}

/// Fold an operator template's append prompts into the ONE carrier, after the
/// runner's composed text: Claude Code refuses two append flags, and the
/// launch seam drops the template's because the spawn owns the flag
/// (`OWNED_TEMPLATE_FLAGS`), so without
/// this an operator's configured prompt would silently vanish.
///
/// `--append-system-prompt <text>` contributes its text and
/// `--append-system-prompt-file <path>` the file's. A file carrier is
/// re-composed into a sibling content-addressed file; its policy SHA is
/// unchanged, because the policy body it delivers is unchanged.
fn fold_template_prompts(
    carrier: Option<SystemPromptCarrier>,
    template_prompts: &[(String, String)],
    working_dir: &Path,
) -> Option<SystemPromptCarrier> {
    let texts: Vec<String> = template_prompts
        .iter()
        .filter_map(|(flag, value)| {
            if flag == crate::session::spawn_prompt::APPEND_SYSTEM_PROMPT_FILE_FLAG {
                match std::fs::read_to_string(resolve_against(working_dir, value)) {
                    Ok(text) => Some(text),
                    Err(e) => {
                        warn!(
                            "launch template {flag} {value} unreadable ({e}) — its prompt is \
                             not appended"
                        );
                        None
                    }
                }
            } else {
                Some(value.clone())
            }
        })
        .filter(|t| !t.trim().is_empty())
        .collect();
    if texts.is_empty() {
        return carrier;
    }
    let appended = texts.join("\n\n");
    info!(
        "appending {} launch template prompt(s) after the runner's system prompt",
        texts.len()
    );
    match carrier {
        None => Some(SystemPromptCarrier::Inline(appended)),
        Some(SystemPromptCarrier::Inline(text)) => Some(SystemPromptCarrier::Inline(format!(
            "{}\n\n{appended}",
            text.trim_end()
        ))),
        Some(SystemPromptCarrier::File { path, policy_sha }) => {
            let folded = std::fs::read_to_string(&path).and_then(|composed| {
                let content = format!("{}\n\n{appended}", composed.trim_end());
                let dir = path.parent().unwrap_or_else(|| Path::new("."));
                let folded = dir.join(crate::session::spawn_prompt::spawn_prompt_file_name(
                    &content,
                ));
                crate::session::spawn_prompt::write_atomic(&folded, content.as_bytes())?;
                Ok(folded)
            });
            match folded {
                Ok(folded) => Some(SystemPromptCarrier::File {
                    path: folded,
                    policy_sha,
                }),
                Err(e) => {
                    warn!(
                        "could not append the launch template's prompt to {} ({e}) — the \
                         session gets the runner's prompt without it",
                        path.display()
                    );
                    Some(SystemPromptCarrier::File { path, policy_sha })
                }
            }
        }
    }
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
