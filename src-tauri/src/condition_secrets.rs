//! Condition-check secrets: held for a live condition session, typed only by
//! the runner's SDK `type` action, and never shown back.
//!
//! Plan
//! `2026-11-06-a-regression-test-run-is-judged-by-what-it-covered-and-a-silent-run-reads-unknown`
//! Phase 13 (decisions D-D7 and D-D7a). Coord renders each credential-keyed
//! field of a condition group's `auth_setup` recipe into the condition-check
//! prompt as a `{{secret:<ref>}}` placeholder, and sends `ref -> value` in the
//! spawn frame's own `secrets` key (the plan's Phase 12). The agent types the
//! PLACEHOLDER; the runner puts the value in on the way to the app. So the
//! value never enters the agent's model context: not its prompt, not a tool
//! result, not its transcript.
//!
//! # Rules
//!
//! 1. **Held per session.** [`register`] holds a frame's map, with the origin
//!    of the frame's `target_url`, from just before the condition-check PTY is
//!    created. The [`Registration`] it returns releases the map when it drops:
//!    on any failed spawn, or, once handed to [`release_at_pty_exit`], when the
//!    PTY exits. A map can be substituted for [`SUBSTITUTE_TTL`]; it stays held
//!    (so it is still scrubbed) until the PTY exits, with [`HOLD_TTL`] as the
//!    backstop for an exit nobody observed.
//! 2. **Substituted in the `type` action's text only, on the way to the
//!    session's own app only.** [`app_bound_type_body`] substitutes
//!    `params.text` and a top-level `text` (ui-bridge's `type` handler reads
//!    `request.params?.text || request.text`) in a COPY of the request: the copy
//!    `mcp::sdk_client::handle_element_action` sends to the app as its
//!    WebSocket payload or HTTP body. It substitutes only when the app the
//!    action lands on is the one the holding session targets: its origin is
//!    the origin of the frame's `target_url`, read where the dispatch chooses
//!    its arm (`mcp::sdk_client::destination_origin`), so one session's secret
//!    is never typed into another app. The request itself is never
//!    substituted, because the IPC fallback, the journey ledger and the
//!    `ui_bridge_events` row all read it.
//! 3. **Never typed as literal text.** While a session holds a secret, a
//!    placeholder anywhere else (another action, another field, a `text` that
//!    is not a string) is refused, so the literal `{{secret:<ref>}}` never
//!    reaches an app as a password. The IPC fallback runs when no SDK app is
//!    connected, so its target is the runner's webview: it never substitutes,
//!    and [`ipc_refusal`] refuses any placeholder that would reach it. The
//!    `/ui-bridge/control/*`, `/ui-bridge/ai/*`, `/ui-bridge/relay/dispatch`
//!    and `/ui-bridge/batch` routes the `/ui-bridge/*` composer serves never
//!    substitute either: while a secret is held,
//!    [`refuse_unsubstituted_placeholders`] refuses any request to them that
//!    carries a placeholder, before its handler runs.
//! 4. **Never shown back.** While any map is held, [`scrub_sdk_responses`]
//!    replaces each held value, and each truncated echo of one (a prefix of at
//!    least [`MIN_ECHO_CHARS`] characters ending in `...` or `…`, the way
//!    ui-bridge shortens a field's value in its summaries), with [`REDACTED`] in
//!    every response of a router that returns app content: the
//!    `/ui-bridge/sdk/*` routers, `/ui-bridge/apps/{app_id}/dispatch`, the
//!    `/ui-bridge/control/*` element and effect routes, whose WebSocket-app
//!    branches answer with the app's own content, and the relay's
//!    `/ui-bridge/relay/dispatch`, which answers with a tab's. [`scrub_action_result`] does
//!    so before an action's ledger rows are written, and [`debug_scrubbed`] and
//!    [`scrub_text`] before the relay logs app content. The mask names no ref,
//!    so nothing shown back hands a reader a ref to replay.
//!
//! An unknown, malformed or ambiguous ref, a placeholder outside a `type`
//! action's text, and a session whose app is not where the action lands are
//! each refused with a typed error ([`SubstitutionError`]). A refusal names a
//! ref only as unknown: the ref text the caller sent, which nothing here will
//! substitute for it (no session holds it, its window has closed, or the
//! surface never substitutes). Nothing here logs a value: log lines carry a
//! run id and counts.
//!
//! # Accepted residuals
//!
//! - **A very short secret over-scrubs.** Scrubbing is substring replacement,
//!   so a one- or two-character value is replaced wherever those characters
//!   occur in a scrubbed response while its session lives. That can garble
//!   unrelated text, a JSON number equal to the value, a base64 image whose
//!   encoding happens to contain it, and even the mask itself. So does a
//!   secret that begins with ordinary text: that text is masked wherever it is
//!   cut short before an ellipsis (`Please wait...`). It fails safe: the value
//!   is never shown; the session reads less of the page.
//! - **Pixels are not scrubbed.** A screenshot of a field that renders its
//!   value in clear text shows the value; only text a response carries is
//!   scrubbed. (A password input renders its value masked.)
//! - **Only the raw and JSON-escaped forms are matched.** A value an app
//!   echoes URL-encoded, HTML-escaped or base64-encoded is not recognised, nor
//!   is a truncated echo shorter than [`MIN_ECHO_CHARS`] characters or one that
//!   ends in anything but `...` or `…`.
//! - **A value byte-scrubbed out of a bare JSON number** leaves an unquoted
//!   mask, so that body no longer parses. It still carries no value.
//! - **The binding reads the destination twice.** The origin check and the
//!   dispatch each read the process-wide active SDK connection and the app
//!   registry's row for it; a switch in the microseconds between the two reads
//!   is not caught. A relay-routed app binds only through the browser origin
//!   its registrant registered with, so one registered by a non-browser
//!   process or by tab key never substitutes.
//! - **Only the SDK element-action route substitutes.** The runner's own
//!   routes do not, and the composer's refuse a placeholder only while a
//!   secret is held: with nothing held a placeholder there is ordinary text.
//!   Three `/ui-bridge/control/*` and `/ui-bridge/ai/*` routes are mounted
//!   outside that composer (`ai/wait-for`, `control/wait-for-app`,
//!   `control/sdk/spawn-headless`) and are not checked; none of them types
//!   text. No secret ever reaches any of them.
//! - **Loopback replay.** `POST /ui-bridge/sdk/element/{id}/action` is a
//!   `TRUSTED_ROUTES` door (`mcp::origin_guard`), so a local process, or a
//!   page on a Trusted origin, that knows a ref can have its value typed into
//!   the session's own app while that app is the active connection and the
//!   session lives. Refs are per run and appear only in that run's prompt and
//!   in its session transcript on disk; nothing the runner shows back names
//!   one.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::mcp::origin_guard::NormOrigin;
use crate::terminal::TerminalManager;

/// Opens a placeholder: `{{secret:<ref>}}`.
pub const PLACEHOLDER_OPEN: &str = "{{secret:";

/// Closes a placeholder.
pub const PLACEHOLDER_CLOSE: &str = "}}";

/// The longest ref, in characters.
///
/// **The ref grammar is the wire contract with coord's prompt builder
/// (Phase 12):** 1 to 128 characters from `[A-Za-z0-9_.:-]`. A frame entry
/// whose ref falls outside it is dropped ([`FrameSecrets`]), so a placeholder
/// naming such a ref is refused as unknown.
const MAX_REF_CHARS: usize = 128;

/// The one action whose text is substituted. ui-bridge matches the action
/// name exactly (`case 'type'` in its element-action handler).
const TYPE_ACTION: &str = "type";

/// How long a held map can be substituted.
///
/// Two hours is four times the 1800-second report deadline the plan's D-D4
/// gives coord's condition-run reaper: a session still typing that late reports
/// into a run coord has already closed. A map past it is still held, and still
/// scrubbed, until its PTY exits ([`HOLD_TTL`]).
pub const SUBSTITUTE_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a held map outlives an exit nobody observed.
///
/// The PTY-exit watcher normally releases a map within [`PTY_EXIT_POLL`] of
/// its session ending, and keeps it held (so a live session's responses stay
/// scrubbed) for as long as the PTY lives. This bounds the case where the exit
/// is never observed: a pane the manager never lets go of. (A watcher dropped
/// with the runtime drops its [`Registration`], which releases the map.)
pub const HOLD_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the exit watcher reads the PTY's liveness flag (an atomic load).
const PTY_EXIT_POLL: Duration = Duration::from_secs(1);

/// The largest non-streaming response the scrub buffers. A larger one is
/// withheld (500) rather than sent unscrubbed; the runner API's request-body
/// limit is the same 100 MiB.
const SCRUB_BUFFER_LIMIT: usize = 100 * 1024 * 1024;

/// What a held value is replaced with in anything the runner shows back.
///
/// Deliberately NOT a valid placeholder: `*` is outside the ref grammar, so the
/// mask names no ref (a reader learns nothing it could replay on the action
/// route), and a `type` action that sends it back is refused as an unknown ref
/// instead of typing it.
pub const REDACTED: &str = "{{secret:*}}";

/// The shortest prefix of a held value the scrub recognises as a truncated
/// echo. ui-bridge shortens a field's value to 12 characters in a form summary
/// and 17 in an element description (`truncate` in its `summary-generator.ts`,
/// via `truncateCodePoints`), so every such echo of a longer secret is at least
/// this long.
pub const MIN_ECHO_CHARS: usize = 8;

/// The endings of a truncated echo: ui-bridge appends `...` (its
/// `summary-generator.ts`, `semantic-snapshot.ts`, `change-tracker.ts`) or `…`
/// (its `search-engine.ts`).
const ELLIPSES: [&str; 2] = ["...", "…"];

/// A placeholder named a ref no live condition session holds.
pub const UNKNOWN_REF_CODE: &str = "CONDITION_SECRET_UNKNOWN_REF";

/// A placeholder named a ref two live sessions hold with different values.
pub const AMBIGUOUS_REF_CODE: &str = "CONDITION_SECRET_AMBIGUOUS_REF";

/// A placeholder sat somewhere other than a `type` action's text.
pub const OUTSIDE_TYPE_TEXT_CODE: &str = "CONDITION_SECRET_OUTSIDE_TYPE_TEXT";

/// The active SDK connection is not the app the holding session targets.
pub const WRONG_APP_CODE: &str = "CONDITION_SECRET_WRONG_APP";

/// A response that could not be scrubbed while a secret was held.
const SCRUB_FAILED_CODE: &str = "CONDITION_SECRET_SCRUB_FAILED";

/// A request to a route that never substitutes, which could not be read to
/// check it for a placeholder while a secret was held.
const REQUEST_UNREADABLE_CODE: &str = "CONDITION_SECRET_REQUEST_UNREADABLE";

fn is_ref_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

fn is_valid_ref(reference: &str) -> bool {
    !reference.is_empty()
        && reference.chars().count() <= MAX_REF_CHARS
        && reference.chars().all(is_ref_char)
}

// =============================================================================
// The frame's `secrets` key
// =============================================================================

/// A spawn frame's `secrets` key: `ref -> value` for each `{{secret:<ref>}}`
/// placeholder coord rendered into the prompt.
///
/// Parsed LENIENTLY, deliberately. A malformed entry (a value that is not a
/// string, or a ref outside the grammar on [`MAX_REF_CHARS`]) is dropped and
/// counted, never a reason to refuse the whole frame: a refused frame posts
/// nothing and leaves the run `running`, while a dropped entry turns the one
/// placeholder naming it into a typed refusal the agent reports. A `secrets`
/// value that is not an object at all counts as one malformed entry; `null`
/// and an absent key are no secrets.
///
/// `Debug` prints COUNTS only, never a ref's value.
#[derive(Clone, Default)]
pub struct FrameSecrets {
    refs: BTreeMap<String, String>,
    malformed: usize,
}

impl FrameSecrets {
    fn from_wire(raw: Value) -> Self {
        let mut secrets = Self::default();
        match raw {
            Value::Null => {}
            Value::Object(entries) => {
                for (reference, value) in entries {
                    match value {
                        Value::String(value) if is_valid_ref(&reference) => {
                            secrets.refs.insert(reference, value);
                        }
                        _ => secrets.malformed += 1,
                    }
                }
            }
            _ => secrets.malformed += 1,
        }
        secrets
    }

    /// How many usable refs the frame carried.
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    /// True when the frame carried no usable ref.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }
}

impl<'de> Deserialize<'de> for FrameSecrets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(Value::deserialize(deserializer)?))
    }
}

impl fmt::Debug for FrameSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSecrets")
            .field("refs", &self.refs.len())
            .field("malformed", &self.malformed)
            .finish()
    }
}

// =============================================================================
// Holding a session's map
// =============================================================================

/// One condition session's held map.
struct Held {
    run_id: String,
    /// The origin of the frame's `target_url`: the one app this session's
    /// values may be typed into. `None` when the URL does not parse, which
    /// binds to nothing, so nothing is ever substituted for it.
    target: Option<NormOrigin>,
    refs: BTreeMap<String, String>,
    /// [`SUBSTITUTE_TTL`] after registration: substitution stops here.
    substitutable_until: Instant,
    /// [`HOLD_TTL`] after registration: the map is released here at the
    /// latest, whether or not its PTY exit was observed.
    held_until: Instant,
}

impl Held {
    /// True when this session may have its values typed into the app at
    /// `active`, the active SDK connection's origin.
    fn binds(&self, active: Option<&NormOrigin>) -> bool {
        match (&self.target, active) {
            (Some(target), Some(active)) => target.same_principal(active),
            _ => false,
        }
    }
}

/// Bumped whenever the held maps change, so [`ScrubSet::current`] rebuilds
/// its cached set only then. Written only while the [`held`] lock is held, so
/// a reader holding that lock sees a generation that matches what it reads.
static HELD_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The last [`ScrubSet`] built, with the [`HELD_GENERATION`] it was built at.
/// It holds the secret values, so it is emptied the moment no map is held.
///
/// Lock order: [`held`] first, then this; nothing takes them the other way
/// round. No tracing layer may call into this module either: the hold-TTL
/// purge's warning is written under the [`held`] lock.
static SCRUB_CACHE: Mutex<Option<(u64, Option<Arc<ScrubSet>>)>> = Mutex::new(None);

/// A change to the held maps, made while their lock is held: the cached scrub
/// set is stale, and with nothing held it is dropped outright.
fn held_changed(sessions: &HashMap<u64, Held>) {
    HELD_GENERATION.fetch_add(1, Ordering::Relaxed);
    if sessions.is_empty() {
        *SCRUB_CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Every held map, keyed by its registration id, never by run id, so two
/// deliveries of one run's frame hold and release independently.
///
/// Reading through this accessor is what enforces [`HOLD_TTL`]: a map past
/// it is dropped on the next read, whoever reads.
fn held() -> MutexGuard<'static, HashMap<u64, Held>> {
    static HELD: OnceLock<Mutex<HashMap<u64, Held>>> = OnceLock::new();
    let mut sessions = HELD
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    let before = sessions.len();
    sessions.retain(|_, session| {
        let live = session.held_until > now;
        if !live {
            warn!(
                "condition_secrets: run_id={}: held secrets reached their hold TTL with no \
                 observed PTY exit; released",
                session.run_id
            );
        }
        live
    });
    if sessions.len() != before {
        held_changed(&sessions);
    }
    sessions
}

/// The release handle of a held map: dropping it releases the map.
///
/// Hand it to [`release_at_pty_exit`] once the session has started; any
/// earlier exit from the spawn path (a refused, failed or never-started spawn)
/// drops it there and then.
#[must_use = "dropping a Registration releases the session's secrets at once"]
pub struct Registration {
    id: u64,
    run_id: String,
    held_until: Instant,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let released = {
            let mut sessions = held();
            let released = sessions.remove(&self.id).is_some();
            if released {
                held_changed(&sessions);
            }
            released
        };
        if released {
            debug!(
                "condition_secrets: run_id={}: released its held secrets",
                self.run_id
            );
        }
    }
}

/// Hold `secrets` for the condition session of `run_id`, bound to the origin
/// of its `target_url`: substitutable for [`SUBSTITUTE_TTL`], held for at most
/// [`HOLD_TTL`]. `None` when the frame carried no usable ref, so there is
/// nothing to hold.
#[must_use = "dropping the Registration releases the session's secrets at once"]
pub fn register(run_id: &str, target_url: &str, secrets: &FrameSecrets) -> Option<Registration> {
    register_for(run_id, target_url, secrets, SUBSTITUTE_TTL, HOLD_TTL)
}

fn register_for(
    run_id: &str,
    target_url: &str,
    secrets: &FrameSecrets,
    substitute_ttl: Duration,
    hold_ttl: Duration,
) -> Option<Registration> {
    if secrets.malformed > 0 {
        warn!(
            "condition_secrets: run_id={run_id}: {} malformed `secrets` entries dropped (a value \
             that is not a string, or a ref outside the placeholder grammar); a placeholder \
             naming one is refused as an unknown ref",
            secrets.malformed
        );
    }
    if secrets.refs.is_empty() {
        return None;
    }
    let target = NormOrigin::parse(target_url);
    if target.is_none() {
        warn!(
            "condition_secrets: run_id={run_id}: target_url `{target_url}` has no origin, so \
             no held secret of this session is ever substituted"
        );
    }
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let now = Instant::now();
    let held_until = now + hold_ttl;
    {
        let mut sessions = held();
        sessions.insert(
            id,
            Held {
                run_id: run_id.to_string(),
                target,
                refs: secrets.refs.clone(),
                substitutable_until: now + substitute_ttl,
                held_until,
            },
        );
        held_changed(&sessions);
    }
    info!(
        "condition_secrets: run_id={run_id}: holding {} secret ref(s) for this session",
        secrets.refs.len()
    );
    Some(Registration {
        id,
        run_id: run_id.to_string(),
        held_until,
    })
}

/// Hand a started session's [`Registration`] to a watcher that releases it
/// when the session's PTY exits (the moment `terminal_id` is no longer a live
/// pane of `terminal_manager`), or at [`HOLD_TTL`]. `None`, a frame that
/// carried no secrets, is a no-op.
pub fn release_at_pty_exit(
    registration: Option<Registration>,
    terminal_manager: &Arc<TerminalManager>,
    terminal_id: &str,
) {
    release_at_pty_exit_polling(registration, terminal_manager, terminal_id, PTY_EXIT_POLL);
}

fn release_at_pty_exit_polling(
    registration: Option<Registration>,
    terminal_manager: &Arc<TerminalManager>,
    terminal_id: &str,
    poll: Duration,
) {
    let Some(registration) = registration else {
        return;
    };
    let manager = Arc::clone(terminal_manager);
    let terminal_id = terminal_id.to_string();
    // The registration moves INTO the task, so a task that is dropped instead
    // of finishing (the runtime shutting down) releases the map as well.
    tokio::spawn(async move {
        while Instant::now() < registration.held_until
            && manager
                .get(&terminal_id)
                .is_some_and(|pane| pane.is_alive())
        {
            tokio::time::sleep(poll).await;
        }
        drop(registration);
    });
}

// =============================================================================
// Substitution
// =============================================================================

/// Why a placeholder was not substituted. Either way, nothing was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstitutionError {
    /// No live condition session holds this ref, or the placeholder is
    /// malformed, or it would reach a surface that never substitutes (the IPC
    /// fallback). `reference` is the text the placeholder named.
    UnknownRef { reference: String },
    /// Two live sessions hold this ref with DIFFERENT values. Typing either
    /// could put one session's secret into another session's app.
    AmbiguousRef { reference: String },
    /// A placeholder sat outside a `type` action's `params.text` and top-level
    /// `text`, the only text this module substitutes: in another action, or
    /// in another field of a `type`. Sent on, the app would get the literal
    /// placeholder as, say, a password. `action` is the request's action name.
    OutsideTypeText { action: String },
    /// The active SDK connection is not the app the holding session targets:
    /// another session's app, a connection a peer switched to, or none at
    /// all. Both are origins as `scheme://host[:port]`; `None` is no target
    /// origin (an unparseable `target_url`) or no active connection, or one
    /// whose origin is unknown.
    WrongApp {
        target: Option<String>,
        active: Option<String>,
    },
}

impl SubstitutionError {
    /// The typed refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownRef { .. } => UNKNOWN_REF_CODE,
            Self::AmbiguousRef { .. } => AMBIGUOUS_REF_CODE,
            Self::OutsideTypeText { .. } => OUTSIDE_TYPE_TEXT_CODE,
            Self::WrongApp { .. } => WRONG_APP_CODE,
        }
    }

    /// The text the refused placeholder named, for the two refusals that are
    /// about one ref. `None` for the others.
    pub fn reference(&self) -> Option<&str> {
        match self {
            Self::UnknownRef { reference } | Self::AmbiguousRef { reference } => Some(reference),
            Self::OutsideTypeText { .. } | Self::WrongApp { .. } => None,
        }
    }

    /// The body an SDK route answers a refused substitution with:
    /// `success: false`, the typed `code`, and `failure_origin: "runner"`.
    /// The runner refused before any UI was touched, so the journey ledger
    /// records nothing for it (`journey::capture::sdk_refusal`). It never
    /// carries a value. Only the unknown-ref refusal names a ref, the text the
    /// caller itself sent; the others do not, because the message also lands
    /// in the `ui_bridge_events` row, and a held ref read there could be
    /// replayed.
    pub fn refusal(&self, detail: &str) -> Value {
        let what = match self {
            Self::UnknownRef { reference } => {
                format!("no live condition-check session holds secret ref `{reference}`")
            }
            Self::AmbiguousRef { .. } => {
                "two live condition-check sessions hold different values for this secret ref"
                    .to_string()
            }
            Self::OutsideTypeText { action } => format!(
                "a `{PLACEHOLDER_OPEN}<ref>{PLACEHOLDER_CLOSE}` placeholder is substituted only \
                 in a `{TYPE_ACTION}` action's `params.text` or `text`, and this `{action}` request \
                 carries one elsewhere"
            ),
            Self::WrongApp { target, active } => format!(
                "the active SDK connection ({}) is not the app this condition check targets ({}); \
                 connect to the target first (POST /ui-bridge/sdk/connect with its target_url)",
                active.as_deref().unwrap_or("none, or one of unknown origin"),
                target.as_deref().unwrap_or("an unparseable target_url"),
            ),
        };
        serde_json::json!({
            "success": false,
            "error": format!("{}: {what}; {detail}. Nothing was typed.", self.code()),
            "code": self.code(),
            "failure_origin": crate::journey::capture::RUNNER_REJECTION_ORIGIN,
            "detail": detail,
            "retryable": false,
        })
    }
}

/// The value the live sessions hold under `reference`, when they agree and
/// one of them targets the app at `active` (the active SDK connection's
/// origin). A session past [`SUBSTITUTE_TTL`] no longer substitutes, though
/// its map is still held for the scrub.
fn resolve(reference: &str, active: Option<&NormOrigin>) -> Result<String, SubstitutionError> {
    let now = Instant::now();
    let sessions = held();
    let holders: Vec<&Held> = sessions
        .values()
        .filter(|session| session.substitutable_until > now && session.refs.contains_key(reference))
        .collect();
    let Some(value) = holders
        .first()
        .and_then(|session| session.refs.get(reference))
    else {
        return Err(SubstitutionError::UnknownRef {
            reference: reference.to_string(),
        });
    };
    if holders
        .iter()
        .any(|session| session.refs.get(reference) != Some(value))
    {
        return Err(SubstitutionError::AmbiguousRef {
            reference: reference.to_string(),
        });
    }
    if !holders.iter().any(|session| session.binds(active)) {
        return Err(SubstitutionError::WrongApp {
            target: holders
                .first()
                .and_then(|session| session.target.as_ref())
                .map(NormOrigin::as_origin_string),
            active: active.map(NormOrigin::as_origin_string),
        });
    }
    Ok(value.clone())
}

/// The ref a placeholder names, and the text after its closing `}}`.
/// `after_open` is the text right after a [`PLACEHOLDER_OPEN`]. On a
/// malformed or unterminated placeholder: the text it named (at most 64
/// characters), for the refusal.
fn parse_ref(after_open: &str) -> Result<(&str, &str), String> {
    let end = after_open
        .find(|c: char| !is_ref_char(c))
        .unwrap_or(after_open.len());
    let (reference, tail) = after_open.split_at(end);
    match tail.strip_prefix(PLACEHOLDER_CLOSE) {
        Some(after_close) if is_valid_ref(reference) => Ok((reference, after_close)),
        _ => {
            let named = after_open
                .split(PLACEHOLDER_CLOSE)
                .next()
                .unwrap_or_default();
            let mut shown: String = named.chars().take(64).collect();
            if named.chars().count() > 64 {
                shown.push('…');
            }
            Err(shown)
        }
    }
}

/// `text` with every placeholder replaced by its held value, for the app at
/// `active`. One placeholder that cannot be substituted refuses the whole
/// text, so a secret is never typed with part of its field missing.
fn substitute(text: &str, active: Option<&NormOrigin>) -> Result<String, SubstitutionError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after_open)) = rest.split_once(PLACEHOLDER_OPEN) {
        out.push_str(before);
        let (reference, after_close) = parse_ref(after_open)
            .map_err(|reference| SubstitutionError::UnknownRef { reference })?;
        out.push_str(&resolve(reference, active)?);
        rest = after_close;
    }
    out.push_str(rest);
    Ok(out)
}

fn carries_placeholder(field: Option<&Value>) -> bool {
    field
        .and_then(Value::as_str)
        .is_some_and(|text| text.contains(PLACEHOLDER_OPEN))
}

fn substitute_field(
    field: Option<&mut Value>,
    active: Option<&NormOrigin>,
) -> Result<(), SubstitutionError> {
    if let Some(Value::String(text)) = field {
        *text = substitute(text, active)?;
    }
    Ok(())
}

/// The copy of an element-action request the APP receives, when the action
/// lands on the app at `active` (its origin).
///
/// - No placeholder anywhere: `None`, meaning the request goes to the app as
///   it is.
/// - A `type` action whose placeholders all sit in a string `params.text` or a
///   string top-level `text`: a copy with each one replaced by its held value,
///   when the holding session targets the app at `active`.
/// - While a condition session holds a secret, a placeholder anywhere else
///   (another action, another field of a `type`, a `text` that is not a
///   string): [`SubstitutionError::OutsideTypeText`], so the literal never
///   reaches the app. With nothing held there is nothing it could stand for,
///   so outside a `type` action's text it is ordinary text.
///
/// `body` itself is never changed.
pub fn app_bound_type_body(
    body: &Value,
    active: Option<&NormOrigin>,
) -> Result<Option<Value>, SubstitutionError> {
    // The common case, no placeholder, never takes the held lock.
    if first_placeholder(body).is_none() {
        return Ok(None);
    }
    let guarding = !held().is_empty();
    app_bound_type_body_while(body, active, guarding)
}

/// [`app_bound_type_body`], with whether any session holds a secret
/// (`guarding`) given rather than read.
fn app_bound_type_body_while(
    body: &Value,
    active: Option<&NormOrigin>,
    guarding: bool,
) -> Result<Option<Value>, SubstitutionError> {
    if first_placeholder(body).is_none() {
        return Ok(None);
    }
    let action = body.get("action").and_then(Value::as_str);
    let outside = || SubstitutionError::OutsideTypeText {
        action: action.unwrap_or("<no action>").chars().take(64).collect(),
    };
    if action != Some(TYPE_ACTION) {
        return if guarding { Err(outside()) } else { Ok(None) };
    }
    // Everything but the two substituted fields must be free of placeholders,
    // and those two only when they are strings: ui-bridge coerces any other
    // `text` to a string and would type the literal.
    let mut rest = body.clone();
    if let Some(fields) = rest.as_object_mut() {
        if fields.get("text").is_some_and(Value::is_string) {
            fields.remove("text");
        }
        if let Some(params) = fields.get_mut("params").and_then(Value::as_object_mut) {
            if params.get("text").is_some_and(Value::is_string) {
                params.remove("text");
            }
        }
    }
    if guarding && first_placeholder(&rest).is_some() {
        return Err(outside());
    }
    let mut app_copy = body.clone();
    if carries_placeholder(body.get("params").and_then(|p| p.get("text"))) {
        substitute_field(
            app_copy.get_mut("params").and_then(|p| p.get_mut("text")),
            active,
        )?;
    }
    if carries_placeholder(body.get("text")) {
        substitute_field(app_copy.get_mut("text"), active)?;
    }
    Ok(Some(app_copy))
}

/// The ref named by the first placeholder in `value`, a string or object key
/// at any depth (for a malformed placeholder, the text it named).
fn first_placeholder(value: &Value) -> Option<String> {
    let in_text = |text: &str| {
        text.split_once(PLACEHOLDER_OPEN)
            .map(|(_, after_open)| match parse_ref(after_open) {
                Ok((reference, _)) => reference.to_string(),
                Err(named) => named,
            })
    };
    match value {
        Value::String(text) => in_text(text),
        Value::Array(items) => items.iter().find_map(first_placeholder),
        Value::Object(fields) => fields
            .iter()
            .find_map(|(key, field)| in_text(key).or_else(|| first_placeholder(field))),
        _ => None,
    }
}

/// The refusal for an element-action request headed for the IPC fallback,
/// when it carries a placeholder anywhere.
///
/// The fallback acts on the runner's own webview and never substitutes, so a
/// placeholder there is refused as an unknown ref, a held one included. The
/// whole request is searched, not only `params.text`: whatever reaches that
/// surface is the literal placeholder, and it is never typed.
pub fn ipc_refusal(body: &Value) -> Option<SubstitutionError> {
    first_placeholder(body).map(|reference| SubstitutionError::UnknownRef { reference })
}

/// The `(body, success, error)` result `handle_element_action` answers and
/// records for a refused substitution.
pub fn refused_action(
    refused: &SubstitutionError,
    detail: &str,
) -> (Json<Value>, bool, Option<String>) {
    let body = refused.refusal(detail);
    let message = body.get("error").and_then(Value::as_str).map(String::from);
    (Json(body), false, message)
}

// =============================================================================
// Scrubbing
// =============================================================================

/// The longest truncated echo the scrub looks for, in characters. ui-bridge's
/// longest cut keeps 57 (`semantic-snapshot.ts`); the bound keeps the prefix
/// list small for a long secret such as a private key.
const MAX_ECHO_CHARS: usize = 256;

/// What the scrub looks for while a secret is held: every form a held value
/// takes in text (raw and JSON-escaped), and every truncated echo of one.
struct ScrubSet {
    /// The raw values, for the exact-number arm of [`ScrubSet::scrub_json`].
    values: Vec<String>,
    /// Each value raw and JSON-escaped, deduplicated, never empty.
    forms: Vec<Vec<u8>>,
    /// Each value's proper prefixes of [`MIN_ECHO_CHARS`] to
    /// [`MAX_ECHO_CHARS`] characters, raw and JSON-escaped, longest first: an
    /// echo is one of these right before an ellipsis.
    prefixes: Vec<Vec<u8>>,
}

fn push_form(list: &mut Vec<Vec<u8>>, form: String) {
    if !form.is_empty() && !list.iter().any(|seen| seen.as_slice() == form.as_bytes()) {
        list.push(form.into_bytes());
    }
}

/// The first index at or after `from` where `needle` occurs in `hay`.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// `hay` with each of `spans` (sorted, disjoint byte ranges) replaced by
/// [`REDACTED`].
fn mask(hay: &[u8], spans: &[(usize, usize)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut at = 0;
    for &(start, end) in spans {
        out.extend_from_slice(hay.get(at..start).unwrap_or_default());
        out.extend_from_slice(REDACTED.as_bytes());
        at = end;
    }
    out.extend_from_slice(hay.get(at..).unwrap_or_default());
    out
}

impl ScrubSet {
    /// `None` when no condition session holds a secret: the path every
    /// response takes outside a condition check. Built once per change to the
    /// held maps ([`HELD_GENERATION`]) and shared until the next one, rather
    /// than rebuilt for every response and every event.
    fn current() -> Option<Arc<Self>> {
        // Lock order: `held` first, then the cache ([`SCRUB_CACHE`]).
        let sessions = held();
        let generation = HELD_GENERATION.load(Ordering::Relaxed);
        let mut cache = SCRUB_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((built_at, set)) = cache.as_ref() {
            if *built_at == generation {
                return set.clone();
            }
        }
        let set = Self::build(&sessions).map(Arc::new);
        *cache = Some((generation, set.clone()));
        set
    }

    fn build(sessions: &HashMap<u64, Held>) -> Option<Self> {
        let mut values: Vec<String> = Vec::new();
        for session in sessions.values() {
            for value in session.refs.values() {
                if !value.is_empty() && !values.contains(value) {
                    values.push(value.clone());
                }
            }
        }
        if values.is_empty() {
            return None;
        }
        let mut forms = Vec::new();
        let mut prefixes = Vec::new();
        for value in &values {
            push_form(&mut forms, value.clone());
            push_form(&mut forms, json_escaped(value));
            // `value[..at]` holds exactly `count` characters.
            for (count, (at, _)) in value.char_indices().enumerate() {
                if (MIN_ECHO_CHARS..=MAX_ECHO_CHARS).contains(&count) {
                    let prefix = value.get(..at).unwrap_or_default();
                    push_form(&mut prefixes, prefix.to_string());
                    push_form(&mut prefixes, json_escaped(prefix));
                }
            }
        }
        prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
        Some(Self {
            values,
            forms,
            prefixes,
        })
    }

    /// The byte ranges of `hay` holding a held value (in either form) or a
    /// truncated echo of one, sorted, with every overlapping or touching pair
    /// merged: two held values that overlap in the text are masked as their
    /// union, never as whichever one starts first.
    fn spans(&self, hay: &[u8]) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        for form in &self.forms {
            let mut from = 0;
            while let Some(at) = find_from(hay, form, from) {
                spans.push((at, at + form.len()));
                from = at + 1;
            }
        }
        if !self.prefixes.is_empty() {
            for ellipsis in ELLIPSES {
                let ellipsis = ellipsis.as_bytes();
                let mut from = 0;
                while let Some(at) = find_from(hay, ellipsis, from) {
                    let echo = self.prefixes.iter().find(|prefix| {
                        at >= prefix.len()
                            && hay.get(at - prefix.len()..at) == Some(prefix.as_slice())
                    });
                    if let Some(prefix) = echo {
                        spans.push((at - prefix.len(), at));
                    }
                    // One byte on, not past the ellipsis: in `....` the cut
                    // may sit after the first dot (a prefix ending in `.`).
                    from = at + 1;
                }
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        merged
    }

    /// Byte-level scrub: every held value, raw and JSON-escaped, and every
    /// truncated echo of one, replaced by [`REDACTED`]. `None` when there is
    /// none.
    fn scrub_bytes(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let spans = self.spans(bytes);
        (!spans.is_empty()).then(|| mask(bytes, &spans))
    }

    /// [`ScrubSet::scrub_bytes`] over text. `None` when `text` holds nothing
    /// held.
    fn scrub_str(&self, text: &str) -> Option<String> {
        self.scrub_bytes(text.as_bytes()).map(|bytes| {
            // Every span starts and ends on a character boundary, because each
            // needle is whole UTF-8, so this is always valid: the lossy arm is
            // a guard, never a path.
            String::from_utf8(bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
        })
    }

    /// Scrub every string and object key at any depth, and every number whose
    /// decimal form IS a held value. True when anything changed.
    fn scrub_json(&self, value: &mut Value) -> bool {
        match value {
            Value::String(text) => match self.scrub_str(text) {
                Some(scrubbed) => {
                    *text = scrubbed;
                    true
                }
                None => false,
            },
            Value::Number(number) => {
                let decimal = number.to_string();
                if self.values.iter().any(|held| *held == decimal) {
                    *value = Value::String(REDACTED.to_string());
                    true
                } else {
                    false
                }
            }
            Value::Array(items) => {
                let mut changed = false;
                for item in items.iter_mut() {
                    changed |= self.scrub_json(item);
                }
                changed
            }
            Value::Object(fields) => {
                let mut changed = false;
                if fields.keys().any(|key| self.scrub_str(key).is_some()) {
                    for (key, field) in std::mem::take(fields) {
                        let mut key = self.scrub_str(&key).unwrap_or(key);
                        // Two keys masked to one spelling keep both fields.
                        let base = key.clone();
                        let mut copy = 1;
                        while fields.contains_key(&key) {
                            copy += 1;
                            key = format!("{base} ({copy})");
                        }
                        fields.insert(key, field);
                    }
                    changed = true;
                }
                for field in fields.values_mut() {
                    changed |= self.scrub_json(field);
                }
                changed
            }
            _ => false,
        }
    }

    /// One chunk of an event stream, scrubbed. axum's `Sse` writes one event
    /// per chunk; a chunk may still carry several (an upstream relayed as it
    /// is), so it is split into events at blank lines first. `None` when
    /// nothing changed.
    fn scrub_sse_frame(&self, frame: &[u8]) -> Option<Vec<u8>> {
        let Ok(text) = std::str::from_utf8(frame) else {
            return self.scrub_bytes(frame);
        };
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        let mut event: Vec<&str> = Vec::new();
        for line in text.split_inclusive('\n') {
            event.push(line);
            if line_content(line).is_empty() {
                changed |= self.scrub_sse_event(&event, &mut out);
                event.clear();
            }
        }
        if !event.is_empty() {
            changed |= self.scrub_sse_event(&event, &mut out);
        }
        changed.then(|| out.into_bytes())
    }

    /// One event's lines (each with its terminator) written to `out`,
    /// scrubbed. An event's `data:` lines are ONE value joined by `\n` (the
    /// event-stream rule), so a held value split across two of them is still
    /// found. Scrubbed, that value is written back at the first `data:` line,
    /// one `data:` line per line of it; every other line is scrubbed as text.
    /// True when anything changed.
    fn scrub_sse_event(&self, lines: &[&str], out: &mut String) -> bool {
        let data_at: Vec<usize> = (0..lines.len())
            .filter(|&i| {
                lines
                    .get(i)
                    .is_some_and(|line| line_content(line).starts_with("data:"))
            })
            .collect();
        let payload = |i: usize| {
            let data = lines
                .get(i)
                .map(|line| line_content(line))
                .and_then(|content| content.strip_prefix("data:"))
                .unwrap_or_default();
            data.strip_prefix(' ').unwrap_or(data)
        };
        let joined: Vec<&str> = data_at.iter().map(|&i| payload(i)).collect();
        let scrubbed_data = if joined.is_empty() {
            None
        } else {
            self.scrub_sse_data(&joined.join("\n"))
        };
        let mut changed = scrubbed_data.is_some();
        for (i, line) in lines.iter().enumerate() {
            let content = line_content(line);
            let terminator = line.get(content.len()..).unwrap_or_default();
            match (
                &scrubbed_data,
                data_at.first() == Some(&i),
                data_at.contains(&i),
            ) {
                (Some(data), true, _) => {
                    // Always `data: `: the reader strips one space after the
                    // colon, so a line of the value that starts with a space
                    // keeps it only behind one.
                    // The last data line's terminator closes the rewritten
                    // block, so a chunk that ended mid-line still does.
                    let last_terminator = data_at
                        .last()
                        .and_then(|&last| lines.get(last))
                        .map(|last| last.get(line_content(last).len()..).unwrap_or_default())
                        .unwrap_or(terminator);
                    let mut rewritten = data.split('\n').peekable();
                    while let Some(part) = rewritten.next() {
                        out.push_str("data: ");
                        out.push_str(part);
                        out.push_str(if rewritten.peek().is_some() {
                            "\n"
                        } else {
                            last_terminator
                        });
                    }
                }
                (Some(_), false, true) => {}
                _ => match self.scrub_str(content) {
                    Some(scrubbed) if !data_at.contains(&i) => {
                        changed = true;
                        out.push_str(&scrubbed);
                        out.push_str(terminator);
                    }
                    _ => out.push_str(line),
                },
            }
        }
        changed
    }

    /// An event's data value, scrubbed: JSON field by field (re-serialized on
    /// one line), else as text. A JSON value in which no field changed is
    /// still scrubbed as text, because a parse can hide what the text carries
    /// (a duplicate key keeps only its last value). `None` when nothing
    /// changed.
    fn scrub_sse_data(&self, data: &str) -> Option<String> {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(data) {
            if self.scrub_json(&mut parsed) {
                return Some(parsed.to_string());
            }
        }
        self.scrub_str(data)
    }
}

/// A line without its `\n` or `\r\n` terminator.
fn line_content(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// `value` as it appears inside a JSON string literal.
fn json_escaped(value: &str) -> String {
    serde_json::to_string(value)
        .ok()
        .and_then(|quoted| {
            quoted
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// An element action's `(body, success, error)` with every held value
/// scrubbed. `handle_element_action` runs it BEFORE the journey ledger and the
/// `ui_bridge_events` row read the result: an app may echo what was typed (a
/// field's new value, an error naming the text it refused).
pub fn scrub_action_result(
    (Json(mut body), success, error): (Json<Value>, bool, Option<String>),
) -> (Json<Value>, bool, Option<String>) {
    let Some(set) = ScrubSet::current() else {
        return (Json(body), success, error);
    };
    set.scrub_json(&mut body);
    let error = error.map(|message| set.scrub_str(&message).unwrap_or(message));
    (Json(body), success, error)
}

/// `format!("{value:?}")` with every held value scrubbed first, for a log
/// line that formats app-sent JSON.
pub fn debug_scrubbed(value: &Option<Value>) -> String {
    match (ScrubSet::current(), value) {
        (Some(set), Some(inner)) => {
            let mut scrubbed = inner.clone();
            set.scrub_json(&mut scrubbed);
            format!("{:?}", Some(scrubbed))
        }
        _ => format!("{value:?}"),
    }
}

/// `text` with every held value, and every truncated echo of one, masked: for
/// a log line or an error detail that may quote app content (serde's error
/// for a mistyped field quotes the value it could not read).
pub fn scrub_text(text: &str) -> Cow<'_, str> {
    match ScrubSet::current().and_then(|set| set.scrub_str(text)) {
        Some(scrubbed) => Cow::Owned(scrubbed),
        None => Cow::Borrowed(text),
    }
}

/// `axum::middleware::from_fn` layer on every router that returns app content
/// to a caller (module rule 4): while a condition session holds a secret, no
/// response carries its value.
///
/// With nothing held a response passes untouched. With something held:
/// - a JSON body is scrubbed field by field ([`ScrubSet::scrub_json`]) and
///   re-serialized only when something changed; any other body, and a JSON
///   body in which no field changed, byte by byte;
/// - an event stream is scrubbed event by event as it flows, never buffered
///   (it may never end), against the sessions live at that event, so a stream
///   opened before a session started is scrubbed once it does;
/// - a body that cannot be read in full is withheld with a 500, never sent
///   unscrubbed.
///
/// Never layer it on a channel that carries a command TO an app (the
/// WebSocket relay, the relay's event stream): it would mask the very value
/// a `type` action substituted for the app.
pub async fn scrub_sdk_responses(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if is_event_stream(response.headers()) {
        return scrub_event_stream(response);
    }
    match ScrubSet::current() {
        Some(set) => scrub_buffered(response, &set).await,
        None => response,
    }
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    content_type(headers).starts_with("text/event-stream")
}

fn scrub_event_stream(response: Response) -> Response {
    let (parts, body) = response.into_parts();
    let frames = body.into_data_stream().map(|frame| {
        frame.map(
            |bytes| match ScrubSet::current().and_then(|set| set.scrub_sse_frame(&bytes)) {
                Some(scrubbed) => Bytes::from(scrubbed),
                None => bytes,
            },
        )
    });
    Response::from_parts(parts, Body::from_stream(frames))
}

async fn scrub_buffered(response: Response, set: &ScrubSet) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, SCRUB_BUFFER_LIMIT).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("condition_secrets: a response could not be read for scrubbing ({e}); withheld");
            return scrub_failed();
        }
    };
    let parsed = if content_type(&parts.headers).contains("json") {
        serde_json::from_slice::<Value>(&bytes).ok()
    } else {
        None
    };
    let mut scrubbed = None;
    if let Some(mut value) = parsed {
        if set.scrub_json(&mut value) {
            match serde_json::to_vec(&value) {
                Ok(serialized) => scrubbed = Some(serialized),
                Err(e) => {
                    warn!(
                        "condition_secrets: a scrubbed response could not be re-serialized \
                         ({e}); withheld"
                    );
                    return scrub_failed();
                }
            }
        }
    }
    // Not JSON, or JSON in which no field changed: the bytes themselves,
    // because a parse can hide what they still carry (a duplicate key keeps
    // only its last value, a number is read in its canonical spelling).
    let scrubbed = scrubbed.or_else(|| set.scrub_bytes(&bytes));
    match scrubbed {
        None => Response::from_parts(parts, Body::from(bytes)),
        Some(scrubbed) => {
            parts.headers.remove(header::CONTENT_LENGTH);
            Response::from_parts(parts, Body::from(scrubbed))
        }
    }
}

fn scrub_failed() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "success": false,
            "error": format!(
                "{SCRUB_FAILED_CODE}: a condition-check session holds a secret and this \
                 response could not be scrubbed, so it was withheld"
            ),
            "code": SCRUB_FAILED_CODE,
        })),
    )
        .into_response()
}

// =============================================================================
// Refusing placeholders on the routes that never substitute
// =============================================================================

/// Path prefixes of the runner's own UI Bridge action surface. None of its
/// routes substitutes a placeholder (only the SDK element-action route does),
/// so whatever text they carry to an app is typed or matched as it stands.
const NON_SUBSTITUTING_PREFIXES: [&str; 2] = ["/ui-bridge/control/", "/ui-bridge/ai/"];

/// Routes outside those prefixes that also carry a caller's text to an app
/// unsubstituted: the relay's dispatch, which forwards its `payload` verbatim
/// to a relay tab, and the batch route, which runs `execute_action` and
/// `fill_form` operations over the same path as `control/batch-actions`.
const NON_SUBSTITUTING_PATHS: [&str; 2] = ["/ui-bridge/relay/dispatch", "/ui-bridge/batch"];

/// Whether `path` is a route that never substitutes and carries text to an app.
fn never_substitutes(path: &str) -> bool {
    NON_SUBSTITUTING_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || NON_SUBSTITUTING_PATHS.contains(&path)
}

/// Why [`refuse_unsubstituted_placeholders`] refused, for the refusal body.
const NON_SUBSTITUTING_DETAIL: &str = "the runner's /ui-bridge/control/*, /ui-bridge/ai/*, \
     /ui-bridge/relay/dispatch and /ui-bridge/batch routes never substitute a secret placeholder, so it would reach the app as literal text; \
     type it with a `type` action on POST /ui-bridge/sdk/element/{id}/action";

/// Request layer for the `/ui-bridge/*` composer: while a condition session
/// holds a secret, a request to a route that never substitutes
/// ([`NON_SUBSTITUTING_PREFIXES`], [`NON_SUBSTITUTING_PATHS`]) that
/// carries a `{{secret:<ref>}}` placeholder, in its query or anywhere in its
/// body, is refused with `422` and [`UNKNOWN_REF_CODE`] (as [`ipc_refusal`]
/// refuses the IPC fallback) before its handler runs. Without it those routes
/// (`page/type-into`, `fill`, `key`, `batch-actions`, a component action, …)
/// would type the literal placeholder into the app, say as a password.
///
/// Any other path, and every request while nothing is held, passes untouched
/// and unbuffered. It reads only the request, never the response, so the
/// relay's command stream that the same composer merges is unaffected. A body
/// that cannot be read in full is refused rather than passed unchecked.
pub async fn refuse_unsubstituted_placeholders(request: Request, next: Next) -> Response {
    if !never_substitutes(request.uri().path()) || held().is_empty() {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, SCRUB_BUFFER_LIMIT).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(
                "condition_secrets: a {} request could not be read to check for a secret \
                 placeholder ({e}); refused",
                parts.uri.path()
            );
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!(
                        "{REQUEST_UNREADABLE_CODE}: a condition-check session holds a secret and \
                         this request could not be read to check it for a placeholder, so it \
                         was refused"
                    ),
                    "code": REQUEST_UNREADABLE_CODE,
                    "failure_origin": crate::journey::capture::RUNNER_REJECTION_ORIGIN,
                })),
            )
                .into_response();
        }
    };
    if let Some(reference) = request_placeholder(parts.uri.query(), &bytes) {
        info!(
            "condition_secrets: refused a secret placeholder on {}, which never substitutes",
            parts.uri.path()
        );
        let refused = SubstitutionError::UnknownRef { reference };
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(refused.refusal(NON_SUBSTITUTING_DETAIL)),
        )
            .into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// The ref named by the first placeholder in a request's query or body. The
/// body is read both as text (so a non-JSON body, and what a JSON parse hides
/// behind a duplicate key, are read) and as JSON (so a `\u`-escaped brace is).
fn request_placeholder(query: Option<&str>, body: &[u8]) -> Option<String> {
    let in_text = |text: String| first_placeholder(&Value::String(text));
    query
        .and_then(|query| {
            // Lossy, as axum's `Query` decodes: one byte that is not UTF-8
            // must not leave the rest of the query unread.
            let decoded =
                urlencoding::decode_binary(query.replace('+', " ").as_bytes()).into_owned();
            in_text(String::from_utf8_lossy(&decoded).into_owned())
        })
        .or_else(|| in_text(String::from_utf8_lossy(body).into_owned()))
        .or_else(|| {
            serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|value| first_placeholder(&value))
        })
}

// =============================================================================
// Test support
// =============================================================================

/// What other modules' tests need to observe this module's guarantees.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::Write;
    use std::sync::{Arc, Mutex, PoisonError};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Every tracing line logged on this thread, at TRACE, while it lives.
    /// Under `#[tokio::test]` (a current-thread runtime) that includes every
    /// task the test spawns.
    ///
    /// A guard, not a closure, because these captures span `.await`s, which
    /// `resource_guard::with_log_capture` cannot. So it applies that helper's
    /// protection itself: tracing's callsite `Interest` cache is
    /// process-global, and while at most one dispatcher is registered a
    /// parallel test with no subscriber that reaches a callsite first caches
    /// `never`, blinding this capture to that line. Two process-lifetime
    /// no-op dispatchers keep tracing-core off that single-dispatcher path,
    /// and the rebuild re-reads every callsite with this capture live.
    pub(crate) struct LogCapture {
        sink: Sink,
        _default: tracing::subscriber::DefaultGuard,
    }

    impl LogCapture {
        pub(crate) fn start() -> Self {
            static NO_OP_DISPATCHERS: std::sync::OnceLock<[tracing::Dispatch; 2]> =
                std::sync::OnceLock::new();
            NO_OP_DISPATCHERS.get_or_init(|| {
                std::array::from_fn(|_| {
                    tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default())
                })
            });
            let sink = Sink::default();
            let writer = sink.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            let default = tracing::subscriber::set_default(subscriber);
            tracing::callsite::rebuild_interest_cache();
            Self {
                sink,
                _default: default,
            }
        }

        pub(crate) fn text(&self) -> String {
            String::from_utf8_lossy(&self.sink.0.lock().unwrap_or_else(PoisonError::into_inner))
                .into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The app every test session targets, and the active connection's origin
    /// when that app is connected: the loopback interface spelled the other
    /// way, which is still the same app.
    const TARGET: &str = "http://localhost:3001";

    fn connected() -> NormOrigin {
        NormOrigin::parse("http://127.0.0.1:3001").expect("an origin")
    }

    fn secrets(pairs: &[(&str, &str)]) -> FrameSecrets {
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(reference, value)| (reference.to_string(), json!(value)))
            .collect();
        serde_json::from_value(Value::Object(map)).expect("a secrets map parses")
    }

    fn hold(run_id: &str, pairs: &[(&str, &str)]) -> Registration {
        register(run_id, TARGET, &secrets(pairs)).expect("a non-empty map is held")
    }

    /// The app copy of `body` with the target app connected.
    fn app_copy(body: &Value) -> Result<Option<Value>, SubstitutionError> {
        app_bound_type_body(body, Some(&connected()))
    }

    fn is_held(reference: &str) -> bool {
        held()
            .values()
            .any(|session| session.refs.contains_key(reference))
    }

    fn scrubbed_values() -> Vec<String> {
        ScrubSet::current()
            .map(|set| set.values.clone())
            .unwrap_or_default()
    }

    /// Wait (bounded) until no session holds `reference`.
    async fn released(reference: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !is_held(reference) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// The frame's `secrets` key parses leniently and its `Debug` shows
    /// counts, never a value.
    #[test]
    fn frame_secrets_parse_leniently_and_debug_prints_counts_only() {
        let parsed: FrameSecrets = serde_json::from_value(json!({
            "cs-frame-pw": "frame-secret-value-Qx1",
            "bad ref": "dropped-value-Qx2",
            "cs-frame-number": 7,
        }))
        .expect("a malformed entry never refuses the frame");
        assert_eq!(parsed.len(), 1);
        let rendered = format!("{parsed:?}");
        assert_eq!(rendered, "FrameSecrets { refs: 1, malformed: 2 }");
        assert!(!rendered.contains("frame-secret-value-Qx1"), "{rendered}");

        let none: FrameSecrets = serde_json::from_value(Value::Null).expect("null is no secrets");
        assert!(none.is_empty());
        assert_eq!(
            format!("{none:?}"),
            "FrameSecrets { refs: 0, malformed: 0 }"
        );
        for shape in [json!("a string"), json!(["a", "list"])] {
            let odd: FrameSecrets = serde_json::from_value(shape.clone()).expect("never refused");
            assert_eq!(
                format!("{odd:?}"),
                "FrameSecrets { refs: 0, malformed: 1 }",
                "{shape}"
            );
        }
        assert!(
            register("run-frame-empty", TARGET, &none).is_none(),
            "nothing to hold"
        );
    }

    /// The condition_check arm reads the frame's `secrets`, and the payload's
    /// redacting `Debug` never prints a value.
    #[test]
    fn condition_check_payload_reads_secrets_and_its_debug_never_prints_a_value() {
        let frame = json!({
            "source": "condition_check",
            "run_id": "018f0000-0000-7000-8000-00000000c513",
            "target_url": "http://localhost:3001",
            "initial_prompt": "Log in: type {{secret:cs-dbg-pw}} into the password field.",
            "secrets": {"cs-dbg-pw": "payload-debug-secret-Wk4"},
        });
        let payload: crate::agent_runtime::ConditionCheckPayload =
            serde_json::from_value(frame).expect("a frame with secrets parses");
        assert_eq!(
            payload.secrets.len(),
            1,
            "the arm reads the frame's secrets"
        );
        let rendered = format!("{payload:?}");
        assert!(
            !rendered.contains("payload-debug-secret-Wk4"),
            "Debug must never print a secret value, got {rendered}"
        );
        assert!(
            rendered.contains("secrets: FrameSecrets { refs: 1, malformed: 0 }"),
            "Debug still says how many refs the frame carried, got {rendered}"
        );

        let without = json!({
            "source": "condition_check",
            "run_id": "018f0000-0000-7000-8000-00000000c514",
            "target_url": "http://localhost:3001",
            "initial_prompt": "no placeholders",
        });
        let payload: crate::agent_runtime::ConditionCheckPayload =
            serde_json::from_value(without).expect("a coord that sends no secrets still parses");
        assert!(payload.secrets.is_empty());
    }

    /// A `type` action is substituted in the app's copy only: `params.text`
    /// and a top-level `text` alike, nothing else in the request, and never
    /// the request itself. A request carrying no placeholder goes as it is.
    #[test]
    fn a_type_action_is_substituted_in_the_app_copy_only() {
        let _held = hold(
            "run-sub",
            &[
                ("cs-sub-pw", "p@ss-sub-Hn5"),
                ("cs-sub-user", "alice-sub-Hn6"),
            ],
        );
        let body = json!({
            "action": "type",
            "params": {"text": "{{secret:cs-sub-pw}}", "clear": true},
            "text": "id {{secret:cs-sub-user}}!",
        });
        let copy = app_copy(&body)
            .expect("held refs")
            .expect("a copy for the app");
        assert_eq!(copy["params"]["text"], json!("p@ss-sub-Hn5"));
        assert_eq!(copy["text"], json!("id alice-sub-Hn6!"));
        assert_eq!(
            copy["params"]["clear"],
            json!(true),
            "the rest of the request is untouched"
        );
        assert_eq!(
            body["params"]["text"],
            json!("{{secret:cs-sub-pw}}"),
            "the request itself is never substituted"
        );

        let top_level_only = json!({"action": "type", "text": "{{secret:cs-sub-pw}}"});
        assert_eq!(
            app_copy(&top_level_only).expect("held").expect("copy")["text"],
            json!("p@ss-sub-Hn5"),
            "a placeholder in a top-level `text` is substituted"
        );

        for sent_as_is in [
            json!({"action": "type", "params": {"text": "plain text"}}),
            json!({"action": "click", "params": {"text": "plain text"}}),
            json!({"params": {}}),
        ] {
            assert_eq!(app_copy(&sent_as_is), Ok(None), "{sent_as_is}");
        }
    }

    /// A placeholder outside a `type` action's `params.text` and `text` is a
    /// typed refusal: another action, another field of a `type`, an action
    /// name ui-bridge would not read as `type`, even an object key. Sent on,
    /// the app would get the literal placeholder as, say, a password. The
    /// refusal names neither the held ref nor its value.
    #[test]
    fn a_placeholder_outside_a_type_actions_text_is_refused_never_sent() {
        let _held = hold("run-outside", &[("cs-out-pw", "outside-value-Ot1")]);
        for (body, action) in [
            (
                json!({"action": "click", "params": {"text": "{{secret:cs-out-pw}}"}}),
                "click",
            ),
            (
                json!({"action": "setValue", "params": {"value": "{{secret:cs-out-pw}}"}}),
                "setValue",
            ),
            (
                json!({"action": "sendKeys", "params": {"text": "{{secret:cs-out-pw}}"}}),
                "sendKeys",
            ),
            (
                json!({"action": "Type", "text": "{{secret:cs-out-pw}}"}),
                "Type",
            ),
            (
                json!({"params": {"text": "{{secret:cs-out-pw}}"}}),
                "<no action>",
            ),
            (
                json!({"action": "type", "params": {"value": "{{secret:cs-out-pw}}"}}),
                "type",
            ),
            (
                json!({"action": "type", "params": {"text": "{{secret:cs-out-pw}}",
                    "fields": [{"value": "{{secret:cs-out-pw}}"}]}}),
                "type",
            ),
            (
                json!({"action": "type", "params": {"{{secret:cs-out-pw}}": "a key"}}),
                "type",
            ),
            (
                json!({"action": "type", "params": {"text": ["{{secret:cs-out-pw}}"]}}),
                "type",
            ),
            (
                json!({"action": "type", "text": {"value": "{{secret:cs-out-pw}}"}}),
                "type",
            ),
        ] {
            let refused = app_copy(&body).expect_err("refused");
            assert_eq!(
                refused,
                SubstitutionError::OutsideTypeText {
                    action: action.to_string()
                },
                "{body}"
            );
            assert_eq!(refused.reference(), None);
            let answer = refused.refusal("the action was not dispatched");
            assert_eq!(answer["code"], json!(OUTSIDE_TYPE_TEXT_CODE));
            assert_eq!(answer["failure_origin"], json!("runner"));
            assert!(crate::journey::capture::sdk_refusal(&answer));
            let text = answer.to_string();
            assert!(
                !text.contains("cs-out-pw") && !text.contains("outside-value-Ot1"),
                "{text}"
            );
        }
    }

    /// With no session holding a secret there is nothing a placeholder could
    /// stand for: outside a `type` action's text it is ordinary text and goes
    /// as it is, while one in a `type` action's text is still refused, never
    /// typed literally. (`guarding` is handed in: tests share the held maps.)
    #[test]
    fn with_nothing_held_a_placeholder_outside_type_text_is_ordinary_text() {
        let click = json!({"action": "click", "params": {"text": "{{secret:cs-none-pw}}"}});
        assert_eq!(app_bound_type_body_while(&click, None, false), Ok(None));
        let other_field = json!({"action": "type", "params": {"value": "{{secret:cs-none-pw}}"}});
        assert_eq!(
            app_bound_type_body_while(&other_field, None, false),
            Ok(Some(other_field.clone())),
            "sent as it is"
        );
        let typed = json!({"action": "type", "params": {"text": "{{secret:cs-none-pw}}"}});
        assert_eq!(
            app_bound_type_body_while(&typed, None, false),
            Err(SubstitutionError::UnknownRef {
                reference: "cs-none-pw".to_string()
            })
        );
        assert_eq!(
            app_bound_type_body_while(&click, None, true),
            Err(SubstitutionError::OutsideTypeText {
                action: "click".to_string()
            }),
            "anti-vacuity: the same request is refused while a secret is held"
        );
    }

    /// An unknown, malformed or unterminated ref is a typed refusal, never the
    /// literal placeholder, and one bad placeholder refuses the whole text. So
    /// is the scrub's own mask, sent back.
    #[test]
    fn an_unknown_or_malformed_ref_is_refused_never_typed_literally() {
        let _held = hold("run-unknown", &[("cs-unk-held", "held-value-Unk7")]);
        for (text, named) in [
            ("{{secret:cs-unk-never-held}}", "cs-unk-never-held"),
            ("before {{secret:bad ref}} after", "bad ref"),
            ("{{secret:}}", ""),
            ("{{secret:cs-unk-unterminated", "cs-unk-unterminated"),
            (
                "{{secret:cs-unk-held}} then {{secret:cs-unk-never-held}}",
                "cs-unk-never-held",
            ),
            (REDACTED, "*"),
        ] {
            let body = json!({"action": "type", "params": {"text": text}});
            let refused = app_copy(&body).expect_err(text);
            assert_eq!(
                refused,
                SubstitutionError::UnknownRef {
                    reference: named.to_string()
                },
                "{text}"
            );
            let answer = refused.refusal("the action was not dispatched");
            assert_eq!(answer["success"], json!(false));
            assert_eq!(answer["code"], json!(UNKNOWN_REF_CODE));
            assert_eq!(answer["failure_origin"], json!("runner"));
            assert!(
                crate::journey::capture::sdk_refusal(&answer),
                "the journey ledger records nothing for a refusal"
            );
            assert!(!answer.to_string().contains("held-value-Unk7"), "{answer}");
        }
    }

    /// One ref held by two live sessions with different values could type one
    /// session's secret into another's app, so it is refused; the same value
    /// under the same ref is not ambiguous.
    #[test]
    fn a_ref_two_sessions_hold_with_different_values_is_refused() {
        let _a = hold("run-amb-a", &[("cs-amb-pw", "tenant-a-pass-Am1")]);
        let _b = hold("run-amb-b", &[("cs-amb-pw", "tenant-b-pass-Am2")]);
        let refused = app_copy(&json!({"action": "type", "text": "{{secret:cs-amb-pw}}"}))
            .expect_err("ambiguous");
        assert_eq!(refused.code(), AMBIGUOUS_REF_CODE);
        let answer = refused.refusal("the action was not dispatched").to_string();
        assert!(
            !answer.contains("tenant-a-pass-Am1")
                && !answer.contains("tenant-b-pass-Am2")
                && !answer.contains("cs-amb-pw"),
            "{answer}"
        );

        let _c = hold("run-amb-c", &[("cs-amb-same", "same-pass-Am3")]);
        let _d = hold("run-amb-d", &[("cs-amb-same", "same-pass-Am3")]);
        assert_eq!(
            app_copy(&json!({"action": "type", "text": "{{secret:cs-amb-same}}"}))
                .expect("not ambiguous")
                .expect("copy")["text"],
            json!("same-pass-Am3")
        );
    }

    /// A session's value is typed only into the app its frame targets: the
    /// active SDK connection must have the origin of the frame's `target_url`
    /// (loopback spelled either way). Another app, no connection, and a
    /// session whose `target_url` has no origin are each refused, naming
    /// neither the ref nor the value.
    #[test]
    fn a_secret_is_typed_only_into_the_app_its_session_targets() {
        let _a = register(
            "run-bind-a",
            TARGET,
            &secrets(&[("cs-bind-a", "bound-a-value-Bd1")]),
        )
        .expect("held");
        let _b = register(
            "run-bind-b",
            "https://app-b.example/login?next=/home",
            &secrets(&[("cs-bind-b", "bound-b-value-Bd2")]),
        )
        .expect("held");
        let _odd = register(
            "run-bind-odd",
            "not a url",
            &secrets(&[("cs-bind-odd", "bound-odd-value-Bd3")]),
        )
        .expect("held");
        let typed = |reference: &str, active: Option<&NormOrigin>| {
            app_bound_type_body(
                &json!({"action": "type", "text": format!("{{{{secret:{reference}}}}}")}),
                active,
            )
        };
        let app_b = NormOrigin::parse("https://app-b.example:443").expect("an origin");

        assert_eq!(
            typed("cs-bind-a", Some(&connected()))
                .expect("own app")
                .expect("copy")["text"],
            json!("bound-a-value-Bd1")
        );
        assert_eq!(
            typed("cs-bind-b", Some(&app_b))
                .expect("own app")
                .expect("copy")["text"],
            json!("bound-b-value-Bd2"),
            "a default port spelled out is the same origin"
        );
        for (reference, active, target, shown) in [
            (
                "cs-bind-a",
                Some(&app_b),
                Some("http://localhost:3001"),
                Some("https://app-b.example"),
            ),
            (
                "cs-bind-b",
                Some(&connected()),
                Some("https://app-b.example"),
                Some("http://127.0.0.1:3001"),
            ),
            ("cs-bind-a", None, Some("http://localhost:3001"), None),
            (
                "cs-bind-odd",
                Some(&connected()),
                None,
                Some("http://127.0.0.1:3001"),
            ),
        ] {
            let refused = typed(reference, active).expect_err(reference);
            assert_eq!(
                refused,
                SubstitutionError::WrongApp {
                    target: target.map(String::from),
                    active: shown.map(String::from),
                },
                "{reference}"
            );
            assert_eq!(refused.code(), WRONG_APP_CODE);
            let answer = refused.refusal("the action was not dispatched").to_string();
            assert!(
                !answer.contains(reference) && !answer.contains("-value-Bd"),
                "{answer}"
            );
        }
    }

    /// The IPC fallback never substitutes: a placeholder anywhere in a request
    /// headed there is refused with the typed unknown-ref error, a held ref
    /// included.
    #[test]
    fn the_ipc_fallback_refuses_every_placeholder() {
        let _held = hold("run-ipc", &[("cs-ipc-held", "ipc-held-value-Ip1")]);
        for (body, named) in [
            (
                json!({"action": "type", "params": {"text": "{{secret:cs-ipc-held}}"}}),
                "cs-ipc-held",
            ),
            (
                json!({"action": "type", "text": "{{secret:cs-ipc-top}}"}),
                "cs-ipc-top",
            ),
            (
                json!({"action": "fill", "params": {"fields": [{"value": "{{secret:cs-ipc-deep}}"}]}}),
                "cs-ipc-deep",
            ),
        ] {
            let refused = ipc_refusal(&body).unwrap_or_else(|| panic!("refused: {body}"));
            assert_eq!(
                refused,
                SubstitutionError::UnknownRef {
                    reference: named.to_string()
                }
            );
            assert_eq!(refused.code(), UNKNOWN_REF_CODE);
        }
        for clean in [
            json!({"action": "type", "params": {"text": "plain"}}),
            json!({"action": "click"}),
        ] {
            assert_eq!(ipc_refusal(&clean), None, "{clean}");
        }
    }

    /// Held values leave strings, object keys and exact-match numbers, raw
    /// and JSON-escaped bytes, each replaced by a mask that names no ref; a
    /// number that is not the value stays.
    #[test]
    fn held_values_are_scrubbed_from_json_text_and_bytes() {
        let _held = hold(
            "run-scrub",
            &[("cs-scrub-pw", "sc\"rub-Sc1"), ("cs-scrub-pin", "4829173")],
        );
        let set = ScrubSet::current().expect("held");
        let mut value = json!({
            "value": "sc\"rub-Sc1",
            "echo": "typed sc\"rub-Sc1!",
            "sc\"rub-Sc1": "as a key",
            "pin": 4829173,
            "list": ["4829173"],
            "other": 4829174,
        });
        assert!(set.scrub_json(&mut value));
        let rendered = value.to_string();
        assert!(
            !rendered.contains("rub-Sc1")
                && !rendered.contains("4829173")
                && !rendered.contains("cs-scrub"),
            "{rendered}"
        );
        assert_eq!(value["echo"], json!("typed {{secret:*}}!"));
        assert_eq!(value["pin"], json!(REDACTED));
        assert_eq!(value["other"], json!(4829174), "a different number stays");

        let raw = "x sc\"rub-Sc1 y \"sc\\\"rub-Sc1\" z";
        let scrubbed =
            String::from_utf8(set.scrub_bytes(raw.as_bytes()).expect("changed")).expect("utf-8");
        assert_eq!(scrubbed, "x {{secret:*}} y \"{{secret:*}}\" z");
        assert_eq!(set.scrub_str("nothing held here"), None);
        assert_eq!(set.scrub_bytes(b"nothing held here"), None);
    }

    /// ui-bridge shortens a field's value before it shows it (`truncate` in
    /// `summary-generator.ts`: the first `max - 3` code points, then `...`;
    /// `search-engine.ts` appends `…`). Every such echo of a held value, raw
    /// or JSON-escaped, is masked, the ellipsis kept. A prefix shorter than
    /// `MIN_ECHO_CHARS`, or one not followed by an ellipsis, is not an echo.
    #[test]
    fn truncated_echoes_of_a_held_value_are_scrubbed() {
        const SECRET: &str = "sk_live_51HtruncEchoQ\"x9-Tr1-padding-to-forty";
        let _held = hold("run-trunc", &[("cs-trunc-key", SECRET)]);
        let set = ScrubSet::current().expect("held");
        // ui-bridge's `truncate(str, max)`, over code points.
        let truncate =
            |max: usize| format!("{}...", SECRET.chars().take(max - 3).collect::<String>());
        let element = format!("input \"API key\" value: \"{}\"", truncate(20));
        let form = format!("    - API key = \"{}\"", truncate(15));
        let found = format!("{}…", SECRET.chars().take(30).collect::<String>());
        for shown in [&element, &form, &found] {
            let scrubbed = set.scrub_str(shown).expect("an echo");
            assert!(!scrubbed.contains("sk_live_51"), "{scrubbed}");
            assert!(
                scrubbed.contains("{{secret:*}}...") || scrubbed.contains("{{secret:*}}…"),
                "the ellipsis is kept: {scrubbed}"
            );
        }
        let escaped = format!(
            "{{\"summary\":\"{}...\"}}",
            json_escaped(&SECRET.chars().take(25).collect::<String>())
        );
        let scrubbed = String::from_utf8(set.scrub_bytes(escaped.as_bytes()).expect("an echo"))
            .expect("utf-8");
        assert!(!scrubbed.contains("truncEcho"), "{scrubbed}");

        // A cut right after a dot: `abcdefg.` then `...` reads `abcdefg....`.
        const DOTTED: &str = "abcdefg.dotted-echo-secret-Tr2";
        let _dotted = hold("run-trunc-dot", &[("cs-trunc-dot", DOTTED)]);
        let set = ScrubSet::current().expect("held");
        let cut = format!("{}...", DOTTED.chars().take(8).collect::<String>());
        assert_eq!(
            set.scrub_str(&cut).as_deref(),
            Some("{{secret:*}}..."),
            "{cut}"
        );

        let short = format!(
            "{}...",
            SECRET.chars().take(MIN_ECHO_CHARS - 1).collect::<String>()
        );
        assert_eq!(set.scrub_str(&short), None, "shorter than an echo");
        let no_ellipsis = SECRET.chars().take(20).collect::<String>();
        assert_eq!(
            set.scrub_str(&no_ellipsis),
            None,
            "a prefix alone is not an echo"
        );
    }

    /// Two object keys that mask to the same spelling stay two fields.
    #[test]
    fn two_keys_masked_alike_keep_both_fields() {
        let _held = hold(
            "run-keys",
            &[
                ("cs-key-a", "key-one-secret-Ky1"),
                ("cs-key-b", "key-two-secret-Ky2"),
            ],
        );
        let set = ScrubSet::current().expect("held");
        let mut value = json!({"key-one-secret-Ky1": 1, "key-two-secret-Ky2": 2});
        assert!(set.scrub_json(&mut value));
        let fields = value.as_object().expect("an object");
        assert_eq!(fields.len(), 2, "{value}");
        assert!(
            fields.contains_key(REDACTED) && fields.contains_key("{{secret:*}} (2)"),
            "{value}"
        );
        let mut kept: Vec<i64> = fields.values().filter_map(Value::as_i64).collect();
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 2]);
    }

    /// Two held values that overlap in a text are masked as their union. A
    /// left-to-right match would mask the first and let most of the second
    /// through.
    #[test]
    fn overlapping_held_values_are_masked_as_their_union() {
        let _held = hold(
            "run-overlap",
            &[
                ("cs-ovl-a", "ovl-head-Ov1-shared"),
                ("cs-ovl-b", "shared-Ov2-tail-end"),
            ],
        );
        let set = ScrubSet::current().expect("held");
        assert_eq!(
            set.scrub_str("[ovl-head-Ov1-shared-Ov2-tail-end]")
                .as_deref(),
            Some("[{{secret:*}}]")
        );
        assert_eq!(
            set.scrub_str("a ovl-head-Ov1-shared b shared-Ov2-tail-end c")
                .as_deref(),
            Some("a {{secret:*}} b {{secret:*}} c"),
            "disjoint occurrences stay separate"
        );
    }

    /// GET `path` from `app`, the body as text.
    async fn fetch(app: &axum::Router, path: &'static str) -> String {
        use tower::ServiceExt;
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::get(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The response layer on a buffered body: JSON and text come back with
    /// the mask where the value was, naming no ref. A JSON body whose parse
    /// hides the value (a duplicate key keeps only its last value) is
    /// scrubbed as bytes. A body carrying no held value passes byte for byte.
    #[tokio::test]
    async fn every_sdk_response_body_is_scrubbed_while_a_secret_is_held() {
        use axum::routing::get;

        const SECRET: &str = "mw-fixture-pa\"ss-Mw1";
        let _held = hold("run-middleware", &[("cs-mw-pw", SECRET)]);
        let app = axum::Router::new()
            .route(
                "/json",
                get(|| async {
                    Json(json!({"value": SECRET, "nested": [{"echo": format!("typed {SECRET}")}]}))
                }),
            )
            .route("/text", get(|| async { format!("plain {SECRET} text") }))
            .route(
                "/duplicate-key",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        format!("{{\"value\":\"{}\",\"value\":\"\"}}", json_escaped(SECRET)),
                    )
                }),
            )
            .route(
                "/clean",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        "{\n  \"pretty\": true\n}",
                    )
                }),
            )
            .layer(axum::middleware::from_fn(scrub_sdk_responses));
        for path in ["/json", "/text", "/duplicate-key"] {
            let body = fetch(&app, path).await;
            assert!(
                !body.contains(SECRET) && !body.contains(&json_escaped(SECRET)),
                "{path} must not carry the value: {body}"
            );
            assert!(
                body.contains(REDACTED) && !body.contains("cs-mw"),
                "{path}: the mask stands where the value was, naming no ref: {body}"
            );
        }
        assert_eq!(
            fetch(&app, "/clean").await,
            "{\n  \"pretty\": true\n}",
            "a body carrying no held value is never re-serialized"
        );
    }

    /// The request layer on the routes that never substitute: while a secret
    /// is held, a placeholder in a `/ui-bridge/control/*` or `/ui-bridge/ai/*`
    /// request's body or query is refused with the typed unknown-ref code
    /// before the handler runs, and a clean request reaches it byte for byte.
    /// A route outside those prefixes is never inspected.
    #[tokio::test]
    async fn a_route_that_never_substitutes_refuses_a_placeholder_while_a_secret_is_held() {
        use axum::routing::post;
        use std::sync::atomic::AtomicUsize;
        use tower::ServiceExt;

        let _held = hold("run-control-refusal", &[("cs-ctl-pw", "control-pw-Ct1")]);
        static REACHED: AtomicUsize = AtomicUsize::new(0);
        async fn echo(body: Bytes) -> Bytes {
            REACHED.fetch_add(1, Ordering::SeqCst);
            body
        }
        let app = axum::Router::new()
            .route("/ui-bridge/control/page/type-into", post(echo))
            .route("/ui-bridge/ai/find", post(echo))
            .route("/ui-bridge/relay/dispatch", post(echo))
            .route("/ui-bridge/batch", post(echo))
            .route("/ui-bridge/commands", post(echo))
            .layer(axum::middleware::from_fn(refuse_unsubstituted_placeholders));
        let send = |uri: &'static str, body: &'static str| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        axum::http::Request::post(uri)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(body))
                            .expect("request"),
                    )
                    .await
                    .expect("response");
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body");
                (status, String::from_utf8_lossy(&bytes).into_owned())
            }
        };

        for (uri, body) in [
            (
                "/ui-bridge/control/page/type-into",
                r#"{"label":"Password","text":"{{secret:cs-ctl-pw}}"}"#,
            ),
            (
                "/ui-bridge/ai/find",
                r#"{"query":"x \u007b{secret:cs-ctl-pw}}"}"#,
            ),
            (
                "/ui-bridge/control/page/type-into?text=%7B%7Bsecret%3Acs-ctl-pw%7D%7D",
                "{}",
            ),
            (
                "/ui-bridge/relay/dispatch",
                r#"{"action":"type","payload":{"text":"{{secret:cs-ctl-pw}}"}}"#,
            ),
            (
                "/ui-bridge/batch",
                r#"{"operations":[{"op":"fill_form","text":"{{secret:cs-ctl-pw}}"}]}"#,
            ),
        ] {
            let before = REACHED.load(Ordering::SeqCst);
            let (status, answer) = send(uri, body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {answer}");
            let answer: Value = serde_json::from_str(&answer).expect("a JSON refusal");
            assert_eq!(answer["code"], UNKNOWN_REF_CODE, "{uri}: {answer}");
            assert_eq!(
                answer["failure_origin"],
                crate::journey::capture::RUNNER_REJECTION_ORIGIN
            );
            assert!(
                !answer.to_string().contains("control-pw-Ct1"),
                "a refusal never carries a value: {answer}"
            );
            assert_eq!(
                REACHED.load(Ordering::SeqCst),
                before,
                "{uri}: the handler must not run"
            );
        }

        let clean = r#"{"label":"Email","text":"user@example.com"}"#;
        assert_eq!(
            send("/ui-bridge/control/page/type-into", clean).await,
            (StatusCode::OK, clean.to_string()),
            "a request carrying no placeholder reaches its handler unchanged"
        );
        let relayed = r#"{"commandId":"c1","result":{"text":"{{secret:cs-ctl-pw}}"}}"#;
        assert_eq!(
            send("/ui-bridge/commands", relayed).await,
            (StatusCode::OK, relayed.to_string()),
            "a route outside the non-substituting prefixes is never inspected"
        );
    }

    /// The query and both readings of the body are searched.
    #[test]
    fn a_request_placeholder_is_found_in_its_query_its_text_and_its_json() {
        assert_eq!(
            request_placeholder(Some("text={{secret:cs-q}}"), b""),
            Some("cs-q".to_string())
        );
        assert_eq!(
            request_placeholder(Some("text=%7B%7Bsecret:cs-pct%7D%7D"), b""),
            Some("cs-pct".to_string())
        );
        assert_eq!(
            request_placeholder(Some("x=%FF&text=%7B%7Bsecret%3Acs-bad%7D%7D"), b""),
            Some("cs-bad".to_string()),
            "a byte that is not UTF-8 must not hide the rest of the query"
        );
        assert_eq!(
            request_placeholder(None, b"plain {{secret:cs-raw}} text"),
            Some("cs-raw".to_string())
        );
        assert_eq!(
            request_placeholder(None, br#"{"t":"\u007b{secret:cs-esc}}"}"#),
            Some("cs-esc".to_string())
        );
        assert_eq!(
            request_placeholder(Some("text=hello"), br#"{"t":"{{not-a-secret}}"}"#),
            None
        );
    }

    /// The composer wraps every `/ui-bridge/*` route in the request refusal,
    /// so its layer must follow every route it registers.
    #[test]
    fn the_ui_bridge_composer_refuses_placeholders_after_every_route() {
        let src = include_str!("mcp/ui_bridge/mod.rs");
        let routes = src
            .split("\npub fn routes() -> ")
            .nth(1)
            .expect("mod.rs routes()")
            .split("\n}\n")
            .next()
            .unwrap_or_default();
        let last_route = routes
            .rfind(".route(")
            .max(routes.rfind(".merge("))
            .expect("a route");
        let layer = routes
            .rfind("crate::condition_secrets::refuse_unsubstituted_placeholders")
            .expect("routes() carries the request refusal layer");
        assert!(
            layer > last_route,
            "the refusal layer must follow every route"
        );
    }

    /// The response layer on an event stream: each event is scrubbed as it
    /// flows, JSON data field by field and other data as text. An event's
    /// `data:` lines are one value (`Event::data` splits a value at its
    /// newlines), so a held value spanning two of them is still caught.
    #[tokio::test]
    async fn every_event_stream_is_scrubbed_event_by_event_while_a_secret_is_held() {
        use axum::response::sse::{Event, Sse};
        use axum::routing::get;

        const SECRET: &str = "sse-fixture-pa\"ss-Ss1";
        const SPLIT: &str = "sse-split-first\nsse-split-second-Ss2";
        let _held = hold(
            "run-event-stream",
            &[("cs-sse-pw", SECRET), ("cs-sse-split", SPLIT)],
        );
        let app = axum::Router::new()
            .route(
                "/sse",
                get(|| async {
                    Sse::new(futures_util::stream::iter([
                        Ok::<_, std::convert::Infallible>(
                            Event::default().data(json!({"value": SECRET}).to_string()),
                        ),
                        Ok(Event::default().data(format!("raw {SECRET}"))),
                        Ok(Event::default().data(format!("split {SPLIT}"))),
                    ]))
                }),
            )
            .layer(axum::middleware::from_fn(scrub_sdk_responses));
        let events = fetch(&app, "/sse").await;
        assert!(
            !events.contains(SECRET) && !events.contains(&json_escaped(SECRET)),
            "the stream must not carry the value: {events}"
        );
        assert!(
            !events.contains("sse-split-second-Ss2") && !events.contains("sse-split-first"),
            "a value split across data lines must not survive: {events}"
        );
        assert!(
            events.contains("data: {\"value\":\"{{secret:*}}\"}")
                && events.contains("data: raw {{secret:*}}")
                && events.contains("data: split {{secret:*}}")
                && !events.contains("cs-sse"),
            "each event carries the mask, naming no ref: {events}"
        );
    }

    /// A held map lives exactly as long as its session: released when its
    /// registration drops, and when its PTY exits (read through a real
    /// terminal manager, as production reads it).
    #[tokio::test]
    async fn a_held_map_is_released_on_drop_and_at_pty_exit() {
        let dropped = hold("run-life-drop", &[("cs-life-drop", "life-drop-Lf1")]);
        assert_eq!(
            resolve("cs-life-drop", Some(&connected())),
            Ok("life-drop-Lf1".to_string())
        );
        drop(dropped);
        assert!(!is_held("cs-life-drop"), "a dropped registration releases");

        let manager = Arc::new(TerminalManager::new());
        let pane = Arc::new(crate::terminal::session::tests::make_test_session(
            Arc::new(Mutex::new(Vec::new())),
        ));
        pane.set_alive_for_test(true);
        manager.insert_for_test("cs-life-pane", Arc::clone(&pane));
        let watched = register(
            "run-life-pty",
            TARGET,
            &secrets(&[("cs-life-pty", "life-pty-Lf2")]),
        );
        release_at_pty_exit_polling(watched, &manager, "cs-life-pane", Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            resolve("cs-life-pty", Some(&connected())),
            Ok("life-pty-Lf2".to_string()),
            "held while the PTY lives"
        );
        pane.set_alive_for_test(false);
        assert!(released("cs-life-pty").await, "released once the PTY exits");
    }

    /// Past its substitution TTL a map substitutes nothing, but its value is
    /// still scrubbed while it is held: a session that outlives the window
    /// can still read a page showing what it typed.
    #[tokio::test]
    async fn a_map_past_its_substitution_ttl_substitutes_nothing_but_is_still_scrubbed() {
        // A window long enough that a loaded box still substitutes inside it.
        let lapsed = register_for(
            "run-life-sub",
            TARGET,
            &secrets(&[("cs-life-sub", "life-sub-Lf4")]),
            Duration::from_secs(2),
            Duration::from_secs(60),
        )
        .expect("held");
        assert_eq!(
            resolve("cs-life-sub", Some(&connected())),
            Ok("life-sub-Lf4".to_string()),
            "anti-vacuity: substituted inside its window"
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(
            resolve("cs-life-sub", Some(&connected())),
            Err(SubstitutionError::UnknownRef {
                reference: "cs-life-sub".to_string()
            }),
            "past its substitution TTL a map substitutes nothing"
        );
        assert!(
            scrubbed_values()
                .iter()
                .any(|value| value == "life-sub-Lf4"),
            "and its value is still scrubbed while it is held"
        );
        drop(lapsed);
    }

    /// A map is released at its hold TTL whatever its PTY does: the backstop
    /// for an exit nobody observed.
    #[tokio::test]
    async fn a_map_is_released_at_its_hold_ttl() {
        let short = register_for(
            "run-life-ttl",
            TARGET,
            &secrets(&[("cs-life-ttl", "life-ttl-Lf3")]),
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .expect("held");
        assert!(released("cs-life-ttl").await, "released at its hold TTL");
        assert!(
            !scrubbed_values()
                .iter()
                .any(|value| value == "life-ttl-Lf3"),
            "an expired value is no longer held anywhere"
        );
        drop(short);
    }

    /// The journey rows a `type` action produces (its edge, closed by the next
    /// snapshot, and the frontier of that snapshot) never carry what was
    /// typed, even when the app reports the field's new value.
    ///
    /// A guard, not a test of the scrub: `record_journey_snapshot` reads the
    /// app's snapshot before any response layer runs, and this holds because
    /// the journey extraction keeps only structural fields (ids, types,
    /// route). It holds without this module, and fails the day that
    /// extraction starts carrying a field's value or text.
    #[test]
    fn journey_rows_from_a_type_action_never_carry_the_value() {
        use crate::journey::capture::{edge_insert, finalize, snapshot_view};
        use crate::journey::cursor::{
            failure_hint, ActionSpec, CursorKey, Cursors, Observed, Provenance,
        };
        use crate::journey::node::{
            affordance_digest, build_node, extract_affordances, page_identity, SpecLookup,
        };

        const SECRET: &str = "journey-fixture-secret-Jr1";
        let field = |value: &str| {
            json!({"elements": [{"id": "password-field", "type": "input", "label": "Password",
                "value": value, "text": value, "actions": ["type"]}]})
        };
        let key = CursorKey::new("tenant-app", None);
        let mut cursors = Cursors::default();
        let observe = |cursors: &mut Cursors, snapshot: &Value| {
            let view = snapshot_view(snapshot);
            let affordances = extract_affordances(view);
            let node = build_node(&page_identity(view), &SpecLookup::NoSpec);
            let observed = Observed {
                node: node.clone(),
                digest: affordance_digest(&affordances),
            };
            let closed = cursors.observe(&key, observed, affordances.clone(), Instant::now());
            (closed, node, affordances)
        };
        let _ = observe(&mut cursors, &field(""));
        // Exactly what `handle_element_action` records for a `type`.
        let action = ActionSpec::element(
            "password-field",
            TYPE_ACTION,
            qontinui_types::journey::ChokePoint::SdkElementAction,
        );
        let displaced = cursors.open(
            &key,
            &action,
            Provenance::default(),
            failure_hint(false),
            Instant::now(),
        );
        assert!(displaced.is_none(), "no earlier action was pending");
        let (closed, node, affordances) = observe(&mut cursors, &field(SECRET));
        let edge = edge_insert(
            &finalize(closed.expect("the snapshot closes the edge")),
            None,
        )
        .expect("a contract-valid edge row");
        let frontier =
            crate::journey::frontier::frontier_batch("tenant-app", &node, &affordances, None)
                .expect("a valid frontier batch")
                .expect("the field is an affordance");
        assert_eq!(
            edge.trigger["actionType"],
            json!("type"),
            "anti-vacuity: {edge:?}"
        );
        assert!(edge.target_fingerprint.is_some(), "anti-vacuity: {edge:?}");
        let rows = format!("{edge:?}\n{frontier:?}");
        assert!(
            !rows.contains(SECRET),
            "a journey row carries the typed value: {rows}"
        );
    }

    /// Every router that returns app content carries the scrub layer AFTER
    /// its last route (`Router::layer` wraps only the routes registered before
    /// it): the two `/ui-bridge/sdk/*` routers, `/ui-bridge/apps/*`, and the
    /// element and effect routes whose WebSocket-app branches answer with the
    /// app's content.
    #[test]
    fn every_sdk_router_carries_the_scrub_layer_after_its_last_route() {
        for (file, src) in [
            ("mcp/sdk_client.rs", include_str!("mcp/sdk_client.rs")),
            (
                "mcp/ui_bridge/sdk_spec_sync.rs",
                include_str!("mcp/ui_bridge/sdk_spec_sync.rs"),
            ),
            ("mcp/app_discovery.rs", include_str!("mcp/app_discovery.rs")),
            (
                "mcp/ui_bridge/effects.rs",
                include_str!("mcp/ui_bridge/effects.rs"),
            ),
            (
                "mcp/ui_bridge/elements.rs",
                include_str!("mcp/ui_bridge/elements.rs"),
            ),
        ] {
            let routes = src
                .split("\npub fn routes() -> ")
                .nth(1)
                .unwrap_or_else(|| panic!("{file}: no routes()"))
                .split("\n}\n")
                .next()
                .unwrap_or_default();
            let last_route = routes
                .rfind(".route(")
                .unwrap_or_else(|| panic!("{file}: no route"));
            let layer = routes
                .rfind("crate::condition_secrets::scrub_sdk_responses")
                .unwrap_or_else(|| panic!("{file}: routes() carries no scrub layer"));
            assert!(
                layer > last_route,
                "{file}: the scrub layer must follow every route"
            );
        }
    }

    /// The relay scrubs the answers of its one route that returns a tab's
    /// content, and never its command stream, which carries a `type` action's
    /// substituted value TO the tabs; nor does the composer layer the whole
    /// `/ui-bridge/*` router, which merges that stream.
    #[test]
    fn the_relay_scrubs_its_dispatch_answers_and_never_its_command_stream() {
        let src = include_str!("mcp/ui_bridge/relay.rs");
        let routes = src
            .split("\npub fn routes() -> ")
            .nth(1)
            .expect("relay routes()")
            .split("\n}\n")
            .next()
            .unwrap_or_default();
        assert_eq!(
            routes
                .matches("crate::condition_secrets::scrub_sdk_responses")
                .count(),
            1,
            "one route carries the layer, not the router"
        );
        assert!(
            routes.contains(
                "post(ui_bridge_relay_dispatch_handler).layer(axum::middleware::from_fn(\n                crate::condition_secrets::scrub_sdk_responses,"
            ),
            "the dispatch route's answers are scrubbed"
        );
        assert!(
            routes.contains("get(ui_bridge_relay_command_stream_handler),\n"),
            "the command stream carries no layer"
        );
        assert!(
            !include_str!("mcp/ui_bridge/mod.rs").contains("scrub_sdk_responses"),
            "the /ui-bridge/* composer must not layer the relay's command stream"
        );
    }

    /// `handle_element_action` reads the active connection's origin, then
    /// substitutes only the app-bound copies for it, answers a refused
    /// substitution without dispatching, refuses a placeholder on its IPC
    /// fallback before anything is typed, and scrubs its result before the
    /// journey ledger and the event row read it. A handler needs an
    /// `ApiState`, which no test builds, so the composition is pinned from
    /// source (the technique `sdk_client`'s own handler pins use).
    #[test]
    fn the_element_action_handler_substitutes_app_copies_only_and_scrubs_before_its_ledgers() {
        let src = include_str!("mcp/sdk_client.rs");
        let handler = src
            .split("\nasync fn handle_element_action(")
            .nth(1)
            .expect("handle_element_action")
            .split("\nasync fn ")
            .next()
            .unwrap_or_default();
        let at = |needle: &str| {
            handler
                .find(needle)
                .unwrap_or_else(|| panic!("handle_element_action lost `{needle}`"))
        };
        let dispatch = at("dispatch_app_request_typed(");
        let payloads =
            at("element_action_app_payloads(&id, &body, tab_id, active_origin.as_ref())");
        assert!(at("let active_origin = active_app_origin(&state).await;") < payloads);
        assert!(payloads < dispatch);
        // rustfmt wraps the refused arm in a block, so the arm and the refusal
        // it answers with are pinned apart, in order.
        let refused_arm = at("Err(refused) =>");
        let answer = at("refused_action(&refused, \"the action was not dispatched\")");
        assert!(payloads < refused_arm && refused_arm < answer && answer < dispatch);
        let refusal = at("crate::condition_secrets::ipc_refusal(&body)");
        assert!(at("ipc_fallback_refusal(&e") < refusal);
        assert!(refusal < at("ui_bridge_request_sync("));
        assert!(
            !handler.contains("app_body.get("),
            "the IPC fallback reads the unsubstituted request"
        );
        let scrub = at("crate::condition_secrets::scrub_action_result(result)");
        assert!(scrub > at("ui_bridge_request_sync("));
        assert!(scrub < at("crate::journey::capture::record_action("));
        assert!(scrub < at("insert_ui_bridge_event("));
    }
}
