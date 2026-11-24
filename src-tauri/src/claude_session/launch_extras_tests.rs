//! [`super::compose_launch_extras`] — the tail and env `ClaudeSession::spawn`
//! actually passes — and the SessionStart route reading the key that env
//! exports.

use super::*;
use crate::claude_session::launch_spec::{render_argv, LaunchConfig, LaunchSpec, PermissionMode};
use crate::workflow::dag_schema::{ToolPolicy, ViolationAction};
use uuid::Uuid;

const PORT: u16 = 23_931;
const PARENT_NONCE: &str = "parent-root-nonce-the-default-tenants";

fn tenant_a() -> Uuid {
    Uuid::from_u128(0xA1A1_0000_0000_4000_8000_0000_0000_00A1)
}

fn tenant_bauhaus() -> Uuid {
    Uuid::parse_str("ce1ec20c-0000-4000-8000-0000000000ce").unwrap()
}

fn pair(tenant: Uuid) {
    crate::auth::AuthManager::new()
        .store_tenant_device_jwt(&tenant, "header.payload.signature")
        .expect("seed a tenant slot in the isolated store");
}

fn worktree_under(root: &Path) -> String {
    let wd = root
        .join("agent-worktrees")
        .join(Uuid::new_v4().simple().to_string())
        .join("repo");
    std::fs::create_dir_all(&wd).unwrap();
    wd.to_string_lossy().into_owned()
}

fn write_parent_mcp_json(root: &Path) {
    let doc = serde_json::json!({ "mcpServers": { "coord-mcp": {
        "type": "http",
        "url": format!("http://127.0.0.1:{PORT}/coord-mcp"),
        "headers": { "X-Qontinui-Proxy-Nonce": PARENT_NONCE }
    }}});
    std::fs::write(root.join(".mcp.json"), doc.to_string()).unwrap();
}

fn deny_policy() -> ToolPolicy {
    ToolPolicy {
        allow: None,
        deny: Some(vec!["Bash:git push --force".to_string()]),
        on_violation: ViolationAction::default(),
    }
}

fn inputs<'a>(
    wd: &'a str,
    session_id: &'a str,
    tool_policy: Option<&'a ToolPolicy>,
    hook_settings: Option<PathBuf>,
    scratch: &'a Path,
) -> LaunchExtrasInputs<'a> {
    LaunchExtrasInputs {
        working_dir: wd,
        session_id,
        config_dir: None,
        tool_policy,
        bound_port: Some(PORT),
        api_port: 4321,
        hook_settings,
        scratch_dir: scratch,
        launch_cfg: no_template(),
        permission: &PermissionMode::BypassPermissions,
        template_pin: None,
    }
}

/// The launch config with no operator template.
fn no_template() -> &'static LaunchConfig {
    static CFG: std::sync::OnceLock<LaunchConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(LaunchConfig::default)
}

fn flag_values<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    argv.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == flag || a.starts_with(&format!("{flag}=")))
        .filter_map(|(i, a)| match a.split_once('=') {
            Some((_, v)) => Some(v),
            None => argv.get(i + 1).map(String::as_str),
        })
        .collect()
}

fn env_value<'a>(env: &'a [(&'static str, Option<String>)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| v.as_deref())
}

/// The parent-file test, through the composition `spawn_body` runs: a cwd
/// with no `.mcp.json` under a root whose file names the default tenant's
/// coord-mcp gets the runner's own `--mcp-config`, and the parent's nonce is
/// nowhere in the launch. Removing the coord-mcp delivery from
/// `compose_launch_extras` reddens this.
#[test]
fn the_spawn_tail_carries_the_runners_coord_mcp_not_the_parents() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    write_parent_mcp_json(&amb.root());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let session_id = format!("ai-{}", Uuid::new_v4());

    let extras =
        compose_launch_extras(inputs(&wd, &session_id, None, None, scratch.path()), |_| {
            None
        })
        .unwrap();

    let configs = flag_values(&extras.extra_required, "--mcp-config");
    assert_eq!(configs.len(), 1, "{:?}", extras.extra_required);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(configs[0]).unwrap()).unwrap();
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&doc).unwrap();
    assert_ne!(nonce, PARENT_NONCE);
    assert!(!extras
        .extra_required
        .iter()
        .any(|a| a.contains(PARENT_NONCE)));
    assert_eq!(
        crate::coord_mcp::terminal_id_for_nonce(&nonce).as_deref(),
        Some(session_id.as_str())
    );
    // The stream-json protocol flags lead the tail, unchanged.
    assert_eq!(
        extras.extra_required[..5],
        [
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose"
        ]
    );
}

/// Two `--settings` do not merge (only the last applies, finding a1a66bfe
/// case d), so a spawn with a tool-policy settings block passes exactly ONE —
/// carrying the hook carrier's hooks AND the policy's deny rules — and an
/// operator template's own `--settings=…` is dropped beside it.
#[test]
fn a_tool_policy_spawn_passes_one_settings_carrying_hooks_and_policy() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let hooks_dir = tempfile::tempdir().unwrap();
    let hook_settings = crate::session::claude_hook::materialize(hooks_dir.path())
        .expect("the hook carrier materializes");
    let policy = deny_policy();
    let session_id = format!("ai-{}", Uuid::new_v4());

    let extras = compose_launch_extras(
        inputs(
            &wd,
            &session_id,
            Some(&policy),
            Some(hook_settings),
            scratch.path(),
        ),
        |_| None,
    )
    .unwrap();

    let spec = LaunchSpec {
        permission: PermissionMode::BypassPermissions,
        extra_required: extras.extra_required.clone(),
        ..Default::default()
    };
    let cfg = LaunchConfig {
        default_template: Some("claude --settings=/operator/settings.json".to_string()),
        ..Default::default()
    };
    let argv = render_argv(&spec, &cfg, "claude");
    let settings = flag_values(&argv, "--settings");
    assert_eq!(settings.len(), 1, "exactly one --settings: {argv:?}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings[0]).unwrap()).unwrap();
    assert!(
        doc["hooks"]["SessionStart"].is_array(),
        "the hook carrier's hooks: {doc}"
    );
    assert_eq!(
        doc["permissions"]["deny"],
        serde_json::json!(["Bash(git push --force:*)"]),
        "the tool policy's deny rules: {doc}"
    );
}

/// With no tool-policy settings block the one `--settings` is the hook
/// carrier itself; with no carrier and no block there is none at all.
#[test]
fn without_a_policy_block_the_settings_is_the_hook_carrier_itself() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let hooks_dir = tempfile::tempdir().unwrap();
    let hook_settings = crate::session::claude_hook::materialize(hooks_dir.path()).unwrap();

    let with_hooks = compose_launch_extras(
        inputs(
            &wd,
            &format!("ai-{}", Uuid::new_v4()),
            None,
            Some(hook_settings.clone()),
            scratch.path(),
        ),
        |_| None,
    )
    .unwrap();
    assert_eq!(
        flag_values(&with_hooks.extra_required, "--settings"),
        vec![hook_settings.to_string_lossy().as_ref()]
    );

    let bare = compose_launch_extras(
        inputs(
            &wd,
            &format!("ai-{}", Uuid::new_v4()),
            None,
            None,
            scratch.path(),
        ),
        |_| None,
    )
    .unwrap();
    assert!(flag_values(&bare.extra_required, "--settings").is_empty());
}

/// The prompt carrier is composed for THIS session's coord-mcp outcome and
/// rides the tail.
#[test]
fn the_prompt_carrier_sees_the_delivery_and_rides_the_tail() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let mut seen = None;

    let extras = compose_launch_extras(
        inputs(
            &wd,
            &format!("ai-{}", Uuid::new_v4()),
            None,
            None,
            scratch.path(),
        ),
        |delivery| {
            seen = Some(delivery);
            Some(SystemPromptCarrier::Inline("the briefing".to_string()))
        },
    )
    .unwrap();

    assert_eq!(seen, Some(CoordMcpDelivery::Provisioned));
    let tail = &extras.extra_required;
    assert_eq!(
        tail[tail.len() - 2..],
        ["--append-system-prompt", "the briefing"]
    );
}

/// The env reaches the child: set before `finalize_child_env`, whose
/// credential scrub keeps the coord-mcp config, the hook port and the AI
/// session id.
#[test]
fn the_child_env_survives_the_finalize_scrub() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let session_id = format!("ai-{}", Uuid::new_v4());
    let extras =
        compose_launch_extras(inputs(&wd, &session_id, None, None, scratch.path()), |_| {
            None
        })
        .unwrap();
    let config = flag_values(&extras.extra_required, "--mcp-config")[0].to_string();

    let mut cmd = std::process::Command::new("dummy");
    cmd.env("QONTINUI_TERMINAL_ID", "inherited-terminal");
    crate::claude_session::ClaudeSession::apply_child_env(&mut cmd, &extras.env);
    crate::claude_session::ClaudeSession::finalize_child_env(&mut cmd, None, &session_id);

    let envs: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let get = |name: &str| {
        envs.iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.clone())
    };
    assert_eq!(get(crate::coord_mcp::MCP_CONFIG_ENV), Some(config));
    assert_eq!(get("QONTINUI_AI_SESSION_ID"), Some(session_id));
    assert_eq!(get("QONTINUI_RUNNER_API_PORT"), Some("4321".to_string()));
    assert!(
        envs.iter()
            .any(|(k, v)| k == "QONTINUI_TERMINAL_ID" && v.is_none()),
        "an inherited terminal id is removed: {envs:?}"
    );
}

/// The Phase 3 route test. A spawn into a cwd declaring a tenant this runner
/// cannot act as exports its AI session id; the SessionStart route, addressed
/// by that id (the hook's new rung), finds the session's nonce and gets the
/// proxy's REFUSAL — so a multi-bound device renders the UNKNOWN notice
/// (`TenantUnresolved`), never the default tenant's body, and a single-bound
/// one keeps the default slot. Dropping the env export reddens this.
#[tokio::test]
async fn the_route_addressed_by_the_exported_id_refuses_an_unbound_declared_tenant() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    pair(tenant_a());
    write_parent_mcp_json(&amb.root());
    let wd = worktree_under(&amb.root());
    std::fs::create_dir_all(Path::new(&wd).join(".qontinui")).unwrap();
    std::fs::write(
        Path::new(&wd).join(".qontinui").join("config.yml"),
        format!("tenant: {}\n", tenant_bauhaus()),
    )
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let session_id = format!("ai-{}", Uuid::new_v4());
    let extras =
        compose_launch_extras(inputs(&wd, &session_id, None, None, scratch.path()), |_| {
            None
        })
        .unwrap();

    let route_id = env_value(&extras.env, "QONTINUI_AI_SESSION_ID")
        .expect("the child exports its AI session id for the policy hook")
        .to_string();
    let read = crate::mcp::policy_tenant::read_session_policy_tenant(
        &route_id,
        crate::claude_session::coord_mcp_delivery::session_workdir(&route_id).as_deref(),
        None,
    )
    .await;

    assert!(
        matches!(
            read,
            crate::mcp::policy_tenant::PolicyTenantRead::ProxyRefused { .. }
        ),
        "the session's own nonce answers, and it refuses: {read:?}"
    );
    use crate::mcp::policy_tenant::{policy_tenant_decision, PolicyTenantDecision};
    assert!(matches!(
        policy_tenant_decision(read.clone(), true),
        PolicyTenantDecision::Unknown(_)
    ));
    assert_eq!(
        policy_tenant_decision(read, false),
        PolicyTenantDecision::DefaultSlot
    );
    drop(extras);
}

/// An operator template's `--settings` is not lost: the launch seam drops it
/// from the argv (the spawn owns `--settings`), so its keys are
/// MERGED into that one file — the runner's hooks and the tool policy win on a
/// conflict, list entries are kept from every layer, and the template's own
/// keys survive.
#[test]
fn a_template_settings_file_is_merged_into_the_one_settings_and_the_runner_wins() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let hooks_dir = tempfile::tempdir().unwrap();
    let hook_settings = crate::session::claude_hook::materialize(hooks_dir.path()).unwrap();
    let hook_doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hook_settings).unwrap()).unwrap();
    let operator = scratch.path().join("operator-settings.json");
    std::fs::write(
        &operator,
        serde_json::json!({
            "model": "operator-model",
            "permissions": { "deny": ["Read(./secrets/**)"] },
            "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": "operator-hook" }] }] }
        })
        .to_string(),
    )
    .unwrap();
    let cfg = LaunchConfig {
        default_template: Some(format!("claude --settings {}", operator.display())),
        ..Default::default()
    };
    let policy = deny_policy();
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(
        &wd,
        &session_id,
        Some(&policy),
        Some(hook_settings),
        scratch.path(),
    );
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let spec = LaunchSpec {
        permission: PermissionMode::BypassPermissions,
        extra_required: extras.extra_required.clone(),
        ..Default::default()
    };
    let argv = render_argv(&spec, &cfg, "claude");
    let settings = flag_values(&argv, "--settings");
    assert_eq!(settings.len(), 1, "{argv:?}");
    assert_ne!(settings[0], operator.to_string_lossy().as_ref());
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings[0]).unwrap()).unwrap();
    assert_eq!(
        doc["model"], "operator-model",
        "the template's own key survives"
    );
    let deny = doc["permissions"]["deny"].as_array().unwrap();
    assert!(
        deny.contains(&serde_json::json!("Read(./secrets/**)")),
        "a template's deny rule only narrows, and is kept: {doc}"
    );
    assert!(
        deny.contains(&serde_json::json!("Bash(git push --force:*)")),
        "{doc}"
    );
    let session_start = doc["hooks"]["SessionStart"].as_array().unwrap();
    assert!(
        session_start.to_string().contains("operator-hook"),
        "the template's hook is kept beside the runner's: {doc}"
    );
    for entry in hook_doc["hooks"]["SessionStart"].as_array().unwrap() {
        assert!(
            session_start.contains(entry),
            "the runner's hook is kept: {doc}"
        );
    }
}

/// A template append prompt (text or file) is appended AFTER the runner's
/// prompt in the one carrier instead of being dropped — on an inline carrier,
/// on a composed-file carrier (whose policy SHA is unchanged), and on a spawn
/// that had no carrier of its own.
#[test]
fn a_template_append_prompt_is_appended_after_the_runners_in_the_one_carrier() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let operator_file = scratch.path().join("operator-prompt.md");
    std::fs::write(&operator_file, "from the operator file").unwrap();
    let cfg = LaunchConfig {
        default_template: Some(format!(
            "claude --append-system-prompt-file {}",
            operator_file.display()
        )),
        ..Default::default()
    };
    let compose = |carrier: Option<SystemPromptCarrier>| {
        let session_id = format!("ai-{}", Uuid::new_v4());
        let mut input = inputs(&wd, &session_id, None, None, scratch.path());
        input.launch_cfg = &cfg;
        let extras = compose_launch_extras(input, move |_| carrier).unwrap();
        let spec = LaunchSpec {
            permission: PermissionMode::BypassPermissions,
            extra_required: extras.extra_required.clone(),
            ..Default::default()
        };
        (extras, render_argv(&spec, &cfg, "claude"))
    };

    // Inline: one append flag, the runner's text first.
    let (extras, argv) = compose(Some(SystemPromptCarrier::Inline("the briefing".into())));
    assert_eq!(
        flag_values(&argv, "--append-system-prompt"),
        vec!["the briefing\n\nfrom the operator file"]
    );
    assert!(
        flag_values(&argv, "--append-system-prompt-file").is_empty(),
        "{argv:?}"
    );
    drop(extras);

    // A composed file: re-composed with the template text appended.
    let composed_dir = scratch.path().join("spawn-prompts");
    std::fs::create_dir_all(&composed_dir).unwrap();
    let composed = composed_dir.join("spawn-composed.md");
    std::fs::write(&composed, "briefing\n\npolicy body").unwrap();
    let (extras, argv) = compose(Some(SystemPromptCarrier::File {
        path: composed.clone(),
        policy_sha: "abc".to_string(),
    }));
    let files = flag_values(&argv, "--append-system-prompt-file");
    assert_eq!(files.len(), 1, "{argv:?}");
    assert!(
        flag_values(&argv, "--append-system-prompt").is_empty(),
        "{argv:?}"
    );
    assert_eq!(
        std::fs::read_to_string(files[0]).unwrap(),
        "briefing\n\npolicy body\n\nfrom the operator file"
    );
    assert_eq!(
        extras.prompt_carrier.as_ref().and_then(|c| c.policy_sha()),
        Some("abc"),
        "the delivered policy body, and so its SHA, is unchanged"
    );
    drop(extras);

    // No carrier of the runner's: the template's prompt alone.
    let (extras, argv) = compose(None);
    assert_eq!(
        flag_values(&argv, "--append-system-prompt"),
        vec!["from the operator file"]
    );
    drop(extras);
}

/// A template `--mcp-config` stays dropped: the runner's coord-mcp config is
/// the only one the spawn passes (plan D1).
#[test]
fn a_template_mcp_config_stays_dropped_beside_the_runners() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let cfg = LaunchConfig {
        default_template: Some("claude --mcp-config /operator/mcp.json".to_string()),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, None, scratch.path());
    input.launch_cfg = &cfg;
    let extras = compose_launch_extras(input, |_| None).unwrap();
    let spec = LaunchSpec {
        permission: PermissionMode::BypassPermissions,
        extra_required: extras.extra_required.clone(),
        ..Default::default()
    };
    let argv = render_argv(&spec, &cfg, "claude");
    let configs = flag_values(&argv, "--mcp-config");
    assert_eq!(configs.len(), 1, "{argv:?}");
    assert_ne!(configs[0], "/operator/mcp.json");
}

/// The rendered argv for `extras` under `cfg`, with the permission and the
/// owned flags `spawn_body` passes.
fn render(extras: &LaunchExtras, cfg: &LaunchConfig, permission: PermissionMode) -> Vec<String> {
    let spec = LaunchSpec {
        permission,
        extra_required: extras.extra_required.clone(),
        owned_flags: extras.owned_flags.clone(),
        ..Default::default()
    };
    render_argv(&spec, cfg, "claude")
}

/// A template whose settings could widen an allow-list — `permissions.allow`
/// and its own hooks — contributes NOTHING under the allow-list posture: the
/// one `--settings` is the runner's hook carrier, byte for byte, and neither
/// the template's allow rule nor its hook reaches the session. (On 86d49ebd9
/// the fold ignored the posture and merged both.)
#[test]
fn under_an_allow_list_a_template_settings_contributes_nothing() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let hooks_dir = tempfile::tempdir().unwrap();
    let hook_settings = crate::session::claude_hook::materialize(hooks_dir.path()).unwrap();
    let operator = scratch.path().join("operator-settings.json");
    std::fs::write(
        &operator,
        serde_json::json!({
            "permissions": { "allow": ["Bash(*)"] },
            "hooks": { "PreToolUse": [{ "hooks": [{ "type": "command", "command": "operator-allow-hook" }] }] }
        })
        .to_string(),
    )
    .unwrap();
    let cfg = LaunchConfig {
        default_template: Some(format!("claude --settings {}", operator.display())),
        session_permission:
            crate::claude_session::launch_spec::SessionPermissionSetting::AllowList {
                tools: vec!["Read".to_string()],
            },
        ..Default::default()
    };
    let permission = cfg
        .session_permission
        .apply(PermissionMode::BypassPermissions);
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(
        &wd,
        &session_id,
        None,
        Some(hook_settings.clone()),
        scratch.path(),
    );
    input.launch_cfg = &cfg;
    input.permission = &permission;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    let settings = flag_values(&argv, "--settings");
    assert_eq!(
        settings,
        vec![hook_settings.to_string_lossy().as_ref()],
        "only the runner's hook carrier: {argv:?}"
    );
    let text = std::fs::read_to_string(settings[0]).unwrap();
    assert!(!text.contains("operator-allow-hook"), "{text}");
    assert!(!text.contains("Bash(*)"), "{text}");
}

/// Under bypass the template layer is folded, but only its NARROWING
/// permissions survive: `allow`, `defaultMode` and `additionalDirectories` are
/// stripped, while `deny` and `ask` are kept because they only narrow the
/// session.
#[test]
fn under_bypass_only_a_templates_narrowing_permissions_are_folded() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    // A RELATIVE template path, resolved against the session's cwd.
    std::fs::write(
        Path::new(&wd).join("operator-settings.json"),
        serde_json::json!({
            "permissions": {
                "allow": ["Bash(*)"],
                "defaultMode": "bypassPermissions",
                "additionalDirectories": ["/"],
                "deny": ["Read(./secrets/**)"],
                "ask": ["Bash(rm:*)"]
            },
            "model": "operator-model"
        })
        .to_string(),
    )
    .unwrap();
    let cfg = LaunchConfig {
        default_template: Some("claude --settings operator-settings.json".to_string()),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, None, scratch.path());
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    let settings = flag_values(&argv, "--settings");
    assert_eq!(settings.len(), 1, "{argv:?}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings[0]).unwrap()).unwrap();
    assert_eq!(
        doc["model"], "operator-model",
        "the relative path resolved: {doc}"
    );
    let permissions = doc["permissions"]
        .as_object()
        .expect("deny and ask survive");
    let mut keys: Vec<&String> = permissions.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["ask", "deny"], "{doc}");
    assert_eq!(
        permissions["deny"],
        serde_json::json!(["Read(./secrets/**)"])
    );
    assert_eq!(permissions["ask"], serde_json::json!(["Bash(rm:*)"]));
}

/// A template can never switch the runner's hooks off: its `disableAllHooks`
/// is stripped, so the hook carrier's SessionStart hook stays live.
#[test]
fn a_template_cannot_disable_the_runners_hooks() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let hooks_dir = tempfile::tempdir().unwrap();
    let hook_settings = crate::session::claude_hook::materialize(hooks_dir.path()).unwrap();
    let operator = scratch.path().join("operator-settings.json");
    std::fs::write(
        &operator,
        serde_json::json!({ "disableAllHooks": true, "model": "operator-model" }).to_string(),
    )
    .unwrap();
    let cfg = LaunchConfig {
        default_template: Some(format!("claude --settings {}", operator.display())),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, Some(hook_settings), scratch.path());
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    let settings = flag_values(&argv, "--settings");
    assert_eq!(settings.len(), 1, "{argv:?}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings[0]).unwrap()).unwrap();
    assert!(doc.get("disableAllHooks").is_none(), "{doc}");
    assert!(doc["hooks"]["SessionStart"].is_array(), "{doc}");
    assert_eq!(doc["model"], "operator-model", "the rest is folded: {doc}");
}

/// The fold is the only route a template's settings can take. A template
/// `--settings` that cannot be read — with no hook carrier and no tool policy,
/// so the tail passes no `--settings` of its own for the name-only dedup to
/// match — still never reaches the argv, and nor does a template append prompt
/// whose file cannot be read.
#[test]
fn an_unreadable_template_settings_never_passes_through_to_the_argv() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    let cfg = LaunchConfig {
        default_template: Some(
            "claude --settings /no/such/operator-settings.json \
             --append-system-prompt-file /no/such/prompt.md --max-turns 7"
                .to_string(),
        ),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, None, scratch.path());
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    assert!(flag_values(&argv, "--settings").is_empty(), "{argv:?}");
    assert!(
        flag_values(&argv, "--append-system-prompt-file").is_empty(),
        "{argv:?}"
    );
    // A template flag the tail never supplies still layers in: only the owned
    // flags were dropped.
    assert_eq!(flag_values(&argv, "--max-turns"), vec!["7"], "{argv:?}");
}

/// An INLINE-JSON template `--settings` is folded like a file, and its value —
/// which can carry a secret — never reaches either log line the fold writes.
#[test]
fn an_inline_template_settings_is_folded_and_its_value_never_logged() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let scratch = tempfile::tempdir().unwrap();
    const SECRET: &str = "sk-inline-secret-value";
    let inline = serde_json::json!({ "env": { "API_KEY": SECRET } }).to_string();
    let cfg = LaunchConfig {
        default_template: Some(format!("claude --settings '{inline}'")),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, None, scratch.path());
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    let settings = flag_values(&argv, "--settings");
    assert_eq!(settings.len(), 1, "{argv:?}");
    assert!(!argv.iter().any(|a| a.contains(SECRET)), "{argv:?}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings[0]).unwrap()).unwrap();
    assert_eq!(doc["env"]["API_KEY"], SECRET, "the inline layer was folded");

    let merge_line = template_settings_merge_line(
        &settings_source(&inline),
        &serde_json::from_str(&inline).unwrap(),
        &session_id,
    );
    assert!(!merge_line.contains(SECRET), "{merge_line}");
    assert!(merge_line.contains("<inline JSON>"), "{merge_line}");
    let unreadable = template_settings_unreadable_line(
        &format!("{{\"token\": \"{SECRET}\""),
        &format!("EOF while parsing near {SECRET}"),
    );
    assert!(!unreadable.contains(SECRET), "{unreadable}");
}

/// On the DECLARED arm the runner passes no MCP flag — the cwd's `.mcp.json`
/// is the session's config — and a template's `--mcp-config` and
/// `--strict-mcp-config` are dropped all the same, so neither can override or
/// discard it.
#[test]
fn on_the_declared_arm_a_templates_mcp_flags_are_dropped_too() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    crate::coord_mcp::write_coord_mcp_proxy_config(&wd, PORT, None);
    let scratch = tempfile::tempdir().unwrap();
    let cfg = LaunchConfig {
        default_template: Some(
            "claude --mcp-config /operator/mcp.json --strict-mcp-config --verbose".to_string(),
        ),
        ..Default::default()
    };
    let session_id = format!("ai-{}", Uuid::new_v4());
    let mut input = inputs(&wd, &session_id, None, None, scratch.path());
    input.launch_cfg = &cfg;

    let extras = compose_launch_extras(input, |_| None).unwrap();

    assert_eq!(
        extras.coord_mcp.delivery(),
        CoordMcpDelivery::WorkdirDeclared
    );
    let argv = render(&extras, &cfg, PermissionMode::BypassPermissions);
    assert!(flag_values(&argv, "--mcp-config").is_empty(), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--strict-mcp-config"), "{argv:?}");
}
