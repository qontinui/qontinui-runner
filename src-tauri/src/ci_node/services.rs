//! Ephemeral, dispatch-scoped SERVICES for a CI dispatch: stand up the
//! containers a `[[services]]` declaration names, health-gate them before the
//! first step runs, hand the steps their connection env, and tear them down on
//! every exit path.
//!
//! **Why this exists.** `ci_node/` provisioned sibling repos and registry
//! tools; it provisioned no DATABASE. qontinui-web's manifest header records
//! the consequence precisely — its `backend-ci.yml` `test` job needs a
//! `pgvector/pgvector:pg16` container and a `redis:7-alpine` container, so
//! "the backend unit suite is NOT gated here; the Actions lane remains its only
//! gate". That is the largest single block of gate coverage this lane cannot
//! reach, and it is unreachable for a structural reason rather than a
//! configuration one: *nothing in a manifest could stand a server up*.
//!
//! # This is NOT `env_agent::apply_services`
//!
//! The runner already has a thing called services, and repurposing it here
//! would be a category error. `apply_services` re-points a **long-lived
//! profile** at canonical's topology and deliberately PRESERVES credentials;
//! its blast radius is the user's persistent environment. What this module
//! creates is disposable, dispatch-scoped and credential-generating: opposite
//! lifetime, opposite blast radius. They share the word and nothing else, so
//! they share no code.
//!
//! # Declarative, from a CLOSED registry — a manifest never names an image
//!
//! `[[services]]` follows the precedent `[[siblings]]` and `[[tools]]` set: the
//! manifest NAMES a curated entry and PINS its version. It cannot supply an
//! image reference, and there is nowhere for one to live
//! (`deny_unknown_fields` on a two-and-a-bit-field struct). An image reference
//! is the container world's URL: accepting one would let a manifest make the
//! runner pull an arbitrary payload from an arbitrary registry and RUN it as a
//! network-listening server on a user's machine, which is strictly worse than
//! the arbitrary-binary-on-PATH case the tool registry already refuses. So the
//! registry below is closed, and adding an entry is a deliberate, reviewable
//! act.
//!
//! The tag a manifest pins must actually name a version
//! ([`super::manifest`]'s `validate_image_tag`): `latest`, `stable`, `alpine`
//! and friends are rejected for exactly the reason `latest` is rejected for a
//! tool — a floating pointer makes two dispatches of one commit incomparable.
//! A tag is still a MUTABLE pointer, so an entry may additionally pin
//! `digest = "sha256:…"`, which is the only true pin; when present it is what
//! the image reference is built from.
//!
//! # No container runtime is a REFUSAL, never a skip
//!
//! A user's machine may have no Docker. That is a legitimate state and this
//! module's answer to it is a loud failure naming what is missing and what was
//! tried ([`detect_runtime`]) — never a silent skip. A skipped service turns a
//! database-backed gate GREEN while testing nothing, which is the single worst
//! outcome available on this lane: it is not a missing gate, it is a lying one.
//! The same reasoning already governs [`super::tools`]'s missing-interpreter
//! path ("silence is never success on this lane").
//!
//! # Health-gating before steps run
//!
//! A container that is *up* is not a server that is *ready*: Postgres's
//! entrypoint runs an initdb bootstrap against a unix socket before it ever
//! listens on TCP. So readiness is probed per kind, over TCP, from INSIDE the
//! container (no host client required):
//!
//! * Postgres — `pg_isready -h 127.0.0.1 -p <container port>`, which the
//!   bootstrap's socket-only temp server does NOT satisfy, followed by one real
//!   `psql -c 'SELECT 1'` before the gate is declared open. "Ready" therefore
//!   means a query was answered, not that a port was open.
//! * Redis — `redis-cli ping` must answer `PONG`.
//!
//! The wait is bounded ([`READINESS_TIMEOUT`]) and a timeout FAILS the dispatch
//! with the last probe output and the container's own log tail. It never
//! proceeds to run steps against a server that has not answered.
//!
//! # Teardown: what is guaranteed, and what is not
//!
//! Guaranteed:
//!
//! * Every container is registered in the [`ServiceStack`] BEFORE it is
//!   created, so a `run` that half-succeeds, a readiness timeout, a failing
//!   later service, a failing step and a cancelled dispatch all leave a stack
//!   that still names it.
//! * The stack lives in the executor's `DispatchWorkspace`, whose `cleanup` is
//!   the single exit path of `run_dispatch` — the same path, and the same
//!   receipt-consuming type, that already guarantees the worktree is removed.
//!   Teardown therefore runs on the failure paths as well as the success path.
//! * [`ServiceStack::drop`] is a blocking last-resort reaper for the one case
//!   the async path cannot cover: a panic unwinding out of the dispatch task.
//!   It is a no-op after a normal teardown (which empties the list).
//!
//! NOT guaranteed — stated rather than implied away:
//!
//! * If the runner PROCESS dies (SIGKILL, power loss, a Job-Object kill of the
//!   whole tree), the containers survive. They are children of the container
//!   daemon, not of this process, so neither `Drop` nor the Windows Job Object
//!   reaches them — the Job Object's kill-on-close covers step children and
//!   says nothing about a daemon's containers. Every container this module
//!   creates therefore carries the label `qontinui.ci.dispatch=<dispatch_id>`,
//!   so the survivors are identifiable and sweepable
//!   (`docker ps -aq --filter label=qontinui.ci.dispatch`). That is a
//!   diagnosis aid, not a guarantee, and it is deliberately not a fleet-wide
//!   auto-sweep: a runner cannot tell another live dispatch's container from an
//!   orphan, and killing a peer's database mid-build would be a worse bug than
//!   the leak it cleans up.
//! * A dispatch removes only containers whose names it minted from its OWN
//!   dispatch id. Nothing else on the machine is ever touched — the developer's
//!   own Postgres, and a peer dispatch's containers, are outside the blast
//!   radius by construction. The one place that is scoped to the dispatch ID
//!   rather than to what this dispatch CREATED is the pre-emptive `rm -f`
//!   before a start, which clears a leftover of a crashed earlier attempt at
//!   the same id. That is deliberate and is the same widening
//!   `prepare_worktree` already makes for the dispatch directory; a dispatch id
//!   is minted once by coord, so the only thing it can hit is this dispatch's
//!   own debris.
//!
//! # Blast radius on disk and on the network
//!
//! Containers are created with **no bind mounts and no volumes** (see
//! [`run_argv`]), so a service cannot write anywhere on the host, and the
//! cleanup guarantee stated in [`super::checkout`] — exactly one directory
//! removed, nothing outside it — is unchanged by this phase: services add
//! nothing to disk at all. Ports are published on `127.0.0.1` only, never
//! `0.0.0.0`, so a dispatch never exposes a database to the network the user's
//! machine is on. The published host port is EPHEMERAL (kernel-assigned), not
//! the service's well-known port, because this lane exists to serve a machine
//! that may already be running its own Postgres on 5432.
//!
//! `127.0.0.1` is also what the exported env spells — never `localhost`, which
//! on Windows resolves to `::1` first and would pay a doomed IPv6 connect
//! before reaching the IPv4-published port (the same measured trap that makes
//! every probe script in this workspace spell the runner's own port
//! `http://127.0.0.1:9876`).
//!
//! # The generated password is never written down
//!
//! It is passed to the container through the runtime child's ENVIRONMENT, with
//! the argv carrying only the bare flag `-e POSTGRES_PASSWORD` (docker and
//! podman both read that as "inherit this name from my environment"). That is
//! not tidiness. An argv is published twice over on this lane: it sits in the
//! host process table for the duration of the call, readable by every other
//! session on a machine that runs many; and it is formatted into the error
//! strings this module logs to the runner's dev log, streams live to coord, and
//! ships in the `log_tail` coord PERSISTS with the dispatch result. The
//! triggers are ordinary failures this module explicitly handles — a port race,
//! an image tag that does not resolve, a pull that exceeds its budget. A
//! per-dispatch password in a persisted remote record is exactly the hazard the
//! generation was supposed to avoid. `tools::run_capture` additionally redacts
//! the value after any `-e`/`--env` flag before formatting an argv into an
//! error, so a future caller that writes the `KEY=VALUE` form leaks nothing.
//!
//! # Env: executor-owned, so the allowlist did not move
//!
//! Steps reach a service through variables the EXECUTOR exports
//! ([`exports_for`]) — `DATABASE_URL` and the libpq `PG*` family, `REDIS_URL`
//! and `REDIS_HOST`/`REDIS_PORT`. They are executor-owned for a hard reason
//! rather than a stylistic one: their values contain a port the manifest cannot
//! know (it is assigned at dispatch time) and a password the manifest must
//! never contain. The manifest's `ENV_ALLOWLIST` therefore gained no
//! connection variables; the keys were added to `EXECUTOR_OWNED_ENV` instead,
//! so a step that tries to set one is rejected with a message pointing at the
//! `[[services]]` entry that actually provides it — the same treatment
//! `CARGO_TARGET_DIR` gets.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::manifest::CiService;
use super::tools::run_capture;

/// Container runtimes tried, in order. A miss is a refusal naming this list —
/// never a silent skip. `podman` is here because its CLI is command-compatible
/// for the four verbs this module uses (`info`, `run`, `exec`, `rm`) and it is
/// what a machine without Docker Desktop is likely to have.
pub(crate) const CONTAINER_RUNTIMES: &[&str] = &["docker", "podman"];

/// Budget for the runtime probe. Generous because a cold Docker Desktop can
/// take tens of seconds to answer its first `info`.
const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Budget for `run -d`, which PULLS the image on first use — that is a
/// hundreds-of-megabytes download on a cold machine.
const CONTAINER_START_TIMEOUT: Duration = Duration::from_secs(900);
/// Budget for one short container command (`exec`, `rm`, `logs`).
const CONTAINER_CMD_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a service may take to answer before the dispatch fails loudly.
const READINESS_TIMEOUT: Duration = Duration::from_secs(180);
/// Gap between readiness probes.
const READINESS_INTERVAL: Duration = Duration::from_secs(1);
/// How many log lines of a sick container are quoted into the failure.
const LOG_TAIL_LINES: &str = "50";
/// Label every container carries, so survivors of a killed runner are
/// identifiable. See the module docs on what this does and does not buy.
pub(crate) const DISPATCH_LABEL: &str = "qontinui.ci.dispatch";
/// Host address services are published on, and the address the exported env
/// spells. IPv4 loopback explicitly — never `localhost`.
const LOOPBACK: &str = "127.0.0.1";
/// Wall-clock budget for the synchronous last-resort reaper in
/// [`ServiceStack::drop`]. Shorter than the async path's budget because it
/// blocks a thread that cannot yield to anything else.
const DROP_REAP_BUDGET: Duration = Duration::from_secs(20);
/// Poll interval while the blocking reaper waits.
const DROP_REAP_POLL: Duration = Duration::from_millis(50);
/// How long the PUBLISHED host port has to start accepting connections once
/// the server itself reports ready.
const HOST_PORT_TIMEOUT: Duration = Duration::from_secs(60);
/// Per-attempt connect budget for the host-side reachability probe.
const HOST_PORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Gap between host-side connect attempts.
const HOST_PORT_INTERVAL: Duration = Duration::from_millis(500);

/// Attempts at picking a free host port before giving up. A port is chosen by
/// binding `:0` and releasing it, so a racing process can take it in the gap;
/// a retry costs one failed `run`.
const PORT_ATTEMPTS: usize = 3;

/// What a curated entry actually is. Two entries can share a kind (`postgres`
/// and `postgres-pgvector` differ only in image), and the kind is what
/// determines credentials, readiness probe and exported env.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceKind {
    Postgres,
    Redis,
}

impl ServiceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ServiceKind::Postgres => "postgres",
            ServiceKind::Redis => "redis",
        }
    }
}

/// One entry of the closed registry.
///
/// The bar for adding one mirrors the tool registry's: (a) a manifest the
/// fleet actually ships asks for it, (b) its readiness is determinable from
/// inside the container without a host client, and (c) the image repository
/// is the one the Actions lane this must agree with already uses.
pub(crate) struct ServiceSpec {
    /// Name a manifest may declare.
    pub name: &'static str,
    pub kind: ServiceKind,
    /// Curated image REPOSITORY. The manifest pins the tag; it never supplies
    /// a repository, and it can never point at another registry.
    pub image_repo: &'static str,
    /// Port the server listens on INSIDE the container.
    pub container_port: u16,
    /// `--memory` ceiling for the container (docker/podman size syntax).
    /// Sized per kind rather than globally: a Postgres running a test suite
    /// legitimately needs more than a cache does.
    pub memory_limit: &'static str,
    /// `--cpus` ceiling.
    pub cpu_limit: &'static str,
}

/// The closed registry.
static KNOWN_SERVICES: &[ServiceSpec] = &[
    // Stock Postgres, for a suite that needs no extensions.
    ServiceSpec {
        name: "postgres",
        kind: ServiceKind::Postgres,
        image_repo: "postgres",
        container_port: 5432,
        memory_limit: "2g",
        cpu_limit: "2",
    },
    // What qontinui-web's `test` job actually uses: a drop-in Postgres that
    // ships the `vector` extension. Stock postgres:16 does not, and every
    // model with a pgvector column fails `CREATE EXTENSION vector` without it.
    ServiceSpec {
        name: "postgres-pgvector",
        kind: ServiceKind::Postgres,
        image_repo: "pgvector/pgvector",
        container_port: 5432,
        memory_limit: "2g",
        cpu_limit: "2",
    },
    ServiceSpec {
        name: "redis",
        kind: ServiceKind::Redis,
        image_repo: "redis",
        container_port: 6379,
        memory_limit: "512m",
        cpu_limit: "1",
    },
];

pub(crate) fn lookup(name: &str) -> Option<&'static ServiceSpec> {
    KNOWN_SERVICES.iter().find(|s| s.name == name)
}

pub(crate) fn known_service_names() -> Vec<&'static str> {
    KNOWN_SERVICES.iter().map(|s| s.name).collect()
}

/// Per-dispatch credentials. Generated by the runner, never declared: a
/// manifest is a committed file and a committed password is a password on
/// GitHub. The user and database names are fixed so the exported env is
/// predictable; only the password is random.
#[derive(Debug, Clone)]
pub(crate) struct ServiceCreds {
    pub user: String,
    pub password: String,
    pub database: String,
}

impl ServiceCreds {
    fn generate() -> Self {
        Self {
            user: "qontinui_ci".to_string(),
            password: format!(
                "{:016x}{:016x}",
                rand::random::<u64>(),
                rand::random::<u64>()
            ),
            database: "qontinui_ci".to_string(),
        }
    }
}

/// The image reference a declaration resolves to. A digest wins over the tag —
/// it is the only reference that cannot be re-pointed under us.
pub(crate) fn image_ref(spec: &ServiceSpec, declared: &CiService) -> String {
    match &declared.digest {
        Some(d) => format!("{}@{}", spec.image_repo, d),
        None => format!("{}:{}", spec.image_repo, declared.version),
    }
}

/// Container name for one service of one dispatch. Dispatch-scoped exactly the
/// way the worktree directory is: the id is already validated to be a plain
/// token (`dispatch_id_is_safe`), so this is a legal container name and it can
/// never collide with a peer dispatch's.
pub(crate) fn container_name(dispatch_id: &str, entry: &str) -> String {
    format!("qontinui-ci-{dispatch_id}-{entry}")
}

/// Names of the env vars the runner sets INSIDE the container. Only the NAMES
/// go on the command line (`-e NAME`, the inherit-from-my-environment form);
/// the values travel out of band via [`container_env`]. See [`run_argv`].
fn container_env_names(kind: ServiceKind) -> &'static [&'static str] {
    match kind {
        ServiceKind::Postgres => &["POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB"],
        // The Actions lane runs redis with no auth and so does this: the port
        // is loopback-only and the container is gone at dispatch end.
        ServiceKind::Redis => &[],
    }
}

/// The VALUES for [`container_env_names`], handed to the container-runtime
/// child through its process environment — never through its argv.
fn container_env(kind: ServiceKind, creds: &ServiceCreds) -> Vec<(String, String)> {
    match kind {
        ServiceKind::Postgres => vec![
            ("POSTGRES_USER".to_string(), creds.user.clone()),
            ("POSTGRES_PASSWORD".to_string(), creds.password.clone()),
            ("POSTGRES_DB".to_string(), creds.database.clone()),
        ],
        ServiceKind::Redis => Vec::new(),
    }
}

/// Env exported to every STEP of the dispatch. Pure, so the wire the steps
/// actually see is testable without a container.
pub(crate) fn exports_for(
    kind: ServiceKind,
    port: u16,
    creds: &ServiceCreds,
) -> Vec<(String, String)> {
    match kind {
        ServiceKind::Postgres => vec![
            (
                "DATABASE_URL".to_string(),
                format!(
                    "postgresql://{}:{}@{LOOPBACK}:{port}/{}",
                    creds.user, creds.password, creds.database
                ),
            ),
            // The libpq family, so a plain `psql` step needs no credentials in
            // the manifest and no URL parsing.
            ("PGHOST".to_string(), LOOPBACK.to_string()),
            ("PGPORT".to_string(), port.to_string()),
            ("PGUSER".to_string(), creds.user.clone()),
            ("PGPASSWORD".to_string(), creds.password.clone()),
            ("PGDATABASE".to_string(), creds.database.clone()),
        ],
        ServiceKind::Redis => vec![
            (
                "REDIS_URL".to_string(),
                format!("redis://{LOOPBACK}:{port}"),
            ),
            ("REDIS_HOST".to_string(), LOOPBACK.to_string()),
            ("REDIS_PORT".to_string(), port.to_string()),
        ],
    }
}

/// Every kind the registry can start. Used to enumerate the executor-owned
/// env keys without hand-maintaining a second list.
const ALL_KINDS: &[ServiceKind] = &[ServiceKind::Postgres, ServiceKind::Redis];

/// Which service kind, if any, owns this env key. This is the manifest
/// validator's rejection rule — a step that sets one of these is pointed at
/// the `[[services]]` declaration that actually provides it instead of having
/// its value silently overridden. Deriving it from [`exported_env_keys`]
/// rather than restating the list in `manifest.rs` means a new export cannot
/// be added without the manifest side closing behind it.
pub(crate) fn kind_exporting(key: &str) -> Option<ServiceKind> {
    ALL_KINDS
        .iter()
        .copied()
        .find(|k| exported_env_keys(*k).contains(&key))
}

/// Every env key the executor exports on behalf of a service kind.
pub(crate) fn exported_env_keys(kind: ServiceKind) -> Vec<&'static str> {
    match kind {
        ServiceKind::Postgres => vec![
            "DATABASE_URL",
            "PGHOST",
            "PGPORT",
            "PGUSER",
            "PGPASSWORD",
            "PGDATABASE",
        ],
        ServiceKind::Redis => vec!["REDIS_URL", "REDIS_HOST", "REDIS_PORT"],
    }
}

/// The `run` argv for one service. Pure, so the properties that matter —
/// loopback-only publish, no bind mounts, the dispatch label, a resource
/// ceiling, and **no credential anywhere on the command line** — are asserted
/// without a container runtime.
///
/// # Why the env flags are BARE
///
/// `-e NAME` (no `=value`) means "inherit NAME from the invoking process's
/// environment", and both docker and podman implement it. That is not a style
/// choice here. An argv reaches two places a secret must never go:
///
/// 1. the host PROCESS TABLE, readable by every other session on the machine
///    for as long as the call runs, and
/// 2. this module's own ERROR STRINGS — which are logged to the runner's dev
///    log, streamed live to coord, and persisted by coord in the dispatch
///    result's `log_tail`. The triggers are ordinary: a port race, a tag that
///    does not resolve, a pull that exceeds its budget.
///
/// So the value goes through [`super::tools::run_capture_env`] instead, and
/// only the NAME is ever written down.
pub(crate) fn run_argv(
    spec: &ServiceSpec,
    image: &str,
    name: &str,
    dispatch_id: &str,
    host_port: u16,
) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--label".to_string(),
        format!("{DISPATCH_LABEL}={dispatch_id}"),
        // Loopback ONLY. `-p 5432:5432` would publish on every interface.
        "-p".to_string(),
        format!("{LOOPBACK}:{host_port}:{}", spec.container_port),
        // A ceiling, for the same reason every other stage of this lane has
        // one (`cargo_build_jobs`, `test_threads`, the dispatch Job Object's
        // committed-memory backstop): the machine is a contributor's, not a
        // disposable Actions VM, and this was the only provisioning stage
        // without a bound. The container is OUTSIDE the dispatch Job Object —
        // it belongs to the daemon — so the Job Object's limit does not cover
        // it and this flag is the only ceiling there is. Where a host cannot
        // enforce the limit (cgroup v1 without swap accounting) the runtime
        // prints a warning and continues; it is not a start failure.
        "--memory".to_string(),
        spec.memory_limit.to_string(),
        "--cpus".to_string(),
        spec.cpu_limit.to_string(),
    ];
    for name in container_env_names(spec.kind) {
        argv.push("-e".to_string());
        // BARE. The value is passed out of band; see this function's docs.
        argv.push((*name).to_string());
    }
    // No `-v`, no `--mount`: a dispatch service writes nowhere on the host.
    argv.push(image.to_string());
    argv
}

/// The removal argv. Force, because a running server will not stop on its own.
fn removal_argv(name: &str) -> Vec<String> {
    vec!["rm".to_string(), "-f".to_string(), name.to_string()]
}

/// Outcome of one readiness probe.
#[derive(Debug)]
enum ReadyState {
    Ready,
    /// Not yet — keep waiting until the budget runs out.
    NotYet(String),
    /// Waiting cannot help (the container is gone or exited): fail now rather
    /// than burn the whole readiness budget on a corpse.
    Fatal(String),
}

/// Every container one dispatch created, and the only thing that may remove
/// them.
///
/// Held by the executor's `DispatchWorkspace`, so teardown rides the same
/// single exit path the worktree cleanup already rides.
pub(crate) struct ServiceStack {
    dispatch_id: String,
    /// The runtime that created these containers. `None` until one is
    /// detected, which only happens when a manifest declares a service.
    runtime: Option<String>,
    /// Container names, registered BEFORE creation.
    containers: Vec<String>,
    /// Every removal command this stack actually ISSUED, recorded inside the
    /// removal loop.
    ///
    /// This exists because the alternative was a set of teardown tests that a
    /// regression could not fail: asserting "the vec is empty afterwards"
    /// passes just as well when the removal loop is deleted and only the
    /// `mem::take` remains. Recording the command binds the test to the
    /// behaviour instead of to the bookkeeping — and it is the ONLY
    /// CI-runnable detector, since the tests that drive a real daemon are
    /// `#[ignore]`d and CI has no Docker.
    removals: RemovalLog,
}

/// Shared handle so a caller that has already moved the stack (the executor's
/// `DispatchWorkspace::cleanup` consumes `self`) can still assert what was
/// removed.
pub(crate) type RemovalLog = std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>;

impl ServiceStack {
    pub(crate) fn new(dispatch_id: &str) -> Self {
        Self {
            dispatch_id: dispatch_id.to_string(),
            runtime: None,
            containers: Vec::new(),
            removals: RemovalLog::default(),
        }
    }

    /// Handle onto [`Self::removals`], for a caller that needs to assert what
    /// teardown did after the stack itself is gone.
    pub(crate) fn removal_log(&self) -> RemovalLog {
        std::sync::Arc::clone(&self.removals)
    }

    fn record_removal(&self, argv: &[String]) {
        if let Ok(mut log) = self.removals.lock() {
            log.push(argv.to_vec());
        }
    }

    /// Stand up every declared service and return the env the steps get.
    ///
    /// An empty declaration list is a true no-op: no runtime is probed, so a
    /// machine with no Docker runs every manifest that does not ask for one.
    pub(crate) async fn provision(
        &mut self,
        services: &[CiService],
        log: &mut (dyn FnMut(String) + Send),
        cancel: &CancellationToken,
    ) -> Result<Vec<(String, String)>, String> {
        self.provision_with(services, CONTAINER_RUNTIMES, READINESS_TIMEOUT, log, cancel)
            .await
    }

    /// [`Self::provision`] with the runtime candidates and readiness budget
    /// injected, so the refusal and the timeout are testable without a
    /// container runtime and without waiting minutes.
    pub(crate) async fn provision_with(
        &mut self,
        services: &[CiService],
        candidates: &[&str],
        ready_timeout: Duration,
        log: &mut (dyn FnMut(String) + Send),
        cancel: &CancellationToken,
    ) -> Result<Vec<(String, String)>, String> {
        if services.is_empty() {
            return Ok(Vec::new());
        }
        let runtime = detect_runtime(candidates, services).await?;
        log(format!(
            "[ci-node] container runtime: {runtime} — provisioning {} service(s)",
            services.len()
        ));
        self.runtime = Some(runtime.clone());

        let mut exports: Vec<(String, String)> = Vec::new();
        for declared in services {
            let spec = lookup(&declared.name).ok_or_else(|| {
                format!(
                    "service '{}' is not in the runner's registry",
                    declared.name
                )
            })?;
            let image = image_ref(spec, declared);
            let name = container_name(&self.dispatch_id, spec.name);
            let creds = ServiceCreds::generate();

            // A stale container from a crashed prior attempt at this dispatch
            // id would make `run --name` fail — the same reason
            // `prepare_worktree` clears a stale dispatch dir before re-adding.
            let _ = run_capture(&runtime, &["rm", "-f", &name], CONTAINER_CMD_TIMEOUT, None).await;

            // REGISTERED BEFORE CREATED. From here every exit path — a failed
            // run, a readiness timeout, a later service's failure, a failed
            // step, a cancel, a panic — tears this container down.
            self.containers.push(name.clone());

            let port = self
                .start(&runtime, spec, &image, &name, &creds, log)
                .await?;
            log(format!(
                "[ci-node] service {} ({}) started as {name} on {LOOPBACK}:{port} — waiting for readiness",
                spec.name, image
            ));
            self.wait_ready(&runtime, spec, &name, &creds, port, ready_timeout, cancel)
                .await?;
            log(format!(
                "[ci-node] service {} ready on {LOOPBACK}:{port}",
                spec.name
            ));
            exports.extend(exports_for(spec.kind, port, &creds));
        }
        Ok(exports)
    }

    /// Create the container, retrying on a fresh port when — and ONLY when —
    /// the chosen one was taken between the probe and the run.
    ///
    /// The retry is scoped to that one diagnosis on purpose. Retrying every
    /// failure would multiply the slowest failure by [`PORT_ATTEMPTS`]: a pull
    /// that hangs to its 900s budget would take 45 minutes to produce its loud
    /// error, which is indistinguishable from a wedged dispatch. Everything
    /// that is not a port collision fails on the first attempt.
    async fn start(
        &self,
        runtime: &str,
        spec: &ServiceSpec,
        image: &str,
        name: &str,
        creds: &ServiceCreds,
        log: &mut (dyn FnMut(String) + Send),
    ) -> Result<u16, String> {
        // The container env VALUES ride the child process's environment, never
        // its argv — see `run_argv`. This is the credential path.
        let child_env = container_env(spec.kind, creds);
        let mut last_err = String::new();
        for attempt in 1..=PORT_ATTEMPTS {
            let port = free_loopback_port()?;
            let argv = run_argv(spec, image, name, &self.dispatch_id, port);
            if attempt == 1 {
                log(format!(
                    "[ci-node] service {} — starting {image} (a first use pulls the image, which can take minutes)",
                    spec.name
                ));
            }
            match run_owned_env(runtime, &argv, CONTAINER_START_TIMEOUT, &child_env).await {
                Ok(_) => return Ok(port),
                Err(e) => {
                    warn!(
                        "ci_node: starting service {} attempt {attempt}/{PORT_ATTEMPTS} failed: {e}",
                        spec.name
                    );
                    let retryable = is_port_conflict(&e);
                    last_err = e;
                    // Whatever the runtime left behind under this name must go
                    // before the next attempt can reuse it.
                    let _ = run_capture(runtime, &["rm", "-f", name], CONTAINER_CMD_TIMEOUT, None)
                        .await;
                    if !retryable {
                        break;
                    }
                    log(format!(
                        "[ci-node] service {} — host port {port} was taken between reservation                          and start; retrying on a fresh port",
                        spec.name
                    ));
                }
            }
        }
        Err(format!(
            "service '{}' could not be started from {image}: {last_err}",
            spec.name
        ))
    }

    /// Block until the service answers, or fail loudly with the last probe
    /// output and the container's own logs.
    ///
    /// Three checks, cheapest-to-fail first:
    ///
    /// 1. the server answers its own protocol INSIDE the container;
    /// 2. (Postgres) the generated credential authenticates over TCP;
    /// 3. the PUBLISHED port is reachable from this process — the only one of
    ///    the three that exercises the path a step actually uses.
    #[allow(clippy::too_many_arguments)]
    async fn wait_ready(
        &self,
        runtime: &str,
        spec: &ServiceSpec,
        name: &str,
        creds: &ServiceCreds,
        host_port: u16,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let label = spec.name;
        let kind = spec.kind;
        let probe_argv = exec_argv(name, &readiness_argv(spec, creds));
        let result = wait_until_ready(label, timeout, READINESS_INTERVAL, cancel, move || {
            let argv = probe_argv.clone();
            async move {
                match container_is_running(runtime, name).await {
                    Ok(true) => {}
                    Ok(false) => {
                        return ReadyState::Fatal(format!(
                            "container {name} is no longer running — the image exited during \
                             startup"
                        ))
                    }
                    Err(e) if is_missing_container(&e) => {
                        // Removed out of band: waiting cannot bring it back, so
                        // fail now instead of burning the whole budget.
                        return ReadyState::Fatal(format!(
                            "container {name} no longer exists: {e}"
                        ));
                    }
                    Err(e) => return ReadyState::NotYet(format!("inspect failed: {e}")),
                }
                match run_owned(runtime, &argv, CONTAINER_CMD_TIMEOUT).await {
                    Ok(out) => match kind {
                        // redis-cli exits 0 even when it prints an error, so
                        // the answer itself is what is checked.
                        ServiceKind::Redis if !out.contains("PONG") => {
                            ReadyState::NotYet(format!("redis-cli ping answered {:?}", out.trim()))
                        }
                        _ => ReadyState::Ready,
                    },
                    Err(e) => ReadyState::NotYet(e),
                }
            }
        })
        .await;

        match result {
            Ok(()) => {}
            Err(e) => {
                let logs = container_logs(runtime, name).await;
                return Err(format!("{e}\n--- {name} logs (tail) ---\n{logs}"));
            }
        }

        // A real query before the gate opens. `pg_isready` proves a listener;
        // this proves the database answers a statement, which is what a step
        // needs.
        //
        // It is addressed to the container's OWN network address rather than to
        // 127.0.0.1, and that detail is the whole point. The official image's
        // initdb writes `host all all 127.0.0.1/32 trust` into `pg_hba.conf`
        // BEFORE the entrypoint appends its `scram-sha-256` line, so a query
        // over in-container loopback authenticates as TRUST: it would pass with
        // no password at all and prove nothing about the credential this
        // dispatch generated. The container's routable address matches the
        // appended rule instead, so a successful query means the password the
        // steps were handed actually authenticates.
        if spec.kind == ServiceKind::Postgres {
            match container_ip(runtime, name).await {
                Ok(ip) => {
                    let mut argv = vec![
                        "exec".to_string(),
                        "-e".to_string(),
                        "PGPASSWORD".to_string(), // BARE — value passed below.
                        name.to_string(),
                        "psql".to_string(),
                        "-U".to_string(),
                        creds.user.clone(),
                        "-d".to_string(),
                        creds.database.clone(),
                        "-h".to_string(),
                        ip,
                        "-tAc".to_string(),
                        "SELECT 1".to_string(),
                    ];
                    argv.shrink_to_fit();
                    run_owned_env(
                        runtime,
                        &argv,
                        CONTAINER_CMD_TIMEOUT,
                        &[("PGPASSWORD".to_string(), creds.password.clone())],
                    )
                    .await
                    .map_err(|e| {
                        format!(
                            "service '{}' reported ready but did not answer an authenticated \
                             SELECT 1 over TCP: {e}",
                            spec.name
                        )
                    })?;
                }
                Err(e) => {
                    // A DISCLOSED degrade, not a silent skip: a rootless setup
                    // may expose no routable container address. Fall back to the
                    // loopback query — which still proves the database answers —
                    // and say plainly what was not proven.
                    warn!(
                        "ci_node: no container network address for {name} ({e}) — the Postgres \
                         credential cannot be verified end-to-end on this runtime; falling back \
                         to an in-container loopback query, which its pg_hba trust rule will \
                         satisfy without a password"
                    );
                    let confirm = vec![
                        "psql".to_string(),
                        "-U".to_string(),
                        creds.user.clone(),
                        "-d".to_string(),
                        creds.database.clone(),
                        "-h".to_string(),
                        LOOPBACK.to_string(),
                        "-tAc".to_string(),
                        "SELECT 1".to_string(),
                    ];
                    run_owned(runtime, &exec_argv(name, &confirm), CONTAINER_CMD_TIMEOUT)
                        .await
                        .map_err(|e| {
                            format!(
                                "service '{}' reported ready but did not answer a SELECT 1: {e}",
                                spec.name
                            )
                        })?;
                }
            }
        }

        // Last, the only check that exercises the path a STEP uses.
        wait_host_port_reachable(spec.name, host_port, HOST_PORT_TIMEOUT, cancel).await?;
        Ok(())
    }

    /// Remove every container this dispatch created. Best-effort and
    /// idempotent: failures are logged, never propagated, because this runs on
    /// failure paths too, and the list is emptied either way so a later
    /// [`Drop`] does not try again.
    pub(crate) async fn teardown(&mut self) {
        let containers = std::mem::take(&mut self.containers);
        if containers.is_empty() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            // Registered but never created (the runtime went away mid-dispatch)
            // — nothing to remove and no way to try.
            warn!(
                "ci_node: {} service container(s) recorded for dispatch {} with no runtime — \
                 nothing to remove",
                containers.len(),
                self.dispatch_id
            );
            return;
        };
        for name in containers {
            let argv = removal_argv(&name);
            self.record_removal(&argv);
            match run_owned(&runtime, &argv, CONTAINER_CMD_TIMEOUT).await {
                Ok(_) => info!("ci_node: removed service container {name}"),
                Err(e) => warn!("ci_node: removing service container {name} failed: {e}"),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(dispatch_id: &str, runtime: Option<&str>, containers: &[&str]) -> Self {
        Self {
            dispatch_id: dispatch_id.to_string(),
            runtime: runtime.map(str::to_string),
            containers: containers.iter().map(|c| (*c).to_string()).collect(),
            removals: RemovalLog::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn container_names(&self) -> &[String] {
        &self.containers
    }
}

impl Drop for ServiceStack {
    /// Last-resort reaper for the one path the async teardown cannot cover: a
    /// panic unwinding out of the dispatch task. A normal teardown empties the
    /// list, so this is a no-op on every ordinary run. It blocks on purpose —
    /// a leaked container outlives the runner, a BOUNDED wait does not. It
    /// cannot help if the PROCESS is killed; see the module docs.
    fn drop(&mut self) {
        let containers = std::mem::take(&mut self.containers);
        if containers.is_empty() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        warn!(
            "ci_node: dispatch {} dropped with {} service container(s) still up — reaping              synchronously",
            self.dispatch_id,
            containers.len()
        );
        for name in containers {
            let argv = removal_argv(&name);
            self.record_removal(&argv);
            if let Err(e) = reap_blocking(&runtime, &argv, DROP_REAP_BUDGET) {
                warn!("ci_node: last-resort reap of {name} failed: {e}");
            }
        }
    }
}

/// Synchronous, WALL-CLOCK-BOUNDED container removal for [`ServiceStack::drop`],
/// which runs where no async runtime can be awaited.
///
/// Two properties the naive `Command::status()` does not have, both taken from
/// this fleet's own failure modes:
///
/// * **A budget.** `status()` waits forever, and a container daemon that is up,
///   listening and DEAF is a documented state here. Blocking a worker thread
///   permanently to reap one container trades a leak for something worse. The
///   child is killed and abandoned when the budget expires, and the failure is
///   logged rather than swallowed.
/// * **The Windows shim fallback.** `std::process::Command` cannot launch a
///   `.cmd`/`.bat`, which is precisely why the async path shares
///   `tools::run_capture`. Without the same fallback here, the reaper would be
///   a silent no-op on exactly the machines whose runtime is a shim — the leak
///   it exists to prevent, wearing a success.
fn reap_blocking(runtime: &str, argv: &[String], budget: Duration) -> Result<(), String> {
    fn spawn(program: &str, argv: &[String]) -> std::io::Result<std::process::Child> {
        crate::process_helpers::no_window(program)
            .args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    }
    let mut child = match spawn(runtime, argv) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && cfg!(target_os = "windows") => {
            let mut shim_argv = vec!["/C".to_string(), runtime.to_string()];
            shim_argv.extend(argv.iter().cloned());
            spawn("cmd.exe", &shim_argv)
                .map_err(|e2| format!("spawn {runtime} (direct: {e}; cmd /C: {e2})"))?
        }
        Err(e) => return Err(format!("spawn {runtime}: {e}")),
    };
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{runtime} {argv:?} exited {status}")),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!(
                        "{runtime} did not answer {argv:?} within {}s — abandoning the reap                          rather than blocking this thread forever. The container may survive;                          it carries the {DISPATCH_LABEL} label",
                        budget.as_secs()
                    ));
                }
                std::thread::sleep(DROP_REAP_POLL);
            }
            Err(e) => return Err(format!("wait on {runtime}: {e}")),
        }
    }
}

/// Locate a container runtime. A miss is a REFUSAL naming what was tried and
/// what is missing — never a skip, and never a fallback to "the database is
/// probably running already".
async fn detect_runtime(candidates: &[&str], services: &[CiService]) -> Result<String, String> {
    let mut tried = Vec::new();
    for candidate in candidates {
        // `info` (not `--version`) on purpose: it answers only when the DAEMON
        // is reachable, so "CLI installed, Docker Desktop not started" is
        // reported as what it is instead of passing detection and failing at
        // `run`.
        match run_capture(
            candidate,
            &["info", "--format", "{{.ServerVersion}}"],
            RUNTIME_PROBE_TIMEOUT,
            None,
        )
        .await
        {
            Ok(text) => {
                info!(
                    "ci_node: using container runtime {candidate} (server {})",
                    text.trim()
                );
                return Ok((*candidate).to_string());
            }
            Err(e) => tried.push(format!("{candidate}: {e}")),
        }
    }
    let declared: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    Err(format!(
        "no container runtime is available on this machine, but this dispatch declares \
         {} service(s) ({}). Tried {candidates:?} — {}. Install Docker (or podman) and make \
         sure its daemon is running, or remove the [[services]] entries from the manifest. \
         The runner will NEVER skip a declared service: a step that ran without its database \
         would turn a database-backed gate green while testing nothing",
        services.len(),
        declared.join(", "),
        tried.join("; ")
    ))
}

/// Poll `probe` until it reports ready, the budget runs out, the dispatch is
/// cancelled, or the probe declares waiting pointless.
async fn wait_until_ready<F, Fut>(
    label: &str,
    timeout: Duration,
    interval: Duration,
    cancel: &CancellationToken,
    mut probe: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ReadyState>,
{
    let started = Instant::now();
    let mut last = "(no probe completed)".to_string();
    loop {
        if cancel.is_cancelled() {
            return Err(format!(
                "service '{label}' abandoned: the dispatch was cancelled while waiting for it \
                 to become ready"
            ));
        }
        match probe().await {
            ReadyState::Ready => return Ok(()),
            ReadyState::NotYet(why) => last = why,
            ReadyState::Fatal(why) => {
                return Err(format!(
                    "service '{label}' cannot become ready: {why}. Failing the dispatch rather \
                     than running steps against a server that is not there"
                ))
            }
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "service '{label}' did not become ready within {}s — last probe: {last}. \
                 Failing the dispatch rather than running steps against a server that has not \
                 answered",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(interval).await;
    }
}

/// The readiness command, run INSIDE the container so no host client is
/// required.
fn readiness_argv(spec: &ServiceSpec, creds: &ServiceCreds) -> Vec<String> {
    match spec.kind {
        // `-h 127.0.0.1` is load-bearing: the entrypoint's initdb bootstrap
        // runs a temporary server on a unix socket only, and a socket-probing
        // pg_isready would report ready before the real server ever listens.
        ServiceKind::Postgres => vec![
            "pg_isready".to_string(),
            "-h".to_string(),
            LOOPBACK.to_string(),
            "-p".to_string(),
            spec.container_port.to_string(),
            "-U".to_string(),
            creds.user.clone(),
            "-d".to_string(),
            creds.database.clone(),
        ],
        ServiceKind::Redis => vec!["redis-cli".to_string(), "ping".to_string()],
    }
}

/// `exec <name> <argv…>` — the readiness probe runs INSIDE the container, so a
/// host `psql`/`redis-cli` is never required.
fn exec_argv(name: &str, argv: &[String]) -> Vec<String> {
    let mut out = vec!["exec".to_string(), name.to_string()];
    out.extend(argv.iter().cloned());
    out
}

/// [`run_capture`] over an owned argv. Every container command in this module
/// is built as `Vec<String>` (so it is a pure, testable value) and borrowed
/// only here.
async fn run_owned(runtime: &str, argv: &[String], timeout: Duration) -> Result<String, String> {
    run_owned_env(runtime, argv, timeout, &[]).await
}

/// [`run_owned`] with environment variables for the child. This is how a
/// credential reaches a container: the argv carries `-e NAME`, the value
/// arrives here, and nothing quotable ever holds the secret.
async fn run_owned_env(
    runtime: &str,
    argv: &[String],
    timeout: Duration,
    envs: &[(String, String)],
) -> Result<String, String> {
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    super::tools::run_capture_env(runtime, &args, timeout, None, envs).await
}

/// Does this `run` failure mean the host port was taken between reservation
/// and start? Pure, so the retry policy is testable without a daemon.
///
/// Matched on the messages docker and podman actually emit. Anything else is
/// NOT retried: see [`ServiceStack::start`] for why a blanket retry is worse
/// than no retry.
fn is_port_conflict(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("port is already allocated")
        || lower.contains("address already in use")
        || lower.contains("bind for ")
        || lower.contains("rootlessport")
}

/// Does this error mean the container is GONE rather than merely unhealthy?
/// A removed-out-of-band container can never become ready, so waiting out the
/// full readiness budget for it is pure delay.
fn is_missing_container(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("no such container")
        || lower.contains("no such object")
        || lower.contains("not found")
}

/// Can a STEP actually reach this service?
///
/// The in-container probes prove the server is up; they say nothing about the
/// published port, and a step connects from the HOST through the runtime's
/// port-forwarding path (on Windows, Docker Desktop's proxy — a recurring flake
/// on this platform). If publish binds but does not forward, every in-container
/// probe passes, the gate opens green, and each step fails with a connection
/// error that reads like a bug in the code under test. That is the lying-gate
/// outcome this module exists to prevent, arriving through a different door, so
/// readiness is not declared until a connection from THIS process succeeds.
async fn wait_host_port_reachable(
    label: &str,
    port: u16,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), String> {
    wait_until_ready(label, timeout, HOST_PORT_INTERVAL, cancel, move || async move {
        match tokio::time::timeout(
            HOST_PORT_CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect((LOOPBACK, port)),
        )
        .await
        {
            Ok(Ok(_stream)) => ReadyState::Ready,
            Ok(Err(e)) => ReadyState::NotYet(format!("connect {LOOPBACK}:{port}: {e}")),
            Err(_) => ReadyState::NotYet(format!(
                "connect {LOOPBACK}:{port} did not answer within {}s",
                HOST_PORT_CONNECT_TIMEOUT.as_secs()
            )),
        }
    })
    .await
    .map_err(|e| {
        format!(
            "{e}. The container reported ready but its PUBLISHED port is not reachable from the              host, so every step would fail with a connection error attributed to the code under              test. This is the runtime's port-forwarding path, not the server"
        )
    })
}

async fn container_is_running(runtime: &str, name: &str) -> Result<bool, String> {
    let out = run_capture(
        runtime,
        &["inspect", "-f", "{{.State.Running}}", name],
        CONTAINER_CMD_TIMEOUT,
        None,
    )
    .await?;
    Ok(out.trim().eq_ignore_ascii_case("true"))
}

/// The container's own routable address. Used to reach Postgres by a route that
/// is NOT in-container loopback, so the image's `pg_hba` trust rule for
/// 127.0.0.1 cannot mask a credential failure.
async fn container_ip(runtime: &str, name: &str) -> Result<String, String> {
    let out = run_capture(
        runtime,
        &[
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
            name,
        ],
        CONTAINER_CMD_TIMEOUT,
        None,
    )
    .await?;
    out.split_whitespace()
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{name} reports no container network address"))
}

async fn container_logs(runtime: &str, name: &str) -> String {
    match run_capture(
        runtime,
        &["logs", "--tail", LOG_TAIL_LINES, name],
        CONTAINER_CMD_TIMEOUT,
        None,
    )
    .await
    {
        Ok(text) => text.trim().to_string(),
        Err(e) => format!("(logs unavailable: {e})"),
    }
}

/// Pick a free loopback port by binding `:0` and releasing it. The gap between
/// release and `run` is a real race, which is why [`ServiceStack::start`]
/// retries on a fresh port rather than trusting the first answer.
fn free_loopback_port() -> Result<u16, String> {
    let listener = TcpListener::bind((LOOPBACK, 0))
        .map_err(|e| format!("could not reserve a loopback port for a service container: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("could not read the reserved port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(name: &str, version: &str) -> CiService {
        CiService {
            name: name.to_string(),
            version: version.to_string(),
            digest: None,
        }
    }

    fn creds() -> ServiceCreds {
        ServiceCreds {
            user: "qontinui_ci".to_string(),
            password: "s3cret".to_string(),
            database: "qontinui_ci".to_string(),
        }
    }

    /// The registry is CLOSED: a manifest names an entry, and neither an image
    /// reference nor a registry host is a name.
    #[test]
    fn registry_is_closed() {
        assert!(lookup("postgres").is_some());
        assert!(lookup("postgres-pgvector").is_some());
        assert!(lookup("redis").is_some());
        assert!(lookup("").is_none());
        assert!(lookup("pgvector/pgvector:pg16").is_none());
        assert!(lookup("ghcr.io/evil/miner").is_none());
        assert_eq!(
            known_service_names(),
            vec!["postgres", "postgres-pgvector", "redis"]
        );
    }

    /// A digest wins over the tag, because a tag is a mutable pointer and a
    /// digest is not.
    #[test]
    fn image_ref_prefers_the_digest() {
        let spec = lookup("postgres-pgvector").unwrap();
        assert_eq!(
            image_ref(spec, &declared("postgres-pgvector", "pg16")),
            "pgvector/pgvector:pg16"
        );
        let mut pinned = declared("postgres-pgvector", "pg16");
        pinned.digest = Some(format!("sha256:{}", "a".repeat(64)));
        assert_eq!(
            image_ref(spec, &pinned),
            format!("pgvector/pgvector@sha256:{}", "a".repeat(64))
        );
    }

    /// Container names are dispatch-scoped exactly the way the worktree
    /// directory is, so one dispatch can never name (and therefore never
    /// remove) another's container.
    #[test]
    fn container_names_are_dispatch_scoped() {
        let a = container_name("d-123", "redis");
        let b = container_name("d-124", "redis");
        assert!(a.contains("d-123"));
        assert_ne!(a, b);
        assert_ne!(a, container_name("d-123", "postgres"));
    }

    /// The blast radius, asserted on the argv: loopback-only publish, the
    /// dispatch label, a resource ceiling, and NO bind mount or volume.
    #[test]
    fn run_argv_publishes_on_loopback_only_and_mounts_nothing() {
        let spec = lookup("postgres-pgvector").unwrap();
        let argv = run_argv(
            spec,
            "pgvector/pgvector:pg16",
            "qontinui-ci-d-123-postgres-pgvector",
            "d-123",
            51999,
        );
        let publish = argv
            .iter()
            .position(|a| a == "-p")
            .map(|i| argv[i + 1].clone())
            .expect("a published port");
        assert_eq!(publish, "127.0.0.1:51999:5432");
        assert!(!publish.starts_with("0.0.0.0"));
        assert!(argv.contains(&format!("{DISPATCH_LABEL}=d-123")));
        for banned in ["-v", "--volume", "--mount", "--privileged", "--network"] {
            assert!(
                !argv.iter().any(|a| a == banned),
                "run argv must not contain {banned}: {argv:?}"
            );
        }
        // Services are the only provisioning stage that leaves something
        // RUNNING, so they carry a ceiling like every other stage of this lane.
        assert!(argv.contains(&"--memory".to_string()));
        assert!(argv.contains(&"--cpus".to_string()));
        // The image is the last token.
        assert_eq!(argv.last().unwrap(), "pgvector/pgvector:pg16");
    }

    /// THE CREDENTIAL NEVER TOUCHES AN ARGV.
    ///
    /// An argv reaches the host process table and — via this module's error
    /// strings — the runner's dev log, the live progress stream, and the
    /// `log_tail` coord PERSISTS with the dispatch result. So the env flags are
    /// emitted in the bare `-e NAME` (inherit-from-my-environment) form and the
    /// values travel through the child's environment instead.
    #[test]
    fn credentials_never_appear_on_the_command_line() {
        let creds = ServiceCreds {
            user: "qontinui_ci".to_string(),
            password: "correcthorsebatterystaple".to_string(),
            database: "qontinui_ci".to_string(),
        };
        for entry in ["postgres", "postgres-pgvector", "redis"] {
            let spec = lookup(entry).unwrap();
            let argv = run_argv(spec, "img:1", "c", "d-123", 51999);
            assert!(
                !argv.iter().any(|a| a.contains(&creds.password)),
                "{entry}: the password reached the argv: {argv:?}"
            );
            // No `KEY=VALUE` env token at all — the bare form is the contract.
            for (i, tok) in argv.iter().enumerate() {
                if tok == "-e" {
                    let value = &argv[i + 1];
                    assert!(
                        !value.contains('='),
                        "{entry}: env flag must be bare, got {value:?}"
                    );
                }
            }
            // …and the values are still delivered, out of band.
            let env = container_env(spec.kind, &creds);
            let names: Vec<&str> = container_env_names(spec.kind).to_vec();
            assert_eq!(env.len(), names.len());
            for (k, _) in &env {
                assert!(names.contains(&k.as_str()), "{k} has no bare flag");
            }
            if spec.kind == ServiceKind::Postgres {
                assert!(env
                    .iter()
                    .any(|(k, v)| k == "POSTGRES_PASSWORD" && v == &creds.password));
            }
        }
    }

    /// Only a port collision is retried. A blanket retry would multiply the
    /// SLOWEST failure by `PORT_ATTEMPTS` — a hung pull would take 45 minutes
    /// to produce its loud error.
    #[test]
    fn only_a_port_collision_is_retried() {
        for retryable in [
            "docker: Error response from daemon: driver failed programming external connectivity: \
             Bind for 127.0.0.1:51999 failed: port is already allocated",
            "address already in use",
            "rootlessport cannot expose privileged port",
        ] {
            assert!(is_port_conflict(retryable), "should retry: {retryable}");
        }
        for fatal in [
            "Unable to find image 'postgres:16-nope' locally / not found",
            "docker did not answer within 900s",
            "permission denied while trying to connect to the Docker daemon socket",
        ] {
            assert!(!is_port_conflict(fatal), "must NOT retry: {fatal}");
        }
    }

    /// A container removed out of band can never become ready, so it hits the
    /// fast Fatal path rather than the full readiness budget.
    #[test]
    fn a_vanished_container_is_diagnosed_not_waited_out() {
        for gone in [
            "Error: No such container: qontinui-ci-d-1-redis",
            "Error: no such object: qontinui-ci-d-1-redis",
            "Error response from daemon: not found",
        ] {
            assert!(is_missing_container(gone), "should be fatal: {gone}");
        }
        assert!(!is_missing_container(
            "container is restarting, wait until it is running"
        ));
    }

    /// Steps see IPv4 loopback and the EPHEMERAL port — never `localhost`
    /// (which resolves to `::1` first on Windows) and never the well-known
    /// port (which the user's own database may already hold).
    #[test]
    fn exports_name_ipv4_loopback_and_the_ephemeral_port() {
        let pg = exports_for(ServiceKind::Postgres, 51999, &creds());
        let url = pg
            .iter()
            .find(|(k, _)| k == "DATABASE_URL")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(
            url,
            "postgresql://qontinui_ci:s3cret@127.0.0.1:51999/qontinui_ci"
        );
        assert!(!url.contains("localhost"));
        assert!(!url.contains(":5432/"));
        for key in ["PGHOST", "PGPORT", "PGUSER", "PGPASSWORD", "PGDATABASE"] {
            assert!(pg.iter().any(|(k, _)| k == key), "missing {key}");
        }
        let redis = exports_for(ServiceKind::Redis, 6399, &creds());
        assert_eq!(
            redis
                .iter()
                .find(|(k, _)| k == "REDIS_URL")
                .map(|(_, v)| v.as_str()),
            Some("redis://127.0.0.1:6399")
        );
        // Every exported key is declared to the manifest validator, so a step
        // trying to set one gets the pointer message instead of a silent
        // override.
        for (k, _) in pg.iter().chain(redis.iter()) {
            let kind = if k.starts_with("REDIS") {
                ServiceKind::Redis
            } else {
                ServiceKind::Postgres
            };
            assert!(
                exported_env_keys(kind).contains(&k.as_str()),
                "{k} is exported but not declared executor-owned"
            );
        }
    }

    /// The generated password is not a constant, and it is long enough not to
    /// be guessable by a local process racing the port.
    #[test]
    fn credentials_are_generated_per_dispatch() {
        let a = ServiceCreds::generate();
        let b = ServiceCreds::generate();
        assert_ne!(a.password, b.password);
        assert_eq!(a.password.len(), 32);
        assert_eq!(a.user, b.user);
    }

    /// Postgres readiness probes TCP, not the unix socket the initdb bootstrap
    /// briefly answers on — otherwise the gate opens on a server that is about
    /// to be shut down and restarted.
    #[test]
    fn postgres_readiness_probes_tcp_not_the_bootstrap_socket() {
        let spec = lookup("postgres").unwrap();
        let argv = readiness_argv(spec, &creds());
        assert_eq!(argv[0], "pg_isready");
        assert!(argv.contains(&"-h".to_string()) && argv.contains(&LOOPBACK.to_string()));
        assert!(argv.contains(&"5432".to_string()));
        let redis = readiness_argv(lookup("redis").unwrap(), &creds());
        assert_eq!(redis, vec!["redis-cli".to_string(), "ping".to_string()]);
        // The probe runs inside the container, so no host client is needed.
        let execed = exec_argv("c1", &redis);
        assert_eq!(execed[0], "exec");
        assert_eq!(execed[1], "c1");
        assert_eq!(execed[2], "redis-cli");
    }

    /// NO CONTAINER RUNTIME IS A REFUSAL. The message names what was tried,
    /// what is missing, and what was declared — and it is an `Err`, so the
    /// dispatch cannot proceed to run steps against a database that is not
    /// there.
    #[tokio::test]
    async fn missing_container_runtime_is_a_refusal_not_a_skip() {
        let mut stack = ServiceStack::new("d-refusal");
        let mut lines: Vec<String> = Vec::new();
        let err = stack
            .provision_with(
                &[
                    declared("postgres-pgvector", "pg16"),
                    declared("redis", "7-alpine"),
                ],
                &["qontinui-no-such-container-runtime"],
                Duration::from_millis(10),
                &mut |l| lines.push(l),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a missing runtime must be a refusal, never an Ok(no services)");
        assert!(err.contains("no container runtime"), "got: {err}");
        assert!(
            err.contains("qontinui-no-such-container-runtime"),
            "the refusal must name what was tried: {err}"
        );
        assert!(
            err.contains("postgres-pgvector") && err.contains("redis"),
            "got: {err}"
        );
        assert!(err.contains("NEVER skip"), "got: {err}");
        assert!(err.contains("Install Docker"), "got: {err}");
        // Nothing was created, so nothing is pending teardown.
        assert!(stack.container_names().is_empty());
    }

    /// A manifest that declares no services never probes for a runtime — a
    /// machine with no Docker keeps running every manifest that does not ask
    /// for one.
    #[tokio::test]
    async fn no_services_declared_needs_no_runtime() {
        let mut stack = ServiceStack::new("d-empty");
        let exports = stack
            .provision_with(
                &[],
                &["qontinui-no-such-container-runtime"],
                Duration::from_millis(10),
                &mut |_l| {},
                &CancellationToken::new(),
            )
            .await
            .expect("no declaration ⇒ no runtime needed");
        assert!(exports.is_empty());
        assert!(stack.container_names().is_empty());
    }

    /// The health gate FAILS LOUDLY on timeout, carrying the last probe
    /// output — it never opens on a server that has not answered.
    #[tokio::test]
    async fn readiness_timeout_fails_loudly() {
        let err = wait_until_ready(
            "postgres",
            Duration::from_millis(120),
            Duration::from_millis(20),
            &CancellationToken::new(),
            || async { ReadyState::NotYet("connection refused".to_string()) },
        )
        .await
        .expect_err("an unready service must fail the dispatch");
        assert!(err.contains("did not become ready within"), "got: {err}");
        assert!(err.contains("connection refused"), "got: {err}");
        assert!(err.contains("postgres"), "got: {err}");
    }

    /// A ready service opens the gate on the first successful probe.
    #[tokio::test]
    async fn readiness_returns_as_soon_as_the_service_answers() {
        let started = Instant::now();
        wait_until_ready(
            "redis",
            Duration::from_secs(30),
            Duration::from_secs(5),
            &CancellationToken::new(),
            || async { ReadyState::Ready },
        )
        .await
        .expect("a ready service must not wait");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A dead container does not burn the whole readiness budget.
    #[tokio::test]
    async fn a_dead_container_fails_immediately_rather_than_waiting_out_the_budget() {
        let started = Instant::now();
        let err = wait_until_ready(
            "postgres",
            Duration::from_secs(30),
            Duration::from_secs(5),
            &CancellationToken::new(),
            || async { ReadyState::Fatal("container exited during startup".to_string()) },
        )
        .await
        .expect_err("a dead container must fail, not wait");
        assert!(err.contains("cannot become ready"), "got: {err}");
        assert!(err.contains("exited during startup"), "got: {err}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A cancelled dispatch stops waiting immediately.
    #[tokio::test]
    async fn cancellation_stops_the_health_gate() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = wait_until_ready(
            "redis",
            Duration::from_secs(30),
            Duration::from_secs(5),
            &cancel,
            || async { ReadyState::NotYet("not yet".to_string()) },
        )
        .await
        .expect_err("a cancelled dispatch must stop waiting");
        assert!(err.contains("cancelled"), "got: {err}");
    }

    /// Teardown ISSUES a forced removal for exactly the containers this
    /// dispatch registered — asserted on the commands, not on the bookkeeping.
    ///
    /// Asserting "the vec is empty afterwards" would pass just as well if the
    /// removal loop were deleted and only the `mem::take` remained, which makes
    /// it no regression detector at all. The recorded commands bind the test to
    /// the behaviour, and they are the only detector CI can run: the tests that
    /// drive a real daemon are `#[ignore]`d and CI has no Docker.
    ///
    /// The runtime named here does not exist, which also pins best-effort: a
    /// removal that cannot run is not an error the caller sees, and it must
    /// still empty the stack so `Drop` does not try again.
    #[tokio::test]
    async fn teardown_issues_a_forced_removal_for_every_container() {
        let mut stack = ServiceStack::for_test(
            "d-teardown",
            Some("qontinui-no-such-container-runtime"),
            &[
                "qontinui-ci-d-teardown-redis",
                "qontinui-ci-d-teardown-postgres",
            ],
        );
        let log = stack.removal_log();
        assert_eq!(stack.container_names().len(), 2);

        stack.teardown().await;

        let issued = log.lock().unwrap().clone();
        assert_eq!(
            issued,
            vec![
                vec![
                    "rm".to_string(),
                    "-f".to_string(),
                    "qontinui-ci-d-teardown-redis".to_string()
                ],
                vec![
                    "rm".to_string(),
                    "-f".to_string(),
                    "qontinui-ci-d-teardown-postgres".to_string()
                ],
            ],
            "teardown must issue a forced removal per registered container"
        );
        assert!(
            stack.container_names().is_empty(),
            "teardown must empty the stack even when the removal command fails"
        );

        // Idempotent: a second teardown issues nothing new, and neither does
        // the Drop that follows.
        stack.teardown().await;
        assert_eq!(log.lock().unwrap().len(), 2);
        drop(stack);
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    /// The removal is a FORCED remove by exact name — never a filter, never a
    /// prune, so nothing outside this dispatch is reachable from it.
    #[test]
    fn removal_targets_one_container_by_exact_name() {
        let argv = removal_argv("qontinui-ci-d-123-redis");
        assert_eq!(argv, vec!["rm", "-f", "qontinui-ci-d-123-redis"]);
        for dangerous in ["prune", "--all", "-a", "--filter", "system"] {
            assert!(
                !argv.iter().any(|a| a == dangerous),
                "{dangerous} in {argv:?}"
            );
        }
    }

    /// A stack that still holds containers is not silently forgotten when it is
    /// dropped without a teardown — the reaper ISSUES the removal. Asserted on
    /// the command, because "does not panic" is satisfied by an empty `drop`.
    #[test]
    fn dropping_a_live_stack_reaps_rather_than_leaking() {
        let stack = ServiceStack::for_test(
            "d-drop",
            Some("qontinui-no-such-container-runtime"),
            &["qontinui-ci-d-drop-redis"],
        );
        let log = stack.removal_log();
        let started = Instant::now();
        drop(stack);
        assert_eq!(
            log.lock().unwrap().clone(),
            vec![vec![
                "rm".to_string(),
                "-f".to_string(),
                "qontinui-ci-d-drop-redis".to_string()
            ]],
            "the last-resort reaper must actually issue the removal"
        );
        // And it is BOUNDED: a wedged daemon must not own this thread forever.
        assert!(started.elapsed() < DROP_REAP_BUDGET);
    }

    /// The blocking reaper honours its wall-clock budget rather than waiting
    /// forever on a daemon that is up, listening and deaf.
    #[test]
    fn the_blocking_reaper_is_bounded() {
        // A program that exists on every supported host and never exits on its
        // own: the platform sleep, asked for far longer than the budget.
        #[cfg(target_os = "windows")]
        let (program, argv) = (
            "powershell",
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Start-Sleep -Seconds 120".to_string(),
            ],
        );
        #[cfg(not(target_os = "windows"))]
        let (program, argv) = ("sleep", vec!["120".to_string()]);

        let started = Instant::now();
        let err = reap_blocking(program, &argv, Duration::from_millis(400))
            .expect_err("a command that outlives the budget must be abandoned");
        assert!(err.contains("did not answer"), "got: {err}");
        assert!(err.contains(DISPATCH_LABEL), "got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "reaper blocked for {:?}",
            started.elapsed()
        );
    }

    /// The host-side gate: readiness is not declared until a connection from
    /// THIS process succeeds, because that is the path a step uses.
    #[tokio::test]
    async fn host_port_gate_passes_on_a_listener_and_fails_loudly_without_one() {
        let listener = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        wait_host_port_reachable(
            "redis",
            port,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .expect("a bound port must satisfy the host gate");

        // Nothing listening: the gate must FAIL, naming the forwarding path —
        // never open green on a port a step cannot reach.
        drop(listener);
        let dead = free_loopback_port().unwrap();
        let err = wait_host_port_reachable(
            "postgres",
            dead,
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .await
        .expect_err("an unreachable published port must fail the dispatch");
        assert!(err.contains("did not become ready"), "got: {err}");
        assert!(
            err.contains("PUBLISHED port is not reachable"),
            "got: {err}"
        );
        assert!(err.contains(&dead.to_string()), "got: {err}");
    }

    // ── Real container runtime ────────────────────────────────────────────
    //
    // `#[ignore]`d because it requires Docker/podman and a network-or-cached
    // image. Run explicitly:
    //   cargo test --bin qontinui-runner ci_node::services::tests::real_ -- \
    //       --ignored --nocapture
    //
    // It is dispatch-id-scoped like every other dispatch, so it can never
    // touch a container it did not create.

    #[tokio::test]
    #[ignore = "requires a container runtime"]
    async fn real_container_lifecycle_starts_gates_and_tears_down() {
        let dispatch_id = format!("t{}", rand::random::<u32>());
        let mut lines: Vec<String> = Vec::new();
        let mut stack = ServiceStack::new(&dispatch_id);
        let exports = stack
            .provision_with(
                &[
                    declared("redis", "7-alpine"),
                    declared("postgres-pgvector", "pg16"),
                ],
                CONTAINER_RUNTIMES,
                Duration::from_secs(180),
                &mut |l| {
                    println!("{l}");
                    lines.push(l);
                },
                &CancellationToken::new(),
            )
            .await
            .expect("both services must come up on a machine with a runtime");

        let url = exports
            .iter()
            .find(|(k, _)| k == "DATABASE_URL")
            .map(|(_, v)| v.clone())
            .expect("DATABASE_URL");
        // KEYS only. Printing the value would put a live credential in a test
        // log for no diagnostic gain — the same discipline this module applies
        // to its own error strings.
        let keys: Vec<&str> = exports.iter().map(|(k, _)| k.as_str()).collect();
        println!("exported keys: {keys:?}");
        assert!(url.starts_with("postgresql://qontinui_ci:"));
        assert_eq!(stack.container_names().len(), 2);

        // The password the steps were handed is NOT on any command line: it
        // reached the container through the child's environment.
        let password = url
            .trim_start_matches("postgresql://qontinui_ci:")
            .split('@')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(password.len(), 32);

        let log = stack.removal_log();
        stack.teardown().await;
        assert!(stack.container_names().is_empty());
        assert_eq!(
            log.lock().unwrap().len(),
            2,
            "teardown must issue one removal per container"
        );

        // Nothing labelled with this dispatch survives.
        let leftovers = run_capture(
            "docker",
            &[
                "ps",
                "-aq",
                "--filter",
                &format!("label={DISPATCH_LABEL}={dispatch_id}"),
            ],
            CONTAINER_CMD_TIMEOUT,
            None,
        )
        .await
        .expect("ps must answer");
        assert!(
            leftovers.trim().is_empty(),
            "containers leaked: {leftovers:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires a container runtime"]
    async fn real_failed_provisioning_still_registers_the_started_container_for_teardown() {
        let dispatch_id = format!("t{}", rand::random::<u32>());
        let mut stack = ServiceStack::new(&dispatch_id);
        // First service starts; the second names a tag that cannot resolve.
        let err = stack
            .provision_with(
                &[
                    declared("redis", "7-alpine"),
                    declared("postgres", "16-qontinui-does-not-exist"),
                ],
                CONTAINER_RUNTIMES,
                Duration::from_secs(180),
                &mut |l| println!("{l}"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("an unresolvable image must fail the dispatch");
        println!("refusal: {err}");
        assert_eq!(
            stack.container_names().len(),
            2,
            "both the started and the attempted container must be registered for teardown"
        );
        stack.teardown().await;
        let leftovers = run_capture(
            "docker",
            &[
                "ps",
                "-aq",
                "--filter",
                &format!("label={DISPATCH_LABEL}={dispatch_id}"),
            ],
            CONTAINER_CMD_TIMEOUT,
            None,
        )
        .await
        .expect("ps must answer");
        assert!(
            leftovers.trim().is_empty(),
            "a failed provisioning leaked containers: {leftovers:?}"
        );
    }
}
