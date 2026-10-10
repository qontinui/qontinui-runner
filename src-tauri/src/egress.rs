//! The six outbound data flows and the per-tenant switch that governs each one,
//! enforced at the SOURCE — before any byte leaves this machine.
//!
//! Plan `2026-10-10-spec-front-end-phase-9-generic-boundary`, Phase 7 (design
//! decisions C4–C7).
//!
//! ## The flows
//!
//! | [`Flow`] | coord domain | what leaves, and where to |
//! |---|---|---|
//! | `TranscriptSync` | `egress_transcript_sync` | AI session transcripts, tenant memory records and memory queries, to coord / qontinui-web |
//! | `CodeMirror` | `egress_code_mirror` | agent branches, `git push`ed to coord's git origin |
//! | `TerminalStream` | `egress_terminal_stream` | raw terminal (PTY) output, to coord and through the web relay |
//! | `Telemetry` | `egress_telemetry` | crash reports (Sentry), OTLP spans, and the relay's `ui-error` / `recent-crash` forwards |
//! | `UpdateCheck` | `egress_update_check` | the updater's request for the latest release manifest |
//! | `SkillMirror` | `egress_skill_mirror` | the `git fetch` of the canonical skill/command corpus |
//!
//! Each flow is one coord fleet-policy domain with levels `on` | `off`, tenant
//! band, ON by default (C4). The domain list is VENDORED here as
//! [`EGRESS_DOMAINS`], the way `fleet_policy_poller` vendors coord's other
//! domain strings, and a drift test compares it against coord's own
//! `EGRESS_DOMAINS` when the sibling checkout is present.
//!
//! ## Resolution (C6)
//!
//! [`permit`] answers, for one flow, from the first rung that has a value:
//!
//! 1. [`LevelSource::Coord`] — the last AUTHORITATIVE answer coord gave THIS
//!    process (written by `mcp::fleet_policy_poller`);
//! 2. [`LevelSource::Persisted`] — the answer a previous process received,
//!    restored from `<config_dir>/egress-levels.json`, so the boot-time flows
//!    (crash reporting, OTLP, the startup update check) obey a tenant `off`
//!    before the first poll;
//! 3. [`LevelSource::Profile`] — the machine profile's `egress.default`
//!    (`~/.qontinui/profiles.json`), the self-hosted deployment's own switch
//!    (C7);
//! 4. [`LevelSource::ProductDefault`] — `on`.
//!
//! Every verdict carries the rung that produced it, so a fallback never poses
//! as a tenant decision ([policy: unknown-must-not-render-as-a-default]).
//!
//! ## What counts as an authoritative coord answer
//!
//! A coord deployment that predates the egress family answers an unknown
//! domain with its generic no-row default — `off`, `resolved_scope: "none"`,
//! and no `default_source`. Treating that as a tenant decision would switch
//! every flow off fleet-wide the day this runner shipped ahead of coord. So a
//! no-row answer counts only when coord also says WHICH default it applied
//! (`default_source: "deployment_profile" | "product"`); an explicit row
//! (`resolved_scope` other than `none`) always counts. See
//! [`classify_coord_answer`].
//!
//! ## Where each switch is enforced
//!
//! The check sits at the point where the flow's bytes would leave — never on
//! ingest. The callers, one per flow:
//!
//! - transcript sync: [`transcript_sync_permitted`], replacing every egress-path
//!   read of `settings::get_cloud_sync_enabled()`, plus the outbox drain
//!   (`session::coord_sync::push_record`) as the last line;
//! - terminal stream: `session::output_pipe::flush`, the drain, the relay's
//!   `terminal_*` handlers and its `terminal-output` forward,
//!   `mcp::remote_terminal::gate_remote_frame` and `RemoteAttachClient`;
//! - code mirror: `agent_pusher::push_one`;
//! - telemetry: the `sentry::init` block in `main`, `otel::init_otel`, and the
//!   relay's `ui-error` / `recent-crash` forwards;
//! - update check: `check_for_updates` / `install_update`;
//! - skill mirror: `canonical_corpus::refresh_into`.
//!
//! `GET /health` carries `egress` ([`health_json`]): every flow's verdict, its
//! source, and how many sends the switch refused in this process.
//!
//! The switch is a preference, never the safeguard (D3): authorisation stays
//! where it already is.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

/// The six coord fleet-policy domains of the egress family, VENDORED from
/// `qontinui-coord/crates/coord/src/fleet_policy.rs` `EGRESS_DOMAINS`.
///
/// Order is [`Flow::ALL`]'s order. `vendored_domains_match_coord` reads coord's
/// copy through the sibling checkout and fails on any difference in members.
pub(crate) const EGRESS_DOMAINS: [&str; 6] = [
    "egress_transcript_sync",
    "egress_code_mirror",
    "egress_terminal_stream",
    "egress_telemetry",
    "egress_update_check",
    "egress_skill_mirror",
];

/// File name of the persisted-answer store, under the per-instance config dir.
const STORE_FILE: &str = "egress-levels.json";

/// Store schema version. A file carrying another value is treated as absent.
const STORE_SCHEMA: u32 = 1;

/// The `resolved_scope` coord answers when no policy row exists.
const RESOLVED_SCOPE_NONE: &str = "none";

/// One outbound data flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Flow {
    TranscriptSync,
    CodeMirror,
    TerminalStream,
    Telemetry,
    UpdateCheck,
    SkillMirror,
}

impl Flow {
    /// Every flow, in [`EGRESS_DOMAINS`] order.
    pub(crate) const ALL: [Flow; 6] = [
        Flow::TranscriptSync,
        Flow::CodeMirror,
        Flow::TerminalStream,
        Flow::Telemetry,
        Flow::UpdateCheck,
        Flow::SkillMirror,
    ];

    /// Position in [`Flow::ALL`] / [`EGRESS_DOMAINS`].
    const fn index(self) -> usize {
        match self {
            Flow::TranscriptSync => 0,
            Flow::CodeMirror => 1,
            Flow::TerminalStream => 2,
            Flow::Telemetry => 3,
            Flow::UpdateCheck => 4,
            Flow::SkillMirror => 5,
        }
    }

    /// The coord fleet-policy domain carrying this flow's switch.
    pub(crate) const fn domain(self) -> &'static str {
        EGRESS_DOMAINS[self.index()]
    }

    /// The flow's key in `/health` `egress` and in refusal frames
    /// (`transcript_sync`, `code_mirror`, …): the domain without its prefix.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Flow::TranscriptSync => "transcript_sync",
            Flow::CodeMirror => "code_mirror",
            Flow::TerminalStream => "terminal_stream",
            Flow::Telemetry => "telemetry",
            Flow::UpdateCheck => "update_check",
            Flow::SkillMirror => "skill_mirror",
        }
    }

    /// Whether a flip of this flow takes effect only at the runner's NEXT start.
    ///
    /// Only telemetry: crash reporting and the OTLP exporter are installed once
    /// at boot. The relay's `ui-error` / `recent-crash` forwards — also
    /// telemetry — re-check live, but the flow as a whole is honest only as
    /// "applies at next start". Every other flow re-checks on each send.
    pub(crate) const fn applies_at_next_start(self) -> bool {
        matches!(self, Flow::Telemetry)
    }

    /// The flow whose domain is `domain`, if any.
    pub(crate) fn from_domain(domain: &str) -> Option<Flow> {
        Flow::ALL.into_iter().find(|f| f.domain() == domain)
    }
}

/// A flow's level: the two values of the domain's tenant-band vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    On,
    Off,
}

impl Level {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Level::On => "on",
            Level::Off => "off",
        }
    }

    /// Exactly `on` / `off`, trimmed and case-insensitive. Anything else is
    /// `None` — callers decide what an unreadable level means for them.
    fn parse(raw: &str) -> Option<Level> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on" => Some(Level::On),
            "off" => Some(Level::Off),
            _ => None,
        }
    }
}

/// Which C6 rung produced a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LevelSource {
    Coord,
    Persisted,
    Profile,
    ProductDefault,
}

impl LevelSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            LevelSource::Coord => "coord",
            LevelSource::Persisted => "persisted",
            LevelSource::Profile => "profile",
            LevelSource::ProductDefault => "product_default",
        }
    }
}

/// What [`permit`] answers for one flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EgressVerdict {
    pub(crate) allowed: bool,
    pub(crate) source: LevelSource,
}

/// The C6 ladder. PURE: the first rung holding a level wins.
pub(crate) fn resolve(
    coord: Option<Level>,
    persisted: Option<Level>,
    profile: Option<Level>,
) -> EgressVerdict {
    let (level, source) = if let Some(l) = coord {
        (l, LevelSource::Coord)
    } else if let Some(l) = persisted {
        (l, LevelSource::Persisted)
    } else if let Some(l) = profile {
        (l, LevelSource::Profile)
    } else {
        (Level::On, LevelSource::ProductDefault)
    };
    EgressVerdict {
        allowed: level == Level::On,
        source,
    }
}

/// Interpret a profile's raw `egress.default`. PURE.
///
/// `on` / `off` as written. An unrecognised value is `off`: a deployment that
/// wrote SOMETHING into `egress.default` was restricting egress, and a typo
/// must not reopen every flow. Absent or blank is "no profile opinion".
pub(crate) fn profile_level(raw: Option<&str>) -> Option<Level> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    Some(Level::parse(raw).unwrap_or(Level::Off))
}

/// What one 2xx fleet-policy answer means for an egress flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordAnswer {
    /// Coord decided: an explicit row, or a no-row default it names the source
    /// of.
    Authoritative(Level),
    /// A no-row answer from a coord that does not know the egress family (no
    /// `default_source`). Its level is coord's generic default for an unknown
    /// domain, not a statement about this flow, so the ladder falls through.
    NotAnEgressAnswer,
}

/// Classify a 2xx `GET /coord/fleet-policy?domain=egress_*` body. PURE.
///
/// - An explicit row (`resolved_scope` present and not `none`) is
///   authoritative.
/// - A no-row answer is authoritative only when coord names the default it
///   applied (`default_source`); without it, coord predates the egress family.
/// - Within an authoritative answer, an unreadable level is `off`: coord said
///   something about this flow, and a level we cannot identify is never an
///   authorisation to send.
pub(crate) fn classify_coord_answer(
    effective_level: Option<&str>,
    resolved_scope: Option<&str>,
    default_source: Option<&str>,
) -> CoordAnswer {
    let scope = resolved_scope.map(str::trim).filter(|s| !s.is_empty());
    let names_default = default_source.map(str::trim).is_some_and(|s| !s.is_empty());
    let explicit_row = scope.is_some_and(|s| s != RESOLVED_SCOPE_NONE);
    if !explicit_row && !names_default {
        return CoordAnswer::NotAnEgressAnswer;
    }
    CoordAnswer::Authoritative(effective_level.and_then(Level::parse).unwrap_or(Level::Off))
}

type Levels = [Option<Level>; 6];

/// The persisted-answer store's on-disk shape.
#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    schema: u32,
    written_at: String,
    /// domain → level. Unknown domains and unreadable levels are dropped.
    levels: std::collections::BTreeMap<String, String>,
}

/// Decode a store file. PURE. A corrupt file, a foreign schema, unknown
/// domains and unreadable levels all read as absent — never a panic, never a
/// guess.
fn decode_store(raw: &str) -> Levels {
    let mut out: Levels = [None; 6];
    let file: StoreFile = match serde_json::from_str(raw) {
        Ok(f) => f,
        Err(e) => {
            warn!("egress: persisted levels unreadable — treating as absent: {e}");
            return out;
        }
    };
    if file.schema != STORE_SCHEMA {
        warn!(
            "egress: persisted levels carry schema {} (expected {STORE_SCHEMA}) — treating as \
             absent",
            file.schema
        );
        return out;
    }
    for (domain, level) in &file.levels {
        if let (Some(flow), Some(level)) = (Flow::from_domain(domain), Level::parse(level)) {
            out[flow.index()] = Some(level);
        }
    }
    out
}

/// Encode a store file. PURE apart from the timestamp.
fn encode_store(levels: &Levels) -> Vec<u8> {
    let file = StoreFile {
        schema: STORE_SCHEMA,
        written_at: chrono::Utc::now().to_rfc3339(),
        levels: Flow::ALL
            .into_iter()
            .filter_map(|f| levels[f.index()].map(|l| (f.domain().to_string(), l.as_str().into())))
            .collect(),
    };
    serde_json::to_vec_pretty(&file).unwrap_or_default()
}

/// Read the store at `path`. READS ONLY: a missing file (or a missing parent
/// directory) is first boot and creates nothing.
fn load_store(path: &Path) -> Levels {
    match std::fs::read_to_string(path) {
        Ok(raw) => decode_store(&raw),
        Err(_) => [None; 6],
    }
}

/// Write the store atomically, creating the parent directory explicitly here —
/// the one writer — rather than as a side effect of resolving the path (the
/// `persist_briefings` precedent). Best-effort: a failure costs durability
/// across the next restart, never the in-memory state.
fn persist_store(path: &Path, levels: &Levels) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("egress: store dir create failed (best-effort): {e}");
            return;
        }
    }
    if let Err(e) = crate::fs_atomic::atomic_write(path, &encode_store(levels)) {
        warn!("egress: persisting levels failed (best-effort): {e}");
    }
}

/// The process's switch state: one instance in production ([`state`]), and
/// private instances in tests.
pub(crate) struct EgressState {
    /// Rung 1 — this process's authoritative coord answers.
    coord: RwLock<Levels>,
    /// Rung 2 — what the store held at boot, updated as answers are persisted.
    persisted: RwLock<Levels>,
    /// Rung 3 — the profile's `egress.default`, read once.
    profile: Option<Level>,
    /// Where rung 2 lives; `None` when the config dir does not resolve.
    store_path: Option<PathBuf>,
    /// Sends each flow's switch refused in this process.
    refused: [AtomicU64; 6],
    /// Telemetry's boot decision: 0 = not decided yet, 1 = allowed, 2 = refused.
    telemetry_boot: AtomicU8,
}

impl EgressState {
    pub(crate) fn new(store_path: Option<PathBuf>, profile: Option<Level>) -> Self {
        let persisted = store_path.as_deref().map(load_store).unwrap_or([None; 6]);
        Self {
            coord: RwLock::new([None; 6]),
            persisted: RwLock::new(persisted),
            profile,
            store_path,
            refused: Default::default(),
            telemetry_boot: AtomicU8::new(0),
        }
    }

    fn read(levels: &RwLock<Levels>) -> Levels {
        *levels.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The C6 verdict for `flow`. Lock-only, safe on any path.
    pub(crate) fn permit(&self, flow: Flow) -> EgressVerdict {
        let i = flow.index();
        resolve(
            Self::read(&self.coord)[i],
            Self::read(&self.persisted)[i],
            self.profile,
        )
    }

    /// Apply one authoritative coord answer: rung 1 now holds it, and the
    /// store is rewritten when (and only when) the persisted value changes —
    /// a steady state rewrites nothing.
    pub(crate) fn record_coord_answer(&self, flow: Flow, level: Level) {
        let i = flow.index();
        self.coord.write().unwrap_or_else(|p| p.into_inner())[i] = Some(level);
        let snapshot = {
            let mut persisted = self.persisted.write().unwrap_or_else(|p| p.into_inner());
            if persisted[i] == Some(level) {
                return;
            }
            persisted[i] = Some(level);
            *persisted
        };
        if let Some(path) = &self.store_path {
            persist_store(path, &snapshot);
        }
    }

    /// Count one send the switch refused.
    pub(crate) fn count_refused(&self, flow: Flow) {
        self.refused[flow.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn refused(&self, flow: Flow) -> u64 {
        self.refused[flow.index()].load(Ordering::Relaxed)
    }

    fn telemetry_boot(&self) -> Option<bool> {
        match self.telemetry_boot.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// The `/health` `egress` object over this state.
    pub(crate) fn health_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for flow in Flow::ALL {
            let verdict = self.permit(flow);
            let mut entry = json!({
                "allowed": verdict.allowed,
                "source": verdict.source.as_str(),
                "domain": flow.domain(),
                "applies_at_next_start": flow.applies_at_next_start(),
                "refused": self.refused(flow),
            });
            if flow == Flow::Telemetry {
                // What the boot actually installed — which can differ from
                // `allowed` until the next start. `null` before the boot
                // decision ran (never a guessed `true`).
                entry["in_effect"] = json!(self.telemetry_boot());
            }
            out.insert(flow.key().to_string(), entry);
        }
        Value::Object(out)
    }
}

/// Where this instance's store lives — the NON-creating resolver, because the
/// first reader is a read (see `fleet_policy_poller::briefing_store_path`).
#[cfg(not(test))]
fn production_store_path() -> Option<PathBuf> {
    crate::settings::resolve_config_dir()
        .ok()
        .map(|(dir, _source)| dir.join(STORE_FILE))
}

/// The active profile's `egress.default`, read once. A profile that cannot be
/// read is "no profile opinion" — the profile loader already logs why.
#[cfg(not(test))]
fn production_profile_level() -> Option<Level> {
    let raw = qontinui_runner_lib::profiles::active_egress_profile()
        .ok()
        .flatten()
        .and_then(|p| p.default);
    let level = profile_level(raw.as_deref());
    if let Some(level) = level {
        info!(
            "egress: machine profile sets egress.default = {}",
            level.as_str()
        );
    }
    level
}

/// The process-global state. Test builds never touch the real config dir or
/// profile: their global starts empty (every flow at the product default), and
/// tests pin levels per thread through [`test_support::pin`].
fn state() -> &'static EgressState {
    static STATE: OnceLock<EgressState> = OnceLock::new();
    STATE.get_or_init(|| {
        #[cfg(not(test))]
        {
            EgressState::new(production_store_path(), production_profile_level())
        }
        #[cfg(test)]
        {
            EgressState::new(None, None)
        }
    })
}

/// The C6 verdict for `flow` right now. Lock-only — safe from synchronous
/// spawn paths, keystroke-rate relay handlers and the boot sequence.
pub(crate) fn permit(flow: Flow) -> EgressVerdict {
    #[cfg(test)]
    if let Some(level) = test_support::pinned(flow) {
        return EgressVerdict {
            allowed: level == Level::On,
            source: LevelSource::Coord,
        };
    }
    state().permit(flow)
}

/// [`permit`], counting a refusal. For call sites that are about to drop a
/// send on the switch's word — the counter is what `/health` reports.
pub(crate) fn permit_or_count(flow: Flow) -> bool {
    let allowed = permit(flow).allowed;
    if !allowed {
        state().count_refused(flow);
    }
    allowed
}

/// Record coord's answer for `flow` (the poller's write door).
pub(crate) fn record_coord_answer(flow: Flow, level: Level) {
    state().record_coord_answer(flow, level);
}

/// Transcript sync's full consent: the user's own `cloud_sync_enabled` AND the
/// tenant's `egress_transcript_sync`. Every egress path of the transcript /
/// tenant-memory family reads THIS, never `get_cloud_sync_enabled()` bare —
/// `no_bare_cloud_sync_reads_outside_the_allowed_files` enforces it.
///
/// The tenant switch is read first: it is lock-only, and when it is off the
/// settings read is skipped.
pub(crate) fn transcript_sync_permitted() -> bool {
    transcript_sync_permitted_with(crate::settings::get_cloud_sync_enabled)
}

/// [`transcript_sync_permitted`] over an injected user-consent read, so tests
/// drive both halves without touching the machine's `settings.json`.
pub(crate) fn transcript_sync_permitted_with(user_consent: impl FnOnce() -> bool) -> bool {
    permit(Flow::TranscriptSync).allowed && user_consent()
}

/// Telemetry's boot decision: the verdict now, recorded so `/health` can say
/// what the running process actually installed (`in_effect`). Called once by
/// the `sentry::init` block and once by `otel::init_otel`; both read the same
/// verdict.
pub(crate) fn telemetry_permitted_at_boot() -> bool {
    let verdict = permit(Flow::Telemetry);
    let st = state();
    let code = if verdict.allowed { 1 } else { 2 };
    if st.telemetry_boot.swap(code, Ordering::AcqRel) != code && !verdict.allowed {
        info!(
            "egress: telemetry is off for this project (source={}) — crash reporting and OTLP \
             export stay uninstalled until a start where it is on",
            verdict.source.as_str()
        );
    }
    verdict.allowed
}

/// The refusal frame a relay `terminal_*` handler answers instead of
/// streaming. `request_id` / `terminal_id` are echoed so the web side can
/// correlate the reply; the console renders it as "Terminal streaming is off
/// for this project".
pub(crate) fn terminal_refusal_frame(data: &Value) -> Value {
    json!({
        "type": "terminal_response",
        "error": "egress_off",
        "flow": Flow::TerminalStream.key(),
        "domain": Flow::TerminalStream.domain(),
        "message": "Terminal streaming is off for this project",
        "request_id": data.get("request_id").cloned().unwrap_or(Value::Null),
        "terminal_id": data.get("terminal_id").cloned().unwrap_or(Value::Null),
    })
}

/// `GET /health` `egress`: every flow's `{allowed, source, domain,
/// applies_at_next_start, refused}` (telemetry adds `in_effect`).
pub(crate) fn health_json() -> Value {
    state().health_json()
}

/// Test pins. Per THREAD, so a test that pins a flow off cannot flip a
/// concurrently running test's flow on another thread — the code under test
/// must run on the pinning thread (a `#[tokio::test]` current-thread runtime,
/// or a plain `#[test]`). A pinned level reads as a coord answer.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{Flow, Level};
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    thread_local! {
        static PINS: RefCell<[Option<Level>; 6]> = const { RefCell::new([None; 6]) };
    }

    pub(crate) fn pinned(flow: Flow) -> Option<Level> {
        PINS.with(|p| p.borrow()[flow.index()])
    }

    /// RAII pin for one flow on this thread; restored on drop (including on a
    /// failing assertion's unwind).
    pub(crate) struct EgressPin {
        flow: Flow,
        previous: Option<Level>,
    }

    impl Drop for EgressPin {
        fn drop(&mut self) {
            PINS.with(|p| p.borrow_mut()[self.flow.index()] = self.previous);
        }
    }

    pub(crate) fn pin(flow: Flow, level: Level) -> EgressPin {
        let previous = PINS.with(|p| {
            let mut p = p.borrow_mut();
            let prev = p[flow.index()];
            p[flow.index()] = Some(level);
            prev
        });
        EgressPin { flow, previous }
    }

    /// A loopback listener that counts the TCP connections made to it — the
    /// observable for "the flow made zero / at least one connection attempt".
    /// Each connection gets a `503` and is closed, so an HTTP client fails
    /// fast instead of waiting on a timeout.
    pub(crate) struct ConnCounter {
        addr: SocketAddr,
        count: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl ConnCounter {
        pub(crate) fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");
            let count = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let (c, s) = (count.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if s.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    c.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            });
            Self { addr, count, stop }
        }

        pub(crate) fn port(&self) -> u16 {
            self.addr.port()
        }

        pub(crate) fn http_base(&self) -> String {
            format!("http://127.0.0.1:{}", self.addr.port())
        }

        pub(crate) fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }

        /// Poll until at least `n` connections arrived or `within` elapsed —
        /// for flows whose send completes on a background thread.
        pub(crate) fn wait_for(&self, n: usize, within: Duration) -> usize {
            let deadline = Instant::now() + within;
            while self.count() < n && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.count()
        }
    }

    impl Drop for ConnCounter {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Unblock the accept loop so its thread ends.
            let _ = TcpStream::connect(self.addr);
        }
    }
}

/// Register, for every [`Flow`], the pair of tests proving its gate: pinned
/// `off` makes zero connection attempts, pinned `on` makes at least one. The
/// `match` is exhaustive, so a flow without a registration is a compile error,
/// and each entry coerces the named test to `fn()`, so a registration naming a
/// test that does not exist is a compile error too.
#[cfg(test)]
macro_rules! register_flow_tests {
    ($($flow:ident => { off: $off:path, on: $on:path $(,)? }),+ $(,)?) => {
        /// The registered pairs, by name, for the census test.
        pub(crate) const FLOW_TESTS: &[(Flow, &str, &str)] =
            &[$((Flow::$flow, stringify!($off), stringify!($on))),+];

        #[allow(dead_code)]
        fn every_flow_registers_its_tests(flow: Flow) -> (fn(), fn()) {
            match flow {
                $(Flow::$flow => ($off as fn(), $on as fn()),)+
            }
        }
    };
}

#[cfg(test)]
register_flow_tests! {
    TranscriptSync => {
        off: crate::session::coord_sync::egress_tests::transcript_chunk_pinned_off_makes_zero_requests,
        on: crate::session::coord_sync::egress_tests::transcript_chunk_pinned_on_makes_a_request,
    },
    CodeMirror => {
        off: crate::agent_pusher::egress_tests::code_mirror_pinned_off_makes_zero_connections,
        on: crate::agent_pusher::egress_tests::code_mirror_pinned_on_makes_a_connection,
    },
    TerminalStream => {
        off: crate::session::output_pipe::egress_tests::terminal_stream_pinned_off_makes_zero_connections,
        on: crate::session::output_pipe::egress_tests::terminal_stream_pinned_on_makes_a_connection,
    },
    Telemetry => {
        off: crate::otel::egress_tests::telemetry_pinned_off_makes_zero_connections,
        on: crate::otel::egress_tests::telemetry_pinned_on_makes_a_connection,
    },
    UpdateCheck => {
        off: crate::commands::execution::system_ops::egress_tests::update_check_pinned_off_makes_zero_connections,
        on: crate::commands::execution::system_ops::egress_tests::update_check_pinned_on_makes_a_connection,
    },
    SkillMirror => {
        off: crate::canonical_corpus::egress_tests::skill_mirror_pinned_off_makes_zero_connections,
        on: crate::canonical_corpus::egress_tests::skill_mirror_pinned_on_makes_a_connection,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn resolution_takes_the_first_rung_with_a_value() {
        use Level::{Off, On};
        use LevelSource::*;
        let cases = [
            ((Some(Off), Some(On), Some(On)), (false, Coord)),
            ((Some(On), Some(Off), Some(Off)), (true, Coord)),
            ((None, Some(Off), Some(On)), (false, Persisted)),
            ((None, Some(On), Some(Off)), (true, Persisted)),
            ((None, None, Some(Off)), (false, Profile)),
            ((None, None, Some(On)), (true, Profile)),
            ((None, None, None), (true, ProductDefault)),
        ];
        for ((c, p, prof), (allowed, source)) in cases {
            assert_eq!(
                resolve(c, p, prof),
                EgressVerdict { allowed, source },
                "coord={c:?} persisted={p:?} profile={prof:?}"
            );
        }
    }

    #[test]
    fn a_coord_answer_without_a_default_source_is_not_a_tenant_decision() {
        // A coord that predates the egress family answers an unknown domain
        // with its generic `off` and resolved_scope "none" — and no
        // default_source. Honouring it would switch every flow off.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), None),
            CoordAnswer::NotAnEgressAnswer
        );
        assert_eq!(
            classify_coord_answer(Some("off"), None, None),
            CoordAnswer::NotAnEgressAnswer
        );
        // The egress-aware coord names its default: authoritative either way.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), Some("deployment_profile")),
            CoordAnswer::Authoritative(Level::Off)
        );
        assert_eq!(
            classify_coord_answer(Some("on"), Some("none"), Some("product")),
            CoordAnswer::Authoritative(Level::On)
        );
        // An explicit row is authoritative with or without the new field.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("tenant"), None),
            CoordAnswer::Authoritative(Level::Off)
        );
        assert_eq!(
            classify_coord_answer(Some(" ON "), Some("tenant"), None),
            CoordAnswer::Authoritative(Level::On)
        );
        // An unreadable level in an authoritative answer fails closed.
        assert_eq!(
            classify_coord_answer(Some("record"), Some("tenant"), None),
            CoordAnswer::Authoritative(Level::Off)
        );
        assert_eq!(
            classify_coord_answer(None, Some("none"), Some("product")),
            CoordAnswer::Authoritative(Level::Off)
        );
    }

    #[test]
    fn profile_default_reads_on_off_and_fails_closed_on_anything_else() {
        assert_eq!(profile_level(None), None);
        assert_eq!(profile_level(Some("  ")), None);
        assert_eq!(profile_level(Some("on")), Some(Level::On));
        assert_eq!(profile_level(Some("OFF")), Some(Level::Off));
        assert_eq!(profile_level(Some("disabled")), Some(Level::Off));
    }

    #[test]
    fn a_coord_answer_persists_and_the_next_process_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(STORE_FILE);

        let first = EgressState::new(Some(path.clone()), None);
        assert_eq!(
            first.permit(Flow::CodeMirror).source,
            LevelSource::ProductDefault
        );
        first.record_coord_answer(Flow::CodeMirror, Level::Off);
        assert_eq!(
            first.permit(Flow::CodeMirror),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Coord
            }
        );
        assert!(path.exists(), "the answer must be written to the store");

        // A new process: no coord answer yet, so rung 2 answers.
        let second = EgressState::new(Some(path.clone()), Some(Level::On));
        assert_eq!(
            second.permit(Flow::CodeMirror),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Persisted
            }
        );
        // A flow coord never answered falls to the profile rung.
        assert_eq!(
            second.permit(Flow::Telemetry),
            EgressVerdict {
                allowed: true,
                source: LevelSource::Profile
            }
        );
    }

    #[test]
    fn a_steady_state_answer_does_not_rewrite_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let st = EgressState::new(Some(path.clone()), None);
        st.record_coord_answer(Flow::Telemetry, Level::Off);
        std::fs::write(&path, b"sentinel").unwrap();
        st.record_coord_answer(Flow::Telemetry, Level::Off);
        assert_eq!(std::fs::read(&path).unwrap(), b"sentinel");
        st.record_coord_answer(Flow::Telemetry, Level::On);
        assert_ne!(std::fs::read(&path).unwrap(), b"sentinel");
    }

    #[test]
    fn a_corrupt_or_foreign_store_reads_as_absent() {
        assert_eq!(decode_store("{not json"), [None; 6]);
        assert_eq!(
            decode_store(r#"{"schema":99,"written_at":"x","levels":{"egress_telemetry":"off"}}"#),
            [None; 6]
        );
        let mixed = decode_store(
            r#"{"schema":1,"written_at":"x","levels":{"egress_telemetry":"off",
                "egress_unknown":"off","egress_code_mirror":"maybe"}}"#,
        );
        assert_eq!(mixed[Flow::Telemetry.index()], Some(Level::Off));
        assert_eq!(mixed[Flow::CodeMirror.index()], None);
        // A missing file creates nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join(STORE_FILE);
        assert_eq!(load_store(&path), [None; 6]);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let mut levels: Levels = [None; 6];
        levels[Flow::SkillMirror.index()] = Some(Level::Off);
        levels[Flow::UpdateCheck.index()] = Some(Level::On);
        let raw = String::from_utf8(encode_store(&levels)).unwrap();
        assert_eq!(decode_store(&raw), levels);
    }

    #[test]
    fn health_json_reports_all_six_flows_with_their_source() {
        let st = EgressState::new(None, Some(Level::Off));
        st.record_coord_answer(Flow::CodeMirror, Level::Off);
        st.count_refused(Flow::CodeMirror);
        let v = st.health_json();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 6);
        for flow in Flow::ALL {
            let e = &obj[flow.key()];
            assert_eq!(e["domain"], flow.domain());
            assert!(e["allowed"].is_boolean());
        }
        assert_eq!(v["code_mirror"]["source"], "coord");
        assert_eq!(v["code_mirror"]["allowed"], false);
        assert_eq!(v["code_mirror"]["refused"], 1);
        assert_eq!(v["telemetry"]["source"], "profile");
        assert_eq!(v["telemetry"]["applies_at_next_start"], true);
        assert!(
            v["telemetry"]["in_effect"].is_null(),
            "no boot decision yet"
        );
        assert_eq!(v["skill_mirror"]["applies_at_next_start"], false);
    }

    #[test]
    fn the_test_pin_is_per_thread_and_restores() {
        assert_eq!(
            permit(Flow::SkillMirror).source,
            LevelSource::ProductDefault
        );
        {
            let _pin = test_support::pin(Flow::SkillMirror, Level::Off);
            assert!(!permit(Flow::SkillMirror).allowed);
            let other = std::thread::spawn(|| permit(Flow::SkillMirror).allowed)
                .join()
                .unwrap();
            assert!(other, "a pin must not leak to another thread");
        }
        assert!(permit(Flow::SkillMirror).allowed);
    }

    #[test]
    fn transcript_sync_needs_both_the_user_and_the_tenant() {
        let _on = test_support::pin(Flow::TranscriptSync, Level::On);
        assert!(transcript_sync_permitted_with(|| true));
        assert!(!transcript_sync_permitted_with(|| false));
        drop(_on);
        let _off = test_support::pin(Flow::TranscriptSync, Level::Off);
        let mut asked = false;
        assert!(!transcript_sync_permitted_with(|| {
            asked = true;
            true
        }));
        assert!(
            !asked,
            "with the tenant switch off the settings read is skipped"
        );
    }

    /// Census: six flows, six distinct domains in vendored order, every flow
    /// registered with a distinct pair of tests.
    #[test]
    fn census_six_flows_six_domains_every_flow_tested() {
        assert_eq!(Flow::ALL.len(), 6);
        let domains: Vec<&str> = Flow::ALL.iter().map(|f| f.domain()).collect();
        assert_eq!(domains, EGRESS_DOMAINS.to_vec());
        assert_eq!(domains.iter().collect::<HashSet<_>>().len(), 6);
        for d in EGRESS_DOMAINS {
            assert!(d.starts_with("egress_"), "{d}");
            assert_eq!(Flow::from_domain(d).map(|f| f.domain()), Some(d));
            assert_eq!(
                d.strip_prefix("egress_"),
                Some(Flow::from_domain(d).unwrap().key())
            );
        }
        assert_eq!(FLOW_TESTS.len(), 6);
        let flows: HashSet<Flow> = FLOW_TESTS.iter().map(|(f, _, _)| *f).collect();
        assert_eq!(flows.len(), 6, "each flow registered exactly once");
        let names: HashSet<&str> = FLOW_TESTS
            .iter()
            .flat_map(|(_, off, on)| [*off, *on])
            .collect();
        assert_eq!(names.len(), 12, "twelve distinct tests");
        for flow in Flow::ALL {
            let (off, on) = every_flow_registers_its_tests(flow);
            assert_ne!(off as usize, on as usize);
        }
    }

    /// Where coord's `fleet_policy.rs` sits relative to this crate when the
    /// sibling checkout is present (an allocated worktree's sibling, or the
    /// workspace's primary checkout).
    fn sibling_coord_fleet_policy() -> Option<PathBuf> {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rel = Path::new("qontinui-coord/crates/coord/src/fleet_policy.rs");
        // <workspace>/qontinui-runner/src-tauri or
        // <workspace>/agent-worktrees/<agent>/qontinui-runner/src-tauri.
        manifest
            .ancestors()
            .skip(2)
            .take(3)
            .map(|dir| dir.join(rel))
            .find(|p| p.is_file())
    }

    /// Extract the members of `EGRESS_DOMAINS` from coord's source: string
    /// literals inside the declaration's brackets, or identifiers resolved
    /// through `const IDENT: &str = "…";` in the same file. `None` when the
    /// file does not declare it yet.
    fn parse_coord_egress_domains(src: &str) -> Option<Vec<String>> {
        let decl = src
            .match_indices("EGRESS_DOMAINS")
            .filter_map(|(i, _)| src.get(i..))
            // The declaration itself: `EGRESS_DOMAINS: <type> =` on one line
            // (a doc-comment mention has no `=` before its line ends).
            .find(|rest| {
                let line = rest.lines().next().unwrap_or("");
                line.contains('=') && line.split('=').next().unwrap_or("").contains(':')
            })?;
        let (_, after_eq) = decl.split_once('=')?;
        let (_, from_open) = after_eq.split_once('[')?;
        let (body, _) = from_open.split_once(']')?;
        let mut members = Vec::new();
        for item in body.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(lit) = item.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                members.push(lit.to_string());
                continue;
            }
            let ident = item.rsplit("::").next().unwrap_or(item).trim();
            let needle = format!("const {ident}: &str = \"");
            let (_, value) = src.split_once(needle.as_str())?;
            let (value, _) = value.split_once('"')?;
            members.push(value.to_string());
        }
        Some(members)
    }

    #[test]
    fn the_coord_domain_parser_reads_literals_and_constants() {
        let lits = r#"pub const EGRESS_DOMAINS: [&str; 2] = ["egress_a", "egress_b"];"#;
        assert_eq!(
            parse_coord_egress_domains(lits),
            Some(vec!["egress_a".to_string(), "egress_b".to_string()])
        );
        let consts = r#"
            pub const EGRESS_A_DOMAIN: &str = "egress_a";
            /// mentions EGRESS_DOMAINS in a doc first
            pub const EGRESS_DOMAINS: [&str; 1] = [
                EGRESS_A_DOMAIN,
            ];
        "#;
        assert_eq!(
            parse_coord_egress_domains(consts),
            Some(vec!["egress_a".to_string()])
        );
        assert_eq!(parse_coord_egress_domains("pub const OTHER: u8 = 1;"), None);
    }

    /// Drift: coord's `EGRESS_DOMAINS` and the vendored copy agree on members.
    ///
    /// UNKNOWN — printed, never counted as a pass — when the sibling checkout
    /// is absent or does not declare `EGRESS_DOMAINS` yet. FAILS when coord
    /// declares it with different members.
    #[test]
    fn vendored_domains_match_coord() {
        let Some(path) = sibling_coord_fleet_policy() else {
            println!(
                "UNKNOWN: egress drift check not run — no sibling qontinui-coord checkout \
                 near {}",
                env!("CARGO_MANIFEST_DIR")
            );
            return;
        };
        let src = std::fs::read_to_string(&path).expect("read coord fleet_policy.rs");
        let Some(coord) = parse_coord_egress_domains(&src) else {
            println!(
                "UNKNOWN: egress drift check not run — {} does not declare EGRESS_DOMAINS yet",
                path.display()
            );
            return;
        };
        let mut coord_sorted = coord.clone();
        coord_sorted.sort();
        let mut ours: Vec<String> = EGRESS_DOMAINS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(
            coord_sorted,
            ours,
            "coord's EGRESS_DOMAINS ({}) and the runner's vendored copy disagree",
            path.display()
        );
        println!("egress drift check PASSED against {}", path.display());
    }

    /// No egress path may read the user consent bare: every read outside the
    /// settings plumbing, this module and the reporting surfaces goes through
    /// [`transcript_sync_permitted`], so a new egress path cannot skip the
    /// tenant switch.
    #[test]
    fn no_bare_cloud_sync_reads_outside_the_allowed_files() {
        const ALLOWED: &[&str] = &[
            "settings.rs",
            "egress.rs",
            "commands/cloud_sync_settings.rs",
            "mcp/sessions.rs",
        ];
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![root.clone()];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                scanned += 1;
                if ALLOWED.contains(&rel.as_str()) {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                for (n, line) in src.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if code.contains("get_cloud_sync_enabled") {
                        offenders.push(format!("{rel}:{}", n + 1));
                    }
                }
            }
        }
        assert!(
            scanned > 100,
            "the scan must actually walk src/ ({scanned})"
        );
        assert!(
            offenders.is_empty(),
            "bare get_cloud_sync_enabled reads outside the allowed files — use \
             crate::egress::transcript_sync_permitted(): {offenders:?}"
        );
    }
}
