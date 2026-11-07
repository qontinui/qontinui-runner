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
//! 1. **Held per session.** [`register`] holds a frame's map from just before
//!    the condition-check PTY is created. The [`Registration`] it returns
//!    releases the map when it drops: on any failed spawn, or, once handed to
//!    [`release_at_pty_exit`], when the PTY exits. [`HOLD_TTL`] is the
//!    backstop for an exit nobody observed.
//! 2. **Substituted in the `type` action only, on the way to the app only.**
//!    [`app_bound_type_body`] substitutes `params.text` and a top-level `text`
//!    (ui-bridge's `type` handler reads `request.params?.text || request.text`)
//!    in a COPY of the request: the copy `mcp::sdk_client::handle_element_action`
//!    sends to the app as its WebSocket payload or HTTP body. The request
//!    itself is never substituted, because the IPC fallback, the journey
//!    ledger and the `ui_bridge_events` row all read it.
//! 3. **Never typed into the runner's own UI.** The IPC fallback runs when no
//!    SDK app is connected, so its target is the runner's webview. It never
//!    substitutes, and [`ipc_refusal`] refuses any placeholder that would
//!    reach it.
//! 4. **Never shown back.** While any map is held, [`scrub_sdk_responses`]
//!    replaces each held value with its placeholder in every `/ui-bridge/sdk/*`
//!    response, [`scrub_action_result`] does so before an action's ledger rows
//!    are written, and [`debug_scrubbed`] before the relay logs an app's change
//!    event.
//!
//! An unknown, malformed or ambiguous ref is refused with a typed error
//! ([`SubstitutionError`]); a placeholder is never typed as literal text.
//! Nothing here logs a value: log lines carry a run id and counts.
//!
//! # Accepted residuals
//!
//! - **A very short secret over-scrubs.** Scrubbing is substring replacement,
//!   so a one- or two-character value is replaced wherever those characters
//!   occur in an SDK response while its session lives. That can garble
//!   unrelated text, a JSON number equal to the value, a base64 image whose
//!   encoding happens to contain it, and even the text of another placeholder.
//!   It fails safe: the value is never shown; the session reads less of the
//!   page.
//! - **Pixels are not scrubbed.** A screenshot of a field that renders its
//!   value in clear text shows the value; only text a response carries is
//!   scrubbed. (A password input renders its value masked.)
//! - **Only the raw and JSON-escaped forms are matched.** A value an app
//!   echoes URL-encoded, HTML-escaped or base64-encoded is not recognised.
//! - **Loopback replay.** `POST /ui-bridge/sdk/element/{id}/action` is a
//!   `TRUSTED_ROUTES` door (`mcp::origin_guard`), so a local process, or a
//!   page on a Trusted origin, that knows a ref can have its value typed into
//!   the connected app while the session lives. Refs are per run and appear
//!   only in that run's prompt.

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

/// How long a held map outlives an exit nobody observed.
///
/// The PTY-exit watcher normally releases a map within [`PTY_EXIT_POLL`] of
/// its session ending; this bounds the case where it never does (a watcher
/// dropped with the runtime, a pane the manager never let go of). Two hours is
/// four times the 1800-second report deadline the plan's D-D4 gives coord's
/// condition-run reaper: a session still typing that late reports into a run
/// coord has already closed.
pub const HOLD_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// How often the exit watcher reads the PTY's liveness flag (an atomic load).
const PTY_EXIT_POLL: Duration = Duration::from_secs(1);

/// The largest non-streaming SDK response the scrub buffers. A larger one is
/// withheld (500) rather than sent unscrubbed; the runner API's request-body
/// limit is the same 100 MiB.
const SCRUB_BUFFER_LIMIT: usize = 100 * 1024 * 1024;

/// A placeholder named a ref no live condition session holds.
pub const UNKNOWN_REF_CODE: &str = "CONDITION_SECRET_UNKNOWN_REF";

/// A placeholder named a ref two live sessions hold with different values.
pub const AMBIGUOUS_REF_CODE: &str = "CONDITION_SECRET_AMBIGUOUS_REF";

/// A response that could not be scrubbed while a secret was held.
const SCRUB_FAILED_CODE: &str = "CONDITION_SECRET_SCRUB_FAILED";

/// `{{secret:<reference>}}`.
pub fn placeholder(reference: &str) -> String {
    format!("{PLACEHOLDER_OPEN}{reference}{PLACEHOLDER_CLOSE}")
}

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
    refs: BTreeMap<String, String>,
    expires_at: Instant,
}

/// Every held map, keyed by its registration id, never by run id, so two
/// deliveries of one run's frame hold and release independently.
///
/// Reading through this accessor is what enforces the TTL: an expired map is
/// dropped on the next read, whoever reads.
fn held() -> MutexGuard<'static, HashMap<u64, Held>> {
    static HELD: OnceLock<Mutex<HashMap<u64, Held>>> = OnceLock::new();
    let mut sessions = HELD
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    sessions.retain(|_, session| {
        let live = session.expires_at > now;
        if !live {
            warn!(
                "condition_secrets: run_id={}: held secrets reached their TTL with no observed \
                 PTY exit; released",
                session.run_id
            );
        }
        live
    });
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
    expires_at: Instant,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if held().remove(&self.id).is_some() {
            debug!(
                "condition_secrets: run_id={}: released its held secrets",
                self.run_id
            );
        }
    }
}

/// Hold `secrets` for the condition session of `run_id`, for at most
/// [`HOLD_TTL`]. `None` when the frame carried no usable ref, so there is
/// nothing to hold.
#[must_use = "dropping the Registration releases the session's secrets at once"]
pub fn register(run_id: &str, secrets: &FrameSecrets) -> Option<Registration> {
    register_for(run_id, secrets, HOLD_TTL)
}

fn register_for(run_id: &str, secrets: &FrameSecrets, ttl: Duration) -> Option<Registration> {
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
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let expires_at = Instant::now() + ttl;
    held().insert(
        id,
        Held {
            run_id: run_id.to_string(),
            refs: secrets.refs.clone(),
            expires_at,
        },
    );
    info!(
        "condition_secrets: run_id={run_id}: holding {} secret ref(s) for this session",
        secrets.refs.len()
    );
    Some(Registration {
        id,
        run_id: run_id.to_string(),
        expires_at,
    })
}

/// Hand a started session's [`Registration`] to a watcher that releases it
/// when the session's PTY exits (the moment `terminal_id` is no longer a live
/// pane of `terminal_manager`), or at its TTL. `None`, a frame that carried
/// no secrets, is a no-op.
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
        while Instant::now() < registration.expires_at
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
}

impl SubstitutionError {
    /// The typed refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownRef { .. } => UNKNOWN_REF_CODE,
            Self::AmbiguousRef { .. } => AMBIGUOUS_REF_CODE,
        }
    }

    /// The text the refused placeholder named.
    pub fn reference(&self) -> &str {
        match self {
            Self::UnknownRef { reference } | Self::AmbiguousRef { reference } => reference,
        }
    }

    /// The body an SDK route answers a refused substitution with:
    /// `success: false`, the typed `code`, and `failure_origin: "runner"`.
    /// The runner refused before any UI was touched, so the journey ledger
    /// records nothing for it (`journey::capture::sdk_refusal`). It names the
    /// ref, never a value.
    pub fn refusal(&self, detail: &str) -> Value {
        let what = match self {
            Self::UnknownRef { .. } => "no live condition-check session holds secret ref",
            Self::AmbiguousRef { .. } => {
                "two live condition-check sessions hold different values for secret ref"
            }
        };
        serde_json::json!({
            "success": false,
            "error": format!(
                "{}: {what} `{}`; {detail}. Nothing was typed.",
                self.code(),
                self.reference()
            ),
            "code": self.code(),
            "failure_origin": crate::journey::capture::RUNNER_REJECTION_ORIGIN,
            "detail": detail,
            "retryable": false,
        })
    }
}

/// The value every live session holds under `reference`, when they agree.
fn resolve(reference: &str) -> Result<String, SubstitutionError> {
    let sessions = held();
    let mut values = sessions
        .values()
        .filter_map(|session| session.refs.get(reference));
    let Some(first) = values.next() else {
        return Err(SubstitutionError::UnknownRef {
            reference: reference.to_string(),
        });
    };
    if values.any(|other| other != first) {
        return Err(SubstitutionError::AmbiguousRef {
            reference: reference.to_string(),
        });
    }
    Ok(first.clone())
}

/// The ref a placeholder names, and the text after its closing `}}`.
/// `after_open` is the text right after a [`PLACEHOLDER_OPEN`].
fn parse_ref(after_open: &str) -> Result<(&str, &str), SubstitutionError> {
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
            Err(SubstitutionError::UnknownRef { reference: shown })
        }
    }
}

/// `text` with every placeholder replaced by its held value. One placeholder
/// that cannot be substituted refuses the whole text, so a secret is never
/// typed with part of its field missing.
fn substitute(text: &str) -> Result<String, SubstitutionError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after_open)) = rest.split_once(PLACEHOLDER_OPEN) {
        out.push_str(before);
        let (reference, after_close) = parse_ref(after_open)?;
        out.push_str(&resolve(reference)?);
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

fn substitute_field(field: Option<&mut Value>) -> Result<(), SubstitutionError> {
    if let Some(Value::String(text)) = field {
        *text = substitute(text)?;
    }
    Ok(())
}

/// The copy of an element-action request the APP receives.
///
/// For a `type` action whose `params.text` or top-level `text` carries a
/// placeholder: a copy with each placeholder in those two fields replaced by
/// its held value. For anything else: `None`, meaning the request goes to the
/// app as it is. `body` itself is never changed.
pub fn app_bound_type_body(body: &Value) -> Result<Option<Value>, SubstitutionError> {
    if body.get("action").and_then(Value::as_str) != Some(TYPE_ACTION) {
        return Ok(None);
    }
    let in_params = carries_placeholder(body.get("params").and_then(|p| p.get("text")));
    let at_top = carries_placeholder(body.get("text"));
    if !in_params && !at_top {
        return Ok(None);
    }
    let mut app_copy = body.clone();
    if in_params {
        substitute_field(app_copy.get_mut("params").and_then(|p| p.get_mut("text")))?;
    }
    if at_top {
        substitute_field(app_copy.get_mut("text"))?;
    }
    Ok(Some(app_copy))
}

/// The ref named by the first placeholder in `value`, a string at any depth.
fn first_placeholder(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            text.split_once(PLACEHOLDER_OPEN)
                .map(|(_, after_open)| match parse_ref(after_open) {
                    Ok((reference, _)) => reference.to_string(),
                    Err(refused) => refused.reference().to_string(),
                })
        }
        Value::Array(items) => items.iter().find_map(first_placeholder),
        Value::Object(fields) => fields.values().find_map(first_placeholder),
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

/// The held values, each paired with the placeholder that replaces it,
/// longest value first, so a value containing another is replaced whole.
struct ScrubSet {
    pairs: Vec<(String, String)>,
}

impl ScrubSet {
    /// `None` when no condition session holds a secret: the path every SDK
    /// response takes outside a condition check.
    fn current() -> Option<Self> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for session in held().values() {
            for (reference, value) in &session.refs {
                if !value.is_empty() && !pairs.iter().any(|(held_value, _)| held_value == value) {
                    pairs.push((value.clone(), placeholder(reference)));
                }
            }
        }
        if pairs.is_empty() {
            return None;
        }
        pairs.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        Some(Self { pairs })
    }

    /// `text` with each held value replaced by its placeholder, in one pass
    /// (an inserted placeholder is never rescanned). `None` when `text` holds
    /// no held value.
    fn scrub_str(&self, text: &str) -> Option<String> {
        if !self
            .pairs
            .iter()
            .any(|(value, _)| text.contains(value.as_str()))
        {
            return None;
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            match self
                .pairs
                .iter()
                .find(|(value, _)| rest.starts_with(value.as_str()))
            {
                Some((value, placeholder)) => {
                    out.push_str(placeholder);
                    rest = rest.get(value.len()..).unwrap_or_default();
                }
                None => {
                    out.push(c);
                    rest = rest.get(c.len_utf8()..).unwrap_or_default();
                }
            }
        }
        Some(out)
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
                match self.pairs.iter().find(|(held, _)| *held == decimal) {
                    Some((_, placeholder)) => {
                        *value = Value::String(placeholder.clone());
                        true
                    }
                    None => false,
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
                        let key = self.scrub_str(&key).unwrap_or(key);
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

    /// Byte-level scrub, for a body that is not JSON: each held value, raw
    /// and in its JSON-escaped form, replaced by its placeholder. `None` when
    /// neither form occurs.
    fn scrub_bytes(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut patterns: Vec<(Vec<u8>, &str)> = Vec::new();
        for (value, placeholder) in &self.pairs {
            for form in [value.clone(), json_escaped(value)] {
                let form = form.into_bytes();
                if !form.is_empty() && !patterns.iter().any(|(seen, _)| *seen == form) {
                    patterns.push((form, placeholder.as_str()));
                }
            }
        }
        patterns.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
        if !patterns.iter().any(|(form, _)| {
            bytes
                .windows(form.len())
                .any(|window| window == form.as_slice())
        }) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len());
        let mut rest = bytes;
        while let Some((&first, tail)) = rest.split_first() {
            match patterns.iter().find(|(form, _)| rest.starts_with(form)) {
                Some((form, placeholder)) => {
                    out.extend_from_slice(placeholder.as_bytes());
                    rest = rest.get(form.len()..).unwrap_or_default();
                }
                None => {
                    out.push(first);
                    rest = tail;
                }
            }
        }
        Some(out)
    }

    /// One server-sent-events frame scrubbed: a `data:` line that parses as
    /// JSON is scrubbed field by field and re-serialized; every other line as
    /// text. `None` when nothing changed.
    fn scrub_sse_frame(&self, frame: &[u8]) -> Option<Vec<u8>> {
        let Ok(text) = std::str::from_utf8(frame) else {
            return self.scrub_bytes(frame);
        };
        let mut changed = false;
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let (content, newline) = match line.strip_suffix('\n') {
                Some(content) => (content, "\n"),
                None => (line, ""),
            };
            let scrubbed = match content.strip_prefix("data:") {
                Some(data) => self.scrub_sse_data(data).map(|data| format!("data:{data}")),
                None => self.scrub_str(content),
            };
            match scrubbed {
                Some(scrubbed) => {
                    changed = true;
                    out.push_str(&scrubbed);
                }
                None => out.push_str(content),
            }
            out.push_str(newline);
        }
        changed.then(|| out.into_bytes())
    }

    fn scrub_sse_data(&self, data: &str) -> Option<String> {
        let (lead, payload) = match data.strip_prefix(' ') {
            Some(payload) => (" ", payload),
            None => ("", data),
        };
        match serde_json::from_str::<Value>(payload) {
            Ok(mut parsed) => self
                .scrub_json(&mut parsed)
                .then(|| format!("{lead}{parsed}")),
            Err(_) => self.scrub_str(data),
        }
    }
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

/// `axum::middleware::from_fn` layer on the `/ui-bridge/sdk/*` routers: while
/// a condition session holds a secret, no response carries its value.
///
/// With nothing held a response passes untouched. With something held:
/// - a JSON body is scrubbed field by field ([`ScrubSet::scrub_json`]) and
///   re-serialized only when something changed; any other body byte by byte;
/// - an event stream is scrubbed frame by frame as it flows, never buffered
///   (it may never end), against the sessions live at that frame, so a
///   stream opened before a session started is scrubbed once it does;
/// - a body that cannot be read in full is withheld with a 500, never sent
///   unscrubbed.
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
            warn!(
                "condition_secrets: an SDK response could not be read for scrubbing ({e}); \
                 withheld"
            );
            return scrub_failed();
        }
    };
    let parsed = if content_type(&parts.headers).contains("json") {
        serde_json::from_slice::<Value>(&bytes).ok()
    } else {
        None
    };
    let scrubbed = match parsed {
        Some(mut value) => {
            if !set.scrub_json(&mut value) {
                None
            } else {
                match serde_json::to_vec(&value) {
                    Ok(serialized) => Some(serialized),
                    Err(e) => {
                        warn!(
                            "condition_secrets: a scrubbed SDK response could not be \
                             re-serialized ({e}); withheld"
                        );
                        return scrub_failed();
                    }
                }
            }
        }
        None => set.scrub_bytes(&bytes),
    };
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

    fn secrets(pairs: &[(&str, &str)]) -> FrameSecrets {
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(reference, value)| (reference.to_string(), json!(value)))
            .collect();
        serde_json::from_value(Value::Object(map)).expect("a secrets map parses")
    }

    fn hold(run_id: &str, pairs: &[(&str, &str)]) -> Registration {
        register(run_id, &secrets(pairs)).expect("a non-empty map is held")
    }

    /// Wait (bounded) until `reference` no longer resolves.
    async fn released(reference: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if resolve(reference).is_err() {
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
            register("run-frame-empty", &none).is_none(),
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
    /// and a top-level `text` alike, nothing else in the request, never the
    /// request itself, and never another action.
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
        let app_copy = app_bound_type_body(&body)
            .expect("held refs")
            .expect("a copy for the app");
        assert_eq!(app_copy["params"]["text"], json!("p@ss-sub-Hn5"));
        assert_eq!(app_copy["text"], json!("id alice-sub-Hn6!"));
        assert_eq!(
            app_copy["params"]["clear"],
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
            app_bound_type_body(&top_level_only)
                .expect("held")
                .expect("copy")["text"],
            json!("p@ss-sub-Hn5"),
            "a placeholder in a top-level `text` is substituted"
        );

        for sent_as_is in [
            json!({"action": "click", "params": {"text": "{{secret:cs-sub-pw}}"}}),
            json!({"params": {"text": "{{secret:cs-sub-pw}}"}}),
            json!({"action": "Type", "text": "{{secret:cs-sub-pw}}"}),
            json!({"action": "type", "params": {"text": "plain text"}}),
            json!({"action": "type", "params": {"value": "{{secret:cs-sub-pw}}"}}),
        ] {
            assert_eq!(app_bound_type_body(&sent_as_is), Ok(None), "{sent_as_is}");
        }
    }

    /// An unknown, malformed or unterminated ref is a typed refusal, never the
    /// literal placeholder, and one bad placeholder refuses the whole text.
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
        ] {
            let body = json!({"action": "type", "params": {"text": text}});
            let refused = app_bound_type_body(&body).expect_err(text);
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
        let refused =
            app_bound_type_body(&json!({"action": "type", "text": "{{secret:cs-amb-pw}}"}))
                .expect_err("ambiguous");
        assert_eq!(refused.code(), AMBIGUOUS_REF_CODE);
        let answer = refused.refusal("the action was not dispatched").to_string();
        assert!(
            !answer.contains("tenant-a-pass-Am1") && !answer.contains("tenant-b-pass-Am2"),
            "{answer}"
        );

        let _c = hold("run-amb-c", &[("cs-amb-same", "same-pass-Am3")]);
        let _d = hold("run-amb-d", &[("cs-amb-same", "same-pass-Am3")]);
        assert_eq!(
            app_bound_type_body(&json!({"action": "type", "text": "{{secret:cs-amb-same}}"}))
                .expect("not ambiguous")
                .expect("copy")["text"],
            json!("same-pass-Am3")
        );
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
    /// and JSON-escaped bytes; a number that is not the value stays.
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
            !rendered.contains("rub-Sc1") && !rendered.contains("4829173"),
            "{rendered}"
        );
        assert_eq!(value["echo"], json!("typed {{secret:cs-scrub-pw}}!"));
        assert_eq!(value["pin"], json!("{{secret:cs-scrub-pin}}"));
        assert_eq!(value["other"], json!(4829174), "a different number stays");

        let raw = "x sc\"rub-Sc1 y \"sc\\\"rub-Sc1\" z";
        let scrubbed =
            String::from_utf8(set.scrub_bytes(raw.as_bytes()).expect("changed")).expect("utf-8");
        assert_eq!(
            scrubbed,
            "x {{secret:cs-scrub-pw}} y \"{{secret:cs-scrub-pw}}\" z"
        );
        assert_eq!(set.scrub_str("nothing held here"), None);
        assert_eq!(set.scrub_bytes(b"nothing held here"), None);
    }

    /// The SDK-router layer: JSON, text and event-stream bodies come back with
    /// the placeholder where the value was, and a body carrying no held value
    /// passes byte for byte.
    #[tokio::test]
    async fn every_sdk_response_body_is_scrubbed_while_a_secret_is_held() {
        use axum::response::sse::{Event, Sse};
        use axum::routing::get;
        use tower::ServiceExt;

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
                "/sse",
                get(|| async {
                    Sse::new(futures_util::stream::iter([
                        Ok::<_, std::convert::Infallible>(
                            Event::default().data(json!({"value": SECRET}).to_string()),
                        ),
                        Ok(Event::default().data(format!("raw {SECRET}"))),
                    ]))
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

        let fetch = |path: &'static str| {
            let app = app.clone();
            async move {
                let response = app
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
        };
        for path in ["/json", "/text", "/sse"] {
            let body = fetch(path).await;
            assert!(
                !body.contains(SECRET) && !body.contains(&json_escaped(SECRET)),
                "{path} must not carry the value: {body}"
            );
            assert!(
                body.contains("{{secret:cs-mw-pw}}"),
                "{path}: the placeholder stands where the value was: {body}"
            );
        }
        assert_eq!(
            fetch("/clean").await,
            "{\n  \"pretty\": true\n}",
            "a body carrying no held value is never re-serialized"
        );
    }

    /// A held map lives exactly as long as its session: released when its
    /// registration drops, when its PTY exits (read through a real terminal
    /// manager, as production reads it), and at its TTL.
    #[tokio::test]
    async fn a_held_map_is_released_on_drop_at_pty_exit_and_at_its_ttl() {
        let dropped = hold("run-life-drop", &[("cs-life-drop", "life-drop-Lf1")]);
        assert_eq!(resolve("cs-life-drop"), Ok("life-drop-Lf1".to_string()));
        drop(dropped);
        assert!(
            resolve("cs-life-drop").is_err(),
            "a dropped registration releases"
        );

        let manager = Arc::new(TerminalManager::new());
        let pane = Arc::new(crate::terminal::session::tests::make_test_session(
            Arc::new(Mutex::new(Vec::new())),
        ));
        pane.set_alive_for_test(true);
        manager.insert_for_test("cs-life-pane", Arc::clone(&pane));
        let watched = register("run-life-pty", &secrets(&[("cs-life-pty", "life-pty-Lf2")]));
        release_at_pty_exit_polling(watched, &manager, "cs-life-pane", Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            resolve("cs-life-pty"),
            Ok("life-pty-Lf2".to_string()),
            "held while the PTY lives"
        );
        pane.set_alive_for_test(false);
        assert!(released("cs-life-pty").await, "released once the PTY exits");

        let short = register_for(
            "run-life-ttl",
            &secrets(&[("cs-life-ttl", "life-ttl-Lf3")]),
            Duration::from_millis(20),
        )
        .expect("held");
        assert!(released("cs-life-ttl").await, "released at its TTL");
        assert!(
            ScrubSet::current()
                .is_none_or(|set| !set.pairs.iter().any(|(value, _)| value == "life-ttl-Lf3")),
            "an expired value is no longer held anywhere"
        );
        drop(short);
    }

    /// The journey rows a `type` action produces (its edge, closed by the next
    /// snapshot, and the frontier of that snapshot) never carry what was
    /// typed, even when the app reports the field's new value.
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

    /// Every `/ui-bridge/sdk/*` router carries the scrub layer AFTER its last
    /// route (`Router::layer` wraps only the routes registered before it).
    #[test]
    fn every_sdk_router_carries_the_scrub_layer_after_its_last_route() {
        for (file, src) in [
            ("mcp/sdk_client.rs", include_str!("mcp/sdk_client.rs")),
            (
                "mcp/ui_bridge/sdk_spec_sync.rs",
                include_str!("mcp/ui_bridge/sdk_spec_sync.rs"),
            ),
        ] {
            let routes = src
                .split("\npub fn routes() -> Router<Arc<ApiState>> {")
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

    /// `handle_element_action` substitutes only the app-bound copies, answers
    /// a refused substitution without dispatching, refuses a placeholder on
    /// its IPC fallback before anything is typed, and scrubs its result
    /// before the journey ledger and the event row read it. A handler needs an
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
        assert!(at("element_action_app_payloads(&id, &body, tab_id)") < dispatch);
        assert!(at("Err(refused) => crate::condition_secrets::refused_action(") < dispatch);
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
