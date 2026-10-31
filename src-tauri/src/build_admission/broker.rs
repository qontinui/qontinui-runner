//! The broker's ledger: tickets, leases, per-ticket secrets, and — in
//! `observe` — the SHADOW schedule that records what enforcement would have
//! done. Pure state: no I/O, no clock (callers pass `now_s`), so every rule is
//! a unit test.
//!
//! ## Observe (Phase 2)
//!
//! Every ticket is granted at once and the build runs exactly as it does today
//! (no job count is imposed). Beside it, the ticket joins a shadow queue that
//! the admission core ([`qontinui_types::build_admission`]) schedules against
//! the shadow leases with the live host facts; when the shadow admits it, the
//! ticket records `would_wait_s` and `would_jobs`. A ticket released while
//! still shadow-queued records that too — it would still have been waiting.
//!
//! ## Secrets
//!
//! Opening a ticket returns a random secret; only its SHA-256 is kept. Every
//! poll and release must present it (compared in constant time), so one local
//! process cannot release or read another session's build.

use std::collections::{BTreeMap, HashSet};

use qontinui_types::build_admission::{
    admit::{Admission, AdmitBasis, Blocking},
    estimate::{estimate, Measurement, Seed},
    order::{schedule, Schedule},
    Class, Estimate, EstimateKey, HostFacts, Lease, LeaseState, MeasureMethod, Policy, Subcommand,
    TargetDirKind, Ticket,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// What a wrapper sends to open a ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketRequest {
    pub repo: String,
    pub subcommand: Subcommand,
    #[serde(default = "dev_profile")]
    pub profile: String,
    /// The resolved output directory cargo will lock (plan D5).
    pub output_dir: String,
    pub target_dir_kind: TargetDirKind,
    #[serde(default)]
    pub requested_jobs: Option<u32>,
    /// Requested class. `operator` is refused here (coord command only) and
    /// `merge` is not verified in this build; both are demoted to `agent`.
    #[serde(default)]
    pub class: Option<Class>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    /// The wrapper's NATIVE OS pid: the lease's liveness and the root of its
    /// build tree. (Git Bash on Windows: `/proc/$$/winpid`, not `$$`.)
    pub pid: u32,
    /// `owner/repo#n` for a merge-class request.
    #[serde(default)]
    pub pr: Option<String>,
}

fn dev_profile() -> String {
    "dev".into()
}

/// A lease's lifecycle. Observe grants at once, so a ticket is `Running` from
/// the moment it is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordState {
    Running,
    Done,
    Failed,
    /// The wrapper was killed by its harness and released on the way out.
    HarnessKilled,
    /// The wrapper died without releasing.
    Lost,
}

impl RecordState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, RecordState::Running)
    }
}

/// Where the ticket stands in the shadow schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shadow {
    Queued,
    Leased,
    Done,
}

/// What enforcement would have done with this ticket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Would {
    /// Seconds from queue to shadow admission (`None` while still waiting, or
    /// if it never got there).
    pub wait_s: Option<u64>,
    pub jobs: Option<u32>,
    pub basis: Option<AdmitBasis>,
    /// Why the shadow held it when it was opened (empty = admitted at once).
    pub blocking_at_open: Vec<Blocking>,
    /// Released while the shadow still had it queued, after this many seconds.
    pub released_while_queued_s: Option<u64>,
}

/// One ticket and its lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// SHA-256 of the secret, hex. Never serialized out of the broker's own
    /// state file.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub secret_hash: String,
    pub request: TicketRequest,
    /// Kernel start time of `request.pid` (pid reuse guard); `None` off Linux.
    pub pid_start: Option<u64>,
    pub class: Class,
    pub class_demoted_reason: Option<String>,
    pub est: Estimate,
    pub queued_at_s: u64,
    pub ended_at_s: Option<u64>,
    pub state: RecordState,
    pub exit_code: Option<i32>,
    /// The highest anonymous memory the broker SAMPLED for this lease's tree
    /// (every process below the wrapper pid, every tick). This, not anything a
    /// caller reports, feeds the estimate history.
    #[serde(default)]
    pub max_sampled_anon_bytes: Option<u64>,
    /// The latest sample.
    #[serde(default)]
    pub current_anon_bytes: Option<u64>,
    /// What the wrapper reported at release: a cross-check only, never history.
    #[serde(default)]
    pub reported_peak_anon_bytes: Option<u64>,
    pub shadow: Shadow,
    pub would: Would,
}

/// Why a poll or release was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessError {
    NotFound,
    BadSecret,
}

/// Why opening a ticket was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Too many tickets are open at once (a runaway or hostile caller).
    TooMany,
}

/// Most tickets that may be open (non-terminal) at once.
pub const MAX_OPEN: usize = 256;
/// A lease older than this is reaped as `lost` whatever its pid says.
pub const MAX_LEASE_S: u64 = 24 * 3600;

/// How a wrapper ended its lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Exit,
    HarnessKilled,
}

pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// One completed, measured lease (the persisted form of a core [`Measurement`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRow {
    pub key: EstimateKey,
    pub peak_bytes: u64,
    pub jobs: u32,
    pub run_s: u64,
}

impl From<&HistoryRow> for Measurement {
    fn from(h: &HistoryRow) -> Self {
        Measurement {
            key: h.key.clone(),
            peak_bytes: h.peak_bytes,
            jobs: h.jobs,
            run_s: h.run_s,
        }
    }
}

/// The ledger.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Broker {
    pub records: BTreeMap<String, Record>,
    /// Completed measurements, oldest first (feeds [`estimate`]).
    pub history: Vec<HistoryRow>,
    #[serde(skip)]
    pub seeds: Vec<Seed>,
}

/// Terminal records kept for the state endpoint and history.
const KEEP_TERMINAL: usize = 200;
/// Measurements kept in total.
const KEEP_HISTORY: usize = 2000;

impl Broker {
    fn key(req: &TicketRequest) -> EstimateKey {
        EstimateKey {
            repo: req.repo.clone(),
            subcommand: req.subcommand,
            profile: req.profile.clone(),
            target_dir_kind: req.target_dir_kind,
            // Peaks are measured by sampling the leased tree until the
            // wrappers place builds in a scope (Phase 3).
            measure_method: MeasureMethod::RssSample,
        }
    }

    /// Open a ticket; observe grants it at once. Returns the record.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        id: String,
        secret_hash: String,
        req: TicketRequest,
        pid_start: Option<u64>,
        facts: &HostFacts,
        policy: &Policy,
        now_s: u64,
    ) -> Result<&Record, OpenError> {
        if self
            .records
            .values()
            .filter(|r| !r.state.is_terminal())
            .count()
            >= MAX_OPEN
        {
            return Err(OpenError::TooMany);
        }
        let (class, demoted) = match req.class.unwrap_or(Class::Agent) {
            Class::Operator => (
                Class::Agent,
                Some("operator class is set only by a coord command".to_owned()),
            ),
            Class::Merge => (
                Class::Agent,
                Some("merge class is not verified in observe".to_owned()),
            ),
            c => (c, None),
        };
        let history: Vec<Measurement> = self.history.iter().map(Into::into).collect();
        let est = estimate(
            &Self::key(&req),
            &history,
            &self.seeds,
            req.requested_jobs,
            policy,
        );
        let rec = Record {
            id: id.clone(),
            secret_hash,
            request: req,
            pid_start,
            class,
            class_demoted_reason: demoted,
            est,
            queued_at_s: now_s,
            ended_at_s: None,
            state: RecordState::Running,
            exit_code: None,
            max_sampled_anon_bytes: None,
            current_anon_bytes: None,
            reported_peak_anon_bytes: None,
            shadow: Shadow::Queued,
            would: Would::default(),
        };
        self.records.insert(id.clone(), rec);
        let shadow_facts = self.shadow_facts(facts);
        let blocking = match self.shadow_admission_of(&id, &shadow_facts, policy) {
            Some(Admission::Wait { blocking }) => blocking,
            _ => Vec::new(),
        };
        if let Some(r) = self.records.get_mut(&id) {
            r.would.blocking_at_open = blocking;
        }
        self.shadow_step(facts, policy, now_s);
        Ok(&self.records[&id])
    }

    /// Check a presented secret for a record.
    pub fn authorize(&self, id: &str, secret: &str) -> Result<&Record, AccessError> {
        let r = self.records.get(id).ok_or(AccessError::NotFound)?;
        let ok: bool = hash_secret(secret)
            .as_bytes()
            .ct_eq(r.secret_hash.as_bytes())
            .into();
        if ok && !r.secret_hash.is_empty() {
            Ok(r)
        } else {
            Err(AccessError::BadSecret)
        }
    }

    /// Release a lease. Idempotent: a second release returns the record as it
    /// is.
    #[allow(clippy::too_many_arguments)]
    pub fn release(
        &mut self,
        id: &str,
        secret: &str,
        reason: ReleaseReason,
        exit_code: Option<i32>,
        reported_peak_anon_bytes: Option<u64>,
        cpus: u32,
        facts: &HostFacts,
        policy: &Policy,
        now_s: u64,
    ) -> Result<&Record, AccessError> {
        self.authorize(id, secret)?;
        let r = self.records.get_mut(id).ok_or(AccessError::NotFound)?;
        if r.state.is_terminal() {
            return Ok(&self.records[id]);
        }
        r.state = match (reason, exit_code) {
            (ReleaseReason::HarnessKilled, _) => RecordState::HarnessKilled,
            (ReleaseReason::Exit, Some(0)) => RecordState::Done,
            (ReleaseReason::Exit, _) => RecordState::Failed,
        };
        r.exit_code = exit_code;
        r.reported_peak_anon_bytes = reported_peak_anon_bytes;
        Self::end(r, now_s);
        if let (RecordState::Done, Some(peak)) = (r.state, r.max_sampled_anon_bytes) {
            let m = HistoryRow {
                key: Self::key(&r.request),
                peak_bytes: peak,
                // Observe imposes no -j, so cargo ran with its default (cpus)
                // unless the caller set one.
                jobs: r.request.requested_jobs.unwrap_or(cpus),
                run_s: now_s.saturating_sub(r.queued_at_s),
            };
            self.history.push(m);
            let excess = self.history.len().saturating_sub(KEEP_HISTORY);
            self.history.drain(..excess);
        }
        // The released lease frees its shadow slot now, not on the next tick.
        self.shadow_step(facts, policy, now_s);
        self.prune();
        Ok(&self.records[id])
    }

    /// Record this tick's sampled memory per running lease (by wrapper pid).
    /// `building` names the leases whose tree held a cargo/rustc/clippy
    /// process this tick: only those samples can raise the peak, so a tick
    /// taken before cargo started (or after it ended) never records a ~0
    /// "peak". Returns whether any peak rose (worth persisting).
    pub fn observe_usage(
        &mut self,
        per_lease: &std::collections::HashMap<u32, u64>,
        building: &HashSet<u32>,
    ) -> bool {
        let mut rose = false;
        for r in self.records.values_mut() {
            if r.state != RecordState::Running {
                continue;
            }
            let now = per_lease.get(&r.request.pid).copied().unwrap_or(0);
            r.current_anon_bytes = Some(now);
            if building.contains(&r.request.pid) && r.max_sampled_anon_bytes.is_none_or(|m| now > m)
            {
                r.max_sampled_anon_bytes = Some(now);
                rose = true;
            }
        }
        rose
    }

    /// The facts the SHADOW decides on. In observe every ticket really runs,
    /// so a ticket the shadow still holds is nevertheless consuming memory;
    /// its sampled anon is added back to MemAvailable so it does not count
    /// against itself or the tickets ahead of it. (PSI cannot be corrected the
    /// same way; the shadow's pressure check reads the real host.)
    pub fn shadow_facts(&self, facts: &HostFacts) -> HostFacts {
        let held: u64 = self
            .records
            .values()
            .filter(|r| r.state == RecordState::Running && r.shadow == Shadow::Queued)
            .filter_map(|r| r.current_anon_bytes)
            .sum();
        let mut f = *facts;
        if let qontinui_types::build_admission::Fact::Measured(a) = f.mem_available_bytes {
            f.mem_available_bytes =
                qontinui_types::build_admission::Fact::Measured(a.saturating_add(held));
        }
        f
    }

    fn end(r: &mut Record, now_s: u64) {
        r.ended_at_s = Some(now_s);
        if r.shadow == Shadow::Queued {
            r.would.released_while_queued_s = Some(now_s.saturating_sub(r.queued_at_s));
        }
        r.shadow = Shadow::Done;
    }

    /// Mark every running lease whose wrapper is gone — or that has run past
    /// [`MAX_LEASE_S`], which no build does (a long-lived pid such as a shell
    /// was named) — as `lost`.
    pub fn reap(&mut self, alive: impl Fn(u32, Option<u64>) -> bool, now_s: u64) -> usize {
        let mut n = 0;
        for r in self.records.values_mut() {
            let too_old = now_s.saturating_sub(r.queued_at_s) > MAX_LEASE_S;
            if r.state == RecordState::Running && (too_old || !alive(r.request.pid, r.pid_start)) {
                r.state = RecordState::Lost;
                Self::end(r, now_s);
                n += 1;
            }
        }
        if n > 0 {
            self.prune();
        }
        n
    }

    /// Wrapper pids of the running leases (their trees are "leased", D4).
    pub fn lease_pids(&self) -> HashSet<u32> {
        self.records
            .values()
            .filter(|r| r.state == RecordState::Running)
            .map(|r| r.request.pid)
            .collect()
    }

    fn shadow_leases(&self) -> Vec<Lease> {
        self.records
            .values()
            .filter(|r| r.shadow == Shadow::Leased)
            .map(|r| Lease {
                id: r.id.clone(),
                class: r.class,
                output_dir: r.request.output_dir.clone(),
                state: LeaseState::Running,
                est_bytes: r.est.bytes,
                // A running lease's expected end is its MEDIAN run time, so
                // the head's shadow time is not pushed late (core D5 backfill).
                expected_duration_s: r.est.duration_p50_s,
                started_at_s: r.queued_at_s + r.would.wait_s.unwrap_or(0),
                paused_at_s: None,
                current_anon_bytes: None,
            })
            .collect()
    }

    fn shadow_queue(&self) -> Vec<Ticket> {
        self.records
            .values()
            .filter(|r| r.shadow == Shadow::Queued)
            .map(|r| Ticket {
                id: r.id.clone(),
                class: r.class,
                output_dir: r.request.output_dir.clone(),
                requested_jobs: r.request.requested_jobs,
                queued_at_s: r.queued_at_s,
                est: r.est,
            })
            .collect()
    }

    fn shadow_admission_of(
        &self,
        id: &str,
        facts: &HostFacts,
        policy: &Policy,
    ) -> Option<Admission> {
        let t = self.shadow_queue().into_iter().find(|t| t.id == id)?;
        Some(qontinui_types::build_admission::admit::admit(
            &t,
            &self.shadow_leases(),
            facts,
            policy,
        ))
    }

    /// Run the shadow scheduler until it holds. Returns how many it admitted.
    pub fn shadow_step(&mut self, real: &HostFacts, policy: &Policy, now_s: u64) -> usize {
        let mut admitted = 0;
        // Each pass admits at most one; the queue bounds the passes.
        for _ in 0..=self.records.len() {
            let queue = self.shadow_queue();
            let leases = self.shadow_leases();
            let facts = self.shadow_facts(real);
            match schedule(&queue, &leases, &facts, now_s, policy) {
                Schedule::Start {
                    ticket_id,
                    jobs,
                    basis,
                    ..
                } => {
                    let Some(r) = self.records.get_mut(&ticket_id) else {
                        break;
                    };
                    r.shadow = Shadow::Leased;
                    r.would.wait_s = Some(now_s.saturating_sub(r.queued_at_s));
                    r.would.jobs = Some(jobs);
                    r.would.basis = Some(basis);
                    admitted += 1;
                }
                Schedule::Hold { .. } => break,
            }
        }
        admitted
    }

    /// Drop the oldest terminal records past [`KEEP_TERMINAL`].
    fn prune(&mut self) {
        let mut terminal: Vec<(u64, String)> = self
            .records
            .values()
            .filter(|r| r.state.is_terminal())
            .map(|r| (r.ended_at_s.unwrap_or(0), r.id.clone()))
            .collect();
        if terminal.len() <= KEEP_TERMINAL {
            return;
        }
        terminal.sort();
        let drop = terminal.len() - KEEP_TERMINAL;
        for (_, id) in terminal.into_iter().take(drop) {
            self.records.remove(&id);
        }
    }

    /// Oldest shadow wait still open, for the state endpoint.
    pub fn oldest_shadow_wait_s(&self, now_s: u64) -> Option<u64> {
        self.records
            .values()
            .filter(|r| r.shadow == Shadow::Queued)
            .map(|r| now_s.saturating_sub(r.queued_at_s))
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::build_admission::{Fact, GIB};

    fn facts() -> HostFacts {
        HostFacts {
            mem_total_bytes: 100 * GIB,
            cpus: 16,
            mem_available_bytes: Fact::Measured(80 * GIB),
            psi_mem_full_avg10: Fact::Measured(0.0),
            non_build_p95_bytes: Fact::Measured(10 * GIB),
            ci_reservation_bytes: Fact::Measured(0),
            unleased_build_rss_bytes: Fact::Measured(0),
            admissions_paused: false,
        }
    }

    fn req(dir: &str, pid: u32) -> TicketRequest {
        TicketRequest {
            repo: "qontinui-coord".into(),
            subcommand: Subcommand::Test,
            profile: "dev".into(),
            output_dir: dir.into(),
            target_dir_kind: TargetDirKind::SharedWarm,
            requested_jobs: None,
            class: None,
            session_id: None,
            worktree: None,
            pid,
            pr: None,
        }
    }

    fn seeded(bytes: u64) -> Broker {
        let mut b = Broker::default();
        b.seeds.push(Seed {
            key: Broker::key(&req("x", 1)),
            bytes,
        });
        b
    }

    fn open(b: &mut Broker, id: &str, dir: &str, pid: u32, f: &HostFacts, now: u64) -> Record {
        b.open(
            id.into(),
            hash_secret(id),
            req(dir, pid),
            None,
            f,
            &Policy::default(),
            now,
        )
        .unwrap()
        .clone()
    }

    #[test]
    fn observe_grants_at_once_and_records_the_shadow_decision() {
        let mut b = seeded(30 * GIB);
        let f = facts(); // budget 100 - 4 (reserve) - 10 = 86 GiB
        let r = open(&mut b, "a", "d1", 1, &f, 0);
        assert_eq!(r.state, RecordState::Running);
        assert_eq!(r.shadow, Shadow::Leased);
        assert_eq!(r.would.wait_s, Some(0));
        assert!(r.would.blocking_at_open.is_empty());
        open(&mut b, "b", "d2", 2, &f, 1);
        // 30 + 30 + 30 > 86: the third would wait, but is still granted.
        let c = open(&mut b, "c", "d3", 3, &f, 2);
        assert_eq!(c.state, RecordState::Running);
        assert_eq!(c.shadow, Shadow::Queued);
        assert!(c
            .would
            .blocking_at_open
            .iter()
            .any(|x| matches!(x, Blocking::Reservation { .. })));
        // A shadow lease ends: the waiter is admitted with its would-wait.
        b.release(
            "a",
            "a",
            ReleaseReason::Exit,
            Some(0),
            None,
            16,
            &facts(),
            &Policy::default(),
            50,
        )
        .unwrap();
        b.shadow_step(&f, &Policy::default(), 50);
        let c = &b.records["c"];
        assert_eq!((c.shadow, c.would.wait_s), (Shadow::Leased, Some(48)));
    }

    #[test]
    fn a_held_output_dir_waits_in_the_shadow_regardless_of_memory() {
        let mut b = seeded(GIB);
        let f = facts();
        open(&mut b, "a", "same", 1, &f, 0);
        let r = open(&mut b, "b", "same", 2, &f, 0);
        assert_eq!(r.shadow, Shadow::Queued);
        assert!(matches!(
            r.would.blocking_at_open[..],
            [Blocking::Target { .. }]
        ));
        b.release(
            "b",
            "b",
            ReleaseReason::Exit,
            Some(1),
            None,
            16,
            &facts(),
            &Policy::default(),
            30,
        )
        .unwrap();
        assert_eq!(b.records["b"].would.released_while_queued_s, Some(30));
        assert_eq!(b.records["b"].state, RecordState::Failed);
    }

    #[test]
    fn secrets_are_required_and_only_a_hash_is_kept() {
        let mut b = seeded(GIB);
        open(&mut b, "a", "d", 1, &facts(), 0);
        assert_ne!(b.records["a"].secret_hash, "a");
        assert_eq!(
            b.authorize("a", "wrong").unwrap_err(),
            AccessError::BadSecret
        );
        assert_eq!(b.authorize("zz", "a").unwrap_err(), AccessError::NotFound);
        assert_eq!(
            b.release(
                "a",
                "wrong",
                ReleaseReason::Exit,
                Some(0),
                None,
                16,
                &facts(),
                &Policy::default(),
                1
            )
            .unwrap_err(),
            AccessError::BadSecret
        );
        assert_eq!(b.records["a"].state, RecordState::Running);
        // A record re-adopted without a hash can never be authorized.
        b.records.get_mut("a").unwrap().secret_hash.clear();
        assert_eq!(b.authorize("a", "").unwrap_err(), AccessError::BadSecret);
    }

    #[test]
    fn operator_and_merge_requests_are_demoted_with_a_reason() {
        let mut b = seeded(GIB);
        let mut r = req("d", 1);
        r.class = Some(Class::Operator);
        let rec = b
            .open(
                "a".into(),
                hash_secret("a"),
                r,
                None,
                &facts(),
                &Policy::default(),
                0,
            )
            .unwrap();
        assert_eq!(rec.class, Class::Agent);
        assert!(rec.class_demoted_reason.is_some());
        let mut r = req("e", 2);
        r.class = Some(Class::Background);
        let rec = b
            .open(
                "b".into(),
                hash_secret("b"),
                r,
                None,
                &facts(),
                &Policy::default(),
                0,
            )
            .unwrap();
        assert_eq!(
            (rec.class, rec.class_demoted_reason.clone()),
            (Class::Background, None)
        );
    }

    #[test]
    fn release_states_and_measurements() {
        let mut b = seeded(GIB);
        let f = facts();
        open(&mut b, "a", "d1", 1, &f, 0);
        open(&mut b, "b", "d2", 2, &f, 0);
        // The broker samples the trees: 5 GiB, then 7 GiB at peak, then 2.
        for gib in [5, 7, 2] {
            b.observe_usage(
                &std::collections::HashMap::from([(1, gib * GIB), (2, GIB)]),
                &HashSet::from([1, 2]),
            );
        }
        assert_eq!(b.records["a"].current_anon_bytes, Some(2 * GIB));
        b.release(
            "a",
            "a",
            ReleaseReason::Exit,
            Some(0),
            // The wrapper's report is a cross-check, never the history.
            Some(999 * GIB),
            16,
            &facts(),
            &Policy::default(),
            90,
        )
        .unwrap();
        b.release(
            "b",
            "b",
            ReleaseReason::HarnessKilled,
            None,
            Some(GIB),
            16,
            &facts(),
            &Policy::default(),
            90,
        )
        .unwrap();
        assert_eq!(b.records["a"].state, RecordState::Done);
        assert_eq!(b.records["b"].state, RecordState::HarnessKilled);
        // Only a clean exit becomes history, with the SAMPLED peak; jobs
        // default to cpus.
        assert_eq!(b.history.len(), 1);
        assert_eq!(b.records["a"].reported_peak_anon_bytes, Some(999 * GIB));
        assert_eq!(
            (
                b.history[0].peak_bytes,
                b.history[0].jobs,
                b.history[0].run_s
            ),
            (7 * GIB, 16, 90)
        );
        // Idempotent.
        let again = b
            .release(
                "a",
                "a",
                ReleaseReason::Exit,
                Some(1),
                None,
                16,
                &facts(),
                &Policy::default(),
                99,
            )
            .unwrap();
        assert_eq!(
            (again.state, again.ended_at_s),
            (RecordState::Done, Some(90))
        );
    }

    #[test]
    fn open_tickets_are_capped() {
        let mut b = seeded(GIB);
        let f = facts();
        for i in 0..MAX_OPEN {
            let id = format!("t{i}");
            open(&mut b, &id, &id, i as u32 + 10, &f, 0);
        }
        let over = b.open(
            "x".into(),
            hash_secret("x"),
            req("x", 1),
            None,
            &f,
            &Policy::default(),
            0,
        );
        assert_eq!(over.err(), Some(OpenError::TooMany));
    }

    /// A ticket the shadow holds still runs in observe; its own memory must
    /// not count against it.
    #[test]
    fn shadow_adds_back_the_memory_of_builds_it_holds() {
        let mut b = seeded(30 * GIB);
        let mut f = facts();
        f.mem_available_bytes = Fact::Measured(40 * GIB);
        open(&mut b, "a", "same", 1, &f, 0);
        open(&mut b, "b", "same", 2, &f, 0); // held by the target check
        b.observe_usage(
            &std::collections::HashMap::from([(1, 10 * GIB), (2, 25 * GIB)]),
            &HashSet::from([1, 2]),
        );
        let sf = b.shadow_facts(&f);
        assert_eq!(sf.mem_available_bytes, Fact::Measured(65 * GIB));
        // Unknown memory stays unknown.
        f.mem_available_bytes = Fact::Unknown;
        assert_eq!(b.shadow_facts(&f).mem_available_bytes, Fact::Unknown);
    }

    #[test]
    fn a_tick_with_no_build_running_never_sets_a_peak() {
        let mut b = seeded(GIB);
        open(&mut b, "a", "d", 1, &facts(), 0);
        let none = HashSet::new();
        assert!(!b.observe_usage(&std::collections::HashMap::from([(1, 3)]), &none));
        assert_eq!(b.records["a"].max_sampled_anon_bytes, None);
        assert_eq!(b.records["a"].current_anon_bytes, Some(3));
        assert!(b.observe_usage(
            &std::collections::HashMap::from([(1, 9 * GIB)]),
            &HashSet::from([1])
        ));
        assert!(!b.observe_usage(
            &std::collections::HashMap::from([(1, 2 * GIB)]),
            &HashSet::from([1])
        ));
        assert_eq!(b.records["a"].max_sampled_anon_bytes, Some(9 * GIB));
        // A build that never showed a cargo/rustc sample leaves no history row.
        open(&mut b, "z", "dz", 2, &facts(), 0);
        b.release(
            "z",
            "z",
            ReleaseReason::Exit,
            Some(0),
            None,
            16,
            &facts(),
            &Policy::default(),
            5,
        )
        .unwrap();
        assert!(b.history.is_empty());
    }

    #[test]
    fn a_lease_older_than_a_day_is_reaped_whatever_its_pid() {
        let mut b = seeded(GIB);
        open(&mut b, "a", "d", 1, &facts(), 0);
        assert_eq!(b.reap(|_, _| true, MAX_LEASE_S), 0);
        assert_eq!(b.reap(|_, _| true, MAX_LEASE_S + 1), 1);
        assert_eq!(b.records["a"].state, RecordState::Lost);
    }

    #[test]
    fn dead_wrappers_are_reaped_as_lost() {
        let mut b = seeded(GIB);
        let f = facts();
        open(&mut b, "a", "d1", 1, &f, 0);
        open(&mut b, "b", "d2", 2, &f, 0);
        assert_eq!(b.lease_pids(), HashSet::from([1, 2]));
        assert_eq!(b.reap(|pid, _| pid == 2, 10), 1);
        assert_eq!(b.records["a"].state, RecordState::Lost);
        assert_eq!(b.lease_pids(), HashSet::from([2]));
    }

    /// Plan Phase 2 requirement (Phase 1 review): a running lease's expected
    /// end is the admitted ticket's p50 run time, never its p90.
    #[test]
    fn shadow_leases_expect_the_p50_duration() {
        let mut b = Broker::default();
        let key = Broker::key(&req("x", 1));
        for (peak, run) in [(GIB, 100), (GIB, 200), (GIB, 900)] {
            b.history.push(HistoryRow {
                key: key.clone(),
                peak_bytes: peak,
                jobs: 16,
                run_s: run,
            });
        }
        open(&mut b, "a", "d", 1, &facts(), 0);
        let est = b.records["a"].est;
        assert_eq!(
            (est.duration_p50_s, est.duration_p90_s),
            (Some(200), Some(900))
        );
        let leases = b.shadow_leases();
        assert_eq!(leases[0].expected_duration_s, Some(200));
    }

    #[test]
    fn terminal_records_are_pruned_running_ones_never() {
        let mut b = seeded(GIB);
        let f = facts();
        open(&mut b, "keep", "k", 99_999, &f, 0);
        for i in 0..(KEEP_TERMINAL + 5) {
            let id = format!("t{i}");
            open(&mut b, &id, &id, i as u32 + 10, &f, 0);
            b.release(
                &id,
                &id,
                ReleaseReason::Exit,
                Some(0),
                None,
                16,
                &facts(),
                &Policy::default(),
                i as u64,
            )
            .unwrap();
        }
        assert_eq!(b.records.len(), KEEP_TERMINAL + 1);
        assert!(b.records.contains_key("keep"));
        assert!(!b.records.contains_key("t0"));
    }

    #[test]
    fn ticket_requests_refuse_unknown_fields() {
        let ok = r#"{"repo":"r","subcommand":"check","output_dir":"o","target_dir_kind":"shared_warm","pid":5}"#;
        let r: TicketRequest = serde_json::from_str(ok).unwrap();
        assert_eq!(r.profile, "dev");
        let bad = r#"{"repo":"r","subcommand":"check","output_dir":"o","target_dir_kind":"shared_warm","pid":5,"clas":"operator"}"#;
        assert!(serde_json::from_str::<TicketRequest>(bad).is_err());
    }
}
