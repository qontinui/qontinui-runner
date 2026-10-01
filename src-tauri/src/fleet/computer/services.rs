//! The service watcher (plan §3.2, Phase 2.3): the latest state of every
//! watched unit on a computer — GitHub Actions runner services and the
//! runner's own units.
//!
//! ## One model, three instruments
//!
//! Every platform lands in [`UnitProps`], and a [`ServiceRow`] (the wire row)
//! is built from that one type, so the three instruments cannot drift into
//! three vocabularies:
//!
//! * **Linux** — systemd over D-Bus through the existing `zbus` dependency
//!   (no root: reading unit properties is unprivileged). The system bus for
//!   `actions.runner.*.service`, the session bus for the runner's own user
//!   units when a session bus exists.
//! * **Windows** — `sc query` / `sc qc` for `actions.runner.*` services.
//! * **WSL guests** — `systemctl show` text read through `wsl.exe` (see
//!   `wsl_guest`), parsed by [`parse_systemctl_show`] — the same parser the
//!   incident fixture exercises.
//!
//! ## Absence is UNKNOWN
//!
//! Every property is `Option`. A `MemoryMax=infinity` is `None` (unbounded is
//! not a number), a `MemoryPeak` the kernel did not record is `None`, and an
//! unreadable `.runner` leaves `runner_name`/`repo` to the unit-name fallback
//! or `None` — never a guess at the repo.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Wire value for `kind`, contract §1 `computer_services.kind`.
pub(crate) const KIND_GH_ACTIONS_RUNNER: &str = "gh_actions_runner";
pub(crate) const KIND_QONTINUI_RUNNER: &str = "qontinui_runner";
pub(crate) const KIND_OTHER_WATCHED: &str = "other_watched";

/// Unit-name patterns the Linux watcher asks systemd for, on both buses.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const WATCHED_PATTERNS: &[&str] =
    &["actions.runner.*.service", "qontinui-runner*.service"];

/// Classify a unit name. `actions.runner.*` is a GitHub runner service on
/// every platform (systemd unit or Windows service); `qontinui-runner*` is this
/// product's own runner unit.
pub(crate) fn classify_unit(unit: &str) -> &'static str {
    if unit.starts_with("actions.runner.") {
        KIND_GH_ACTIONS_RUNNER
    } else if unit.starts_with("qontinui-runner") {
        KIND_QONTINUI_RUNNER
    } else {
        KIND_OTHER_WATCHED
    }
}

/// Everything read about one unit, from any instrument.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct UnitProps {
    pub(crate) unit: String,
    pub(crate) active_state: Option<String>,
    pub(crate) sub_state: Option<String>,
    pub(crate) result: Option<String>,
    pub(crate) restart: Option<String>,
    pub(crate) oom_policy: Option<String>,
    pub(crate) memory_max: Option<u64>,
    pub(crate) memory_peak: Option<u64>,
    pub(crate) n_restarts: Option<u32>,
    pub(crate) state_changed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) exec_main_status: Option<i32>,
    pub(crate) working_directory: Option<String>,
    pub(crate) exec_start_path: Option<String>,
    pub(crate) control_group: Option<String>,
}

/// One `computer_services` row on the wire (contract §3 `services[]`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ServiceRow {
    pub(crate) unit: String,
    pub(crate) kind: String,
    pub(crate) active_state: Option<String>,
    pub(crate) sub_state: Option<String>,
    pub(crate) result: Option<String>,
    pub(crate) restart_policy: Option<String>,
    pub(crate) oom_policy: Option<String>,
    pub(crate) memory_max: Option<u64>,
    pub(crate) memory_peak: Option<u64>,
    pub(crate) n_restarts: Option<u32>,
    pub(crate) state_changed_at: Option<String>,
    pub(crate) runner_name: Option<String>,
    pub(crate) repo: Option<String>,
}

/// A watched unit as the event deriver needs it: the wire row plus the cgroup
/// `memory.events` `oom_kill` counter used to attribute an OOM kill.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WatchedUnit {
    pub(crate) row: ServiceRow,
    pub(crate) cgroup_oom_kill: Option<u64>,
}

/// coord stores the byte columns as `bigint`; a value that does not fit is
/// not a measurement it can hold, so it is UNKNOWN rather than wrapped.
fn fits_bigint(v: u64) -> Option<u64> {
    (v <= i64::MAX as u64).then_some(v)
}

/// Build the wire row. `runner` is the `.runner` identity for a GitHub runner
/// (`(runner_name, repo)`), already resolved by the caller.
pub(crate) fn row_from_props(
    p: &UnitProps,
    runner: Option<(Option<String>, Option<String>)>,
) -> ServiceRow {
    let kind = classify_unit(&p.unit);
    let (mut runner_name, repo) = runner.unwrap_or((None, None));
    if kind == KIND_GH_ACTIONS_RUNNER && runner_name.is_none() {
        runner_name = runner_name_from_unit(&p.unit);
    }
    ServiceRow {
        unit: p.unit.clone(),
        kind: kind.to_string(),
        active_state: p.active_state.clone(),
        sub_state: p.sub_state.clone(),
        result: p.result.clone(),
        restart_policy: p.restart.clone(),
        oom_policy: p.oom_policy.clone(),
        memory_max: p.memory_max.and_then(fits_bigint),
        memory_peak: p.memory_peak.and_then(fits_bigint),
        n_restarts: p.n_restarts,
        state_changed_at: p
            .state_changed_at
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        runner_name,
        repo,
    }
}

// ---------------------------------------------------------------------------
// `systemctl show` text (WSL guests, and the synthetic fixtures)
// ---------------------------------------------------------------------------

/// The `-p` list the WSL guest probe asks for. Kept here beside the parser so
/// the two cannot disagree on a property name.
pub(crate) const SHOW_PROPERTIES: &str = "Id,ActiveState,SubState,Result,Restart,OOMPolicy,MemoryMax,MemoryPeak,NRestarts,StateChangeTimestamp,ExecMainStatus,WorkingDirectory,ExecStart,ControlGroup,LoadState";

/// `infinity` / `[not set]` / empty → `None`; a number → `Some`.
fn parse_limit(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() || v == "infinity" || v.starts_with('[') {
        return None;
    }
    v.parse::<u64>().ok().filter(|n| *n != u64::MAX)
}

fn non_empty(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty() && v != "[not set]" && v != "n/a").then(|| v.to_string())
}

/// Parse `StateChangeTimestamp` as `systemctl show` prints it:
/// `Wed 2026-09-30 01:48:16 UTC` (the probe forces `TZ=UTC`) or `@<epoch>`
/// (`--timestamp=unix`). Any other zone is `None` rather than a timestamp
/// silently shifted by an unknown offset.
pub(crate) fn parse_show_timestamp(v: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let v = v.trim();
    if let Some(epoch) = v.strip_prefix('@') {
        let secs = epoch.parse::<i64>().ok()?;
        return chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0);
    }
    let mut parts = v.split_whitespace();
    let _weekday = parts.next()?;
    let date = parts.next()?;
    let time = parts.next()?;
    let zone = parts.next()?;
    if zone != "UTC" || parts.next().is_some() {
        return None;
    }
    let naive =
        chrono::NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S")
            .ok()?;
    Some(naive.and_utc())
}

/// First `path=` in an `ExecStart={ path=/x/runsvc.sh ; argv[]=… }` value.
fn parse_exec_start_path(v: &str) -> Option<String> {
    let rest = v.split_once("path=")?.1;
    let path = rest.split([' ', ';']).next()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Parse `systemctl show` output for one or more units (blocks separated by a
/// blank line). A block without `Id=` is dropped — a row nobody can name is
/// not a row — and so is a unit systemd reports as `LoadState=not-found`.
pub(crate) fn parse_systemctl_show(text: &str) -> Vec<UnitProps> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut p = UnitProps::default();
        let mut not_found = false;
        for line in block.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "Id" => p.unit = v.trim().to_string(),
                "ActiveState" => p.active_state = non_empty(v),
                "SubState" => p.sub_state = non_empty(v),
                "Result" => p.result = non_empty(v),
                "Restart" => p.restart = non_empty(v),
                "OOMPolicy" => p.oom_policy = non_empty(v),
                "MemoryMax" => p.memory_max = parse_limit(v),
                "MemoryPeak" => p.memory_peak = parse_limit(v),
                "NRestarts" => p.n_restarts = v.trim().parse().ok(),
                "StateChangeTimestamp" => p.state_changed_at = parse_show_timestamp(v),
                "ExecMainStatus" => p.exec_main_status = v.trim().parse().ok(),
                "WorkingDirectory" => p.working_directory = non_empty(v),
                "ExecStart" => p.exec_start_path = parse_exec_start_path(v),
                "ControlGroup" => p.control_group = non_empty(v),
                "LoadState" => not_found = v.trim() == "not-found",
                _ => {}
            }
        }
        if !p.unit.is_empty() && !not_found {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GitHub runner identity: `.runner`
// ---------------------------------------------------------------------------

/// `(agentName, owner/repo)` from a runner's `.runner` JSON.
///
/// The BOM strip is load-bearing: `config.sh` writes `.runner` as UTF-8 WITH a
/// BOM and `serde_json` rejects a leading U+FEFF (the supervisor's
/// `agent_name_from_runner_file` records the same fact). `repo` is the
/// `owner/repo` path of `gitHubUrl` — the key coord's registrar uses — and
/// `None` for an org-level runner, whose URL names no single repo.
pub(crate) fn parse_runner_file(json: &str) -> Option<(Option<String>, Option<String>)> {
    let json = json.trim_start_matches('\u{feff}').trim();
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let name = v
        .get("agentName")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let repo = v
        .get("gitHubUrl")
        .and_then(|x| x.as_str())
        .and_then(repo_from_github_url);
    Some((name, repo))
}

fn repo_from_github_url(url: &str) -> Option<String> {
    let path = url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split_once("://")
        .map(|(_, rest)| rest)?;
    let mut segs = path.split('/').skip(1); // drop the host
    let owner = segs.next().filter(|s| !s.is_empty())?;
    let repo = segs.next().filter(|s| !s.is_empty())?;
    segs.next().is_none().then(|| format!("{owner}/{repo}"))
}

/// `actions.runner.<scope>.<runner_name>[.service]` → `<runner_name>`.
///
/// The fallback when `.runner` is unreadable — which is the NORMAL case on a
/// host whose runner lives in another account's `0700` home (a fleet host's
/// `/home/runner`). The scope (`<owner>-<repo>` or `<owner>`) cannot be split
/// back into owner and repo unambiguously, so this never yields a repo.
pub(crate) fn runner_name_from_unit(unit: &str) -> Option<String> {
    let mid = unit.strip_prefix("actions.runner.")?;
    let mid = mid.strip_suffix(".service").unwrap_or(mid);
    let (_scope, name) = mid.split_once('.')?;
    (!name.is_empty()).then(|| name.to_string())
}

/// The runner install dir for a unit: `WorkingDirectory` (minus systemd's
/// `-`/`!` prefixes; `~` is not resolvable here), else the directory of
/// `ExecStart`'s first path.
pub(crate) fn runner_dir_of(p: &UnitProps) -> Option<PathBuf> {
    if let Some(wd) = p.working_directory.as_deref() {
        let wd = wd.trim_start_matches(['-', '!', '+']);
        if wd.starts_with('/') {
            return Some(PathBuf::from(wd));
        }
    }
    let exec = p.exec_start_path.as_deref()?;
    Path::new(exec).parent().map(Path::to_path_buf)
}

/// Read and parse `<dir>/.runner`. `None` when unreadable (permissions are the
/// common case) or unparseable.
pub(crate) fn read_runner_file(dir: &Path) -> Option<(Option<String>, Option<String>)> {
    let text = std::fs::read_to_string(dir.join(".runner")).ok()?;
    parse_runner_file(&text)
}

/// A cgroup v2 `memory.events` `oom_kill` counter for `control_group`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn read_cgroup_oom_kill(control_group: Option<&str>) -> Option<u64> {
    let cg = control_group?.trim();
    if cg.is_empty() || !cg.starts_with('/') {
        return None;
    }
    let text = std::fs::read_to_string(format!("/sys/fs/cgroup{cg}/memory.events")).ok()?;
    crate::fleet::host_axes::parse_memory_events_oom_kill(&text)
}

/// Resolve the `.runner` identity for a GitHub runner unit (`None` for any
/// other kind) and build its [`WatchedUnit`].
pub(crate) fn watched_from_props(p: &UnitProps, read_cgroup: bool) -> WatchedUnit {
    let runner = if classify_unit(&p.unit) == KIND_GH_ACTIONS_RUNNER {
        runner_dir_of(p).and_then(|d| read_runner_file(&d))
    } else {
        None
    };
    WatchedUnit {
        row: row_from_props(p, runner),
        cgroup_oom_kill: if read_cgroup {
            read_cgroup_oom_kill(p.control_group.as_deref())
        } else {
            None
        },
    }
}

/// What one tick's service scan produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ServiceScan {
    /// `None` = no service information this tick (the instrument failed or
    /// does not exist) — coord leaves stored rows untouched.
    pub(crate) units: Option<Vec<WatchedUnit>>,
    /// The service KINDS this scan saw completely (contract amendment A2):
    /// for each kind listed, every source that can host that kind answered,
    /// so a missing row of that kind is a unit that is really gone. Kinds not
    /// listed are a delta for that kind and delete nothing.
    pub(crate) complete_kinds: Vec<&'static str>,
}

/// How the session (user) bus fared this scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionBus {
    /// Answered.
    Ok,
    /// This process has no session bus by design (a system-service runner):
    /// no `DBUS_SESSION_BUS_ADDRESS` and no `$XDG_RUNTIME_DIR/bus` socket —
    /// there are no user units to lose.
    AbsentByDesign,
    /// A session bus exists (or is advertised) and did not answer fully.
    Failed,
}

/// Whether a missing session-bus connection is "absent by design" rather than
/// a failure: `DBUS_SESSION_BUS_ADDRESS` unset/blank AND no socket at
/// `$XDG_RUNTIME_DIR/bus`. Anything else is a bus that SHOULD have answered —
/// a user-unit runner restarted while its bus was unreachable must not claim
/// its own kind complete and delete its own rows. PURE over its inputs.
pub(crate) fn session_bus_absent_by_design(
    dbus_session_bus_address: Option<&str>,
    xdg_runtime_dir: Option<&Path>,
) -> bool {
    let no_address = dbus_session_bus_address.is_none_or(|a| a.trim().is_empty());
    let no_socket = xdg_runtime_dir.is_none_or(|d| !d.join("bus").exists());
    no_address && no_socket
}

/// Which kinds a Linux host scan saw completely (amendment A2). Both watched
/// patterns are scanned on BOTH buses, so either kind is complete only when the
/// system bus answered fully AND the session bus answered fully or is absent by
/// design. PURE.
pub(crate) fn linux_complete_kinds(system_ok: bool, session: SessionBus) -> Vec<&'static str> {
    if system_ok && session != SessionBus::Failed {
        vec![KIND_GH_ACTIONS_RUNNER, KIND_QONTINUI_RUNNER]
    } else {
        Vec::new()
    }
}

/// Whether a `ListUnitFilesByPatterns` error makes the bus scan partial. It
/// is the ONLY source of installed-but-unloaded units, so any failure does —
/// except `UnknownMethod`, an older systemd (< 230) that has no such method,
/// where `ListUnitsByPatterns` is all the bus can tell. PURE.
pub(crate) fn unit_files_error_is_partial(dbus_error_name: Option<&str>) -> bool {
    dbus_error_name != Some("org.freedesktop.DBus.Error.UnknownMethod")
}

// ---------------------------------------------------------------------------
// Linux: systemd over D-Bus
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::time::Duration;

    use tracing::debug;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    use super::{linux_complete_kinds, ServiceScan, SessionBus, UnitProps, WATCHED_PATTERNS};

    const DEST: &str = "org.freedesktop.systemd1";
    const MGR_PATH: &str = "/org/freedesktop/systemd1";
    const MGR_IFACE: &str = "org.freedesktop.systemd1.Manager";
    const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
    /// Bound on one bus's whole scan. A wedged systemd must cost this tick's
    /// service rows, never the sampler loop.
    const BUS_TIMEOUT: Duration = Duration::from_secs(5);

    /// The two buses, connected lazily and kept across ticks.
    #[derive(Default)]
    pub(crate) struct Watcher {
        system: Option<zbus::Connection>,
        session: Option<zbus::Connection>,
    }

    type ListedUnit = (
        String,
        String,
        String,
        String,
        String,
        String,
        OwnedObjectPath,
        u32,
        String,
        OwnedObjectPath,
    );

    impl Watcher {
        pub(crate) async fn scan(&mut self) -> ServiceScan {
            let mut by_unit: BTreeMap<String, UnitProps> = BTreeMap::new();
            let mut system_ok = false;
            let mut session = SessionBus::Failed;
            let mut any_answered = false;

            // System bus: GitHub runner services and a system-level runner.
            if self.system.is_none() {
                self.system = tokio::time::timeout(BUS_TIMEOUT, zbus::Connection::system())
                    .await
                    .ok()
                    .and_then(Result::ok);
            }
            if let Some(conn) = self.system.as_ref() {
                match tokio::time::timeout(BUS_TIMEOUT, scan_bus(conn)).await {
                    Ok(Ok((units, whole))) => {
                        any_answered = true;
                        system_ok = whole;
                        for u in units {
                            by_unit.entry(u.unit.clone()).or_insert(u);
                        }
                    }
                    other => {
                        debug!("fleet::computer: system-bus scan failed: {other:?}");
                        self.system = None;
                    }
                }
            }

            // Session bus: the runner's own user units.
            if self.session.is_none() {
                self.session = tokio::time::timeout(BUS_TIMEOUT, zbus::Connection::session())
                    .await
                    .ok()
                    .and_then(Result::ok);
            }
            match self.session.as_ref() {
                Some(conn) => match tokio::time::timeout(BUS_TIMEOUT, scan_bus(conn)).await {
                    Ok(Ok((units, whole))) => {
                        any_answered = true;
                        session = if whole {
                            SessionBus::Ok
                        } else {
                            SessionBus::Failed
                        };
                        for u in units {
                            by_unit.entry(u.unit.clone()).or_insert(u);
                        }
                    }
                    other => {
                        debug!("fleet::computer: session-bus scan failed: {other:?}");
                        self.session = None;
                        session = SessionBus::Failed;
                    }
                },
                None => {
                    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
                    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
                    if super::session_bus_absent_by_design(addr.as_deref(), xdg.as_deref()) {
                        session = SessionBus::AbsentByDesign;
                    }
                }
            }

            if !any_answered {
                return ServiceScan::default();
            }
            let units = by_unit
                .values()
                .map(|p| super::watched_from_props(p, true))
                .collect();
            ServiceScan {
                units: Some(units),
                complete_kinds: linux_complete_kinds(system_ok, session),
            }
        }
    }

    async fn call<B, R>(
        conn: &zbus::Connection,
        path: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let reply = conn
            .call_method(Some(DEST), path, Some(iface), method, body)
            .await?;
        reply.body().deserialize::<R>()
    }

    /// Every watched unit systemd knows on this bus — loaded units AND
    /// installed-but-unloaded unit files (a stopped runner whose unit was
    /// garbage-collected is exactly the state worth reporting).
    ///
    /// Returns the units read plus whether EVERY unit was read: a per-unit
    /// `LoadUnit`/`GetAll` failure skips that unit and marks the scan partial
    /// (sent as a delta, deleting nothing) rather than voiding the whole bus.
    ///
    /// `LoadUnit` is not a pure read: for an installed-but-unloaded unit file
    /// it makes systemd load the unit into memory (the unit is not started;
    /// systemd's own GC unloads it again when nothing references it). It is
    /// the only unprivileged way to read an unloaded unit's properties, which
    /// is exactly the state a dead runner service tends to be in.
    async fn scan_bus(conn: &zbus::Connection) -> zbus::Result<(Vec<UnitProps>, bool)> {
        let no_states: Vec<&str> = Vec::new();
        let listed: Vec<ListedUnit> = call(
            conn,
            MGR_PATH,
            MGR_IFACE,
            "ListUnitsByPatterns",
            &(no_states.clone(), WATCHED_PATTERNS),
        )
        .await?;
        let mut names: BTreeSet<String> = listed
            .into_iter()
            .filter(|u| u.2 != "not-found")
            .map(|u| u.0)
            .collect();
        let mut whole = true;
        let files = match call::<_, Vec<(String, String)>>(
            conn,
            MGR_PATH,
            MGR_IFACE,
            "ListUnitFilesByPatterns",
            &(no_states, WATCHED_PATTERNS),
        )
        .await
        {
            Ok(f) => f,
            Err(e) => {
                let name = match &e {
                    zbus::Error::MethodError(n, _, _) => Some(n.as_str().to_string()),
                    zbus::Error::FDO(f) => match **f {
                        zbus::fdo::Error::UnknownMethod(_) => {
                            Some("org.freedesktop.DBus.Error.UnknownMethod".to_string())
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if super::unit_files_error_is_partial(name.as_deref()) {
                    debug!("fleet::computer: ListUnitFilesByPatterns failed: {e}");
                    whole = false;
                }
                Vec::new()
            }
        };
        {
            for (path, _state) in files {
                if let Some(name) = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                {
                    // `foo@.service` is a template, not an instance.
                    if !name.contains("@.") {
                        names.insert(name.to_string());
                    }
                }
            }
        }

        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let path: OwnedObjectPath =
                match call(conn, MGR_PATH, MGR_IFACE, "LoadUnit", &(name.as_str(),)).await {
                    Ok(p) => p,
                    Err(e) => {
                        debug!("fleet::computer: LoadUnit {name} failed: {e}");
                        whole = false;
                        continue;
                    }
                };
            let unit: HashMap<String, OwnedValue> = match call(
                conn,
                path.as_str(),
                PROPS_IFACE,
                "GetAll",
                &("org.freedesktop.systemd1.Unit",),
            )
            .await
            {
                Ok(u) => u,
                Err(e) => {
                    debug!("fleet::computer: GetAll(Unit) {name} failed: {e}");
                    whole = false;
                    continue;
                }
            };
            if str_prop(&unit, "LoadState").as_deref() == Some("not-found") {
                continue;
            }
            let svc: HashMap<String, OwnedValue> = match call(
                conn,
                path.as_str(),
                PROPS_IFACE,
                "GetAll",
                &("org.freedesktop.systemd1.Service",),
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    debug!("fleet::computer: GetAll(Service) {name} failed: {e}");
                    whole = false;
                    continue;
                }
            };
            out.push(props_from_maps(name, &unit, &svc));
        }
        Ok((out, whole))
    }

    fn str_prop(m: &HashMap<String, OwnedValue>, k: &str) -> Option<String> {
        match &**m.get(k)? {
            Value::Str(s) => {
                let s = s.as_str().trim();
                (!s.is_empty()).then(|| s.to_string())
            }
            _ => None,
        }
    }

    fn u64_prop(m: &HashMap<String, OwnedValue>, k: &str) -> Option<u64> {
        match &**m.get(k)? {
            Value::U64(n) => Some(*n),
            _ => None,
        }
    }

    fn u32_prop(m: &HashMap<String, OwnedValue>, k: &str) -> Option<u32> {
        match &**m.get(k)? {
            Value::U32(n) => Some(*n),
            _ => None,
        }
    }

    fn i32_prop(m: &HashMap<String, OwnedValue>, k: &str) -> Option<i32> {
        match &**m.get(k)? {
            Value::I32(n) => Some(*n),
            _ => None,
        }
    }

    /// `ExecStart` is `a(sasbttttuii)`; the first struct's first field is the
    /// binary path.
    fn exec_start_path(m: &HashMap<String, OwnedValue>) -> Option<String> {
        let Value::Array(a) = &**m.get("ExecStart")? else {
            return None;
        };
        let Value::Structure(s) = a.inner().first()? else {
            return None;
        };
        match s.fields().first()? {
            Value::Str(p) => Some(p.as_str().to_string()).filter(|p| !p.is_empty()),
            _ => None,
        }
    }

    /// D-Bus sentinels: `u64::MAX` is systemd's "infinity" / "not set".
    fn limit(v: Option<u64>) -> Option<u64> {
        v.filter(|n| *n != u64::MAX)
    }

    fn props_from_maps(
        unit: String,
        u: &HashMap<String, OwnedValue>,
        s: &HashMap<String, OwnedValue>,
    ) -> UnitProps {
        let state_changed_at = u64_prop(u, "StateChangeTimestamp")
            .filter(|us| *us > 0)
            .and_then(|us| i64::try_from(us).ok())
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_micros);
        UnitProps {
            unit,
            active_state: str_prop(u, "ActiveState"),
            sub_state: str_prop(u, "SubState"),
            result: str_prop(s, "Result"),
            restart: str_prop(s, "Restart"),
            oom_policy: str_prop(s, "OOMPolicy"),
            memory_max: limit(u64_prop(s, "MemoryMax")),
            memory_peak: limit(u64_prop(s, "MemoryPeak")),
            n_restarts: u32_prop(s, "NRestarts"),
            state_changed_at,
            exec_main_status: i32_prop(s, "ExecMainStatus"),
            working_directory: str_prop(s, "WorkingDirectory"),
            exec_start_path: exec_start_path(s),
            control_group: str_prop(s, "ControlGroup"),
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: `sc query` / `sc qc`
// ---------------------------------------------------------------------------

/// One service block from `sc query`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScService {
    pub(crate) name: String,
    /// The numeric `STATE` code (`4  RUNNING` → 4). Numeric, not the word:
    /// the code is what the SCM defines; the word is presentation.
    pub(crate) state_code: Option<u32>,
    pub(crate) win32_exit_code: Option<u32>,
}

/// First integer in a `sc` value (`"4  RUNNING"` → 4, `"1067  (0x42b)"` → 1067).
fn sc_leading_u32(v: &str) -> Option<u32> {
    v.split_whitespace().next()?.parse().ok()
}

/// Parse `sc query` output into service blocks. Field labels (`SERVICE_NAME`,
/// `STATE`, `WIN32_EXIT_CODE`) are not localized by `sc.exe`.
pub(crate) fn parse_sc_query(text: &str) -> Vec<ScService> {
    let mut out: Vec<ScService> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "SERVICE_NAME" => out.push(ScService {
                name: v.trim().to_string(),
                state_code: None,
                win32_exit_code: None,
            }),
            "STATE" => {
                if let Some(s) = out.last_mut() {
                    s.state_code = sc_leading_u32(v);
                }
            }
            "WIN32_EXIT_CODE" => {
                if let Some(s) = out.last_mut() {
                    s.win32_exit_code = sc_leading_u32(v);
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether an `sc query` listing was cut short. `sc` enumerates into a fixed
/// buffer (4 KiB by default, `bufsize=`) and, when more services exist than
/// fit, prints only the first page plus `Enum: more data, need N bytes start
/// resume at index M` (or `More data is available.`). A box with many services
/// then simply lacks the later ones — so a truncated listing must never be
/// sent as a complete snapshot (it would DELETE the rows it missed).
pub(crate) fn sc_query_truncated(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("more data") || t.contains("resume at index")
}

/// Whether an `sc query` listing may be sent as a COMPLETE snapshot: the
/// command exited 0 AND shows no truncation notice. The exit code is the
/// locale-independent signal — a short buffer ends `sc` with
/// `ERROR_MORE_DATA` (234) — and the notice text is a second check, since it
/// may be localized. Any other non-zero exit is equally "not the whole list".
pub(crate) fn sc_listing_complete(exit_code: Option<i32>, text: &str) -> bool {
    exit_code == Some(0) && !sc_query_truncated(text)
}

/// `BINARY_PATH_NAME` from `sc qc <name>`, quotes stripped.
pub(crate) fn parse_sc_qc_binary_path(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "BINARY_PATH_NAME").then(|| {
                let v = v.trim();
                // `"C:\actions-runner\bin\RunnerService.exe"` — quoted when the
                // path has spaces; an unquoted path may carry trailing args.
                match v.strip_prefix('"') {
                    Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
                    None => v.split_whitespace().next().unwrap_or("").to_string(),
                }
            })
        })
        .filter(|p| !p.is_empty())
}

/// `C:\actions-runner\bin\RunnerService.exe` → `C:\actions-runner` (the dir
/// holding `.runner`). Pure string work so it is testable off Windows.
pub(crate) fn windows_runner_dir(binary_path: &str) -> Option<String> {
    let bin_dir = binary_path.rsplit_once('\\')?.0;
    let root = bin_dir.rsplit_once('\\')?;
    root.1
        .eq_ignore_ascii_case("bin")
        .then(|| root.0.to_string())
        .filter(|r| !r.is_empty())
}

/// Map an SCM state to the systemd vocabulary coord stores.
///
/// `STOPPED` with a non-zero `WIN32_EXIT_CODE` is `failed` (the service died
/// with an error); `1077` (`ERROR_SERVICE_NEVER_STARTED`) is a service that
/// simply has not run, i.e. `inactive`.
pub(crate) fn props_from_sc(s: &ScService) -> UnitProps {
    let (active, sub) = match s.state_code {
        Some(1) => {
            let failed = matches!(s.win32_exit_code, Some(c) if c != 0 && c != 1077);
            (if failed { "failed" } else { "inactive" }, "stopped")
        }
        Some(2) => ("activating", "start_pending"),
        Some(3) => ("deactivating", "stop_pending"),
        Some(4) => ("active", "running"),
        Some(5) => ("reloading", "continue_pending"),
        Some(6) => ("reloading", "pause_pending"),
        Some(7) => ("active", "paused"),
        _ => ("", ""),
    };
    let result = match (s.state_code, s.win32_exit_code) {
        (Some(1), Some(0 | 1077)) => Some("success".to_string()),
        (Some(1), Some(_)) => Some("exit-code".to_string()),
        _ => None,
    };
    UnitProps {
        unit: s.name.clone(),
        active_state: non_empty(active),
        sub_state: non_empty(sub),
        result,
        ..UnitProps::default()
    }
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::time::Duration;

    use super::{
        parse_sc_qc_binary_path, parse_sc_query, props_from_sc, read_runner_file, row_from_props,
        sc_listing_complete, windows_runner_dir, ServiceScan, WatchedUnit, KIND_GH_ACTIONS_RUNNER,
    };

    const SC_TIMEOUT: Duration = Duration::from_secs(5);

    /// `(exit code, stdout)`; `None` only when `sc` could not be run or timed
    /// out. A non-zero exit still returns its output — a truncated listing
    /// exits 234 with the first page on stdout.
    async fn sc(args: &[&str]) -> Option<(Option<i32>, String)> {
        let mut cmd = crate::process_helpers::tokio_no_window("sc.exe");
        cmd.args(args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let out = tokio::time::timeout(SC_TIMEOUT, cmd.output())
            .await
            .ok()?
            .ok()?;
        Some((
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }

    /// Every `actions.runner.*` Windows service. `units: None` when `sc query`
    /// itself did not answer.
    pub(crate) async fn scan() -> ServiceScan {
        // A large buffer so a normal box lists every service in one page; the
        // truncation check below covers the box that still overflows it.
        let Some((code, listing)) = sc(&[
            "query", "type=", "service", "state=", "all", "bufsize=", "262144",
        ])
        .await
        else {
            return ServiceScan::default();
        };
        let complete = sc_listing_complete(code, &listing);
        if !complete {
            static LOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::info!(
                    "fleet::computer: `sc query` exited {code:?} or was truncated — the Windows \
                     service list is sent as a partial (delta) scan"
                );
            }
        }
        let mut units = Vec::new();
        for s in parse_sc_query(&listing)
            .into_iter()
            .filter(|s| s.name.starts_with("actions.runner."))
        {
            let props = props_from_sc(&s);
            let runner = match sc(&["qc", s.name.as_str()]).await {
                Some((Some(0), qc)) => parse_sc_qc_binary_path(&qc)
                    .and_then(|p| windows_runner_dir(&p))
                    .and_then(|d| read_runner_file(std::path::Path::new(&d))),
                _ => None,
            };
            units.push(WatchedUnit {
                row: row_from_props(&props, runner),
                cgroup_oom_kill: None,
            });
        }
        ServiceScan {
            units: Some(units),
            complete_kinds: if complete {
                vec![KIND_GH_ACTIONS_RUNNER]
            } else {
                Vec::new()
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The 2026-09-30 incident unit's SHAPE (synthetic unit name, paths and
    /// numbers), as `TZ=UTC systemctl show -p …` prints a unit after the
    /// kernel OOM killer took its cgroup: `Result=oom-kill`
    /// with `ExecMainStatus=0` (the main process was SIGKILLed, it did not
    /// exit non-zero), `OOMPolicy=stop`, `Restart=no` — so it stayed dead.
    pub(crate) const INCIDENT_SHOW: &str = "\
Id=actions.runner.example-org-example-repo.fleetbox.service
Restart=no
Result=oom-kill
NRestarts=0
OOMPolicy=stop
ExecMainStatus=0
ExecStart={ path=/opt/actions-runner-example-repo/runsvc.sh ; argv[]=/opt/actions-runner-example-repo/runsvc.sh ; ignore_errors=no ; start_time=[Tue 2026-09-29 09:12:40 UTC] ; stop_time=[Wed 2026-09-30 01:48:16 UTC] ; pid=4242 ; code=killed ; status=9/KILL }
ControlGroup=
MemoryPeak=21474836480
MemoryMax=infinity
WorkingDirectory=/opt/actions-runner-example-repo
LoadState=loaded
ActiveState=failed
SubState=failed
StateChangeTimestamp=Wed 2026-09-30 01:48:16 UTC
";

    /// A healthy unit in the post-fix shape
    /// (`OOMPolicy=continue`, `Restart=always`, a 16 GiB `MemoryMax`), plus a
    /// second block, to pin the multi-unit split.
    pub(crate) const HEALTHY_SHOW: &str = "\
Restart=always
Result=success
NRestarts=0
OOMPolicy=continue
ExecMainStatus=0
ExecStart={ path=/opt/actions-runner-example-repo/runsvc.sh ; argv[]=/opt/actions-runner-example-repo/runsvc.sh ; ignore_errors=no ; start_time=[Wed 2026-09-30 18:28:57 UTC] ; stop_time=[n/a] ; pid=4343 ; code=(null) ; status=0/0 }
ControlGroup=/system.slice/actions.runner.example-org-example-repo.fleetbox.service
MemoryPeak=12884901888
MemoryMax=17179869184
WorkingDirectory=/opt/actions-runner-example-repo
Id=actions.runner.example-org-example-repo.fleetbox.service
ActiveState=active
SubState=running
StateChangeTimestamp=Wed 2026-09-30 18:28:57 UTC

Id=qontinui-runner.service
ActiveState=active
SubState=running
Result=success
Restart=on-failure
OOMPolicy=stop
MemoryMax=infinity
MemoryPeak=[not set]
NRestarts=2
StateChangeTimestamp=@1790000000
ControlGroup=/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service

Id=actions.runner.gone.service
LoadState=not-found
ActiveState=inactive
";

    #[test]
    fn the_incident_unit_parses_to_failed_oom_kill() {
        let units = parse_systemctl_show(INCIDENT_SHOW);
        assert_eq!(units.len(), 1);
        let u = &units[0];
        assert_eq!(
            u.unit,
            "actions.runner.example-org-example-repo.fleetbox.service"
        );
        assert_eq!(u.active_state.as_deref(), Some("failed"));
        assert_eq!(u.sub_state.as_deref(), Some("failed"));
        assert_eq!(u.result.as_deref(), Some("oom-kill"));
        assert_eq!(u.exec_main_status, Some(0));
        assert_eq!(u.oom_policy.as_deref(), Some("stop"));
        assert_eq!(u.restart.as_deref(), Some("no"));
        assert_eq!(u.memory_max, None, "infinity is unbounded, not a number");
        assert_eq!(u.memory_peak, Some(21_474_836_480));
        assert_eq!(
            u.control_group, None,
            "the cgroup is gone once the unit died"
        );

        let row = row_from_props(u, None);
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "unit": "actions.runner.example-org-example-repo.fleetbox.service",
                "kind": "gh_actions_runner",
                "active_state": "failed",
                "sub_state": "failed",
                "result": "oom-kill",
                "restart_policy": "no",
                "oom_policy": "stop",
                "memory_max": null,
                "memory_peak": 21474836480_u64,
                "n_restarts": 0,
                "state_changed_at": "2026-09-30T01:48:16Z",
                "runner_name": "fleetbox",
                "repo": null
            })
        );
    }

    #[test]
    fn multi_unit_show_output_splits_and_drops_not_found() {
        let units = parse_systemctl_show(HEALTHY_SHOW);
        let names: Vec<&str> = units.iter().map(|u| u.unit.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "actions.runner.example-org-example-repo.fleetbox.service",
                "qontinui-runner.service"
            ]
        );
        assert_eq!(units[0].memory_max, Some(17_179_869_184));
        assert_eq!(
            units[0].control_group.as_deref(),
            Some("/system.slice/actions.runner.example-org-example-repo.fleetbox.service")
        );
        assert_eq!(units[1].memory_peak, None, "[not set] is unknown");
        assert_eq!(units[1].n_restarts, Some(2));
        assert_eq!(
            units[1].state_changed_at.map(|t| t.timestamp()),
            Some(1_790_000_000)
        );
        assert_eq!(classify_unit(&units[1].unit), "qontinui_runner");
    }

    #[test]
    fn only_utc_timestamps_are_trusted() {
        assert!(parse_show_timestamp("Wed 2026-09-30 01:48:16 UTC").is_some());
        assert_eq!(parse_show_timestamp("Wed 2026-09-30 03:48:16 CEST"), None);
        assert_eq!(parse_show_timestamp(""), None);
        assert_eq!(parse_show_timestamp("n/a"), None);
    }

    #[test]
    fn unit_kinds_classify() {
        assert_eq!(
            classify_unit("actions.runner.example-org-third-repo.fleetbox.service"),
            "gh_actions_runner"
        );
        assert_eq!(
            classify_unit("actions.runner.qontinui.winbox"),
            "gh_actions_runner"
        );
        assert_eq!(
            classify_unit("qontinui-runner-test.service"),
            "qontinui_runner"
        );
        assert_eq!(classify_unit("sshd.service"), "other_watched");
    }

    /// Synthetic, in the shape `config.sh` writes: UTF-8 with a BOM.
    const RUNNER_FILE: &str = "\u{feff}{\n  \"agentId\": 22,\n  \"agentName\": \"fleetbox\",\n  \"poolId\": 1,\n  \"poolName\": \"Default\",\n  \"serverUrl\": \"https://pipelines.actions.example.invalid/\",\n  \"gitHubUrl\": \"https://github.com/example-org/example-repo\",\n  \"workFolder\": \"_work\"\n}";

    #[test]
    fn runner_file_yields_name_and_owner_repo() {
        assert_eq!(
            parse_runner_file(RUNNER_FILE),
            Some((
                Some("fleetbox".to_string()),
                Some("example-org/example-repo".to_string())
            ))
        );
        // Org-level runner: no single repo.
        assert_eq!(
            parse_runner_file(r#"{"agentName":"x","gitHubUrl":"https://github.com/example-org"}"#),
            Some((Some("x".to_string()), None))
        );
        assert_eq!(parse_runner_file("not json"), None);
    }

    #[test]
    fn runner_name_falls_back_to_the_unit_name_but_never_guesses_a_repo() {
        assert_eq!(
            runner_name_from_unit("actions.runner.example-org-example-repo.fleetbox.service")
                .as_deref(),
            Some("fleetbox")
        );
        assert_eq!(
            runner_name_from_unit("actions.runner.example-org-example-repo.winbox").as_deref(),
            Some("winbox")
        );
        assert_eq!(runner_name_from_unit("qontinui-runner.service"), None);
    }

    #[test]
    fn runner_dir_prefers_working_directory_then_exec_start() {
        let mut p = UnitProps {
            working_directory: Some("-/opt/runner/ar".into()),
            exec_start_path: Some("/opt/x/runsvc.sh".into()),
            ..UnitProps::default()
        };
        assert_eq!(runner_dir_of(&p), Some(PathBuf::from("/opt/runner/ar")));
        p.working_directory = Some("~".into());
        assert_eq!(runner_dir_of(&p), Some(PathBuf::from("/opt/x")));
    }

    /// Synthetic, in the shape of `sc query type= service state= all` (two runner
    /// services and one unrelated service).
    pub(crate) const SC_QUERY: &str = "\r
SERVICE_NAME: actions.runner.example-org-example-repo.winbox\r
DISPLAY_NAME: GitHub Actions Runner (example-org-example-repo.winbox)\r
        TYPE               : 10  WIN32_OWN_PROCESS  \r
        STATE              : 4  RUNNING \r
                                (STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\r
        WIN32_EXIT_CODE    : 0  (0x0)\r
        SERVICE_EXIT_CODE  : 0  (0x0)\r
        CHECKPOINT         : 0x0\r
        WAIT_HINT          : 0x0\r
\r
SERVICE_NAME: actions.runner.example-org-other-repo.winbox\r
DISPLAY_NAME: GitHub Actions Runner (example-org-other-repo.winbox)\r
        TYPE               : 10  WIN32_OWN_PROCESS  \r
        STATE              : 1  STOPPED \r
        WIN32_EXIT_CODE    : 1067  (0x42b)\r
        SERVICE_EXIT_CODE  : 0  (0x0)\r
        CHECKPOINT         : 0x0\r
        WAIT_HINT          : 0x0\r
\r
SERVICE_NAME: Spooler\r
DISPLAY_NAME: Print Spooler\r
        TYPE               : 110  WIN32_OWN_PROCESS  (interactive)\r
        STATE              : 1  STOPPED \r
        WIN32_EXIT_CODE    : 1077  (0x435)\r
";

    #[test]
    fn sc_query_parses_and_maps_to_the_systemd_vocabulary() {
        let svcs = parse_sc_query(SC_QUERY);
        assert_eq!(svcs.len(), 3);
        assert_eq!(
            svcs[0].name,
            "actions.runner.example-org-example-repo.winbox"
        );
        assert_eq!(svcs[0].state_code, Some(4));
        assert_eq!(svcs[1].win32_exit_code, Some(1067));

        let running = row_from_props(&props_from_sc(&svcs[0]), None);
        assert_eq!(running.active_state.as_deref(), Some("active"));
        assert_eq!(running.sub_state.as_deref(), Some("running"));
        assert_eq!(running.kind, "gh_actions_runner");
        assert_eq!(running.runner_name.as_deref(), Some("winbox"));

        let died = props_from_sc(&svcs[1]);
        assert_eq!(died.active_state.as_deref(), Some("failed"));
        assert_eq!(died.result.as_deref(), Some("exit-code"));

        let never = props_from_sc(&svcs[2]);
        assert_eq!(never.active_state.as_deref(), Some("inactive"));
        assert_eq!(never.result.as_deref(), Some("success"));
    }

    #[test]
    fn linux_kinds_are_complete_only_when_every_hosting_bus_answered() {
        assert_eq!(
            linux_complete_kinds(true, SessionBus::Ok),
            vec!["gh_actions_runner", "qontinui_runner"]
        );
        assert_eq!(
            linux_complete_kinds(true, SessionBus::AbsentByDesign),
            vec!["gh_actions_runner", "qontinui_runner"]
        );
        // A user bus that failed: both patterns are scanned there too, so
        // neither kind is known complete.
        assert!(linux_complete_kinds(true, SessionBus::Failed).is_empty());
        // No system bus: neither kind is known complete.
        assert!(linux_complete_kinds(false, SessionBus::Ok).is_empty());
    }

    #[test]
    fn a_session_bus_is_absent_by_design_only_when_nothing_advertises_one() {
        let dir = std::env::temp_dir().join(format!("qontinui-session-bus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Nothing advertised: absent by design.
        assert!(session_bus_absent_by_design(None, None));
        assert!(session_bus_absent_by_design(Some(" "), Some(&dir)));
        // An address is set: a bus that should have answered.
        assert!(!session_bus_absent_by_design(
            Some("unix:path=/run/user/1000/bus"),
            None
        ));
        // A socket exists in XDG_RUNTIME_DIR: likewise.
        std::fs::write(dir.join("bus"), b"").unwrap();
        assert!(!session_bus_absent_by_design(None, Some(&dir)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_unit_file_listing_makes_the_scan_partial_except_on_old_systemd() {
        assert!(unit_files_error_is_partial(None));
        assert!(unit_files_error_is_partial(Some(
            "org.freedesktop.DBus.Error.AccessDenied"
        )));
        assert!(unit_files_error_is_partial(Some(
            "org.freedesktop.DBus.Error.Timeout"
        )));
        assert!(!unit_files_error_is_partial(Some(
            "org.freedesktop.DBus.Error.UnknownMethod"
        )));
    }

    #[test]
    fn only_a_clean_exit_without_a_notice_is_a_complete_sc_listing() {
        assert!(sc_listing_complete(Some(0), SC_QUERY));
        // ERROR_MORE_DATA with the first page and NO (or a localized) notice.
        assert!(!sc_listing_complete(Some(234), SC_QUERY));
        assert!(!sc_listing_complete(Some(5), ""));
        assert!(!sc_listing_complete(None, SC_QUERY));
        let cut =
            format!("{SC_QUERY}Enum: more data, need 5156 bytes start resume at index 79\r\n");
        assert!(!sc_listing_complete(Some(0), &cut));
    }

    #[test]
    fn a_truncated_sc_listing_is_detected() {
        assert!(!sc_query_truncated(SC_QUERY));
        let cut =
            format!("{SC_QUERY}\r\nEnum: more data, need 5156 bytes start resume at index 79\r\n");
        assert!(sc_query_truncated(&cut));
        assert!(sc_query_truncated("[SC] EnumQueryServicesStatus: OpenService FAILED 234:\r\n\r\nMore data is available.\r\n"));
    }

    #[test]
    fn sc_qc_binary_path_resolves_the_runner_dir() {
        let qc = "[SC] QueryServiceConfig SUCCESS\r\n\r\nSERVICE_NAME: actions.runner.example.winbox\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        START_TYPE         : 2   AUTO_START\r\n        BINARY_PATH_NAME   : \"C:\\actions-runner\\bin\\RunnerService.exe\"\r\n        DISPLAY_NAME       : GitHub Actions Runner\r\n";
        let p = parse_sc_qc_binary_path(qc).unwrap();
        assert_eq!(p, r"C:\actions-runner\bin\RunnerService.exe");
        assert_eq!(
            windows_runner_dir(&p).as_deref(),
            Some(r"C:\actions-runner")
        );
        assert_eq!(windows_runner_dir(r"C:\x\RunnerService.exe"), None);
    }
}
