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
/// from the argv (this tail supplies the one `--settings`), so its keys are
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
        "{doc}"
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
