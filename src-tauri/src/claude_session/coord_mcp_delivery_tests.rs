//! [`super::deliver_for_ai_session`] arm by arm, and the lease that undoes it.
//!
//! Every test runs inside an isolated ambient: the nonce registry persists to
//! the fixture's secure store, `session_restore_dir()` resolves inside it, and
//! `$QONTINUI_ROOT` is a directory the test owns — so "a parent `.mcp.json`
//! under the workspace root" is a file the test wrote, not the box's.

use super::*;
use crate::coord_mcp::CoordMcpDelivery;
use uuid::Uuid;

const PORT: u16 = 23_917;

fn tenant_a() -> Uuid {
    Uuid::from_u128(0xA1A1_0000_0000_4000_8000_0000_0000_00A1)
}

/// The declared-but-unpaired tenant of the 9ab6bae0 reproduction (a
/// bauhaus-b2b worktree on a qontinui device).
fn tenant_bauhaus() -> Uuid {
    Uuid::parse_str("ce1ec20c-0000-4000-8000-0000000000ce").unwrap()
}

fn pair(tenant: Uuid) {
    crate::auth::AuthManager::new()
        .store_tenant_device_jwt(&tenant, "header.payload.signature")
        .expect("seed a tenant slot in the isolated store");
}

fn key() -> String {
    format!("ai-session-{}", Uuid::new_v4())
}

/// `<root>/agent-worktrees/<id>/repo`, created, with NO `.mcp.json` of its own.
fn worktree_under(root: &Path) -> String {
    let wd = root
        .join("agent-worktrees")
        .join(Uuid::new_v4().simple().to_string())
        .join("repo");
    std::fs::create_dir_all(&wd).unwrap();
    wd.to_string_lossy().into_owned()
}

/// The workspace root's `.mcp.json`: a coord-mcp keyed to the DEFAULT tenant
/// (the parent the defect loaded) beside one of the operator's own servers.
const PARENT_NONCE: &str = "parent-root-nonce-the-default-tenants";

fn write_parent_mcp_json(root: &Path) {
    let doc = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{PORT}/coord-mcp"),
                "headers": { "X-Qontinui-Proxy-Nonce": PARENT_NONCE }
            },
            "parent-tool": { "command": "parent-tool-from-root" }
        }
    });
    std::fs::write(root.join(".mcp.json"), doc.to_string()).unwrap();
}

fn read_doc(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn env_value<'a>(env: &'a [(&'static str, Option<String>)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| v.as_deref())
}

fn argv_path(argv: &[String]) -> PathBuf {
    let i = argv
        .iter()
        .position(|a| a == "--mcp-config")
        .expect("an --mcp-config flag");
    PathBuf::from(&argv[i + 1])
}

/// The defect's own shape: a cwd with no `.mcp.json` under a root whose file
/// names the default tenant's coord-mcp. The session gets the runner's file —
/// additive (arm A, finding a1a66bfe case b), so no `--strict-mcp-config` —
/// carrying a fresh nonce minted for THIS session's key, and the parent's
/// coord-mcp is nowhere in it.
#[test]
fn a_parent_only_mcp_json_is_replaced_by_the_runners_own_coord_mcp() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    write_parent_mcp_json(&amb.root());
    let (wd, key) = (worktree_under(&amb.root()), key());

    let delivered = deliver_for_ai_session(&wd, &key, None, Some(PORT)).unwrap();

    assert_eq!(delivered.delivery(), CoordMcpDelivery::Provisioned);
    let argv = delivered.argv().to_vec();
    assert_eq!(argv.len(), 2, "{argv:?}");
    assert_eq!(argv[0], "--mcp-config");
    assert!(!argv.iter().any(|a| a == "--strict-mcp-config"), "{argv:?}");
    let file = argv_path(&argv);
    assert!(
        !file.starts_with(&wd),
        "the runner file lives in app-data, never the cwd"
    );
    let doc = read_doc(&file);
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&doc)
        .expect("the runner file carries its own nonce");
    assert_ne!(nonce, PARENT_NONCE);
    assert!(!doc.to_string().contains(PARENT_NONCE));
    assert_eq!(
        crate::coord_mcp::terminal_id_for_nonce(&nonce).as_deref(),
        Some(key.as_str()),
        "the nonce is minted for this session's key"
    );
    let servers = doc["mcpServers"].as_object().unwrap();
    assert_eq!(servers.keys().collect::<Vec<_>>(), vec!["coord-mcp"]);

    let env = delivered.child_env(&key, 4242);
    assert_eq!(
        env_value(&env, crate::coord_mcp::MCP_CONFIG_ENV),
        Some(file.to_string_lossy().as_ref())
    );
    assert_eq!(env_value(&env, AI_SESSION_ID_ENV), Some(key.as_str()));
    assert_eq!(env_value(&env, RUNNER_API_PORT_ENV), Some("4242"));
    assert!(
        env.iter()
            .any(|(n, v)| *n == TERMINAL_ID_ENV && v.is_none()),
        "an inherited terminal id is removed from the child"
    );
    assert_eq!(session_workdir(&key).as_deref(), Some(wd.as_str()));
}

/// The 9ab6bae0 reproduction: a cwd declaring a tenant this runner holds no
/// credential for. The nonce is minted with no spawn tenant, so the proxy
/// decides per request — and its decision is the typed refusal, NOT the
/// device's default tenant.
#[test]
fn a_declared_tenant_with_no_slot_is_refused_by_the_proxy_never_defaulted() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    pair(tenant_a());
    write_parent_mcp_json(&amb.root());
    let (wd, key) = (worktree_under(&amb.root()), key());
    std::fs::create_dir_all(Path::new(&wd).join(".qontinui")).unwrap();
    std::fs::write(
        Path::new(&wd).join(".qontinui").join("config.yml"),
        format!("tenant: {}\n", tenant_bauhaus()),
    )
    .unwrap();

    let delivered = deliver_for_ai_session(&wd, &key, None, Some(PORT)).unwrap();
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&argv_path(
        delivered.argv(),
    )))
    .unwrap();

    assert_ne!(
        crate::coord_mcp::proxy_session_pin_for_nonce(&nonce),
        crate::session::tenant_pin::TenantPin::Pinned(tenant_bauhaus()),
        "no spawn tenant was chosen, so none is frozen onto the key"
    );
    let decision = crate::coord_mcp::session_tenant_or_refuse(Some(&nonce));
    assert!(
        decision.is_err(),
        "the proxy must refuse an unbound declared tenant, not answer {decision:?}"
    );
    assert_ne!(decision.ok(), Some(Some(tenant_a())));
}

/// A cwd whose own `.mcp.json` declares coord-mcp keeps it: no runner flag
/// (the cwd file beats the ancestor, case f), the file is untouched, and an
/// inherited `QONTINUI_MCP_CONFIG` is removed rather than left pointing
/// elsewhere.
#[test]
fn a_cwd_that_declares_coord_mcp_keeps_its_own_entry() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    write_parent_mcp_json(&amb.root());
    let (wd, key) = (worktree_under(&amb.root()), key());
    crate::coord_mcp::write_coord_mcp_proxy_config(&wd, PORT, None);
    let before = std::fs::read_to_string(Path::new(&wd).join(".mcp.json")).unwrap();

    let delivered = deliver_for_ai_session(&wd, &key, None, Some(PORT)).unwrap();

    assert_eq!(delivered.delivery(), CoordMcpDelivery::WorkdirDeclared);
    assert!(delivered.argv().is_empty(), "{:?}", delivered.argv());
    assert_eq!(
        std::fs::read_to_string(Path::new(&wd).join(".mcp.json")).unwrap(),
        before
    );
    let env = delivered.child_env(&key, 4242);
    assert!(env
        .iter()
        .any(|(n, v)| *n == crate::coord_mcp::MCP_CONFIG_ENV && v.is_none()));
}

/// No bound port: nothing can be provisioned, so the session starts STRICT —
/// with every non-coord server it would otherwise have had (local over the
/// nearest project file over user) and no coord-mcp from anywhere — and the
/// NOT PROVISIONED breadcrumb names why.
#[test]
fn no_port_fails_closed_to_a_strict_carry_through_with_no_coord_mcp() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    write_parent_mcp_json(&amb.root());
    let (wd, key) = (worktree_under(&amb.root()), key());
    // The cwd's own file, with no coord-mcp: it overrides the parent's
    // `parent-tool` (nearest wins) and adds one of its own.
    std::fs::write(
        Path::new(&wd).join(".mcp.json"),
        serde_json::json!({ "mcpServers": {
            "parent-tool": { "command": "parent-tool-from-cwd" },
            "cwd-tool": { "command": "cwd-tool" }
        }})
        .to_string(),
    )
    .unwrap();
    let account = amb.dir().join("account");
    std::fs::create_dir_all(&account).unwrap();
    let project = crate::claude_session::workspace_trust::project_key(Path::new(&wd)).unwrap();
    let mut projects = serde_json::Map::new();
    projects.insert(
        project,
        serde_json::json!({ "mcpServers": {
            "local-tool": { "command": "local-tool" },
            "cwd-tool": { "command": "cwd-tool-local-scope" }
        }}),
    );
    std::fs::write(
        account.join(".claude.json"),
        serde_json::json!({
            "mcpServers": {
                "user-tool": { "command": "user-tool" },
                "coord-mcp": { "command": "user-scope-coord" }
            },
            "projects": projects,
        })
        .to_string(),
    )
    .unwrap();

    let delivered =
        deliver_for_ai_session(&wd, &key, Some(account.to_str().unwrap()), None).unwrap();

    assert_eq!(delivered.delivery(), CoordMcpDelivery::Unprovisioned);
    let argv = delivered.argv().to_vec();
    assert_eq!(argv[..2], ["--strict-mcp-config", "--mcp-config"]);
    let doc = read_doc(&argv_path(&argv));
    let servers = doc["mcpServers"].as_object().unwrap();
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();
    assert_eq!(
        names,
        vec!["cwd-tool", "local-tool", "parent-tool", "user-tool"],
        "every non-coord server, and no coord-mcp from any scope"
    );
    assert_eq!(servers["parent-tool"]["command"], "parent-tool-from-cwd");
    assert_eq!(servers["cwd-tool"]["command"], "cwd-tool-local-scope");
    assert!(!doc.to_string().contains(PARENT_NONCE));
    let breadcrumb =
        std::fs::read_to_string(Path::new(&wd).join(crate::coord_mcp::COORD_MCP_STATUS_FILE))
            .expect("the NOT PROVISIONED breadcrumb");
    assert!(breadcrumb.contains("NOT PROVISIONED"), "{breadcrumb}");
}

/// Two sessions in one cwd get two files and two live nonces — never one
/// shared credential.
#[test]
fn two_spawns_in_one_cwd_get_two_files_and_two_nonces() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let (a, b) = (key(), key());

    let da = deliver_for_ai_session(&wd, &a, None, Some(PORT)).unwrap();
    let db = deliver_for_ai_session(&wd, &b, None, Some(PORT)).unwrap();

    let (fa, fb) = (argv_path(da.argv()), argv_path(db.argv()));
    assert_ne!(fa, fb);
    let na = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&fa)).unwrap();
    let nb = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&fb)).unwrap();
    assert_ne!(na, nb);
    assert!(crate::coord_mcp::proxy_nonce_is_valid(&na));
    assert!(crate::coord_mcp::proxy_nonce_is_valid(&nb));
}

/// What a delivery leaves behind, for the spawn-then-close round trip.
fn residue(key: &str, nonce: Option<&str>, files: &[&Path]) -> (bool, usize, bool, bool) {
    (
        nonce.is_some_and(crate::coord_mcp::proxy_nonce_is_valid),
        files.iter().filter(|f| f.exists()).count(),
        crate::coord_mcp::terminal_spawn_default_tenant(key).is_some(),
        session_workdir(key).is_some(),
    )
}

/// Spawn then close leaves nothing: the nonce is no longer accepted (and so
/// not persisted), the config file is gone, and the delivery record and the
/// lease entry are dropped — on the close path AND on the lease-drop path,
/// on the provisioned AND the strict arm. A second retire is a no-op.
#[test]
fn spawn_then_close_leaves_no_nonce_file_or_record_behind() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let nothing = (false, 0, false, false);

    // Provisioned, retired by the close.
    let k = key();
    let d = deliver_for_ai_session(&wd, &k, None, Some(PORT)).unwrap();
    let file = argv_path(d.argv());
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&file)).unwrap();
    assert_eq!(residue(&k, Some(&nonce), &[&file]), (true, 1, true, true));
    assert!(retire_session_coord_mcp(&k));
    assert_eq!(residue(&k, Some(&nonce), &[&file]), nothing);
    assert!(!retire_session_coord_mcp(&k), "a repeat close is a no-op");
    drop(d.into_lease()); // the waiter's later retire finds nothing of its own

    // Provisioned, retired by the child's exit (the lease the waiter holds).
    let k = key();
    let d = deliver_for_ai_session(&wd, &k, None, Some(PORT)).unwrap();
    let file = argv_path(d.argv());
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&file)).unwrap();
    d.into_lease().retire();
    assert_eq!(residue(&k, Some(&nonce), &[&file]), nothing);

    // Strict (no port), retired by the lease.
    let k = key();
    let d = deliver_for_ai_session(&wd, &k, None, None).unwrap();
    let file = argv_path(d.argv());
    assert_eq!(residue(&k, None, &[&file]), (false, 1, true, true));
    drop(d);
    assert_eq!(residue(&k, None, &[&file]), nothing);
}

/// A rate-limit restart re-delivers under the SAME session id while the old
/// waiter may still be finishing. The predecessor's retire must leave the
/// successor's nonce, file and record alone.
#[test]
fn a_predecessors_retire_never_touches_its_successor_under_the_same_key() {
    let amb = crate::test_env::isolated_ambient();
    amb.write_active_tenant_id(tenant_a());
    let wd = worktree_under(&amb.root());
    let k = key();

    let first = deliver_for_ai_session(&wd, &k, None, Some(PORT)).unwrap();
    let second = deliver_for_ai_session(&wd, &k, None, Some(PORT)).unwrap();
    let file = argv_path(second.argv());
    let nonce = crate::coord_mcp_config::proxy_nonce_from_config_doc(&read_doc(&file)).unwrap();

    first.into_lease().retire();
    assert_eq!(residue(&k, Some(&nonce), &[&file]), (true, 1, true, true));

    second.into_lease().retire();
    assert_eq!(
        residue(&k, Some(&nonce), &[&file]),
        (false, 0, false, false)
    );
}
