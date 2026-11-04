//! The coord recurring-agent reconciler — plan
//! `2026-10-29-recurring-agents-declared-in-coord-reconciled-by-runner`,
//! Phase 4b (decisions D5, D9).
//!
//! On the PRIMARY runner only (started beside `start_scheduler_service`, under
//! the same `instance::is_secondary()` gate), every [`POLL_INTERVAL`] this
//! loop reads coord's resolved recurring-agent list for this device and turns
//! it into managed [`ScheduledTask`]s in the runner's own scheduler. The
//! managed rows in Postgres ARE the persisted last-applied set: an offline
//! runner keeps firing them on schedule, and nothing here clears one on a
//! failed poll.
//!
//! # What coord serves (matched exactly)
//!
//! `GET /coord/agent-recurring-agents`, device JWT. Handler
//! `get_device_agents` (qontinui-coord `crates/coord/src/recurring_agents_store.rs:1326-1345`
//! at origin/main `eed57ed83`): the tenant and device come from the verified
//! credential, never an argument. Body built by `device_read_core`
//! (`:821-913`):
//!
//! ```text
//! {"tenant_id", "device_id", "spec_version": 1,
//!  "agents": [device_entry...], "unresolvable": [{"name","version","error","message"}],
//!  "run_requests": [{"name","request_id","requested_at"}], "served_at"}
//! ```
//!
//! Each `agents[]` entry is `device_entry` (`:1038-1055`): the qontinui-schemas
//! [`RecurringAgentSpec`] wire shape (`name`, `catalog_ref`, `enabled`,
//! `schedule`, `timezone`, `catch_up_policy`, `params`, `task_spec`,
//! `conditions`, `target`, `version`, `digest`) plus `row_version` and the
//! digested `spec` object (whose `description` becomes the task description).
//! Any read coord cannot make is a 5xx, never a shorter list (`:27-35`), and an
//! agent whose target facts are UNKNOWN, or whose stored row this coord build
//! cannot resolve, is served under `unresolvable` — "keep the last-applied
//! task" (`:863-867`, `:882-884`). `run_requests` is Phase 4c's, ignored here.
//!
//! # The contract this module keeps
//!
//! - **Digests, never versions.** A task is (re)written when its stored
//!   `managed_digest` differs from the served `digest` (plan Phase 2), or when
//!   the task rendered on THIS device this pass differs from the stored row
//!   (a moved workspace root or bash, or a local edit through the ordinary
//!   task PUT, which keeps coord's digest). A version-only change writes
//!   nothing.
//! - **Fail-safe deletes.** A managed task is deleted only when its tenant's
//!   poll answered 2xx with a WELL-FORMED list that no longer names it. A 401,
//!   404, 5xx, timeout, undecodable or malformed body, or a body naming another
//!   tenant changes nothing (`looping_agent/desired_state.rs:16-23`).
//! - **Never an unmanaged task.** The planner reads, writes and deletes only
//!   rows carrying a managed key; a same-name unmanaged task blocks the create
//!   and is reported `name_collision_unmanaged` (Phase 6 does adoption).
//! - **D9, in the reconciler itself.** The HTTP-handler Script / requireProbe
//!   refusals key on an HTTP caller's class, and this in-process path never
//!   passes through them. A `Script` command or a `requireProbe` command is
//!   applied only when the agent has a `catalog_ref` AND the command equals
//!   THIS binary's own pin for that entry ([`PROBE_PINS`], [`SCRIPT_PINS`]);
//!   the runner resolves the pin's tokens itself. The pin is over the command
//!   ARGV only: the script it names is read from
//!   `<workspace_root>/.claude/skills/...` on disk, and its CONTENTS are not
//!   checked against this binary's bundled copy. Custom agents are
//!   `RemoteAgent` only.
//! - **Tenants are proven, not defaulted.** One poll per tenant in coord's own
//!   bound set for this device (`pair::coord_bound_tenants_for_ask`, the
//!   heartbeat's record, fresh enough to act on), each with that tenant's own
//!   credential, and a body is accepted only when coord's `tenant_id` echo is
//!   the tenant asked for. An UNKNOWN binding set WITHHOLDS everything (the
//!   plan adapter's precedent, `plan_workunit_adapter/trigger.rs`
//!   `WorkUnitWritePosture`), and a managed row of a tenant outside the proven
//!   set is left alone and reported `foreign_tenant` (plan Phase 0.1).
//!
//! The per-agent outcome is kept in a [`ReconcileReport`] ([`last_report`]) for
//! Phase 4c's status push (`POST /coord/recurring-agents/status/:device_id`,
//! body `DeviceStatusReport` at `crates/coord/src/recurring_agents.rs:1569-1612`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::database::pg::scheduler::{ManagedUpsert, ManagedUpsertError};
use crate::database::pg::PgDb;
use crate::scheduler::{
    ManagedBy, RecurringAgentSpec, RecurringAgentState, ScheduleConditions, ScheduledTask,
    ScheduledTaskExt, ScheduledTaskType, MANAGED_SOURCE_COORD, REFUSED_COMMAND,
    UNAPPLIED_TENANT_UNPROVEN, UNAPPLIED_WORKSPACE_ROOT_UNSET,
};

/// The reconcile cadence — the fleet-policy poller's (`mcp/fleet_policy_poller.rs`
/// `POLL_INTERVAL`), per plan D5.
pub const POLL_INTERVAL: Duration = Duration::from_secs(45);

/// coord's device read route (qontinui-coord `recurring_agents_store.rs:24`).
pub const DEVICE_READ_PATH: &str = "/coord/agent-recurring-agents";

/// The resolved-spec shape this build understands (coord
/// `recurring_agents::SPEC_VERSION`). Any other value is not applied.
pub const SUPPORTED_SPEC_VERSION: u64 = 1;

/// The token a served path carries instead of a device path (coord
/// `recurring_agents::WORKSPACE_ROOT_TOKEN`).
pub const WORKSPACE_ROOT_TOKEN: &str = "{workspace_root}";
const BASH_TOKEN: &str = "{bash}";
const BUNDLED_SKILL_TOKEN_PREFIX: &str = "{bundled_skill:";

/// `unapplied_reason`s this module reports beyond the qontinui-schemas ones.
pub const REASON_NAME_COLLISION_UNMANAGED: &str = "name_collision_unmanaged";
pub const REASON_FOREIGN_TENANT: &str = "foreign_tenant";
pub const REASON_EFFORT_FIELDS_UNSUPPORTED: &str = "effort_fields_unsupported";
pub const REASON_WORKSPACE_ROOT_UNKNOWN: &str = "workspace_root_unknown";
pub const REASON_WORKING_DIRECTORY_NOT_TOKENIZED: &str = "working_directory_not_tokenized";
pub const REASON_PROBE_UNRESOLVABLE: &str = "probe_unresolvable";
pub const REASON_SPEC_INVALID: &str = "spec_invalid";
pub const REASON_INVALID_TIMEZONE: &str = "invalid_timezone";
pub const REASON_REFUSED_TASK_TYPE: &str = "refused_task_type";
pub const REASON_REFUSED_TOOL_WIDENING: &str = "refused_tool_widening";
pub const REASON_COORD_UNRESOLVABLE_PREFIX: &str = "coord_unresolvable:";
pub const REASON_SPEC_UNPARSEABLE: &str = "spec_unparseable";
pub const REASON_SPEC_PROJECTION_MISMATCH: &str = "spec_projection_mismatch";
pub const REASON_NAME_COLLISION_FOREIGN_TENANT: &str = "name_collision_foreign_tenant";
pub const REASON_PATH_NOT_UNDER_WORKSPACE_ROOT: &str = "path_not_under_workspace_root";
pub const REASON_APPLY_CONFLICT: &str = "apply_conflict";
pub const REASON_APPLY_FAILED: &str = "apply_failed";
pub const REASON_MANAGED_KEY_INDEX_MISSING: &str = "managed_key_index_missing";

// ===========================================================================
// D9 — this binary's own pins
// ===========================================================================

/// A `requireProbe` command this binary will run for a catalog entry. The
/// command is compared, token for token, with what coord serves; only an
/// exact match is applied, and the tokens are then resolved HERE
/// ([`resolve_probe_argv`]) — coord's text is never executed as given.
#[derive(Debug, Clone, Copy)]
pub struct ProbePin {
    /// The catalog entry (`catalog_ref`) the pin belongs to.
    pub catalog_ref: &'static str,
    /// coord's `ProbePin::pin_id` for the same probe (informational: logs
    /// and tests name a pin by it; matching is by `catalog_ref` + `command`).
    #[allow(dead_code)]
    pub pin_id: &'static str,
    /// The command in coord's token form.
    pub command: &'static [&'static str],
}

/// Every probe pin this build carries. One entry per coord catalog entry with a
/// `ProbePin` at qontinui-coord origin/main `eed57ed83`
/// (`crates/coord/src/recurring_agents.rs:568-581`: return-to-main only). The
/// script NAME it carries is one this binary bundles
/// (`src/fleet_skills/return-to-main/machine-quiesce-check.sh`), pinned by
/// `tests::every_probe_pin_names_a_script_this_binary_bundles`; the pin fixes
/// the argv, not the bytes of the file found on disk at run time.
pub static PROBE_PINS: &[ProbePin] = &[ProbePin {
    catalog_ref: "return-to-main",
    pin_id: "return-to-main/machine-quiesce-check",
    command: &[
        BASH_TOKEN,
        "{bundled_skill:return-to-main}/machine-quiesce-check.sh",
        "--for",
        "checkout-ff",
        "--exit-quiet-if-any-repo",
        "--root",
        WORKSPACE_ROOT_TOKEN,
    ],
}];

/// A `Script` task command this binary will run for a catalog entry.
#[derive(Debug, Clone, Copy)]
pub struct ScriptPin {
    pub catalog_ref: &'static str,
    pub command: &'static str,
}

/// Every Script pin this build carries: NONE. No coord catalog entry is a
/// `Script` task at qontinui-coord `eed57ed83` (every template renders
/// `RemoteAgent`, `recurring_agents.rs:1448-1468`), so every served `Script`
/// is refused until a pin for it ships here.
pub static SCRIPT_PINS: &[ScriptPin] = &[];

fn probe_pin(catalog_ref: &str) -> Option<&'static ProbePin> {
    PROBE_PINS.iter().find(|p| p.catalog_ref == catalog_ref)
}

fn script_pin(catalog_ref: &str) -> Option<&'static ScriptPin> {
    SCRIPT_PINS.iter().find(|p| p.catalog_ref == catalog_ref)
}

// ===========================================================================
// The served list
// ===========================================================================

#[derive(Debug, Deserialize)]
struct DeviceAgentsWire {
    tenant_id: Uuid,
    spec_version: u64,
    agents: Vec<Value>,
    unresolvable: Vec<UnresolvableWire>,
}

#[derive(Debug, Deserialize)]
struct UnresolvableWire {
    name: String,
    error: String,
}

/// One served agent, parsed.
#[derive(Debug, Clone)]
pub struct ServedAgent {
    pub spec: RecurringAgentSpec,
    /// `spec.description` of the digested spec object.
    pub description: Option<String>,
    pub skip_if_completed: bool,
    pub auto_fix_on_failure: bool,
}

/// One name the served list accounts for.
#[derive(Debug, Clone)]
pub enum ServedEntry {
    Agent(Box<ServedAgent>),
    /// Named, but not applicable by this build: coord could not resolve it, or
    /// this build could not parse it. Its last-applied task is KEPT.
    Unresolvable {
        reason: String,
    },
}

/// A well-formed device list for one tenant.
#[derive(Debug, Clone)]
pub struct DesiredList {
    pub tenant_id: Uuid,
    pub entries: BTreeMap<String, ServedEntry>,
}

/// The top-level fields coord serves beside `spec` are projections of it
/// (`device_entry`, `recurring_agents_store.rs:1038-1055`), and the digest is
/// over `spec` only. A projection that disagrees with `spec` means the digest
/// does not describe what would be applied, so such an entry is not applied
/// (kept as unresolvable). Pairs: (top-level key, `spec` key).
const SPEC_PROJECTIONS: &[(&str, &str)] = &[
    ("task_spec", "task"),
    ("conditions", "conditions"),
    ("schedule", "schedule"),
    ("timezone", "timezone"),
    ("enabled", "enabled"),
    ("catch_up_policy", "catch_up_policy"),
    ("catalog_ref", "catalog_ref"),
    ("params", "params"),
];

fn projections_match(raw: &Value) -> bool {
    let Some(spec) = raw.get("spec").filter(|s| s.is_object()) else {
        return false;
    };
    SPEC_PROJECTIONS.iter().all(|(top, inner)| {
        raw.get(*top).unwrap_or(&Value::Null) == spec.get(*inner).unwrap_or(&Value::Null)
    })
}

/// Parse coord's body for a poll made as `requested`. `Err` = not a
/// well-formed list, which must change NOTHING: an unexpected shape, a
/// `spec_version` this build does not know, a `tenant_id` echo that is not the
/// tenant asked for, an agent entry with no name, or a name served twice.
///
/// An agent entry that HAS a name but does not parse as a
/// [`RecurringAgentSpec`] is still accounted for (as unresolvable), so its
/// last-applied task is kept rather than read as untargeted.
pub fn parse_device_list(body: &Value, requested: Uuid) -> Result<DesiredList, String> {
    let wire: DeviceAgentsWire =
        serde_json::from_value(body.clone()).map_err(|e| format!("body shape: {e}"))?;
    if wire.tenant_id != requested {
        return Err(format!(
            "coord answered for tenant {} but tenant {requested} was asked for",
            wire.tenant_id
        ));
    }
    if wire.spec_version != SUPPORTED_SPEC_VERSION {
        return Err(format!(
            "spec_version {} is not the {SUPPORTED_SPEC_VERSION} this build applies",
            wire.spec_version
        ));
    }
    let mut entries = BTreeMap::new();
    let mut insert = |name: String, entry: ServedEntry| -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("an entry has a blank name".to_string());
        }
        if entries.insert(name.clone(), entry).is_some() {
            return Err(format!("agent `{name}` is served twice"));
        }
        Ok(())
    };
    for raw in &wire.agents {
        let Some(name) = raw.get("name").and_then(Value::as_str) else {
            return Err("an agent entry has no name".to_string());
        };
        let entry = match serde_json::from_value::<RecurringAgentSpec>(raw.clone()) {
            Ok(_) if !projections_match(raw) => ServedEntry::Unresolvable {
                reason: REASON_SPEC_PROJECTION_MISMATCH.to_string(),
            },
            Ok(spec) if spec.name == name => {
                let body = raw.get("spec");
                let flag = |k: &str| body.and_then(|b| b.get(k)).and_then(Value::as_bool);
                ServedEntry::Agent(Box::new(ServedAgent {
                    description: body
                        .and_then(|b| b.get("description"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    skip_if_completed: flag("skip_if_completed").unwrap_or(false),
                    auto_fix_on_failure: flag("auto_fix_on_failure").unwrap_or(false),
                    spec,
                }))
            }
            _ => ServedEntry::Unresolvable {
                reason: REASON_SPEC_UNPARSEABLE.to_string(),
            },
        };
        insert(name.to_string(), entry)?;
    }
    for u in wire.unresolvable {
        let code: String = u.error.chars().take(200).collect();
        insert(
            u.name,
            ServedEntry::Unresolvable {
                reason: format!("{REASON_COORD_UNRESOLVABLE_PREFIX}{code}"),
            },
        )?;
    }
    Ok(DesiredList {
        tenant_id: wire.tenant_id,
        entries,
    })
}

// ===========================================================================
// What the planner sees and produces
// ===========================================================================

/// This device's `paths.workspace_root` setting, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRootRead {
    Set(PathBuf),
    Unset,
    /// settings.json could not be read: its defaults are not this device's.
    Unknown(String),
}

/// The device facts a render resolves tokens against.
pub struct LocalContext<'a> {
    pub workspace_root: WorkspaceRootRead,
    /// The bash a probe's `{bash}` resolves to; `None` = none found.
    pub bash: Option<String>,
    /// Whether a resolved bundled-script path exists.
    pub path_exists: &'a (dyn Fn(&Path) -> bool + Sync),
}

/// What the device holds about its tenants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum BindingsRead {
    /// coord's own bound set for this device, fresh enough to act on.
    Proven(Vec<Uuid>),
    /// Not established. Everything is withheld.
    Unknown(String),
}

/// How one tenant's poll went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum TenantPollStatus {
    /// A well-formed list was read and reconciled.
    Reconciled,
    /// No credential for this tenant on this device: not polled.
    NoCredential,
    /// Non-2xx, transport or decode failure: nothing changed.
    FetchFailed(String),
    /// 2xx, but not a well-formed list for this tenant: nothing changed.
    Malformed(String),
}

/// The task a managed upsert writes, before the executor merges it over the
/// stored row.
#[derive(Debug, Clone)]
pub struct DesiredTask {
    pub name: String,
    pub description: Option<String>,
    pub schedule: crate::scheduler::ScheduleExpression,
    pub task: ScheduledTaskType,
    pub conditions: Option<ScheduleConditions>,
    pub enabled: bool,
    pub skip_if_completed: bool,
    pub auto_fix_on_failure: bool,
    pub catch_up_policy: crate::scheduler::CatchUpPolicy,
    pub timezone: Option<String>,
    pub managed: ManagedBy,
}

/// One write the executor performs.
#[derive(Debug, Clone)]
pub enum PlannedAction {
    /// Create (no `existing`) or update the task carrying `desired.managed`'s key.
    Upsert {
        existing: Option<Box<ScheduledTask>>,
        desired: Box<DesiredTask>,
    },
    /// Delete the managed task `task_id` (only while it carries this key).
    Delete {
        tenant_id: Uuid,
        agent: String,
        task_id: String,
    },
}

/// What happened to one agent on this device — Phase 4c's status line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentOutcome {
    pub tenant_id: Uuid,
    pub name: String,
    pub state: RecurringAgentState,
    /// Set exactly when `state` is `unapplied` or `refused`.
    pub reason: Option<String>,
    /// The digest of the task this device actually holds for the agent.
    pub applied_digest: Option<String>,
    pub applied_version: Option<u32>,
    /// The digest coord served this poll, when it served one.
    pub served_digest: Option<String>,
    pub task_id: Option<String>,
}

/// One tenant's slice of a reconcile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TenantReport {
    pub tenant_id: Uuid,
    pub poll: TenantPollStatus,
    pub agents: Vec<AgentOutcome>,
    /// Agents whose managed task this pass deleted (untargeted / removed).
    pub deleted: Vec<String>,
}

/// One reconcile, in full. Held in [`last_report`] for Phase 4c.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    pub reconciled_at: String,
    pub bindings: BindingsRead,
    pub tenants: Vec<TenantReport>,
    /// Managed rows left alone because their tenant is not proven here.
    pub unproven: Vec<AgentOutcome>,
    /// Why `paths.workspace_root` could not be read this pass (settings.json
    /// unreadable), when it could not; agents needing it are `unapplied`.
    pub workspace_root_unknown: Option<String>,
    /// Set when the pass could not run at all (the local task read failed).
    pub error: Option<String>,
}

/// The planner's output: the writes, plus the report they will produce.
#[derive(Debug, Clone)]
pub struct Plan {
    pub actions: Vec<PlannedAction>,
    pub tenants: Vec<TenantReport>,
    pub unproven: Vec<AgentOutcome>,
}

fn outcome(
    tenant_id: Uuid,
    name: &str,
    state: RecurringAgentState,
    reason: Option<&str>,
    stored: Option<&ScheduledTask>,
    served_digest: Option<&str>,
) -> AgentOutcome {
    let managed = stored.and_then(|t| t.managed.as_ref());
    AgentOutcome {
        tenant_id,
        name: name.to_string(),
        state,
        reason: reason.map(str::to_string),
        applied_digest: managed.and_then(|m| m.digest.clone()),
        applied_version: managed.map(|m| m.version),
        served_digest: served_digest.map(str::to_string),
        task_id: stored.map(|t| t.id.clone()),
    }
}

/// The coord-managed key of `task`, when it carries one with a UUID tenant.
fn coord_key(task: &ScheduledTask) -> Option<(Uuid, &str)> {
    let m = task.managed.as_ref()?;
    if m.source != MANAGED_SOURCE_COORD {
        return None;
    }
    Some((Uuid::parse_str(&m.tenant_id).ok()?, m.agent.as_str()))
}

/// A managed row of the coord source whose tenant is not a UUID is foreign by
/// construction (no proven tenant can equal it).
fn is_coord_managed(task: &ScheduledTask) -> bool {
    task.managed
        .as_ref()
        .is_some_and(|m| m.source == MANAGED_SOURCE_COORD)
}

// ===========================================================================
// The planner (pure)
// ===========================================================================

/// Plan one reconcile. PURE: no clock, no IO.
///
/// - `bindings` Unknown → no action at all; every coord-managed row reported
///   `unapplied: tenant_unproven`.
/// - a managed row whose tenant is not in the proven set → untouched,
///   `unapplied: foreign_tenant` (never adopted, never deleted).
/// - per proven tenant, `polls` carries its list or why there is none; only a
///   list changes anything for that tenant.
pub fn plan(
    bindings: &BindingsRead,
    polls: &[(Uuid, Result<DesiredList, TenantPollStatus>)],
    local: &[ScheduledTask],
    ctx: &LocalContext<'_>,
) -> Plan {
    let mut out = Plan {
        actions: Vec::new(),
        tenants: Vec::new(),
        unproven: Vec::new(),
    };
    let proven: BTreeSet<Uuid> = match bindings {
        BindingsRead::Proven(t) => t.iter().copied().collect(),
        BindingsRead::Unknown(_) => {
            for task in local.iter().filter(|t| is_coord_managed(t)) {
                let agent = task
                    .managed
                    .as_ref()
                    .map(|m| m.agent.as_str())
                    .unwrap_or("");
                let tenant = coord_key(task).map(|(t, _)| t).unwrap_or_default();
                out.unproven.push(outcome(
                    tenant,
                    agent,
                    RecurringAgentState::Unapplied,
                    Some(UNAPPLIED_TENANT_UNPROVEN),
                    Some(task),
                    None,
                ));
            }
            return out;
        }
    };
    // Names a coord-managed row of an UNPROVEN tenant occupies: creating a
    // proven tenant's task under one would leave two tasks firing.
    let mut foreign_names: BTreeSet<&str> = BTreeSet::new();
    for task in local.iter().filter(|t| is_coord_managed(t)) {
        let key = coord_key(task);
        if key.is_some_and(|(t, _)| proven.contains(&t)) {
            continue;
        }
        foreign_names.insert(task.name.as_str());
        if let Some(m) = &task.managed {
            foreign_names.insert(m.agent.as_str());
        }
        let agent = task
            .managed
            .as_ref()
            .map(|m| m.agent.as_str())
            .unwrap_or("");
        out.unproven.push(outcome(
            key.map(|(t, _)| t).unwrap_or_default(),
            agent,
            RecurringAgentState::Unapplied,
            Some(REASON_FOREIGN_TENANT),
            Some(task),
            None,
        ));
    }
    for (tenant, poll) in polls {
        if !proven.contains(tenant) {
            // Never act on a list for a tenant the device has not proven.
            continue;
        }
        let mut report = TenantReport {
            tenant_id: *tenant,
            poll: TenantPollStatus::Reconciled,
            agents: Vec::new(),
            deleted: Vec::new(),
        };
        match poll {
            Err(status) => report.poll = status.clone(),
            Ok(list) if list.tenant_id != *tenant => {
                report.poll = TenantPollStatus::Malformed(format!(
                    "list is for tenant {}, not {tenant}",
                    list.tenant_id
                ));
            }
            Ok(list) => plan_tenant(
                *tenant,
                list,
                local,
                &foreign_names,
                ctx,
                &mut out.actions,
                &mut report,
            ),
        }
        out.tenants.push(report);
    }
    out
}

/// Does the stored managed task already hold exactly what `desired` would
/// write? Compared field by field through serde (the stored row is what the
/// store reads back), plus coord's digest via the managed key. `enabled` is
/// one-way: a local disable under an enabled declaration is a pause (kept,
/// reported `paused`), while a task enabled locally under a DISABLED
/// declaration is not current.
fn is_current(stored: &ScheduledTask, desired: &DesiredTask) -> bool {
    fn same<T: Serialize>(a: &T, b: &T) -> bool {
        serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
    }
    // The managed key and coord's DIGEST — never `version`, which is display
    // only (plan Phase 2): a version-only difference is not a write.
    let key_and_digest = stored.managed.as_ref().is_some_and(|m| {
        m.source == desired.managed.source
            && m.tenant_id == desired.managed.tenant_id
            && m.agent == desired.managed.agent
            && m.digest == desired.managed.digest
    });
    key_and_digest
        && stored.name == desired.name
        && stored.description == desired.description
        && same(&stored.schedule, &desired.schedule)
        && same(&stored.task, &desired.task)
        && same(&stored.conditions, &desired.conditions)
        && stored.skip_if_completed == desired.skip_if_completed
        && stored.auto_fix_on_failure == desired.auto_fix_on_failure
        && same(&stored.catch_up_policy, &desired.catch_up_policy)
        && stored.timezone == desired.timezone
        && (desired.enabled || !stored.enabled)
}

fn plan_tenant(
    tenant: Uuid,
    list: &DesiredList,
    local: &[ScheduledTask],
    foreign_names: &BTreeSet<&str>,
    ctx: &LocalContext<'_>,
    actions: &mut Vec<PlannedAction>,
    report: &mut TenantReport,
) {
    let mine: BTreeMap<&str, &ScheduledTask> = local
        .iter()
        .filter_map(|t| match coord_key(t) {
            Some((t_id, agent)) if t_id == tenant => Some((agent, t)),
            _ => None,
        })
        .collect();
    let unmanaged_names: BTreeSet<&str> = local
        .iter()
        .filter(|t| t.managed.is_none())
        .map(|t| t.name.as_str())
        .collect();

    for (name, entry) in &list.entries {
        let stored = mine.get(name.as_str()).copied();
        let agent = match entry {
            ServedEntry::Unresolvable { reason } => {
                report.agents.push(outcome(
                    tenant,
                    name,
                    RecurringAgentState::Unapplied,
                    Some(reason),
                    stored,
                    None,
                ));
                continue;
            }
            ServedEntry::Agent(a) => a,
        };
        let served = agent.spec.digest.as_deref();
        if stored.is_none() {
            // Creating would put a second task of this name on the device.
            let collision = if unmanaged_names.contains(name.as_str()) {
                Some(REASON_NAME_COLLISION_UNMANAGED)
            } else if foreign_names.contains(name.as_str()) {
                // A row an unproven tenant's declaration left behind (e.g.
                // before a re-bind) still fires; a second one would double it.
                Some(REASON_NAME_COLLISION_FOREIGN_TENANT)
            } else {
                None
            };
            if let Some(reason) = collision {
                report.agents.push(outcome(
                    tenant,
                    name,
                    RecurringAgentState::Unapplied,
                    Some(reason),
                    None,
                    served,
                ));
                continue;
            }
        }
        // Rendered EVERY pass, not only on a digest change: the device-resolved
        // half (workspace root, bash path) can move under an unchanged digest,
        // and a local edit through the ordinary task PUT keeps coord's digest.
        match render(tenant, agent, ctx) {
            Err((state, reason)) => {
                report
                    .agents
                    .push(outcome(tenant, name, state, Some(reason), stored, served));
            }
            Ok(desired) if stored.is_some_and(|t| is_current(t, &desired)) => {
                let task = stored.expect("guarded by is_some_and");
                let state = if !task.enabled && desired.enabled {
                    RecurringAgentState::Paused
                } else {
                    RecurringAgentState::Applied
                };
                report
                    .agents
                    .push(outcome(tenant, name, state, None, stored, served));
            }
            Ok(desired) => {
                let mut o = outcome(
                    tenant,
                    name,
                    RecurringAgentState::Applied,
                    None,
                    stored,
                    served,
                );
                o.applied_digest = desired.managed.digest.clone();
                o.applied_version = Some(desired.managed.version);
                report.agents.push(o);
                actions.push(PlannedAction::Upsert {
                    existing: stored.map(|t| Box::new(t.clone())),
                    desired: Box::new(desired),
                });
            }
        }
    }

    // Untargeted / removed: on THIS well-formed list only.
    for (agent, task) in &mine {
        if !list.entries.contains_key(*agent) {
            report.deleted.push((*agent).to_string());
            actions.push(PlannedAction::Delete {
                tenant_id: tenant,
                agent: (*agent).to_string(),
                task_id: task.id.clone(),
            });
        }
    }
}

type Refusal = (RecurringAgentState, &'static str);

fn unapplied(reason: &'static str) -> Refusal {
    (RecurringAgentState::Unapplied, reason)
}

fn refused(reason: &'static str) -> Refusal {
    (RecurringAgentState::Refused, reason)
}

/// Turn one served agent into the task this device would run, or say why it
/// will not. D9 first (so a refusal is reported as one), then shape, then the
/// fields this build does not bind, then token resolution.
fn render(
    tenant: Uuid,
    agent: &ServedAgent,
    ctx: &LocalContext<'_>,
) -> Result<DesiredTask, Refusal> {
    let spec = &agent.spec;
    let Some(task) = spec.task_spec.clone() else {
        return Err(unapplied(REASON_SPEC_INVALID));
    };
    let catalog = spec.catalog_ref.as_deref();

    // ---- D9 -------------------------------------------------------------
    match &task {
        ScheduledTaskType::RemoteAgent {
            allowed_tools,
            mcp_connections,
            ..
        } => {
            // Neither the catalog nor a custom agent may widen the scheduled
            // RemoteAgent tool set (coord `validate_custom_task_spec`).
            if !allowed_tools.is_empty() || !mcp_connections.is_empty() {
                return Err(refused(REASON_REFUSED_TOOL_WIDENING));
            }
        }
        ScheduledTaskType::Script { command, .. } => {
            let pinned = catalog
                .and_then(script_pin)
                .is_some_and(|p| p.command == command);
            if !pinned {
                return Err(refused(REFUSED_COMMAND));
            }
        }
        _ => return Err(refused(REASON_REFUSED_TASK_TYPE)),
    }
    if let Some(probe) = spec
        .conditions
        .as_ref()
        .and_then(|c| c.require_probe.as_ref())
    {
        let pinned = catalog.and_then(probe_pin).is_some_and(|p| {
            p.command.len() == probe.command.len()
                && p.command.iter().zip(&probe.command).all(|(a, b)| *a == b)
        });
        if !pinned {
            return Err(refused(REFUSED_COMMAND));
        }
    }

    // ---- shape ----------------------------------------------------------
    if spec.validate().is_err() || spec.digest.is_none() {
        return Err(unapplied(REASON_SPEC_INVALID));
    }

    // ---- fields this build does not bind (coord#3623) --------------------
    // `scheduler_service.rs` binds RemoteAgent with `..` until plan
    // 2026-10-27-spawn-time-effort-and-ultracode-profiles-for-every-fleet-spawn-path
    // Phase 6: reporting `applied` at a digest naming a level the session will
    // not run at would be false. Pinned by
    // `tests::the_scheduler_still_does_not_bind_the_effort_fields`.
    if let ScheduledTaskType::RemoteAgent {
        effort,
        ultracode,
        effort_profile,
        ..
    } = &task
    {
        if effort.is_some() || ultracode.is_some() || effort_profile.is_some() {
            return Err(unapplied(REASON_EFFORT_FIELDS_UNSUPPORTED));
        }
    }

    if let Some(tz) = &spec.timezone {
        if crate::scheduler::ScheduleZone::parse(tz).is_err() {
            return Err(unapplied(REASON_INVALID_TIMEZONE));
        }
    }

    // ---- token resolution -------------------------------------------------
    let mut task = task;
    match &mut task {
        ScheduledTaskType::RemoteAgent {
            working_directory, ..
        }
        | ScheduledTaskType::Script {
            working_directory, ..
        } => {
            *working_directory = resolve_working_directory(working_directory.take(), ctx)?;
        }
        _ => return Err(refused(REASON_REFUSED_TASK_TYPE)),
    }
    let conditions = match spec.conditions.clone() {
        None => None,
        Some(mut c) => {
            if let Some(probe) = c.require_probe.as_mut() {
                probe.command = resolve_probe_argv(&probe.command, ctx)?;
            }
            // A repo-inactivity watch names paths this device stats. Coord
            // serves none today (no catalog entry carries one, and a custom
            // agent may carry no `conditions`), so one served here must be
            // `{workspace_root}[/<relative>]`, resolved under the same rule as
            // a working directory — never a device path from a fleet-wide row.
            if let Some(repo) = c.require_repo_inactive.as_mut() {
                for watch in &mut repo.repositories {
                    let resolved = watch
                        .path
                        .strip_prefix(WORKSPACE_ROOT_TOKEN)
                        .map(|rest| (rest, workspace_root(ctx)))
                        .ok_or_else(|| unapplied(REASON_PATH_NOT_UNDER_WORKSPACE_ROOT))?;
                    let (rest, root) = resolved;
                    watch.path = join_relative(root?, rest)
                        .ok_or_else(|| unapplied(REASON_PATH_NOT_UNDER_WORKSPACE_ROOT))?
                        .to_string_lossy()
                        .into_owned();
                }
            }
            if crate::scheduler_probe::validate_conditions(&c).is_err() {
                return Err(unapplied(REASON_SPEC_INVALID));
            }
            Some(c)
        }
    };

    Ok(DesiredTask {
        name: spec.name.clone(),
        description: Some(agent.description.clone().unwrap_or_else(|| {
            format!(
                "Recurring agent `{}`. Managed by coord (recurring agents).",
                spec.name
            )
        })),
        schedule: spec.schedule.clone(),
        task,
        conditions,
        enabled: spec.enabled,
        skip_if_completed: agent.skip_if_completed,
        auto_fix_on_failure: agent.auto_fix_on_failure,
        catch_up_policy: spec.catch_up_policy,
        timezone: spec.timezone.clone(),
        managed: spec.managed_by(tenant.to_string()),
    })
}

fn workspace_root<'a>(ctx: &'a LocalContext<'_>) -> Result<&'a Path, Refusal> {
    match &ctx.workspace_root {
        WorkspaceRootRead::Set(p) => Ok(p.as_path()),
        WorkspaceRootRead::Unset => Err(unapplied(UNAPPLIED_WORKSPACE_ROOT_UNSET)),
        WorkspaceRootRead::Unknown(_) => Err(unapplied(REASON_WORKSPACE_ROOT_UNKNOWN)),
    }
}

/// `{root}/a/b` → `base` joined with `a`, `b`, or `None` when the result could
/// land anywhere but under `base`. `rest` is empty (→ `base`) or `/`-led.
///
/// Every segment must be ONE plain path component on every platform: no `\`
/// (a Windows separator, so `..\..\x` cannot hide a traversal inside one
/// `/`-segment), no `:` (a drive or a stream: `C:\Windows` and `C:x` push as
/// absolute or drive-relative on Windows), no trailing `.` or space (Win32
/// strips them, so `.. ` and `...` resolve as `..`), no control character,
/// and exactly one `Component::Normal` (refusing `""`, `.`, `..` and anything
/// rooted). The
/// joined path is then required to start with `base`, as a belt over the
/// segment rule.
fn join_relative(base: &Path, rest: &str) -> Option<PathBuf> {
    if rest.is_empty() {
        return Some(base.to_path_buf());
    }
    let rest = rest.strip_prefix('/')?;
    let mut p = base.to_path_buf();
    for seg in rest.split('/') {
        if !is_plain_segment(seg) {
            return None;
        }
        p.push(seg);
    }
    p.starts_with(base).then_some(p)
}

/// One plain path component on every platform (see [`join_relative`]).
fn is_plain_segment(seg: &str) -> bool {
    // Win32 path normalization strips a component's trailing dots and spaces,
    // so `.. ` and `...` resolve as `..` there; a control character (NUL
    // included) is never a legitimate component. At least as strict as
    // coord's mirror of this rule (qontinui-coord#3676).
    if seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.ends_with(['.', ' '])
        || seg.chars().any(|c| c == '\\' || c == ':' || c.is_control())
    {
        return false;
    }
    let mut components = Path::new(seg).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

/// A served working directory is `{workspace_root}` or
/// `{workspace_root}/<relative>` (coord refuses anything else for a custom
/// agent, and every catalog template is the bare token). `None` stays `None`
/// (the runner's project root).
fn resolve_working_directory(
    wd: Option<String>,
    ctx: &LocalContext<'_>,
) -> Result<Option<String>, Refusal> {
    let Some(wd) = wd else {
        return Ok(None);
    };
    let Some(rest) = wd.strip_prefix(WORKSPACE_ROOT_TOKEN) else {
        return Err(unapplied(REASON_WORKING_DIRECTORY_NOT_TOKENIZED));
    };
    let root = workspace_root(ctx)?;
    join_relative(root, rest)
        .map(|p| Some(p.to_string_lossy().into_owned()))
        .ok_or_else(|| unapplied(REASON_WORKING_DIRECTORY_NOT_TOKENIZED))
}

/// Resolve a PINNED probe argv's tokens against this device: `{bash}` → the
/// bash this runner found, `{workspace_root}[/rel]` → the setting,
/// `{bundled_skill:<name>}[/rel]` → `<workspace_root>/.claude/skills/<name>`
/// (where the runner provisions its bundled skills for a session working at
/// the workspace root — the path today's `schedule-return-to-main.sh` install
/// resolves to). A bundled script that is not there makes the probe
/// unrunnable, which is reported rather than installed: a probe that cannot
/// run is no gate.
fn resolve_probe_argv(argv: &[String], ctx: &LocalContext<'_>) -> Result<Vec<String>, Refusal> {
    let mut out = Vec::with_capacity(argv.len());
    for arg in argv {
        let resolved = if arg == BASH_TOKEN {
            ctx.bash
                .clone()
                .ok_or_else(|| unapplied(REASON_PROBE_UNRESOLVABLE))?
        } else if let Some(rest) = arg.strip_prefix(WORKSPACE_ROOT_TOKEN) {
            let root = workspace_root(ctx)?;
            join_relative(root, rest)
                .ok_or_else(|| unapplied(REASON_PROBE_UNRESOLVABLE))?
                .to_string_lossy()
                .into_owned()
        } else if let Some(after) = arg.strip_prefix(BUNDLED_SKILL_TOKEN_PREFIX) {
            let (skill, rest) = after
                .split_once('}')
                .ok_or_else(|| unapplied(REASON_PROBE_UNRESOLVABLE))?;
            if skill.is_empty() || skill.contains(['/', '\\', '.', ':']) {
                return Err(unapplied(REASON_PROBE_UNRESOLVABLE));
            }
            let dir = workspace_root(ctx)?
                .join(".claude")
                .join("skills")
                .join(skill);
            let path =
                join_relative(&dir, rest).ok_or_else(|| unapplied(REASON_PROBE_UNRESOLVABLE))?;
            if !rest.is_empty() && !(ctx.path_exists)(&path) {
                return Err(unapplied(REASON_PROBE_UNRESOLVABLE));
            }
            path.to_string_lossy().into_owned()
        } else if arg.starts_with('{') {
            return Err(unapplied(REASON_PROBE_UNRESOLVABLE));
        } else {
            arg.clone()
        };
        out.push(resolved);
    }
    Ok(out)
}

// ===========================================================================
// The executor
// ===========================================================================

fn upsert_request(d: &DesiredTask) -> crate::mcp::scheduler::UpsertManagedTaskRequest {
    crate::mcp::scheduler::UpsertManagedTaskRequest {
        name: d.name.clone(),
        description: d.description.clone(),
        schedule: d.schedule.clone(),
        task: d.task.clone(),
        // coord's `enabled` is the declaration: it applies on every digest
        // change. A local disable between changes is left alone and reported
        // `paused`; surviving a digest change needs D6's override record
        // (Phase 4c/4d), which this build does not have.
        enabled: Some(d.enabled),
        skip_if_completed: d.skip_if_completed,
        auto_fix_on_failure: d.auto_fix_on_failure,
        success_criteria: None,
        conditions: d.conditions.clone(),
        catch_up_policy: Some(d.catch_up_policy),
        catch_up_grace_seconds: None,
        launch_failure_backoff_seconds: None,
        timezone: d.timezone.clone(),
        version: d.managed.version,
        digest: d.managed.digest.clone(),
    }
}

/// Apply `plan`'s writes and settle each agent's outcome on what happened.
async fn execute(pg: &PgDb, plan: Plan) -> (Vec<TenantReport>, Vec<AgentOutcome>) {
    let Plan {
        actions,
        mut tenants,
        unproven,
    } = plan;
    let zone = crate::scheduler::ScheduleZone::from_settings(
        &pg.get_scheduler_settings().await.unwrap_or_default(),
    );
    for action in actions {
        match action {
            PlannedAction::Upsert { existing, desired } => {
                let tenant = Uuid::parse_str(&desired.managed.tenant_id).unwrap_or_default();
                let name = desired.managed.agent.clone();
                let request = upsert_request(&desired);
                let mut task = crate::mcp::scheduler::merge_managed_task(
                    existing.as_deref(),
                    request,
                    desired.managed.clone(),
                );
                task.next_run = if task.enabled {
                    crate::scheduler::task_next_run_rfc3339(&task, chrono::Utc::now(), zone)
                } else {
                    None
                };
                task.touch();
                let result = pg
                    .upsert_managed_scheduled_task(
                        &task,
                        existing.as_ref().map(|t| t.modified_at.as_str()),
                    )
                    .await;
                let settle = |o: &mut AgentOutcome| match &result {
                    Ok(ManagedUpsert::Created(id)) | Ok(ManagedUpsert::Updated(id)) => {
                        o.task_id = Some(id.clone());
                    }
                    Ok(ManagedUpsert::Conflict) => {
                        revert_to_stored(o, existing.as_deref(), REASON_APPLY_CONFLICT)
                    }
                    Err(ManagedUpsertError::KeyIndexMissing(_)) => {
                        revert_to_stored(o, existing.as_deref(), REASON_MANAGED_KEY_INDEX_MISSING)
                    }
                    Err(ManagedUpsertError::Other(_)) => {
                        revert_to_stored(o, existing.as_deref(), REASON_APPLY_FAILED)
                    }
                };
                if let Some(o) = find_outcome(&mut tenants, tenant, &name) {
                    settle(o);
                }
                match &result {
                    Ok(ManagedUpsert::Created(id)) => info!(
                        "recurring agents: created managed task `{name}` ({id}) for tenant {tenant} \
                         at digest {}",
                        desired.managed.digest.as_deref().unwrap_or("-")
                    ),
                    Ok(ManagedUpsert::Updated(id)) => info!(
                        "recurring agents: updated managed task `{name}` ({id}) for tenant {tenant} \
                         to digest {}",
                        desired.managed.digest.as_deref().unwrap_or("-")
                    ),
                    Ok(ManagedUpsert::Conflict) => warn!(
                        "recurring agents: managed task `{name}` for tenant {tenant} changed while \
                         it was being applied; retried on the next poll"
                    ),
                    Err(e) => warn!(
                        "recurring agents: applying `{name}` for tenant {tenant} failed ({e}); \
                         the stored task is unchanged"
                    ),
                }
            }
            PlannedAction::Delete {
                tenant_id,
                agent,
                task_id,
            } => {
                match pg
                    .delete_managed_scheduled_task(
                        &task_id,
                        MANAGED_SOURCE_COORD,
                        &tenant_id.to_string(),
                        &agent,
                    )
                    .await
                {
                    Ok(true) => info!(
                        "recurring agents: deleted managed task `{agent}` ({task_id}) for tenant \
                         {tenant_id}: coord no longer serves it to this device"
                    ),
                    Ok(false) => info!(
                        "recurring agents: managed task `{agent}` ({task_id}) was already gone"
                    ),
                    Err(e) => {
                        warn!("recurring agents: deleting `{agent}` ({task_id}) failed: {e}");
                        if let Some(t) = tenants.iter_mut().find(|t| t.tenant_id == tenant_id) {
                            t.deleted.retain(|a| a != &agent);
                        }
                    }
                }
            }
        }
    }
    (tenants, unproven)
}

fn find_outcome<'a>(
    tenants: &'a mut [TenantReport],
    tenant: Uuid,
    name: &str,
) -> Option<&'a mut AgentOutcome> {
    tenants
        .iter_mut()
        .find(|t| t.tenant_id == tenant)?
        .agents
        .iter_mut()
        .find(|a| a.name == name)
}

fn revert_to_stored(o: &mut AgentOutcome, stored: Option<&ScheduledTask>, reason: &str) {
    let m = stored.and_then(|t| t.managed.as_ref());
    o.state = RecurringAgentState::Unapplied;
    o.reason = Some(reason.to_string());
    o.applied_digest = m.and_then(|m| m.digest.clone());
    o.applied_version = m.map(|m| m.version);
    o.task_id = stored.map(|t| t.id.clone());
}

// ===========================================================================
// Shared state (Phase 4c reads it)
// ===========================================================================

static LAST_REPORT: OnceLock<RwLock<Option<ReconcileReport>>> = OnceLock::new();

fn report_cell() -> &'static RwLock<Option<ReconcileReport>> {
    LAST_REPORT.get_or_init(|| RwLock::new(None))
}

/// The most recent reconcile, or `None` before the first one finished. The
/// status push (Phase 4c) consumes it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn last_report() -> Option<ReconcileReport> {
    report_cell().read().ok().and_then(|r| r.clone())
}

fn store_report(report: ReconcileReport) {
    if let Ok(mut cell) = report_cell().write() {
        *cell = Some(report);
    }
}

// ===========================================================================
// Device reads (blocking, off the runtime)
// ===========================================================================

fn read_bindings() -> BindingsRead {
    use qontinui_runner_lib::pair::CoordBoundTenantsRead;
    match qontinui_runner_lib::pair::coord_bound_tenants_for_ask() {
        CoordBoundTenantsRead::Known(ids) => BindingsRead::Proven(ids),
        CoordBoundTenantsRead::Unknown(why) => BindingsRead::Unknown(why.to_string()),
    }
}

fn read_workspace_root() -> WorkspaceRootRead {
    let loaded = crate::settings::load_settings_full();
    if !loaded.is_authoritative() {
        return WorkspaceRootRead::Unknown(
            loaded
                .error
                .unwrap_or_else(|| "settings.json unreadable".to_string()),
        );
    }
    match loaded.settings.paths.workspace_root {
        Some(root) if !root.trim().is_empty() => WorkspaceRootRead::Set(PathBuf::from(root.trim())),
        _ => WorkspaceRootRead::Unset,
    }
}

/// The bash a pinned probe's `{bash}` resolves to. Windows: an explicit Git
/// Bash, never the WSL launcher (`scheduler_script::windows_bash_paths`).
/// Elsewhere: `bash`, found on PATH by the exec.
fn resolve_bash() -> Option<String> {
    #[cfg(windows)]
    {
        let env_var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let pfs: Vec<String> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|k| env_var(k))
            .collect();
        let path_dirs: Vec<String> = std::env::var("PATH")
            .map(|p| {
                p.split(';')
                    .filter(|d| !d.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let local = env_var("LOCALAPPDATA");
        crate::scheduler_script::windows_bash_paths(&pfs, local.as_deref(), &path_dirs)
            .into_iter()
            .find(|p| Path::new(p).is_file())
    }
    #[cfg(not(windows))]
    {
        Some("bash".to_string())
    }
}

async fn poll_tenant(tenant: Uuid) -> Result<DesiredList, TenantPollStatus> {
    let bearer = tokio::task::spawn_blocking(move || crate::auth::device_bearer_for(Some(&tenant)))
        .await
        .ok()
        .flatten();
    let Some(bearer) = bearer else {
        return Err(TenantPollStatus::NoCredential);
    };
    let fetched =
        crate::mcp::fleet_policy_poller::fetch_coord_json_with::<Value>(DEVICE_READ_PATH, &bearer)
            .await;
    poll_status_for(fetched, tenant)
}

/// One poll's fetch result → the tenant's list, or why there is none. PURE.
/// Every failure is an `Err` — which the planner turns into "change nothing"
/// for that tenant — never an empty list.
fn poll_status_for(
    fetched: Result<Value, crate::mcp::fleet_policy_poller::FetchError>,
    tenant: Uuid,
) -> Result<DesiredList, TenantPollStatus> {
    use crate::mcp::fleet_policy_poller::FetchError;
    match fetched {
        Ok(body) => parse_device_list(&body, tenant).map_err(TenantPollStatus::Malformed),
        Err(FetchError::NoJwt) => Err(TenantPollStatus::NoCredential),
        Err(FetchError::AuthOrAbsent(code)) => Err(TenantPollStatus::FetchFailed(format!(
            "coord status {code}"
        ))),
        Err(FetchError::Failed(e)) => Err(TenantPollStatus::FetchFailed(e)),
    }
}

// ===========================================================================
// The loop
// ===========================================================================

/// What the loop last logged, so a steady state logs once.
#[derive(Default)]
struct LogState {
    last_summary: Option<String>,
    startup_assertion_done: bool,
}

/// One reconcile pass.
async fn reconcile_once(pg: &PgDb, log: &mut LogState) {
    let bindings = tokio::task::spawn_blocking(read_bindings)
        .await
        .unwrap_or_else(|e| BindingsRead::Unknown(format!("binding read panicked: {e}")));
    let stored = match pg.get_all_stored_scheduled_tasks().await {
        Ok(rows) => rows.into_iter().map(|s| s.task).collect::<Vec<_>>(),
        Err(e) => {
            warn!("recurring agents: the local task read failed ({e}); nothing reconciled");
            store_report(ReconcileReport {
                reconciled_at: chrono::Utc::now().to_rfc3339(),
                bindings,
                tenants: Vec::new(),
                unproven: Vec::new(),
                workspace_root_unknown: None,
                error: Some(format!("local task read failed: {e}")),
            });
            return;
        }
    };
    let mut polls = Vec::new();
    if let BindingsRead::Proven(tenants) = &bindings {
        for tenant in tenants {
            polls.push((*tenant, poll_tenant(*tenant).await));
        }
    }
    let (workspace_root, bash) =
        tokio::task::spawn_blocking(|| (read_workspace_root(), resolve_bash()))
            .await
            .unwrap_or_else(|e| {
                (
                    WorkspaceRootRead::Unknown(format!("settings read panicked: {e}")),
                    None,
                )
            });
    let workspace_root_unknown = match &workspace_root {
        WorkspaceRootRead::Unknown(why) => Some(why.clone()),
        _ => None,
    };
    let exists = |p: &Path| p.is_file();
    let planned = {
        let ctx = LocalContext {
            workspace_root,
            bash,
            path_exists: &exists,
        };
        plan(&bindings, &polls, &stored, &ctx)
    };

    if !log.startup_assertion_done {
        log.startup_assertion_done = true;
        let foreign = planned
            .unproven
            .iter()
            .filter(|o| o.reason.as_deref() == Some(REASON_FOREIGN_TENANT))
            .count();
        if foreign > 0 {
            warn!(
                "recurring agents: {foreign} managed task(s) carry a tenant this device has not \
                 proven it is bound to — left alone and reported foreign_tenant, never adopted \
                 or deleted (plan 2026-10-29 Phase 0.1)"
            );
        }
    }

    let (tenants, unproven) = execute(pg, planned).await;
    let report = ReconcileReport {
        reconciled_at: chrono::Utc::now().to_rfc3339(),
        bindings,
        tenants,
        unproven,
        workspace_root_unknown,
        error: None,
    };
    let summary = summarize(&report);
    if log.last_summary.as_deref() != Some(summary.as_str()) {
        match &report.bindings {
            BindingsRead::Unknown(why) => warn!(
                "recurring agents: this device's tenant bindings are UNKNOWN ({why}) — \
                 WITHHOLDING every apply and delete; managed tasks keep running as last applied"
            ),
            BindingsRead::Proven(_) => info!("recurring agents: {summary}"),
        }
        log.last_summary = Some(summary);
    }
    store_report(report);
}

/// A timestamp-free one-line summary, for edge-triggered logging.
fn summarize(r: &ReconcileReport) -> String {
    let mut parts = Vec::new();
    for t in &r.tenants {
        let poll = match &t.poll {
            TenantPollStatus::Reconciled => "reconciled".to_string(),
            TenantPollStatus::NoCredential => "no credential (not polled)".to_string(),
            TenantPollStatus::FetchFailed(e) => format!("poll failed ({e}); nothing changed"),
            TenantPollStatus::Malformed(e) => format!("list malformed ({e}); nothing changed"),
        };
        let agents = t
            .agents
            .iter()
            .map(|a| match &a.reason {
                Some(r) => format!("{}={:?}:{r}", a.name, a.state),
                None => format!("{}={:?}", a.name, a.state),
            })
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("tenant {}: {poll} [{agents}]", t.tenant_id));
    }
    if !r.unproven.is_empty() {
        parts.push(format!(
            "{} unproven-tenant task(s) left alone",
            r.unproven.len()
        ));
    }
    if let Some(why) = &r.workspace_root_unknown {
        parts.push(format!("paths.workspace_root UNKNOWN ({why})"));
    }
    if parts.is_empty() {
        "no proven tenant to reconcile".to_string()
    } else {
        parts.join("; ")
    }
}

/// Start the reconciler (primary runner only — the caller gates it beside
/// `start_scheduler_service`). Supervised: a panic respawns the loop rather
/// than freezing the managed set unreconciled until a runner restart.
pub fn start(pg: Arc<PgDb>) {
    crate::mcp::task_supervisor::spawn_supervised_forever(
        "Recurring-agent reconciler",
        Duration::from_secs(5),
        Duration::from_secs(300),
        Duration::from_secs(120),
        move || run_loop(pg.clone()),
    );
}

async fn run_loop(pg: Arc<PgDb>) {
    info!(
        "recurring agents: reconciler started (polls {DEVICE_READ_PATH} every {}s)",
        POLL_INTERVAL.as_secs()
    );
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut log = LogState::default();
    loop {
        tick.tick().await;
        reconcile_once(&pg, &mut log).await;
    }
}

#[cfg(test)]
mod tests;
