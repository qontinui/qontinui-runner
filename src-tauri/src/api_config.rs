//! Central registry for internal-service endpoint URLs.
//!
//! Resolution priority for each getter: ENV VAR override → compile-time default.
//! All getters return owned `String` for caller convenience. Call these instead
//! of hardcoding `"http://localhost:N"` anywhere in the Rust backend.
//!
//! # Recognized environment variables
//!
//! | Variable                    | Service                                | Default                                  |
//! |-----------------------------|----------------------------------------|------------------------------------------|
//! | `QONTINUI_WEB_BACKEND_URL`  | qontinui-web FastAPI backend (override)| (falls through to `QONTINUI_API_URL`)    |
//! | `QONTINUI_API_URL`          | qontinui-web FastAPI backend           | `http://127.0.0.1:8000` (debug) / prod   |
//! | `QONTINUI_RUNNER_API_URL`   | This runner's MCP HTTP API             | `http://127.0.0.1:{actual_port}`         |
//! | `QONTINUI_PORT`             | Bootstrap port for runner MCP API      | `9876`                                   |
//! | `QONTINUI_SUPERVISOR_URL`   | Supervisor HTTP API                    | `http://127.0.0.1:9875`                  |
//! | `TAURI_DEV_SERVER_URL`      | Tauri/Vite dev server (debug only)     | `http://localhost:1420`                  |
//!
//! Other internal services (OTel collector, embedding service, local AI
//! providers like vLLM/Gemma/Ollama, PRM service) are configured through their
//! own settings structs and are intentionally NOT routed through this module.

/// Default supervisor HTTP port (per `proj_arch_supervisor_test_login`).
pub const DEFAULT_SUPERVISOR_PORT: u16 = 9875;

/// Default Tauri dev server (Vite) port for debug builds.
pub const DEFAULT_TAURI_DEV_PORT: u16 = 1420;

/// Default qontinui-web FastAPI backend port.
pub const DEFAULT_BACKEND_PORT: u16 = 8000;

/// Canonical Qontinui production backend FQDN. Single source of truth for
/// `get_api_base_url` and `settings::default_web_integration_backend_url`.
pub const PROD_API_BASE_URL: &str = "https://api.qontinui.io";

/// Canonical Qontinui production web-frontend FQDN (the Next.js app on Vercel).
///
/// Production is a **split** deployment: `PROD_API_BASE_URL` (`api.qontinui.io`)
/// serves only `/api/v1/*`, while user-facing pages like `/login` and
/// `/connect-runner` are served by the web frontend at `qontinui.io`. The `api.`
/// host has no login page, so any UI that sends the user "to log in" must target
/// this origin, not the backend. See [`derive_web_base_url`].
pub const PROD_WEB_BASE_URL: &str = "https://qontinui.io";

/// Derive the user-facing web-frontend origin from an API `backend_url`.
///
/// Production splits the API (`https://api.qontinui.io`) from the web frontend
/// (`https://qontinui.io`) — the only difference is the leading `api.` host
/// label. When the backend host begins with `api.`, this strips that single
/// label (preserving scheme and port) to yield the frontend origin. For any
/// other host — localhost dev, a bare IP, or a unified deployment where one
/// origin serves both — the backend URL is returned unchanged, so the
/// long-standing "same origin serves the SPA" fallback and any explicit
/// `web_base_url` override keep working.
///
/// The returned value has no trailing slash and no path.
pub fn derive_web_base_url(backend_url: &str) -> String {
    let trimmed = backend_url.trim().trim_end_matches('/');
    // Canonical mapping: the production API host maps to the production web
    // origin. The general `api.`-stripping below also yields this, but pinning
    // it keeps the two constants coupled even if the web FQDN ever diverges
    // from a simple label strip.
    if trimmed == PROD_API_BASE_URL {
        return PROD_WEB_BASE_URL.to_string();
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(parts) => parts,
        None => return trimmed.to_string(),
    };
    // Authority is everything up to the first '/'; a ':' splits off the port.
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    match host.strip_prefix("api.") {
        // Only rewrite when an `api.` label was actually present.
        Some(frontend_host) => match port {
            Some(p) => format!("{}://{}:{}", scheme, frontend_host, p),
            None => format!("{}://{}", scheme, frontend_host),
        },
        None => trimmed.to_string(),
    }
}

/// Pure precedence resolver for [`get_api_base_url`] — no I/O so the ordering
/// is unit-testable. Splitting the decision out matches the codebase's
/// `next_action` / `resolve_pair_tenant_id` style.
///
/// `persisted` is `Some(url)` only when web-integration is enabled AND the
/// persisted `backend_url` is present; blank/whitespace values at any level
/// are skipped (treated as unset).
///
/// Resolution order:
/// 1. `env_web` (`QONTINUI_WEB_BACKEND_URL`) — operator/test explicit override
/// 2. `env_api` (`QONTINUI_API_URL`) — legacy explicit override
/// 3. `persisted` — the paired backend the user signed into (the one that can
///    verify this device's JWT). NEW: closes the prod/local device-JWT split
///    where a debug relay verified against local while pairing minted against
///    prod. See `plans/2026-07-08-runner-relay-honor-persisted-backend-url.md`.
/// 4. build default: debug `http://127.0.0.1:8000` (IPv4 — the backend only
///    binds IPv4; `localhost` may resolve to IPv6 `::1` first) / release
///    `PROD_API_BASE_URL`.
///
/// A trailing slash is trimmed so callers can safely `format!("{base}/api/...")`.
///
/// # Why this returns the arm and not just the URL
///
/// The value alone cannot be attributed: `https://api.qontinui.io` is what you
/// get from `QONTINUI_API_URL`, from the persisted paired backend, AND from a
/// release build with nothing configured at all — three completely different
/// remediations behind one identical string. Phase 1 of
/// `2026-08-20-effective-config-provenance-and-env-generation` derived the arm
/// in a SECOND function walking the same rungs; that second copy of the
/// precedence rule is exactly the divergence hazard the plan names as its
/// dominant correctness risk, so Phase 2 folded it back in here. There is now
/// ONE traversal of the four rungs, and it emits the value and the arm together
/// — they can no longer disagree by construction, because nothing computes them
/// separately.
///
/// This is the same `(value, source)` shape `profiles::coord_base_with_source`
/// already has; the config report ASKS this function rather than re-deriving.
///
/// # Why a release build refuses a loopback persisted value
///
/// Rung 3 is a JSON field, and its DEBUG default is
/// `http://127.0.0.1:8000` ([`crate::settings::default_web_integration_backend_url`]).
/// A `settings.json` written by a debug build — or copied from a dev box, or
/// carried across a debug→release upgrade of the same install — therefore hands
/// a RELEASE runner a backend that only that one machine can reach. Nothing
/// fails: the runner registers its device WebSocket with the local backend and
/// reports itself healthy, while `coord.devices.ws_session_id` stays NULL in
/// prod and every mobile cloud-relay call 503s. That fault ran undetected for a
/// long time precisely because rung 3 outranks the release build default and
/// said nothing about it.
///
/// So when `is_debug == false`, a persisted value whose HOST is loopback
/// ([`is_loopback_backend_url`]) is dropped from the ladder and the release
/// build default applies, under its own arm
/// ([`ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected`]) so the report can
/// say "a persisted value was OVERRIDDEN" rather than the very different "none
/// was configured". [`get_api_base_url_with_source`] turns that arm into one
/// loud warning per process.
///
/// DEBUG builds are untouched — local dev must keep resolving to
/// `http://127.0.0.1:8000`, whether that comes from the persisted rung or the
/// build default.
pub(crate) fn resolve_api_base_url(
    env_web: Option<String>,
    env_api: Option<String>,
    persisted: Option<String>,
    is_debug: bool,
) -> (String, ApiBaseUrlArm) {
    // Blank/whitespace at any rung is "unset", not "configured to empty" — an
    // exported-but-empty env var is how a shell communicates absence.
    let usable = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let persisted = usable(persisted);
    // RELEASE builds refuse a LOOPBACK persisted `backend_url`. See the
    // "Why a release build refuses a loopback persisted value" section above.
    // Only the persisted rung is filtered: the two env rungs are an operator
    // typing a value at this process's start, which is a deliberate act with a
    // visible cause; the persisted rung is a JSON file written once, months
    // ago, possibly by a DEBUG build of this same runner.
    let persisted_loopback_rejected = persisted
        .as_deref()
        .is_some_and(|p| persisted_backend_url_refused(p, is_debug));
    let persisted = if persisted_loopback_rejected {
        None
    } else {
        persisted
    };
    let (pick, arm) = usable(env_web)
        .map(|v| (v, ApiBaseUrlArm::EnvWebBackendUrl))
        .or_else(|| usable(env_api).map(|v| (v, ApiBaseUrlArm::EnvApiUrl)))
        .or_else(|| persisted.map(|v| (v, ApiBaseUrlArm::PersistedBackendUrl)))
        .unwrap_or_else(|| {
            if is_debug {
                (
                    format!("http://127.0.0.1:{}", DEFAULT_BACKEND_PORT),
                    ApiBaseUrlArm::BuildDefaultDebug,
                )
            } else if persisted_loopback_rejected {
                (
                    PROD_API_BASE_URL.to_string(),
                    ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected,
                )
            } else {
                (
                    PROD_API_BASE_URL.to_string(),
                    ApiBaseUrlArm::BuildDefaultRelease,
                )
            }
        });
    (pick.trim().trim_end_matches('/').to_string(), arm)
}

/// Would a build with this `is_debug` flag REFUSE `raw` as the persisted
/// `web_integration.backend_url`?
///
/// This is the SINGLE expression of the release-build loopback refusal
/// documented on [`resolve_api_base_url`]. It exists as a named predicate
/// rather than an inline `!is_debug && …` because the persisted field has
/// readers OUTSIDE the four-rung ladder, and a refusal only the ladder honours
/// is not a refusal — it is a DIVERGENCE, which is the precise fault the ladder
/// was built to prevent.
///
/// # Who else has to ask
///
/// Two subsystems dial the persisted `backend_url` without going through
/// [`get_api_base_url`], and both are load-bearing for the outage that
/// motivated the refusal:
///
/// - [`crate::mcp::device_jwt_refresher`] MINTS the device JWT against it. The
///   relay DIALS [`get_api_base_url`]. If only one of the two refuses, the
///   runner mints a credential at one backend and presents it at another —
///   re-opening the prod/local device-JWT split that the persisted rung was
///   added to close (plan `2026-07-08-runner-relay-honor-persisted-backend-url`),
///   only pointing the other way.
/// - [`crate::memory::tenant_sync::resolve_web_base`] uploads the tenant's
///   memory records to it, and its own contract is that it yields the SAME base
///   the relay and every `/api/v1/*` caller use.
///
/// `is_debug` is a parameter rather than a `cfg!` so the rule stays pure and
/// unit-testable at both settings; live callers pass `cfg!(debug_assertions)`.
///
/// A blank value is NOT refused — it is not loopback, it is unset, and each
/// caller already has its own "nothing configured" branch that must keep
/// firing.
pub(crate) fn persisted_backend_url_refused(raw: &str, is_debug: bool) -> bool {
    !is_debug && is_loopback_backend_url(raw)
}

/// Does this backend URL point at the LOCAL machine's loopback interface?
///
/// The host is PARSED, never substring-matched: `https://api.qontinui.io/?next=
/// http://127.0.0.1:8000` contains the literal `127.0.0.1` and is not loopback,
/// while `http://127.9.9.9:8000` contains none of the usual spellings and is.
/// Covers every spelling the item names — `localhost` (and any `*.localhost`
/// subdomain, which RFC 6761 reserves as loopback), the whole `127.0.0.0/8`
/// block rather than just `127.0.0.1`, and IPv6 `::1` in both its bare and
/// bracketed forms (the parser strips the brackets, so one arm covers both).
///
/// A value the URL parser cannot make a host out of is NOT loopback: this
/// predicate gates a refusal, so an unparseable value must fall through to the
/// normal precedence and fail loudly at dial time rather than be silently
/// swapped for a different backend. Two spellings get a retry before that
/// verdict, because they are what an operator genuinely hand-types into a
/// settings file and neither is a legal URL as written: a scheme-less
/// authority (`127.0.0.1:8000`, `localhost:8000`), which the parser reads as a
/// bare scheme with no host, and a bare IPv6 literal (`::1`), which is not a
/// legal authority unbracketed.
fn is_loopback_backend_url(raw: &str) -> bool {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed)
        .ok()
        .filter(|u| u.host().is_some())
        // Scheme-less: `127.0.0.1:8000` / `localhost:8000` read as a bare
        // scheme with no host, so retry them as `http://`.
        .or_else(|| {
            url::Url::parse(&format!("http://{trimmed}"))
                .ok()
                .filter(|u| u.host().is_some())
        })
        // A bare IPv6 literal (`::1`) is not a legal URL authority unbracketed,
        // so the retry above cannot see it either. Bracket it and try once more.
        .or_else(|| url::Url::parse(&format!("http://[{trimmed}]")).ok());
    match parsed.as_ref().and_then(url::Url::host) {
        // Covers 127.0.0.0/8 in full, not just 127.0.0.1.
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        // `::1`, and `[::1]` — the parser has already stripped the brackets.
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

/// Emitted at most once per process when a release build refused a loopback
/// persisted `backend_url`.
///
/// # Why once, and why not inside [`resolve_api_base_url`]
///
/// The resolver is pure and is re-run by every one of the ~70
/// [`get_api_base_url`] call sites — heartbeat, task-sync and workflow-sync run
/// it on a timer — so warning there would emit the same line thousands of times
/// an hour and train every reader to filter it out. The fault this warns about
/// is a persisted setting that cannot change while the process runs, so one
/// loud line per process start says everything a repeat would. The arm itself
/// stays queryable forever via [`get_api_base_url_with_source`] and the config
/// report, which is the durable half.
fn warn_persisted_loopback_rejected(rejected: &str, used: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            rejected_backend_url = %rejected,
            using_backend_url = %used,
            arm = %ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected.as_str(),
            "REFUSING persisted web_integration.backend_url '{rejected}': it is a LOOPBACK \
             address and this is a RELEASE build. Using '{used}' (the release build default) \
             instead. A loopback backend is the DEBUG build default \
             (settings::default_web_integration_backend_url); a release runner that honours it \
             registers its device WebSocket with a backend only this machine can reach, so \
             coord.devices.ws_session_id stays NULL in prod and every mobile cloud-relay call \
             503s. FIX: set web_integration.backend_url in settings.json to the backend this \
             runner actually paired with (or export QONTINUI_WEB_BACKEND_URL), then start a new \
             runner. Until then this runner talks to '{used}', which may not be the backend that \
             minted its device JWT."
        );
    });
}

/// Get API base URL for qontinui-web backend.
///
/// This is the SINGLE source of truth for the web-backend base across every
/// runner subsystem (auth, workflow-sync, heartbeat, task-sync, …). Previously
/// `heartbeat.rs` honored `QONTINUI_WEB_BACKEND_URL` while workflow-sync only
/// honored `QONTINUI_API_URL`, so the two could resolve to different hosts and
/// silently diverge (one path 401'ing against the wrong backend). Folding both
/// vars in here — plus the persisted paired backend below — guarantees every
/// caller resolves to the same host the user actually signed into.
///
/// Precedence is documented on [`resolve_api_base_url`]; this wrapper supplies
/// the I/O (env vars + `load_settings()`). `load_settings()` reads env + the
/// JSON file directly and does NOT call back into `get_api_base_url()`, so
/// there is no recursion; an absent/unparseable settings file yields
/// `Settings::default()`, whose `backend_url` == the build default, collapsing
/// step 3 into step 4.
///
/// The ~70 call sites of this function want a URL to dial, not provenance, so
/// the arm is bound and dropped HERE — visibly, at the one place that does the
/// I/O — rather than in a `.0` wrapper that hides the discard from every reader.
/// Anything that does care (the config report, a diagnostic, an error body)
/// calls [`get_api_base_url_with_source`] and gets the arm from the resolver
/// itself.
pub fn get_api_base_url() -> String {
    let (url, _arm) = get_api_base_url_with_source();
    url
}

/// [`get_api_base_url`] plus WHICH of the four documented rungs produced it.
///
/// This is the live-I/O door: it gathers the four inputs from this process and
/// hands them to [`resolve_api_base_url`], so a caller asking "where did the
/// backend URL come from?" is answered by the same traversal that produced the
/// URL every other subsystem is using. No consumer — the config report
/// included — is allowed a second implementation of the precedence order.
pub(crate) fn get_api_base_url_with_source() -> (String, ApiBaseUrlArm) {
    let inputs = gather_api_base_url_inputs();
    // Kept for the warning below: the resolver DROPS a rejected persisted
    // value, and a warning that could not name what it rejected would be as
    // unactionable as the silence it replaces.
    let persisted = inputs.persisted.clone();
    let (url, arm) = resolve_api_base_url(
        inputs.env_web,
        inputs.env_api,
        inputs.persisted,
        inputs.is_debug,
    );
    if arm == ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected {
        warn_persisted_loopback_rejected(persisted.as_deref().unwrap_or("<unset>"), &url);
    }
    (url, arm)
}

/// The four inputs [`resolve_api_base_url`] weighs, gathered from the live
/// process in one place.
///
/// Extracted so that "what are the four inputs, and where does each come from?"
/// is answered exactly ONCE. [`get_api_base_url`] and the config report both
/// consume this, so the report can never be looking at a different set of
/// inputs than the value every other subsystem actually resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiBaseUrlInputs {
    /// `QONTINUI_WEB_BACKEND_URL` — operator/test explicit override.
    pub env_web: Option<String>,
    /// `QONTINUI_API_URL` — legacy explicit override.
    pub env_api: Option<String>,
    /// The persisted paired backend, present only when web-integration is
    /// ENABLED (a disabled integration means "don't reach web", so its stored
    /// URL must not override the build default).
    pub persisted: Option<String>,
    /// Whether this is a debug build (selects which build default applies).
    pub is_debug: bool,
}

/// Read the four inputs from env + settings. The only I/O in the resolution.
///
/// **This is not a read.** `load_settings()` is `load_settings_full()`, which
/// runs `claude_accounts::load_with_migration()` (writing
/// `claude-accounts.json`), can mint a `local_user_id` UUID and call
/// `save_settings` — rewriting the operator's real `settings.json` — and reaches
/// the OS keyring. That is correct for the ~70 runtime callers, which want the
/// same fully-overlaid document every other subsystem resolves against; it is
/// disqualifying for a diagnostic. Anything holding an already-read `Settings`
/// must call [`api_base_url_inputs_from`] instead — see its docs.
pub(crate) fn gather_api_base_url_inputs() -> ApiBaseUrlInputs {
    api_base_url_inputs_from(&crate::settings::load_settings())
}

/// [`gather_api_base_url_inputs`] over a `Settings` the caller already holds —
/// the READ-ONLY twin, whose only I/O is two `std::env::var` calls.
///
/// # Why this exists
///
/// `config_report`'s layer 1 was deliberately moved off `load_settings_full` and
/// onto the non-mutating `settings::read_settings_from_disk`, precisely because
/// the full loader writes `claude-accounts.json`, mints a `local_user_id` UUID
/// into the operator's real `settings.json` and reaches the OS keyring. Layer 5
/// then undid all of it one line later by calling
/// [`gather_api_base_url_inputs`], whose first statement is `load_settings()` —
/// the same loader, reached through a different door. The report's layer-1 row
/// still said `settings::read_settings_from_disk`, so the report ACTIVELY
/// CONCEALED the write it had just performed.
///
/// # Why a disk-read `Settings` yields the same rung here
///
/// The persisted rung reads exactly two fields —
/// `web_integration.{enabled, backend_url}` — and the only overlay
/// `load_settings_full` applies to either is
/// [`crate::settings::apply_web_integration_env_overlay`], which a caller
/// handing in a disk-read document is expected to have applied itself (the
/// config report does, in `config_report_cmd::settings_derived_inputs`). The
/// tier/`local_user_id` migration, the Restate port overrides and the tier
/// overlays — the three things that make the full loader a writer — touch no
/// `web_integration` field, so with that one overlay applied the two doors
/// resolve the same rung and the same value. Nothing here re-implements the
/// overlay; that would be the second-copy defect this module's `(value, arm)`
/// shape exists to prevent.
pub(crate) fn api_base_url_inputs_from(s: &crate::settings::Settings) -> ApiBaseUrlInputs {
    ApiBaseUrlInputs {
        env_web: std::env::var("QONTINUI_WEB_BACKEND_URL").ok(),
        env_api: std::env::var("QONTINUI_API_URL").ok(),
        persisted: s
            .web_integration
            .enabled
            .then(|| s.web_integration.backend_url.clone()),
        is_debug: cfg!(debug_assertions),
    }
}

/// Which rung of [`resolve_api_base_url`]'s documented four-rung order produced
/// the value — the house `(value, source)` shape that `profiles::CoordBaseSource`
/// already has and this resolver does not.
///
/// The arm names are stable wire strings: they appear verbatim in the config
/// report and are meant to be greppable and comparable across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiBaseUrlArm {
    /// Env `QONTINUI_WEB_BACKEND_URL` won.
    EnvWebBackendUrl,
    /// Env `QONTINUI_API_URL` won.
    EnvApiUrl,
    /// The persisted paired backend won (web-integration enabled).
    PersistedBackendUrl,
    /// Nothing configured; the debug build default (`127.0.0.1:8000`) applied.
    BuildDefaultDebug,
    /// Nothing configured; the release build default ([`PROD_API_BASE_URL`])
    /// applied.
    BuildDefaultRelease,
    /// A release build REFUSED a loopback persisted `backend_url` and fell
    /// through to [`PROD_API_BASE_URL`]. Distinct from
    /// [`ApiBaseUrlArm::BuildDefaultRelease`] on purpose: the value is
    /// identical, but "a persisted setting was overridden" and "nothing was
    /// configured" are different faults with different remediations — the
    /// first leaves a wrong value in `settings.json` that will keep being
    /// refused every start until someone edits it. See
    /// [`resolve_api_base_url`].
    BuildDefaultReleaseLoopbackRejected,
}

impl ApiBaseUrlArm {
    /// Stable wire string.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ApiBaseUrlArm::EnvWebBackendUrl => "env:QONTINUI_WEB_BACKEND_URL",
            ApiBaseUrlArm::EnvApiUrl => "env:QONTINUI_API_URL",
            ApiBaseUrlArm::PersistedBackendUrl => "persisted:web_integration.backend_url",
            ApiBaseUrlArm::BuildDefaultDebug => "build_default:debug",
            ApiBaseUrlArm::BuildDefaultRelease => "build_default:release",
            ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected => {
                "build_default:release:persisted_loopback_rejected"
            }
        }
    }

    /// The NAME of the slot this rung's value came out of — what a credential
    /// classifier has to be told in order to judge the value.
    ///
    /// `env_generations::classify_env_var` is a function of `(name, value)`, and
    /// one of its three arms is joint: a `*_URL` / `*_URI` / `*_DSN` NAME whose
    /// VALUE carries URL userinfo at all (not merely a password). A caller that
    /// classified this rung's value under a made-up label would silently lose
    /// that arm — `scheme://ops@host` would print the account name — so the name
    /// handed to the classifier is the real slot: the env variable for the two
    /// env rungs, the settings field path for the persisted rung, and a
    /// field-shaped label for the build defaults so all five are judged under
    /// the same connection-string rule.
    ///
    /// Every one of the five ends in `_URL` once upper-cased, which is what makes
    /// the joint arm reachable for all of them. That is a property of this
    /// mapping, not a coincidence, and
    /// `config_report_cmd::tests::config_report_api_arm_origin_names_are_url_named`
    /// asserts it against literals.
    pub(crate) fn value_origin_name(self) -> &'static str {
        match self {
            ApiBaseUrlArm::EnvWebBackendUrl => "QONTINUI_WEB_BACKEND_URL",
            ApiBaseUrlArm::EnvApiUrl => "QONTINUI_API_URL",
            ApiBaseUrlArm::PersistedBackendUrl => "web_integration.backend_url",
            ApiBaseUrlArm::BuildDefaultDebug
            | ApiBaseUrlArm::BuildDefaultRelease
            // The VALUE this arm yields is the build default; the rejected
            // persisted value is named in the warning, not here.
            | ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected => "build_default.backend_url",
        }
    }
}

/// MCP API base URL for the runner's own HTTP server.
///
/// Resolution order:
/// 1. `QONTINUI_RUNNER_API_URL` environment variable (if set)
/// 2. `http://127.0.0.1:{port}` where `port` comes from
///    [`crate::mcp::types::get_mcp_api_port`] (`QONTINUI_PORT` env var, then
///    the `MCP_API_PORT` constant fallback).
///
/// Note: callers that have an `AppState` should prefer
/// [`crate::mcp::types::get_self_base_url`], which reads the actually-bound
/// port from `app_state.api_port` (an `AtomicU16` set at bind time). This
/// getter is for paths without `AppState` access (e.g. helper modules,
/// pre-bind probes).
pub fn get_runner_api_url() -> String {
    if let Ok(url) = std::env::var("QONTINUI_RUNNER_API_URL") {
        return url.trim_end_matches('/').to_string();
    }
    crate::mcp::types::get_self_base_url_from_env()
}

/// Supervisor HTTP API base URL.
///
/// Resolution order:
/// 1. `QONTINUI_SUPERVISOR_URL` environment variable (if set)
/// 2. `http://127.0.0.1:9875`
pub fn get_supervisor_url() -> String {
    std::env::var("QONTINUI_SUPERVISOR_URL")
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", DEFAULT_SUPERVISOR_PORT))
}

/// Supervisor TCP socket address (`host:port`) for raw connect probes.
/// Best-effort parses [`get_supervisor_url`]; falls back to
/// `127.0.0.1:{DEFAULT_SUPERVISOR_PORT}` if the URL can't be parsed.
pub fn get_supervisor_socket_addr() -> String {
    let url = get_supervisor_url();
    // Strip scheme and any path.
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url.as_str());
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("127.0.0.1:{}", DEFAULT_SUPERVISOR_PORT)
    }
}

/// Tauri dev server (Vite) URL — dev builds only. Returns `None` in release.
///
/// Resolution order (debug builds):
/// 1. `TAURI_DEV_SERVER_URL` environment variable (set by Tauri at build time)
/// 2. `http://localhost:1420`
pub fn get_tauri_dev_server_url() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    Some(
        std::env::var("TAURI_DEV_SERVER_URL")
            .unwrap_or_else(|_| format!("http://localhost:{}", DEFAULT_TAURI_DEV_PORT)),
    )
}

/// IPC response callback URL used by JS snippets the backend injects into
/// the WebView. Same host/port as the runner MCP API.
pub fn get_ipc_response_url() -> String {
    format!("{}/ui-bridge/ipc-response", get_runner_api_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::env_lock;

    #[test]
    fn supervisor_url_uses_default_port() {
        let _env_lock = env_lock();
        // We can't reliably clear env in a multi-test process, so just assert
        // the default port appears in the fallback path.
        std::env::remove_var("QONTINUI_SUPERVISOR_URL");
        let url = get_supervisor_url();
        assert!(
            url.contains(&DEFAULT_SUPERVISOR_PORT.to_string()),
            "supervisor URL should contain default port: {}",
            url
        );
    }

    #[test]
    fn ipc_response_url_appends_path() {
        let url = get_ipc_response_url();
        assert!(
            url.ends_with("/ui-bridge/ipc-response"),
            "ipc response url should end with /ui-bridge/ipc-response: {}",
            url
        );
    }

    #[test]
    fn tauri_dev_server_url_only_in_debug() {
        let url = get_tauri_dev_server_url();
        if cfg!(debug_assertions) {
            assert!(url.is_some());
        } else {
            assert!(url.is_none());
        }
    }

    /// Phase 9 calibration: lock in the canonical production backend URL.
    /// `PROD_API_BASE_URL` is the single source of truth used by both
    /// `get_api_base_url` (auth endpoints) and
    /// `settings::default_web_integration_backend_url` (WS relay default).
    /// A drift between the two surfaces is exactly the Phase 6 defect this
    /// constant was introduced to prevent — see plans/2026-05-20-runner-
    /// tier-decoupling.md.
    #[test]
    fn prod_api_base_url_is_canonical() {
        assert_eq!(PROD_API_BASE_URL, "https://api.qontinui.io");
    }

    #[test]
    fn derive_web_base_url_strips_prod_api_label() {
        // The headline case: api.qontinui.io (no login page) → qontinui.io.
        assert_eq!(derive_web_base_url(PROD_API_BASE_URL), PROD_WEB_BASE_URL);
        assert_eq!(
            derive_web_base_url("https://api.qontinui.io/"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_non_api_hosts() {
        // Localhost dev + unified deployments have no `api.` label to strip,
        // so the backend origin is returned unchanged (old fallback behavior).
        assert_eq!(
            derive_web_base_url("http://localhost:8000"),
            "http://localhost:8000"
        );
        assert_eq!(
            derive_web_base_url("http://127.0.0.1:8000/"),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            derive_web_base_url("https://qontinui.io"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn resolve_api_base_url_precedence() {
        let web = || Some("https://web.example".to_string());
        let api = || Some("https://api.example".to_string());
        let persisted = || Some("https://persisted.example".to_string());

        // env_web wins over everything.
        assert_eq!(
            resolve_api_base_url(web(), api(), persisted(), true),
            (
                "https://web.example".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
        // env_api wins over persisted + default.
        assert_eq!(
            resolve_api_base_url(None, api(), persisted(), true),
            ("https://api.example".to_string(), ApiBaseUrlArm::EnvApiUrl)
        );
        // persisted wins over the build default.
        assert_eq!(
            resolve_api_base_url(None, None, persisted(), true),
            (
                "https://persisted.example".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
    }

    #[test]
    fn resolve_api_base_url_build_defaults() {
        // All absent → debug default is the IPv4-pinned localhost.
        assert_eq!(
            resolve_api_base_url(None, None, None, true),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::BuildDefaultDebug
            )
        );
        // All absent → release default is prod.
        assert_eq!(
            resolve_api_base_url(None, None, None, false),
            (
                "https://api.qontinui.io".to_string(),
                ApiBaseUrlArm::BuildDefaultRelease
            )
        );
    }

    #[test]
    fn resolve_api_base_url_skips_blank_and_trims() {
        // Blank / whitespace at any level is treated as unset (not selected).
        assert_eq!(
            resolve_api_base_url(
                Some("   ".to_string()),
                Some("".to_string()),
                Some("https://persisted.example".to_string()),
                true,
            ),
            (
                "https://persisted.example".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
        // A blank persisted with no env falls through to the build default.
        assert_eq!(
            resolve_api_base_url(None, None, Some("  ".to_string()), true),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::BuildDefaultDebug
            )
        );
        // Trailing slash is trimmed on the chosen value.
        assert_eq!(
            resolve_api_base_url(Some("https://web.example/".to_string()), None, None, true),
            (
                "https://web.example".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
    }

    /// Regression for the prod/local device-JWT split (plan 2026-07-08): a
    /// debug build whose user signed into a non-default backend must resolve
    /// to THAT backend, not the localhost build default — so the relay
    /// verifies against the same coord that minted the device JWT.
    ///
    /// The ARM is what makes this regression legible: the returned string is
    /// byte-identical to the release build default, so a report that printed
    /// only the value could not tell "the user paired with prod" from "this is
    /// a release build with nothing configured" — two different bugs.
    #[test]
    fn resolve_api_base_url_debug_honors_persisted_prod() {
        assert_eq!(
            resolve_api_base_url(None, None, Some(PROD_API_BASE_URL.to_string()), true),
            (
                "https://api.qontinui.io".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
    }

    /// The arm vocabulary is a WIRE contract — it is printed verbatim in the
    /// config report and compared across machines — so it is pinned to
    /// literals here rather than to the enum's own `as_str`, which would pin
    /// nothing.
    #[test]
    fn api_base_url_arm_wire_strings_are_stable() {
        assert_eq!(
            ApiBaseUrlArm::EnvWebBackendUrl.as_str(),
            "env:QONTINUI_WEB_BACKEND_URL"
        );
        assert_eq!(ApiBaseUrlArm::EnvApiUrl.as_str(), "env:QONTINUI_API_URL");
        assert_eq!(
            ApiBaseUrlArm::PersistedBackendUrl.as_str(),
            "persisted:web_integration.backend_url"
        );
        assert_eq!(
            ApiBaseUrlArm::BuildDefaultDebug.as_str(),
            "build_default:debug"
        );
        assert_eq!(
            ApiBaseUrlArm::BuildDefaultRelease.as_str(),
            "build_default:release"
        );
        assert_eq!(
            ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected.as_str(),
            "build_default:release:persisted_loopback_rejected"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_scheme_and_port() {
        assert_eq!(
            derive_web_base_url("http://api.example.test:8080"),
            "http://example.test:8080"
        );
        // A host that merely starts with the letters "api" but has no `api.`
        // label must NOT be rewritten.
        assert_eq!(
            derive_web_base_url("https://apiserver.example.test"),
            "https://apiserver.example.test"
        );
    }

    /// Every spelling of "this machine" the item names is refused by a RELEASE
    /// build, and the release build default applies under the arm that says a
    /// persisted value was OVERRIDDEN rather than absent.
    ///
    /// This is the mobile-cloud-relay outage in one assertion: a release runner
    /// had `http://127.0.0.1:8000` persisted (the DEBUG default, inherited),
    /// honoured it, registered its device WebSocket with the local backend, and
    /// left `coord.devices.ws_session_id` NULL in prod for as long as it ran.
    #[test]
    fn release_refuses_every_loopback_spelling_of_persisted_backend_url() {
        for spelling in [
            "http://127.0.0.1:8000",
            "http://127.0.0.1:8000/",
            "http://localhost:8000",
            "http://LOCALHOST:8000",
            "https://localhost",
            "http://api.localhost:8000",
            // 127.0.0.0/8 in full — not just the .1 host.
            "http://127.0.0.2:8000",
            "http://127.1.2.3:8000",
            "http://[::1]:8000",
            "http://[::1]",
            // Scheme-less spellings an operator genuinely types into JSON.
            "127.0.0.1:8000",
            "localhost:8000",
            "::1",
        ] {
            assert_eq!(
                resolve_api_base_url(None, None, Some(spelling.to_string()), false),
                (
                    PROD_API_BASE_URL.to_string(),
                    ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected
                ),
                "release build must refuse persisted loopback {spelling}"
            );
        }
    }

    /// The refusal is narrow: a release build still honours a persisted value
    /// that points at a REAL remote backend — that rung exists to close the
    /// prod/local device-JWT split (plan 2026-07-08) and must keep working.
    #[test]
    fn release_honors_a_remote_persisted_backend_url() {
        for remote in [
            "https://api.qontinui.io",
            "https://backend.example.test:8443",
            // Not loopback: a private LAN address is a legitimate paired
            // backend that other devices CAN reach.
            "http://192.168.1.50:8000",
            // Host merely CONTAINS a loopback spelling — parsed, not matched.
            "https://localhost.example.test",
            "https://api.qontinui.io/?next=http://127.0.0.1:8000",
            // 128.0.0.1 is one bit outside 127.0.0.0/8.
            "http://128.0.0.1:8000",
        ] {
            let (url, arm) = resolve_api_base_url(None, None, Some(remote.to_string()), false);
            assert_eq!(
                arm,
                ApiBaseUrlArm::PersistedBackendUrl,
                "release build must honour persisted remote {remote}"
            );
            assert_eq!(url, remote.trim_end_matches('/'));
        }
    }

    /// The predicate the OUT-OF-LADDER readers ask agrees with the ladder's own
    /// verdict, for every spelling, at both build settings.
    ///
    /// This is the anti-divergence assertion. `device_jwt_refresher` (which
    /// MINTS the device JWT) and `memory::tenant_sync::resolve_web_base` (which
    /// uploads memory records) read the persisted `backend_url` directly, so a
    /// refusal only `resolve_api_base_url` honoured would mean the runner mints
    /// a credential at one backend and presents it at another. Asserting the
    /// two against each other — rather than restating the rule — is what makes
    /// that class of drift a test failure instead of a production outage.
    #[test]
    fn refusal_predicate_agrees_with_the_ladder_at_both_build_settings() {
        let loopback = [
            "http://127.0.0.1:8000",
            "http://LOCALHOST:8000",
            "http://api.localhost:8000",
            "http://127.1.2.3:8000",
            "http://[::1]:8000",
            "127.0.0.1:8000",
            "::1",
        ];
        let remote = [
            "https://api.qontinui.io",
            "http://192.168.1.50:8000",
            "https://localhost.example.test",
            "http://128.0.0.1:8000",
        ];
        for is_debug in [true, false] {
            for candidate in loopback.iter().chain(remote.iter()) {
                let (_, arm) =
                    resolve_api_base_url(None, None, Some((*candidate).to_string()), is_debug);
                let ladder_refused = arm == ApiBaseUrlArm::BuildDefaultReleaseLoopbackRejected;
                assert_eq!(
                    persisted_backend_url_refused(candidate, is_debug),
                    ladder_refused,
                    "predicate and ladder must agree on {candidate} (is_debug={is_debug})"
                );
                // And the refusal is exactly "release AND loopback".
                assert_eq!(
                    ladder_refused,
                    !is_debug && loopback.contains(candidate),
                    "unexpected verdict for {candidate} (is_debug={is_debug})"
                );
            }
        }
    }

    /// A BLANK persisted value is unset, not loopback. Both out-of-ladder
    /// readers have their own "nothing configured" branch below the check —
    /// the refresher's `pair_base.is_empty()` bail and `resolve_web_base`'s
    /// fall-through — and refusing blank here would jump the queue and hide
    /// the unconfigured case behind a loopback verdict it does not deserve.
    #[test]
    fn blank_persisted_backend_url_is_not_refused() {
        for blank in ["", "   ", "\t\n"] {
            for is_debug in [true, false] {
                assert!(
                    !persisted_backend_url_refused(blank, is_debug),
                    "blank must be unset, not refused (is_debug={is_debug})"
                );
            }
        }
    }

    /// DEBUG builds are untouched — local dev keeps pointing at the local
    /// backend, from the persisted rung, with the persisted arm.
    #[test]
    fn debug_still_honors_a_loopback_persisted_backend_url() {
        for spelling in [
            "http://127.0.0.1:8000",
            "http://localhost:8000",
            "http://[::1]:8000",
        ] {
            assert_eq!(
                resolve_api_base_url(None, None, Some(spelling.to_string()), true),
                (spelling.to_string(), ApiBaseUrlArm::PersistedBackendUrl),
                "debug build must keep honouring persisted {spelling}"
            );
        }
        // And with nothing persisted at all, the debug default is still local.
        assert_eq!(
            resolve_api_base_url(None, None, None, true),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::BuildDefaultDebug
            )
        );
    }

    /// Only the PERSISTED rung is filtered. An operator who exports a loopback
    /// override into a release build is making a deliberate, visible choice
    /// (that is how you point a release runner at a local backend on purpose),
    /// and the higher rungs outrank the persisted one anyway.
    #[test]
    fn release_loopback_refusal_does_not_touch_the_env_rungs() {
        assert_eq!(
            resolve_api_base_url(
                Some("http://127.0.0.1:8000".to_string()),
                None,
                Some("http://localhost:8000".to_string()),
                false,
            ),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
        assert_eq!(
            resolve_api_base_url(
                None,
                Some("http://localhost:8000".to_string()),
                Some("http://127.0.0.1:8000".to_string()),
                false,
            ),
            (
                "http://localhost:8000".to_string(),
                ApiBaseUrlArm::EnvApiUrl
            )
        );
    }

    /// A blank persisted value in a release build is ABSENT, not refused — the
    /// two arms must stay distinguishable, because only one of them means
    /// "there is a wrong value sitting in settings.json".
    #[test]
    fn release_blank_persisted_is_absent_not_rejected() {
        assert_eq!(
            resolve_api_base_url(None, None, Some("   ".to_string()), false),
            (
                PROD_API_BASE_URL.to_string(),
                ApiBaseUrlArm::BuildDefaultRelease
            )
        );
        assert_eq!(
            resolve_api_base_url(None, None, None, false),
            (
                PROD_API_BASE_URL.to_string(),
                ApiBaseUrlArm::BuildDefaultRelease
            )
        );
    }

    /// The predicate parses a HOST; it does not substring-match a URL. Both
    /// directions of that are load-bearing, so both are pinned.
    #[test]
    fn is_loopback_backend_url_judges_the_parsed_host() {
        for yes in [
            "http://127.0.0.1:8000",
            "http://127.255.255.254",
            "http://[::1]:8000",
            "http://localhost",
            "http://localhost.:8000",
            "http://deep.sub.localhost:8000",
            "  http://127.0.0.1:8000  ",
        ] {
            assert!(is_loopback_backend_url(yes), "{yes} is loopback");
        }
        for no in [
            "https://api.qontinui.io",
            "http://192.168.1.50:8000",
            "http://10.0.0.1",
            "https://not-localhost.example.test",
            "https://localhosting.example.test",
            "https://api.qontinui.io/proxy?to=http://localhost:8000",
            // Unparseable → NOT loopback: a refusal must never be the silent
            // answer to a value nobody could read.
            "",
            "not a url at all",
        ] {
            assert!(!is_loopback_backend_url(no), "{no} is not loopback");
        }
    }
}
