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
//! | `QONTINUI_API_URL`          | qontinui-web FastAPI backend           | `PROD_API_BASE_URL` (every build)        |
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

/// Canonical Qontinui production backend FQDN. Single source of truth for
/// `get_api_base_url` and `settings::default_web_integration_backend_url`.
///
/// It is the default web backend in EVERY build, debug included. A runner that
/// should talk to a local qontinui-web backend says so deliberately: `api_url`
/// in the active profile in `~/.qontinui/profiles.json`, or an exported
/// `QONTINUI_WEB_BACKEND_URL`.
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
/// 3. `profile_api_url` — the active profile's `api_url` in
///    `~/.qontinui/profiles.json`: the per-machine choice. It outranks the
///    settings file because `settings.json` is a whole-document struct
///    rewritten on every save, so a value in it cannot be told from a default
///    a build wrote back. Like the env rungs it is a deliberate act and is
///    NOT subject to the machine-local refusal below (a machine that develops
///    against a local qontinui-web says so in its profile); the persisted
///    refusal exists because a settings file can be copied or inherited
///    unnoticed, which a profile edit is not.
/// 4. `persisted` — the paired backend the user signed into (the one that can
///    verify this device's JWT). Closes the prod/local device-JWT split where a
///    relay verified against one backend while pairing minted against another.
///    See `plans/2026-07-08-runner-relay-honor-persisted-backend-url.md`.
/// 5. build default: [`PROD_API_BASE_URL`], in EVERY build (debug included).
///
/// To point a runner at a local backend, set `api_url` in the active profile in
/// `~/.qontinui/profiles.json` or export `QONTINUI_WEB_BACKEND_URL`. No build
/// flavour defaults to a local backend.
///
/// A trailing slash is trimmed so callers can safely `format!("{base}/api/...")`.
///
/// # Why this returns the arm and not just the URL
///
/// The value alone cannot be attributed: `https://api.qontinui.io` is what you
/// get from `QONTINUI_API_URL`, from the persisted paired backend, AND from a
/// build with nothing configured at all — three completely different
/// remediations behind one identical string. Phase 1 of
/// `2026-08-20-effective-config-provenance-and-env-generation` derived the arm
/// in a SECOND function walking the same rungs; that second copy of the
/// precedence rule is exactly the divergence hazard the plan names as its
/// dominant correctness risk, so Phase 2 folded it back in here. There is now
/// ONE traversal of the five rungs, and it emits the value and the arm together
/// — they can no longer disagree by construction, because nothing computes them
/// separately.
///
/// This is the same `(value, source)` shape `profiles::coord_base_with_source`
/// already has; the config report ASKS this function rather than re-deriving.
///
/// # Why a machine-local persisted value is refused
///
/// Rung 4 is a JSON field. Older DEBUG builds defaulted it to
/// `http://127.0.0.1:8000` and, because `settings.json` is serialized whole on
/// every save, wrote that default back into the operator's file. A
/// `settings.json` carrying a loopback value — written by such a build, copied
/// from a dev box, or carried across an upgrade of the same install — therefore
/// hands a runner a backend only that one machine can reach, and usually one
/// that is not even running: sign-in fails with
/// `POST http://127.0.0.1:8000/api/v1/devices/pair-cli failed`. A runner that
/// does reach a local backend registers its device WebSocket there while
/// `coord.devices.ws_session_id` stays NULL in prod and every mobile
/// cloud-relay call 503s.
///
/// So a persisted value whose HOST is machine-local
/// ([`persisted_backend_url_refused`]) is dropped from the ladder in every
/// build and the build default applies, under its own arm
/// ([`ApiBaseUrlArm::BuildDefaultLoopbackRejected`]) so the report can say "a
/// persisted value was OVERRIDDEN" rather than the very different "none was
/// configured". [`get_api_base_url_with_source`] turns that arm into one loud
/// warning per process.
pub(crate) fn resolve_api_base_url(inputs: &ApiBaseUrlInputs) -> (String, ApiBaseUrlArm) {
    // Blank/whitespace at any rung is "unset", not "configured to empty" — an
    // exported-but-empty env var is how a shell communicates absence.
    let usable = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
    let persisted = usable(&inputs.persisted);
    // A MACHINE-LOCAL persisted `backend_url` is refused. See "Why a
    // machine-local persisted value is refused" above. Only the persisted rung
    // is filtered: the env and profile rungs are deliberate acts with a visible
    // cause; the persisted rung is a JSON file written once, possibly months
    // ago, possibly by an older build of this same runner.
    let persisted_loopback_rejected = persisted
        .as_deref()
        .is_some_and(persisted_backend_url_refused);
    let persisted = if persisted_loopback_rejected {
        None
    } else {
        persisted
    };
    let (pick, arm) = usable(&inputs.env_web)
        .map(|v| (v, ApiBaseUrlArm::EnvWebBackendUrl))
        .or_else(|| usable(&inputs.env_api).map(|v| (v, ApiBaseUrlArm::EnvApiUrl)))
        .or_else(|| usable(&inputs.profile_api_url).map(|v| (v, ApiBaseUrlArm::ProfileApiUrl)))
        .or_else(|| persisted.map(|v| (v, ApiBaseUrlArm::PersistedBackendUrl)))
        .unwrap_or_else(|| {
            let arm = if persisted_loopback_rejected {
                ApiBaseUrlArm::BuildDefaultLoopbackRejected
            } else {
                ApiBaseUrlArm::BuildDefault
            };
            (PROD_API_BASE_URL.to_string(), arm)
        });
    (pick.trim().trim_end_matches('/').to_string(), arm)
}

/// Is `raw` REFUSED as the persisted `web_integration.backend_url`?
///
/// This is the SINGLE expression of the machine-local refusal documented on
/// [`resolve_api_base_url`]. It exists as a named predicate rather than an
/// inline check because the persisted field has readers OUTSIDE the ladder,
/// and a refusal only the ladder honours is not a refusal — it is a
/// DIVERGENCE, which is the precise fault the ladder was built to prevent.
///
/// # Who else has to ask
///
/// Subsystems that dial the persisted `backend_url` without going through
/// [`get_api_base_url`], each load-bearing for the outage that motivated the
/// refusal:
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
/// - `main`'s plan & prompt library body sync passes it to
///   `plan_workunit_adapter::trigger::spawn_if_configured`, which POSTs every
///   plan and prompt body to it. That call site answers a refusal with `None`
///   rather than the build default — unlike the two above — because its own
///   guard is "return None rather than guess", and a bulk artifact upload is
///   the wrong place to guess a destination. The refusal is still honoured;
///   only the fallback differs, and it says so in a warning of its own.
///
/// The rule is the same in every build. A blank value is NOT refused — it is
/// not loopback, it is unset, and each caller already has its own "nothing
/// configured" branch that must keep firing.
///
/// # What counts as machine-local
///
/// Two host classes, because the fault is "a backend only THIS machine can
/// reach" and loopback is not the only way to spell that:
///
/// - [`is_loopback_backend_url`] — `127.0.0.0/8`, `::1`, `localhost`.
/// - [`is_unspecified_backend_url`] — `0.0.0.0` and `::`, the *bind-all*
///   addresses. A dev server prints "listening on `0.0.0.0:8000`", so that is
///   the string an operator copies into `settings.json`; dialed rather than
///   bound it means "this host" and reaches the same local backend a loopback
///   value would. Refusing one spelling of the outage and honouring the other
///   would leave the hole the refusal exists to close.
pub(crate) fn persisted_backend_url_refused(raw: &str) -> bool {
    is_loopback_backend_url(raw) || is_unspecified_backend_url(raw)
}

/// The web-backend base an INTERACTIVE sign-in or pair should dial, given the
/// base the UI asked for.
///
/// The UI seeds that request from `get_web_integration_status`, which reports
/// the RAW persisted `web_integration.backend_url` (it is the editable form
/// field). On an install whose `settings.json` still carries a machine-local
/// value written back by an older debug build, that request is
/// `http://127.0.0.1:8000` — the value the ladder refuses — and sign-in failed
/// with `POST http://127.0.0.1:8000/api/v1/devices/pair-cli failed` against a
/// backend nobody started. Worse, a sign-in that DID reach a local backend
/// would mint the device JWT there while the relay dials the ladder's answer,
/// re-opening the prod/local device-JWT split.
///
/// So a machine-local request is honoured only when a deliberate override
/// selects it: the ladder itself resolves to it (`QONTINUI_WEB_BACKEND_URL` /
/// `QONTINUI_API_URL` / profile `api_url`), or it equals the caller's own
/// `extra_override` (see [`interactive_pair_base_with_override`]). Otherwise
/// the fallback is dialed instead. Any other request (a remote backend the
/// operator typed) is dialed as given.
///
/// The Cognito sign-in paths (`commands::auth::finalize_signed_in`) then stage
/// the dialed base into `settings.json`, which also heals the stale persisted
/// value. `redeem_pair_code` does NOT persist a backend URL, so on that path the
/// stale value stays on disk (refused by the ladder) until a sign-in or a
/// Settings save replaces it.
pub(crate) fn interactive_pair_base(requested: &str) -> String {
    interactive_pair_base_with_override(requested, None)
}

/// [`interactive_pair_base`] for a caller with one more deliberate override of
/// its own — `(name, value)`, e.g. `redeem_pair_code`'s `QONTINUI_WEB_BASE`.
/// A blank value counts as unset. When present, a machine-local request equal
/// to it is honoured, and it (not the ladder) is the fallback for a refused
/// request, matching that caller's own precedence.
pub(crate) fn interactive_pair_base_with_override(
    requested: &str,
    extra_override: Option<(&str, &str)>,
) -> String {
    let extra_override = extra_override
        .map(|(name, v)| (name, v.trim().trim_end_matches('/')))
        .filter(|(_, v)| !v.is_empty());
    let (ladder_url, _arm) = get_api_base_url_with_source();
    let chosen =
        choose_interactive_pair_base(requested, &ladder_url, extra_override.map(|(_, v)| v));
    let requested = requested.trim().trim_end_matches('/');
    if chosen != requested {
        let overrides = match extra_override {
            Some((name, _)) => format!(
                "{name}, QONTINUI_WEB_BACKEND_URL, QONTINUI_API_URL, or `api_url` in the active \
                 profile in ~/.qontinui/profiles.json"
            ),
            None => "QONTINUI_WEB_BACKEND_URL, QONTINUI_API_URL, or `api_url` in the active \
                     profile in ~/.qontinui/profiles.json"
                .to_string(),
        };
        let source = match extra_override {
            Some((name, _)) => format!("the {name} override"),
            None => "the same backend the relay uses".to_string(),
        };
        tracing::warn!(
            requested_backend_url = %requested,
            using_backend_url = %chosen,
            "sign-in/pair: REFUSING requested backend '{requested}': it is a MACHINE-LOCAL \
             address and no deliberate override ({overrides}) selects it. Dialing '{chosen}' \
             instead — {source}."
        );
    }
    chosen
}

/// The pure half of [`interactive_pair_base_with_override`]: `requested`
/// (trimmed, no trailing slash) unless it is machine-local and neither the
/// ladder nor `extra_override` selects it, in which case the fallback —
/// `extra_override` when present, else `ladder_url`.
fn choose_interactive_pair_base(
    requested: &str,
    ladder_url: &str,
    extra_override: Option<&str>,
) -> String {
    let norm = |v: &str| v.trim().trim_end_matches('/').to_string();
    let requested = norm(requested);
    let ladder_url = norm(ladder_url);
    let extra_override = extra_override.map(norm).filter(|v| !v.is_empty());
    let selected = requested == ladder_url || extra_override.as_deref() == Some(requested.as_str());
    if persisted_backend_url_refused(&requested) && !selected {
        extra_override.unwrap_or(ladder_url)
    } else {
        requested
    }
}

/// Parse `raw` down to a URL [`Host`](url::Host), or `None` if no reading of it
/// yields one.
///
/// Extracted so every host-class predicate judges the SAME parse. Two
/// predicates asking the same question of two different parsers is the drift
/// [`persisted_backend_url_refused`] exists to prevent, one level down.
///
/// A value no arm can make a host out of yields `None`, and every caller reads
/// that as "not my class": these predicates gate a REFUSAL, so an unparseable
/// value must fall through to the normal precedence and fail loudly at dial
/// time rather than be silently swapped for a different backend.
///
/// Two spellings get a retry before that verdict, because they are what an
/// operator genuinely hand-types into a settings file and neither is a legal
/// URL as written: a scheme-less authority (`127.0.0.1:8000`,
/// `localhost:8000`), which the parser reads as a bare scheme with no host, and
/// a bare IPv6 literal (`::1`), which is not a legal authority unbracketed.
fn backend_url_host(raw: &str) -> Option<url::Host> {
    let trimmed = raw.trim();
    url::Url::parse(trimmed)
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
        .or_else(|| url::Url::parse(&format!("http://[{trimmed}]")).ok())
        .as_ref()
        .and_then(url::Url::host)
        .map(|h| h.to_owned())
}

/// Is this backend URL's host the *unspecified* address — `0.0.0.0` or `::`?
///
/// Separate from [`is_loopback_backend_url`] because it is a different host
/// class and the names have to stay honest: `0.0.0.0` is not a loopback
/// address, it is the wildcard a server BINDS to. But the two are the same
/// fault when a client DIALS them — every mainstream stack maps a connect to
/// the unspecified address onto the local host — so
/// [`persisted_backend_url_refused`] takes either.
///
/// This is a reachable spelling, not a theoretical one: `uvicorn`/`vite` and
/// friends announce themselves as listening on `0.0.0.0:<port>`, which is the
/// line an operator copies. It can never be a legitimate REMOTE backend, so
/// refusing it costs nothing.
fn is_unspecified_backend_url(raw: &str) -> bool {
    match backend_url_host(raw) {
        Some(url::Host::Ipv4(ip)) => ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_unspecified(),
        _ => false,
    }
}

/// Does this backend URL point at the LOCAL machine's loopback interface?
///
/// The host is PARSED, never substring-matched: `https://api.qontinui.io/?next=
/// http://127.0.0.1:8000` contains the literal `127.0.0.1` and is not loopback,
/// while `http://127.9.9.9:8000` contains none of the usual spellings and is.
/// Covers every spelling the item names — `localhost` (and any `*.localhost`
/// subdomain, which RFC 6761 reserves as loopback), the whole `127.0.0.0/8`
/// block rather than just `127.0.0.1`, and IPv6 `::1` in both its bare and
/// bracketed forms (the parser strips the brackets, so one arm covers both)
/// plus its IPv4-mapped spelling `::ffff:127.0.0.1`, which
/// `Ipv6Addr::is_loopback()` alone does NOT recognize.
///
/// It does NOT cover `0.0.0.0` / `::` — those are the unspecified address, a
/// different host class with its own predicate
/// ([`is_unspecified_backend_url`]); [`persisted_backend_url_refused`] is what
/// takes either.
///
/// A value the URL parser cannot make a host out of is NOT loopback — see
/// [`backend_url_host`], which owns the parse and the two retries.
fn is_loopback_backend_url(raw: &str) -> bool {
    match backend_url_host(raw) {
        // Covers 127.0.0.0/8 in full, not just 127.0.0.1.
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        // `::1`, and `[::1]` — the parser has already stripped the brackets.
        //
        // `Ipv6Addr::is_loopback` is TRUE only for `::1`, so it says false for
        // `::ffff:127.0.0.1` — the IPv4-mapped form of a loopback address,
        // which reaches the very same local backend. Unmap first and re-ask,
        // or the refusal has a spelling-shaped hole in it.
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

/// Emitted at most once per process when the ladder refused a
/// machine-local persisted `backend_url` — see
/// [`persisted_backend_url_refused`] for the two host classes that qualify.
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
            arm = %ApiBaseUrlArm::BuildDefaultLoopbackRejected.as_str(),
            "REFUSING persisted web_integration.backend_url '{rejected}': it is a MACHINE-LOCAL \
             address (loopback, or the unspecified bind-all address 0.0.0.0 / ::). Using '{used}' \
             (the build default, production in every build) instead. A persisted machine-local \
             value is usually a stale default written back by an older debug build; honouring it \
             dials a backend only this machine can reach (sign-in fails when none is running, and \
             a runner that does reach one leaves coord.devices.ws_session_id NULL in prod). To use \
             a local backend deliberately, set `api_url` in the active profile in \
             ~/.qontinui/profiles.json or export QONTINUI_WEB_BACKEND_URL. To silence this \
             warning, set web_integration.backend_url in settings.json to the backend this runner \
             actually paired with."
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
/// step 4 into step 5.
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

/// [`get_api_base_url`] plus WHICH of the five documented rungs produced it.
///
/// This is the live-I/O door: it gathers the inputs from this process and
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
    let (url, arm) = resolve_api_base_url(&inputs);
    if arm == ApiBaseUrlArm::BuildDefaultLoopbackRejected {
        warn_persisted_loopback_rejected(persisted.as_deref().unwrap_or("<unset>"), &url);
    }
    (url, arm)
}

/// The inputs [`resolve_api_base_url`] weighs, gathered from the live
/// process in one place.
///
/// Extracted so that "what are the inputs, and where does each come from?"
/// is answered exactly ONCE. [`get_api_base_url`] and the config report both
/// consume this, so the report can never be looking at a different set of
/// inputs than the value every other subsystem actually resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiBaseUrlInputs {
    /// `QONTINUI_WEB_BACKEND_URL` — operator/test explicit override.
    pub env_web: Option<String>,
    /// `QONTINUI_API_URL` — legacy explicit override.
    pub env_api: Option<String>,
    /// The active profile's `api_url` (`~/.qontinui/profiles.json`), the
    /// per-machine rung between the env vars and the persisted value.
    pub profile_api_url: Option<String>,
    /// The persisted paired backend, present only when web-integration is
    /// ENABLED (a disabled integration means "don't reach web", so its stored
    /// URL must not override the build default). See [`persisted_input`].
    pub persisted: Option<String>,
}

/// Read the inputs from env + profile + settings. The only I/O in the resolution.
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
/// the READ-ONLY twin, whose only I/O is two `std::env::var` calls and a pure
/// read of `~/.qontinui/profiles.json`
/// ([`qontinui_runner_lib::profiles::api_url_with_source`], which writes and
/// warns about nothing).
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
        profile_api_url: qontinui_runner_lib::profiles::api_url_with_source().map(|(url, _)| url),
        persisted: persisted_input(s),
    }
}

/// The persisted rung's input from a `Settings`: the stored `backend_url` when
/// web-integration is enabled, else `None`.
///
/// No value is filtered here. A machine-local value is refused by the ladder
/// itself ([`persisted_backend_url_refused`]), under an arm of its own, so the
/// refusal is visible rather than silently attributed to the build default.
/// A value equal to the build default stays `PersistedBackendUrl`: the URL is
/// identical either way, and dropping it would flip the arm to
/// [`ApiBaseUrlArm::BuildDefault`], which [`configured_only`] maps to `None`,
/// so every "configured, else unconfigured" reader (body-sync, tenant-sync)
/// would treat an untouched `settings.json` as unconfigured.
pub(crate) fn persisted_input(s: &crate::settings::Settings) -> Option<String> {
    s.web_integration
        .enabled
        .then(|| s.web_integration.backend_url.clone())
}

/// [`resolve_api_base_url`] over a `Settings` the caller already holds: the
/// ladder's answer for readers that hold a settings snapshot instead of going
/// through [`get_api_base_url_with_source`].
pub(crate) fn resolve_api_base_url_from(
    settings: &crate::settings::Settings,
) -> (String, ApiBaseUrlArm) {
    resolve_api_base_url(&api_base_url_inputs_from(settings))
}

/// The backend base URL the operator CONFIGURED, with the rung that supplied
/// it, or `None` when nothing was: the "configured, else unconfigured"
/// question readers ask when they must not guess (the always-answering
/// [`get_api_base_url`] falls through to a build default). `Some` only for the
/// four configured arms; the two build-default arms yield `None`.
pub(crate) fn configured_api_base_from(
    settings: &crate::settings::Settings,
) -> Option<(String, ApiBaseUrlArm)> {
    configured_only(resolve_api_base_url_from(settings))
}

/// The pure half of [`configured_api_base_from`]: keep a resolution only when
/// a configured rung produced it.
fn configured_only(resolved: (String, ApiBaseUrlArm)) -> Option<(String, ApiBaseUrlArm)> {
    match resolved.1 {
        ApiBaseUrlArm::EnvWebBackendUrl
        | ApiBaseUrlArm::EnvApiUrl
        | ApiBaseUrlArm::ProfileApiUrl
        | ApiBaseUrlArm::PersistedBackendUrl => Some(resolved),
        ApiBaseUrlArm::BuildDefault | ApiBaseUrlArm::BuildDefaultLoopbackRejected => None,
    }
}

/// Which rung of [`resolve_api_base_url`]'s documented five-rung order produced
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
    /// The active profile's `api_url` (`~/.qontinui/profiles.json`) won.
    ProfileApiUrl,
    /// The persisted paired backend won (web-integration enabled).
    PersistedBackendUrl,
    /// Nothing configured; the build default ([`PROD_API_BASE_URL`], the same
    /// in every build) applied.
    BuildDefault,
    /// The ladder REFUSED a machine-local persisted `backend_url` and fell
    /// through to [`PROD_API_BASE_URL`]. Distinct from
    /// [`ApiBaseUrlArm::BuildDefault`] on purpose: the value is identical, but
    /// "a persisted setting was overridden" and "nothing was configured" are
    /// different faults with different remediations — the first leaves a wrong
    /// value in `settings.json` that will keep being refused every start until
    /// someone edits it. See [`resolve_api_base_url`].
    BuildDefaultLoopbackRejected,
}

impl ApiBaseUrlArm {
    /// Stable wire string.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ApiBaseUrlArm::EnvWebBackendUrl => "env:QONTINUI_WEB_BACKEND_URL",
            ApiBaseUrlArm::EnvApiUrl => "env:QONTINUI_API_URL",
            ApiBaseUrlArm::ProfileApiUrl => "profile:api_url",
            ApiBaseUrlArm::PersistedBackendUrl => "persisted:web_integration.backend_url",
            ApiBaseUrlArm::BuildDefault => "build_default",
            ApiBaseUrlArm::BuildDefaultLoopbackRejected => {
                "build_default:persisted_loopback_rejected"
            }
        }
    }

    /// One-line operator remedy for a connection failure to the URL this arm
    /// chose — what to edit or unset to point the runner somewhere else.
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            ApiBaseUrlArm::EnvWebBackendUrl => {
                "Unset or correct the QONTINUI_WEB_BACKEND_URL environment variable."
            }
            ApiBaseUrlArm::EnvApiUrl => {
                "Unset or correct the QONTINUI_API_URL environment variable."
            }
            ApiBaseUrlArm::ProfileApiUrl => "Edit `api_url` in ~/.qontinui/profiles.json.",
            ApiBaseUrlArm::PersistedBackendUrl => {
                "Edit `web_integration.backend_url` in settings.json, or set `api_url` in the active profile in ~/.qontinui/profiles.json."
            }
            ApiBaseUrlArm::BuildDefaultLoopbackRejected => {
                "A machine-local `web_integration.backend_url` in settings.json was refused, so the production default applies; to use a local backend set `api_url` in the active profile in ~/.qontinui/profiles.json, or export QONTINUI_WEB_BACKEND_URL."
            }
            ApiBaseUrlArm::BuildDefault => {
                "Nothing is configured, so the production default applies (in every build); to use a local backend set `api_url` in the active profile in ~/.qontinui/profiles.json, or export QONTINUI_WEB_BACKEND_URL."
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
    /// env rungs, the profile file and key for the profile rung, the settings field
    /// path for the persisted rung, and a
    /// field-shaped label for the build defaults so every arm is judged under
    /// the same connection-string rule.
    ///
    /// Every one of them ends in `_URL` once upper-cased, which is what makes
    /// the joint arm reachable for all of them. That is a property of this
    /// mapping, not a coincidence, and
    /// `config_report_cmd::tests::config_report_api_arm_origin_names_are_url_named`
    /// asserts it against literals.
    pub(crate) fn value_origin_name(self) -> &'static str {
        match self {
            ApiBaseUrlArm::EnvWebBackendUrl => "QONTINUI_WEB_BACKEND_URL",
            ApiBaseUrlArm::EnvApiUrl => "QONTINUI_API_URL",
            ApiBaseUrlArm::ProfileApiUrl => "profiles.json api_url",
            ApiBaseUrlArm::PersistedBackendUrl => "web_integration.backend_url",
            ApiBaseUrlArm::BuildDefault
            // The VALUE this arm yields is the build default; the rejected
            // persisted value is named in the warning, not here.
            | ApiBaseUrlArm::BuildDefaultLoopbackRejected => "build_default.backend_url",
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

/// Supervisor `host:port` for raw connect probes, taken from
/// [`get_supervisor_url`]. `None` when that URL names no explicit port.
///
/// It used to fall back to `127.0.0.1:{DEFAULT_SUPERVISOR_PORT}` in that case,
/// so a probe could answer for an address the configured URL never named —
/// and the observation would then report the URL, not what was probed.
pub fn get_supervisor_socket_addr() -> Option<String> {
    supervisor_host_port(&get_supervisor_url())
}

/// PURE: the `host:port` authority of a supervisor URL (scheme and path
/// stripped), or `None` when it names no explicit port.
pub fn supervisor_host_port(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    // `[::1]` alone contains ':' but no port; require a port after the last
    // `]` (IPv6 literal) or anywhere (host name / IPv4).
    let tail = host_port.rsplit_once(']').map(|x| x.1).unwrap_or(host_port);
    let (_, port) = tail.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    Some(host_port.to_string())
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

    /// The three legacy rungs as positional arguments, with no profile value —
    /// keeps the pre-profile precedence tests readable. Tests that involve the
    /// profile rung build an [`ApiBaseUrlInputs`] directly.
    fn resolve(
        env_web: Option<String>,
        env_api: Option<String>,
        persisted: Option<String>,
    ) -> (String, ApiBaseUrlArm) {
        resolve_api_base_url(&ApiBaseUrlInputs {
            env_web,
            env_api,
            profile_api_url: None,
            persisted,
        })
    }

    fn inputs_all(profile: &str, persisted: &str) -> ApiBaseUrlInputs {
        ApiBaseUrlInputs {
            env_web: Some("https://web.example".to_string()),
            env_api: Some("https://api.example".to_string()),
            profile_api_url: Some(profile.to_string()),
            persisted: Some(persisted.to_string()),
        }
    }

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
    fn supervisor_host_port_requires_an_explicit_port() {
        assert_eq!(
            supervisor_host_port("http://127.0.0.1:4242").as_deref(),
            Some("127.0.0.1:4242")
        );
        assert_eq!(
            supervisor_host_port("http://localhost:4242/x/y").as_deref(),
            Some("localhost:4242")
        );
        assert_eq!(
            supervisor_host_port("http://[::1]:4242").as_deref(),
            Some("[::1]:4242")
        );
        // No port: never silently substitute a default the URL did not name.
        assert_eq!(supervisor_host_port("http://sup.example"), None);
        assert_eq!(supervisor_host_port("http://[::1]"), None);
        assert_eq!(supervisor_host_port("http://host:notaport"), None);
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
            resolve(web(), api(), persisted()),
            (
                "https://web.example".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
        // env_api wins over persisted + default.
        assert_eq!(
            resolve(None, api(), persisted()),
            ("https://api.example".to_string(), ApiBaseUrlArm::EnvApiUrl)
        );
        // persisted wins over the build default.
        assert_eq!(
            resolve(None, None, persisted()),
            (
                "https://persisted.example".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
    }

    /// The operator decision this pins: the build default is PRODUCTION in
    /// every build, debug included. A debug runner with nothing configured
    /// must never dial a local backend nobody started.
    #[test]
    fn resolve_api_base_url_build_default_is_prod() {
        assert_eq!(
            resolve(None, None, None),
            (PROD_API_BASE_URL.to_string(), ApiBaseUrlArm::BuildDefault)
        );
    }

    #[test]
    fn resolve_api_base_url_skips_blank_and_trims() {
        // Blank / whitespace at any level is treated as unset (not selected).
        assert_eq!(
            resolve(
                Some("   ".to_string()),
                Some("".to_string()),
                Some("https://persisted.example".to_string()),
            ),
            (
                "https://persisted.example".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
        // A blank persisted with no env falls through to the build default.
        assert_eq!(
            resolve(None, None, Some("  ".to_string())),
            (PROD_API_BASE_URL.to_string(), ApiBaseUrlArm::BuildDefault)
        );
        // Trailing slash is trimmed on the chosen value.
        assert_eq!(
            resolve(Some("https://web.example/".to_string()), None, None),
            (
                "https://web.example".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
    }

    /// Regression for the prod/local device-JWT split (plan 2026-07-08): a
    /// runner whose user signed into a backend must resolve to THAT backend,
    /// with the persisted arm — so the relay verifies against the same coord
    /// that minted the device JWT.
    ///
    /// The ARM is what makes this regression legible: the returned string is
    /// byte-identical to the build default, so a report that printed only the
    /// value could not tell "the user paired with prod" from "nothing is
    /// configured" — two different bugs.
    #[test]
    fn resolve_api_base_url_honors_persisted_prod() {
        assert_eq!(
            resolve(None, None, Some(PROD_API_BASE_URL.to_string())),
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
        assert_eq!(ApiBaseUrlArm::ProfileApiUrl.as_str(), "profile:api_url");
        assert_eq!(
            ApiBaseUrlArm::PersistedBackendUrl.as_str(),
            "persisted:web_integration.backend_url"
        );
        assert_eq!(ApiBaseUrlArm::BuildDefault.as_str(), "build_default");
        assert_eq!(
            ApiBaseUrlArm::BuildDefaultLoopbackRejected.as_str(),
            "build_default:persisted_loopback_rejected"
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

    /// Every spelling of "this machine" is refused as a persisted value, and
    /// the build default applies under the arm that says a persisted value was
    /// OVERRIDDEN rather than absent.
    ///
    /// Two outages in one assertion: a release runner that honoured an
    /// inherited `http://127.0.0.1:8000` left `coord.devices.ws_session_id`
    /// NULL in prod, and a debug runner that honoured the same value written
    /// back by an older debug build failed sign-in with
    /// `POST http://127.0.0.1:8000/api/v1/devices/pair-cli failed`.
    #[test]
    fn refuses_every_loopback_spelling_of_persisted_backend_url() {
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
                resolve(None, None, Some(spelling.to_string())),
                (
                    PROD_API_BASE_URL.to_string(),
                    ApiBaseUrlArm::BuildDefaultLoopbackRejected
                ),
                "must refuse persisted loopback {spelling}"
            );
        }
    }

    /// The refusal is narrow: a persisted value that points at a REAL remote
    /// backend is still honoured — that rung exists to close the prod/local
    /// device-JWT split (plan 2026-07-08) and must keep working.
    #[test]
    fn honors_a_remote_persisted_backend_url() {
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
            let (url, arm) = resolve(None, None, Some(remote.to_string()));
            assert_eq!(
                arm,
                ApiBaseUrlArm::PersistedBackendUrl,
                "must honour persisted remote {remote}"
            );
            assert_eq!(url, remote.trim_end_matches('/'));
        }
    }

    /// The predicate the OUT-OF-LADDER readers ask agrees with the ladder's own
    /// verdict, for every spelling.
    ///
    /// This is the anti-divergence assertion. `device_jwt_refresher` (which
    /// MINTS the device JWT) and `memory::tenant_sync::resolve_web_base` (which
    /// uploads memory records) read the persisted `backend_url` directly, so a
    /// refusal only `resolve_api_base_url` honoured would mean the runner mints
    /// a credential at one backend and presents it at another. Asserting the
    /// two against each other — rather than restating the rule — is what makes
    /// that class of drift a test failure instead of a production outage.
    #[test]
    fn refusal_predicate_agrees_with_the_ladder() {
        let loopback = [
            "http://127.0.0.1:8000",
            "http://LOCALHOST:8000",
            "http://api.localhost:8000",
            "http://127.1.2.3:8000",
            "http://[::1]:8000",
            "127.0.0.1:8000",
            "::1",
            // IPv4-mapped IPv6 loopback — the same local backend, spelled the
            // one way `Ipv6Addr::is_loopback()` alone says false to.
            "http://[::ffff:127.0.0.1]:8000",
        ];
        let remote = [
            "https://api.qontinui.io",
            "http://192.168.1.50:8000",
            "https://localhost.example.test",
            "http://128.0.0.1:8000",
            // Mapped, but mapped onto a REMOTE address: unmapping must re-ask
            // `is_loopback()`, not treat every mapped address as local.
            "http://[::ffff:8.8.8.8]:8000",
        ];
        for candidate in loopback.iter().chain(remote.iter()) {
            let (_, arm) = resolve(None, None, Some((*candidate).to_string()));
            let ladder_refused = arm == ApiBaseUrlArm::BuildDefaultLoopbackRejected;
            assert_eq!(
                persisted_backend_url_refused(candidate),
                ladder_refused,
                "predicate and ladder must agree on {candidate}"
            );
            // And the refusal is exactly "loopback".
            assert_eq!(
                ladder_refused,
                loopback.contains(candidate),
                "unexpected verdict for {candidate}"
            );
        }
    }

    /// One shared input table drives the ladder, the configured-only helper
    /// and the out-of-ladder readers' mappings, so none can drift: the helper
    /// answers exactly for the four configured arms; a persisted value the
    /// readers must treat as REFUSED is exactly the loopback-rejected arm,
    /// whose URL is the build default they defer to; and a blank persisted
    /// value is plainly unconfigured (helper `None`, not refused).
    #[test]
    fn configured_helper_and_reader_mappings_agree_over_one_table() {
        let persisted_values = [
            None,
            Some("".to_string()),
            Some("   ".to_string()),
            Some("http://127.0.0.1:8000".to_string()),
            Some("http://[::1]:8000".to_string()),
            Some("https://api.qontinui.io".to_string()),
            Some("http://192.168.1.50:8000".to_string()),
        ];
        let env = [None, Some("https://env.example".to_string())];
        let profile = [None, Some("https://profile.example".to_string())];
        for persisted in &persisted_values {
            for e in &env {
                for p in &profile {
                    let resolved = resolve_api_base_url(&ApiBaseUrlInputs {
                        env_web: e.clone(),
                        env_api: None,
                        profile_api_url: p.clone(),
                        persisted: persisted.clone(),
                    });
                    let (url, arm) = resolved.clone();
                    let configured = configured_only(resolved);
                    let ctx = format!("{persisted:?} env={e:?} profile={p:?}");
                    let is_configured_arm = matches!(
                        arm,
                        ApiBaseUrlArm::EnvWebBackendUrl
                            | ApiBaseUrlArm::EnvApiUrl
                            | ApiBaseUrlArm::ProfileApiUrl
                            | ApiBaseUrlArm::PersistedBackendUrl
                    );
                    assert_eq!(configured.is_some(), is_configured_arm, "{ctx}");
                    if let Some((cu, ca)) = &configured {
                        assert_eq!((cu, *ca), (&url, arm), "{ctx}");
                    }
                    // Readers' refusal fallback: a refused persisted value
                    // shows up as the loopback-rejected arm and nothing else.
                    let refused_persisted = e.is_none()
                        && p.is_none()
                        && persisted
                            .as_deref()
                            .is_some_and(|v| persisted_backend_url_refused(v.trim()));
                    assert_eq!(
                        refused_persisted,
                        arm == ApiBaseUrlArm::BuildDefaultLoopbackRejected,
                        "{ctx}"
                    );
                    if refused_persisted {
                        assert!(configured.is_none(), "{ctx}");
                        assert_eq!(url, PROD_API_BASE_URL, "{ctx}");
                    }
                }
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
            assert!(
                !persisted_backend_url_refused(blank),
                "blank must be unset, not refused"
            );
        }
    }

    /// Only the PERSISTED rung is filtered. An operator who exports a loopback
    /// override is making a deliberate, visible choice (that is how you point
    /// a runner at a local backend on purpose), and the higher rungs outrank
    /// the persisted one anyway.
    #[test]
    fn loopback_refusal_does_not_touch_the_env_rungs() {
        assert_eq!(
            resolve(
                Some("http://127.0.0.1:8000".to_string()),
                None,
                Some("http://localhost:8000".to_string()),
            ),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
        assert_eq!(
            resolve(
                None,
                Some("http://localhost:8000".to_string()),
                Some("http://127.0.0.1:8000".to_string()),
            ),
            (
                "http://localhost:8000".to_string(),
                ApiBaseUrlArm::EnvApiUrl
            )
        );
    }

    /// A blank persisted value is ABSENT, not refused — the two arms must stay
    /// distinguishable, because only one of them means "there is a wrong value
    /// sitting in settings.json".
    #[test]
    fn blank_persisted_is_absent_not_rejected() {
        assert_eq!(
            resolve(None, None, Some("   ".to_string())),
            (PROD_API_BASE_URL.to_string(), ApiBaseUrlArm::BuildDefault)
        );
        assert_eq!(
            resolve(None, None, None),
            (PROD_API_BASE_URL.to_string(), ApiBaseUrlArm::BuildDefault)
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
            // IPv4-mapped IPv6 loopback. `Ipv6Addr::is_loopback()` is FALSE for
            // these, so they need the explicit unmap in the Ipv6 arm; each one
            // reaches exactly the same local backend `127.0.0.1` does.
            "http://[::ffff:127.0.0.1]:8000",
            "http://[::ffff:7f00:1]:8000",
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
            // The unspecified address is NOT loopback — it is refused by
            // `is_unspecified_backend_url` instead, so that the two predicate
            // names stay honest about which host class each one judges.
            "http://0.0.0.0:8000",
            "http://[::]:8000",
            // An IPv4-MAPPED address that is not loopback stays remote: the
            // unmap must re-ask `is_loopback()`, not assume mapped == local.
            "http://[::ffff:8.8.8.8]:8000",
        ] {
            assert!(!is_loopback_backend_url(no), "{no} is not loopback");
        }
    }

    /// The bind-all address is refused too, and by its OWN predicate.
    ///
    /// `0.0.0.0` is what a dev server prints when it starts ("listening on
    /// 0.0.0.0:8000"), so it is the string an operator copies into
    /// `settings.json` — a reachable spelling of the same outage, not a
    /// theoretical one. Dialed rather than bound it means "this host", so a
    /// runner honouring it talks to a backend only this machine can reach,
    /// exactly as a loopback value would.
    #[test]
    fn refuses_the_unspecified_bind_all_backend_url() {
        for yes in [
            "http://0.0.0.0:8000",
            "http://0.0.0.0",
            "0.0.0.0:8000",
            "http://[::]:8000",
            "http://[0:0:0:0:0:0:0:0]:8000",
            "  http://0.0.0.0:8000  ",
        ] {
            assert!(is_unspecified_backend_url(yes), "{yes} is unspecified");
            assert!(persisted_backend_url_refused(yes), "REFUSES {yes}");
            // The LADDER must reach the same verdict as the predicate — the
            // same anti-drift assertion the loopback class already carries.
            let (_, arm) = resolve(None, None, Some(yes.to_string()));
            assert_eq!(
                arm,
                ApiBaseUrlArm::BuildDefaultLoopbackRejected,
                "predicate and ladder must agree on {yes}"
            );
        }
        for no in [
            "https://api.qontinui.io",
            "http://127.0.0.1:8000",
            "http://localhost:8000",
            "http://192.168.1.50:8000",
            // Not a substring match: the literal appears in the query, not the
            // host, and the host is what decides.
            "https://api.qontinui.io/proxy?to=http://0.0.0.0:8000",
            "",
            "not a url at all",
        ] {
            assert!(!is_unspecified_backend_url(no), "{no} is not unspecified");
        }
    }

    /// The whole point of [`backend_url_host`] is that every host-class
    /// predicate judges ONE parse. Pin that they agree on what the host IS,
    /// rather than each growing its own reader.
    #[test]
    fn the_two_host_class_predicates_share_one_parse() {
        // Every spelling either predicate accepts must be a value the shared
        // parser could read a host out of — otherwise one of them is parsing
        // somewhere else.
        for machine_local in [
            "http://127.0.0.1:8000",
            "127.0.0.1:8000",
            "::1",
            "http://[::ffff:127.0.0.1]:8000",
            "http://0.0.0.0:8000",
            "http://[::]:8000",
        ] {
            assert!(
                backend_url_host(machine_local).is_some(),
                "{machine_local} parses to a host"
            );
            assert!(
                persisted_backend_url_refused(machine_local),
                "REFUSES {machine_local}"
            );
        }
        // And the two classes are disjoint — nothing is both, so the OR in
        // `persisted_backend_url_refused` can never double-count a spelling.
        for any in [
            "http://127.0.0.1:8000",
            "http://0.0.0.0:8000",
            "http://[::]:8000",
            "http://[::1]:8000",
            "https://api.qontinui.io",
        ] {
            assert!(
                !(is_loopback_backend_url(any) && is_unspecified_backend_url(any)),
                "{any} belongs to exactly one host class"
            );
        }
    }

    /// Full precedence chain, every rung configured, peeled one at a time from
    /// the top.
    #[test]
    fn precedence_chain_across_all_rungs() {
        let mut i = inputs_all("https://profile.example", "https://persisted.example");
        assert_eq!(
            resolve_api_base_url(&i),
            (
                "https://web.example".to_string(),
                ApiBaseUrlArm::EnvWebBackendUrl
            )
        );
        i.env_web = None;
        assert_eq!(
            resolve_api_base_url(&i),
            ("https://api.example".to_string(), ApiBaseUrlArm::EnvApiUrl)
        );
        i.env_api = None;
        assert_eq!(
            resolve_api_base_url(&i),
            (
                "https://profile.example".to_string(),
                ApiBaseUrlArm::ProfileApiUrl
            )
        );
        i.profile_api_url = None;
        assert_eq!(
            resolve_api_base_url(&i),
            (
                "https://persisted.example".to_string(),
                ApiBaseUrlArm::PersistedBackendUrl
            )
        );
        i.persisted = None;
        assert_eq!(
            resolve_api_base_url(&i),
            (PROD_API_BASE_URL.to_string(), ApiBaseUrlArm::BuildDefault)
        );
    }

    /// A profile value outranks a persisted LOOPBACK one, and is itself never
    /// refused for being loopback (a deliberate act — it is the documented door
    /// for developing against a local backend).
    #[test]
    fn profile_beats_persisted_loopback_and_is_not_refused() {
        let mut i = ApiBaseUrlInputs {
            env_web: None,
            env_api: None,
            profile_api_url: Some("https://api.qontinui.io".to_string()),
            persisted: Some("http://127.0.0.1:8000".to_string()),
        };
        assert_eq!(
            resolve_api_base_url(&i),
            (
                "https://api.qontinui.io".to_string(),
                ApiBaseUrlArm::ProfileApiUrl
            )
        );
        // A loopback PROFILE value is honoured.
        i.profile_api_url = Some("http://127.0.0.1:8000/".to_string());
        assert_eq!(
            resolve_api_base_url(&i),
            (
                "http://127.0.0.1:8000".to_string(),
                ApiBaseUrlArm::ProfileApiUrl
            )
        );
    }

    #[test]
    fn blank_profile_api_url_is_unset() {
        for blank in ["", "  ", "\t"] {
            let i = ApiBaseUrlInputs {
                env_web: None,
                env_api: None,
                profile_api_url: Some(blank.to_string()),
                persisted: Some("https://persisted.example".to_string()),
            };
            assert_eq!(
                resolve_api_base_url(&i),
                (
                    "https://persisted.example".to_string(),
                    ApiBaseUrlArm::PersistedBackendUrl
                )
            );
        }
    }

    /// The operator's stale file, end to end through the live input door: a
    /// `settings.json` carrying the old debug default `http://127.0.0.1:8000`
    /// (written back by an older debug build's settings save) must NOT win in
    /// any build — the ladder refuses it and answers with production.
    ///
    /// Drives `api_base_url_inputs_from` with env locked and cleared; the
    /// profile input it reads from disk is overwritten before resolving so the
    /// developer's real profiles.json cannot leak in.
    #[test]
    fn stale_persisted_debug_default_does_not_win() {
        let _env_lock = env_lock();
        std::env::remove_var("QONTINUI_WEB_BACKEND_URL");
        std::env::remove_var("QONTINUI_API_URL");
        let mut settings = crate::settings::Settings::default();
        settings.web_integration.enabled = true;
        settings.web_integration.backend_url = "http://127.0.0.1:8000".to_string();
        let mut i = api_base_url_inputs_from(&settings);
        assert_eq!(i.persisted.as_deref(), Some("http://127.0.0.1:8000"));
        i.profile_api_url = None;
        assert_eq!(
            resolve_api_base_url(&i),
            (
                PROD_API_BASE_URL.to_string(),
                ApiBaseUrlArm::BuildDefaultLoopbackRejected
            )
        );
    }

    /// A persisted value equal to the build default is KEPT and attributed to
    /// the persisted rung in every build: the URL is identical either way, and
    /// dropping it would un-configure the "configured, else unconfigured"
    /// readers (body-sync, tenant-sync).
    #[test]
    fn default_equal_persisted_value_stays_configured() {
        for v in [
            PROD_API_BASE_URL,
            "https://api.qontinui.io/",
            "  https://api.qontinui.io ",
        ] {
            let mut settings = crate::settings::Settings::default();
            settings.web_integration.enabled = true;
            settings.web_integration.backend_url = v.to_string();
            let resolved = resolve_api_base_url(&ApiBaseUrlInputs {
                env_web: None,
                env_api: None,
                profile_api_url: None,
                persisted: persisted_input(&settings),
            });
            assert_eq!(
                resolved,
                (
                    PROD_API_BASE_URL.to_string(),
                    ApiBaseUrlArm::PersistedBackendUrl
                ),
                "{v:?}"
            );
            assert!(configured_only(resolved).is_some(), "{v:?}");
        }
    }

    /// Sign-in must never dial the stale machine-local value the UI read out
    /// of `settings.json` — unless a deliberate override resolves the ladder
    /// to that very backend.
    #[test]
    fn interactive_pair_base_replaces_an_unselected_machine_local_request() {
        // The operator's case: stale 127.0.0.1:8000, nothing deliberate set.
        assert_eq!(
            choose_interactive_pair_base("http://127.0.0.1:8000", PROD_API_BASE_URL, None),
            PROD_API_BASE_URL
        );
        assert_eq!(
            choose_interactive_pair_base(" http://0.0.0.0:8000/ ", PROD_API_BASE_URL, None),
            PROD_API_BASE_URL
        );
        // A deliberate local override (env/profile) resolves the ladder to the
        // same backend: honoured.
        assert_eq!(
            choose_interactive_pair_base("http://127.0.0.1:8000/", "http://127.0.0.1:8000", None),
            "http://127.0.0.1:8000"
        );
        // A deliberate override to a DIFFERENT local spelling wins over the
        // stale one — the ladder is the authority.
        assert_eq!(
            choose_interactive_pair_base("http://127.0.0.1:8000", "http://localhost:8001", None),
            "http://localhost:8001"
        );
        // A remote request is dialed as given, whatever the ladder says.
        assert_eq!(
            choose_interactive_pair_base("https://backend.example.test/", PROD_API_BASE_URL, None),
            "https://backend.example.test"
        );
    }

    /// `redeem_pair_code`'s `QONTINUI_WEB_BASE` is a deliberate override on
    /// that path: a machine-local request equal to it is honoured, and it is
    /// the fallback (not the ladder) for a refused request. Blank is unset.
    #[test]
    fn interactive_pair_base_honours_the_callers_extra_override() {
        assert_eq!(
            choose_interactive_pair_base(
                "http://127.0.0.1:8000",
                PROD_API_BASE_URL,
                Some("http://127.0.0.1:8000/")
            ),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            choose_interactive_pair_base(
                "http://127.0.0.1:8000",
                PROD_API_BASE_URL,
                Some("https://web.example.test")
            ),
            "https://web.example.test"
        );
        assert_eq!(
            choose_interactive_pair_base("http://127.0.0.1:8000", PROD_API_BASE_URL, Some("  ")),
            PROD_API_BASE_URL
        );
        // A remote request still wins over the extra override.
        assert_eq!(
            choose_interactive_pair_base(
                "https://backend.example.test",
                PROD_API_BASE_URL,
                Some("http://127.0.0.1:8000")
            ),
            "https://backend.example.test"
        );
    }

    #[test]
    fn disabled_web_integration_yields_no_persisted_input() {
        let mut s = crate::settings::Settings::default();
        s.web_integration.enabled = false;
        s.web_integration.backend_url = "https://elsewhere.example".to_string();
        assert_eq!(persisted_input(&s), None);
    }

    #[test]
    fn remedy_names_the_place_to_fix_for_every_arm() {
        for (arm, needle) in [
            (ApiBaseUrlArm::EnvWebBackendUrl, "QONTINUI_WEB_BACKEND_URL"),
            (ApiBaseUrlArm::EnvApiUrl, "QONTINUI_API_URL"),
            (ApiBaseUrlArm::ProfileApiUrl, "profiles.json"),
            (
                ApiBaseUrlArm::PersistedBackendUrl,
                "web_integration.backend_url",
            ),
            (ApiBaseUrlArm::BuildDefault, "api_url"),
            (
                ApiBaseUrlArm::BuildDefaultLoopbackRejected,
                "web_integration.backend_url",
            ),
        ] {
            assert!(
                arm.remedy().contains(needle),
                "{arm:?} remedy {:?} must mention {needle}",
                arm.remedy()
            );
        }
    }
}
