//! The runner's side of UI Bridge device pairing (`@qontinui/ui-bridge-native`
//! 0.11.0 and later).
//!
//! A phone's UI Bridge server admits a caller on the phone itself, which is what an
//! `adb forward` delivers, and refuses anyone else unless each request carries
//! `X-UI-Bridge-Auth: <ts>.<nonce>.<sig>`: an HMAC over the request keyed by a token
//! the phone issued. The token never crosses the network after pairing. A bearer
//! would not do, because the device id is public in the mDNS record, so whoever
//! announced it would be handed the bearer on the next connect.
//!
//! - [`pair_with_device`] calls the phone's `POST /ui-bridge/pair` and stores what it
//!   returns, keyed by the phone's own `deviceId`. Over a cable the phone asks for no
//!   code. On Wi-Fi the operator reads a 6-digit code off the app's Settings.
//! - [`DeviceTokenStore`] keeps the tokens in memory and in an encrypted file of
//!   their own, so a sign-out does not unpair phones.
//! - [`DeviceGate`] is what the phone's open `/ui-bridge/health` says about this
//!   caller. It decides whether a connection signs.
//! - [`DeviceSigner`] signs one request, byte for byte as `canonicalSignatureInput`
//!   in ui-bridge-native's `src/server/pairing.ts`.
//!
//! Nothing here logs a token or a code.

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::secure_storage::EncryptedJsonFile;

/// The request header carrying `<ts>.<nonce>.<sig>`.
pub const UI_BRIDGE_AUTH_HEADER: &str = "X-UI-Bridge-Auth";

/// The phone's pair route. It is not under a configurable base path: the phone
/// answers it only at its network entry, ahead of every other route.
pub const PAIR_PATH: &str = "/ui-bridge/pair";

/// The phone's refusal code for a caller that must sign and did not, or whose
/// signature failed.
pub const PAIRING_REQUIRED: &str = "PAIRING_REQUIRED";
/// The phone's refusal code for a signature whose timestamp is outside its window.
/// Its answer carries `data.serverTime`, so one re-signed retry cures it.
pub const PAIRING_TIMESTAMP_REFUSED: &str = "PAIRING_TIMESTAMP_REFUSED";

/// The encrypted file beside `auth_tokens.enc` that holds the tokens.
const DEVICE_TOKENS_FILE: &str = "ui_bridge_device_tokens.enc";

/// Longest token accepted from a phone. The phone mints 64 hex characters.
const MAX_TOKEN_LEN: usize = 1024;

/// How often a phone this runner holds no token for may re-read the file: another
/// runner on this machine may have paired it, or an earlier read may have failed.
const RELOAD_INTERVAL: Duration = Duration::from_secs(5);

/// The phone's refusal code for a caller it holds off after a signature it refused.
pub const PAIRING_BACKOFF: &str = "PAIRING_BACKOFF";

/// How long a pair call may take. The phone answers only after its secure store
/// has written the token, and bounds that write at 10 s.
const PAIR_TIMEOUT: Duration = Duration::from_secs(15);

// ============================================================================
// Token store
// ============================================================================

#[derive(Default, Serialize, Deserialize)]
struct DeviceTokenFile {
    #[serde(default)]
    devices: BTreeMap<String, StoredDeviceToken>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDeviceToken {
    token: String,
    paired_at: i64,
}

// Hand-written, so no debug print of the file can show a token.
impl std::fmt::Debug for DeviceTokenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceTokenFile")
            .field("devices", &self.devices.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl std::fmt::Debug for StoredDeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredDeviceToken")
            .field("token", &"<redacted>")
            .field("paired_at", &self.paired_at)
            .finish()
    }
}

/// The tokens phones issued to this runner, keyed by each phone's `deviceId`.
///
/// Reads come from memory: a signer asks on every request. The file is read on
/// first use, and again, at most every [`RELOAD_INTERVAL`], while a read failed or
/// a phone is asked for that memory has no token for, so a pairing another runner
/// on this machine made is found, and a failed read is not kept for the process's
/// life. A file that exists but cannot be read leaves every phone reading as
/// unpaired, and only an explicit pairing may rewrite it.
///
/// The file is small and read or written only on those occasions and at a
/// pairing, so its I/O runs where it is asked for rather than on a blocking pool.
pub struct DeviceTokenStore {
    file: Option<EncryptedJsonFile>,
    /// `None` until a read of the file succeeds.
    tokens: RwLock<Option<BTreeMap<String, String>>>,
    /// When the file was last read, successfully or not.
    last_read: Mutex<Option<Instant>>,
}

impl DeviceTokenStore {
    /// A store that keeps nothing on disk.
    pub fn in_memory() -> Self {
        Self {
            file: None,
            tokens: RwLock::new(Some(BTreeMap::new())),
            last_read: Mutex::new(None),
        }
    }

    /// A store backed by `file`, read on first use.
    pub fn persisted(file: EncryptedJsonFile) -> Self {
        Self {
            file: Some(file),
            tokens: RwLock::new(None),
            last_read: Mutex::new(None),
        }
    }

    /// Read the file into memory, unless it was read within [`RELOAD_INTERVAL`]
    /// (`force` skips that wait). A failed read leaves memory as it was.
    fn reload(&self, force: bool) {
        let Some(file) = &self.file else {
            return;
        };
        {
            let mut last_read = self
                .last_read
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !force && last_read.is_some_and(|at| at.elapsed() < RELOAD_INTERVAL) {
                return;
            }
            *last_read = Some(Instant::now());
        }
        match file.load::<DeviceTokenFile>() {
            Ok(stored) => {
                let tokens = stored
                    .devices
                    .into_iter()
                    .map(|(device_id, entry)| (device_id, entry.token))
                    .collect();
                *self.tokens.write().unwrap_or_else(PoisonError::into_inner) = Some(tokens);
            }
            Err(e) => warn!(
                "UI Bridge device tokens could not be read ({e:#}): phones read as unpaired \
                 until a read succeeds, and only an explicit pairing may rewrite the file"
            ),
        }
    }

    fn lookup<R>(
        &self,
        read: impl Fn(Option<&BTreeMap<String, String>>) -> Option<R>,
    ) -> Option<R> {
        let cached = || {
            read(
                self.tokens
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref(),
            )
        };
        if let Some(found) = cached() {
            return Some(found);
        }
        self.reload(false);
        cached()
    }

    /// The token `device_id` issued to this runner.
    pub fn token_for(&self, device_id: &str) -> Option<String> {
        self.lookup(|tokens| tokens?.get(device_id).cloned())
    }

    /// Whether `device_id` issued this runner a token.
    pub fn has_token(&self, device_id: &str) -> bool {
        self.lookup(|tokens| tokens?.contains_key(device_id).then_some(()))
            .is_some()
    }

    /// The phones this runner holds a token for.
    pub fn paired_device_ids(&self) -> Vec<String> {
        if self
            .tokens
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
        {
            self.reload(false);
        }
        self.tokens
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|tokens| tokens.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Keep `token` for `device_id`, on disk first and then in memory, so memory
    /// never holds a token the file lost. `explicit` is set for a pairing the
    /// operator asked for, which may rebuild an unreadable file.
    pub fn store(&self, device_id: &str, token: &str, explicit: bool) -> Result<(), String> {
        if let Some(file) = &self.file {
            let entry = StoredDeviceToken {
                token: token.to_string(),
                paired_at: chrono::Utc::now().timestamp_millis(),
            };
            file.update(explicit, |stored: &mut DeviceTokenFile| {
                stored.devices.insert(device_id.to_string(), entry);
            })
            .map_err(|e| format!("{e:#}"))?;
            // The file now holds every phone's token, this one among them: read it
            // back rather than patch a memory that a failed read may have left empty.
            self.reload(true);
        }
        self.tokens
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(BTreeMap::new)
            .insert(device_id.to_string(), token.to_string());
        Ok(())
    }
}

/// The store every connection signs from.
///
/// A test build keeps it in memory, so no test reads or writes the operator's
/// tokens whatever its environment.
pub fn device_tokens() -> Arc<DeviceTokenStore> {
    static STORE: OnceLock<Arc<DeviceTokenStore>> = OnceLock::new();
    STORE.get_or_init(|| Arc::new(process_store())).clone()
}

#[cfg(not(test))]
fn process_store() -> DeviceTokenStore {
    match EncryptedJsonFile::in_storage_dir(DEVICE_TOKENS_FILE) {
        Ok(file) => DeviceTokenStore::persisted(file),
        Err(e) => {
            warn!(
                "UI Bridge device tokens have no file ({e:#}): pairings last only until the \
                 runner exits"
            );
            DeviceTokenStore::in_memory()
        }
    }
}

#[cfg(test)]
fn process_store() -> DeviceTokenStore {
    let _ = DEVICE_TOKENS_FILE;
    DeviceTokenStore::in_memory()
}

// ============================================================================
// What the phone's open health says
// ============================================================================

/// What a phone's open `GET /ui-bridge/health` says about the caller that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGate {
    /// The phone's stable id, as its mDNS record publishes it.
    pub device_id: Option<String>,
    /// The phone's clock, epoch ms.
    pub server_time_ms: Option<i64>,
    /// The phone needs this caller to sign.
    pub required: bool,
    /// The phone counted this caller as authenticated: on the phone itself, or signed.
    pub authenticated: bool,
}

impl DeviceGate {
    /// Read the gate out of a health answer, enveloped or flat. `None` when the
    /// answer has no `pairing` block: a server with no gate, such as a web app or a
    /// phone on a UI Bridge that predates pairing.
    pub fn from_health(health: &serde_json::Value) -> Option<Self> {
        let data = health
            .get("data")
            .filter(|data| data.is_object())
            .unwrap_or(health);
        let pairing = data.get("pairing")?;
        let required = pairing.get("required")?.as_bool()?;
        let authenticated = pairing
            .get("authenticated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Some(Self {
            device_id: data
                .get("deviceId")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            server_time_ms: data.get("serverTime").and_then(serde_json::Value::as_i64),
            required,
            authenticated,
        })
    }

    /// The phone admitted the caller as being on the phone itself, which over the
    /// network only an `adb forward` (or the iOS sidecar) delivers. A phone that
    /// trusts its whole network (`allowUnauthenticatedNetwork`) reads `false`.
    pub fn admits_as_local(&self) -> bool {
        !self.required && self.authenticated
    }
}

// ============================================================================
// Signing
// ============================================================================

/// The `X-UI-Bridge-Auth` value for one request.
///
/// The signed string is `METHOD \n path?query \n ts \n nonce \n sha256(body)`, in
/// UTF-8, with lowercase hex for the digest, and the key is the token string's own
/// UTF-8 bytes rather than its hex-decoded value. `path_and_query` is the request
/// target exactly as sent. `ts_ms` is on the PHONE's clock.
pub fn signature_header(
    token: &str,
    method: &str,
    path_and_query: &str,
    ts_ms: i64,
    nonce: &str,
    body: &[u8],
) -> String {
    let body_sha256 = hex::encode(Sha256::digest(body));
    let input = format!("{method}\n{path_and_query}\n{ts_ms}\n{nonce}\n{body_sha256}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(input.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    format!("{ts_ms}.{nonce}.{sig}")
}

/// A nonce the phone accepts (`[A-Za-z0-9_-]{16,64}`), fresh for each request.
fn fresh_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Signs a connection's requests for one phone.
///
/// The token is looked up on every request rather than held, so a pairing that
/// replaces it takes effect on the next request without a reconnect.
pub struct DeviceSigner {
    device_id: String,
    store: Arc<DeviceTokenStore>,
    /// The phone's clock minus this machine's, in ms.
    clock_offset_ms: AtomicI64,
}

impl DeviceSigner {
    /// A signer for `device_id`, with the clock offset learned from the phone's
    /// `serverTime`. The phone accepts a timestamp within 300 s of its clock, so the
    /// round trip's share of the offset does not matter.
    pub fn new(
        device_id: String,
        store: Arc<DeviceTokenStore>,
        server_time_ms: Option<i64>,
    ) -> Self {
        let signer = Self {
            device_id,
            store,
            clock_offset_ms: AtomicI64::new(0),
        };
        if let Some(server_time_ms) = server_time_ms {
            signer.resync(server_time_ms);
        }
        signer
    }

    /// The phone this signer signs for.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Whether the phone issued this runner a token.
    pub fn has_token(&self) -> bool {
        self.store.has_token(&self.device_id)
    }

    /// Re-learn the offset from a `serverTime` the phone just sent.
    pub fn resync(&self, server_time_ms: i64) {
        let offset = server_time_ms - chrono::Utc::now().timestamp_millis();
        self.clock_offset_ms.store(offset, Ordering::Relaxed);
    }

    /// The header for one request, or `None` when no token is held, in which case
    /// the request goes unsigned and the phone answers `PAIRING_REQUIRED`.
    pub fn sign(&self, method: &str, path_and_query: &str, body: &[u8]) -> Option<String> {
        let token = self.store.token_for(&self.device_id)?;
        let ts_ms =
            chrono::Utc::now().timestamp_millis() + self.clock_offset_ms.load(Ordering::Relaxed);
        Some(signature_header(
            &token,
            method,
            path_and_query,
            ts_ms.max(0),
            &fresh_nonce(),
            body,
        ))
    }
}

// ============================================================================
// Pairing
// ============================================================================

/// Why a pair call did not end with a stored token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// The phone answered the pair route with 404: its UI Bridge predates pairing,
    /// so it needs no token and accepts callers on the network unpaired.
    AppTooOld,
    /// The phone refused, with its code and message.
    Refused {
        status: u16,
        code: String,
        message: String,
    },
    /// The phone could not be reached.
    Unreachable(String),
    /// The phone answered with something that is not a pairing.
    BadAnswer(String),
    /// The token could not be kept.
    Store(String),
    /// The phone answered with a token other than the one this runner holds for
    /// it, and this pairing may not replace it ([`PairOptions::may_replace`]).
    TokenHeld { device_id: String },
}

impl PairError {
    /// The machine-readable code a caller can branch on.
    pub fn code(&self) -> &str {
        match self {
            Self::AppTooOld => "PAIRING_APP_TOO_OLD",
            Self::Refused { code, .. } if !code.is_empty() => code,
            Self::Refused { .. } => "PAIRING_REFUSED",
            Self::Unreachable(_) => "PAIRING_UNREACHABLE",
            Self::BadAnswer(_) => "PAIRING_BAD_ANSWER",
            Self::Store(_) => "PAIRING_STORE_FAILED",
            Self::TokenHeld { .. } => PAIRING_TOKEN_HELD,
        }
    }

    /// What to tell the operator.
    pub fn message(&self) -> String {
        match self {
            Self::AppTooOld => {
                "The app on this device answered POST /ui-bridge/pair with 404: its UI \
                 Bridge predates pairing (before @qontinui/ui-bridge-native 0.11.0), so it needs \
                 no token and accepts callers on the network unpaired."
                    .to_string()
            }
            Self::Refused { code, message, .. } => match code.as_str() {
                "PAIRING_NOT_ARMED" => "The device has no pairing code armed. Tap Pair in the \
                     app's Settings, then send the 6-digit code it shows within about 2 \
                     minutes, or pair over a cable, which needs no code."
                    .to_string(),
                "PAIRING_CODE_REQUIRED" => "Off a cable the device needs the 6-digit code its \
                     Settings shows: send it as {\"code\": \"123456\"}."
                    .to_string(),
                "PAIRING_CODE_INVALID" => "That code is not the one the device shows. Five wrong \
                     codes disarm it, and the app's Settings must then arm a new one."
                    .to_string(),
                "PAIRING_UNCONFIGURED" => "The app on this device has not configured pairing, so \
                     it accepts only callers on the phone itself: reach it over a cable (adb \
                     forward)."
                    .to_string(),
                _ => format!("The device refused to pair ({code}): {message}"),
            },
            Self::Unreachable(e) => format!("The device could not be reached to pair: {e}"),
            Self::BadAnswer(e) => format!("The device's pair answer was not usable: {e}"),
            Self::Store(e) => format!("The device paired, but its token could not be kept: {e}"),
            Self::TokenHeld { device_id } => format!(
                "This runner already holds a different pairing token for device {device_id}, so \
                 the new one was not kept. A phone gives every pairing the same token until the \
                 app's Settings revoke it, so a different one means either the pairing was \
                 revoked there or another device is answering as this one. That pairing spent \
                 the code. If you revoked it, tap Pair in the app's Settings again and send the \
                 new code as {{\"code\": \"123456\", \"replace\": true}}, or pair over a USB cable."
            ),
        }
    }
}

/// The refusal of a pairing that would replace a token this runner holds.
pub const PAIRING_TOKEN_HELD: &str = "PAIRING_TOKEN_HELD";

/// How [`pair_with_device`] asks a phone for its token.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairOptions<'a> {
    /// The 6-digit code the app's Settings shows. None is needed by a caller the
    /// phone admits as on the phone itself (a cable).
    pub code: Option<&'a str>,
    /// The operator asked for this pairing, rather than the runner fetching a token
    /// by itself. See [`DeviceTokenStore::store`].
    pub explicit: bool,
    /// The `deviceId` the caller has just read from the phone's health; read here
    /// when `None`.
    pub expected_device_id: Option<&'a str>,
    /// Whether a different token may replace one this runner already holds for the
    /// phone. A phone hands every pairing the same token until the app revokes it,
    /// so a different one is a revoke or another device answering as this one, and
    /// only an operator's pairing over a cable, or one that says it revoked, may
    /// replace it.
    pub may_replace: bool,
}

/// The phone this runner now holds a token for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    /// The phone's own id, which the token is keyed by.
    pub device_id: String,
}

/// A client for the phone's open routes: no `Authorization`, no `Origin`, no
/// redirects, and direct rather than through a configured proxy.
fn open_route_client(timeout: Duration) -> Result<reqwest::Client, PairError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| PairError::Unreachable(format!("could not build a client: {e}")))
}

/// What the open `GET /ui-bridge/health` at `base_url` says. `Ok(None)` is a
/// server with no pairing gate, or one that does not answer health as JSON.
async fn read_gate(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Option<DeviceGate>, PairError> {
    let url = format!("{}/ui-bridge/health", base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| PairError::Unreachable(e.without_url().to_string()))?;
    Ok(resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .as_ref()
        .and_then(DeviceGate::from_health))
}

/// Ask the phone at `base_url` for a token and keep it.
///
/// A caller on the phone itself (an `adb forward`) needs no code, and a second
/// call returns the same token, so a re-fetch never invalidates another runner.
/// See [`PairOptions`].
///
/// The token is kept only under the `deviceId` the phone's own health names, which
/// is the id a connect looks it up by. An answer naming another id, or a health
/// answer naming none, is refused and nothing is kept. A token other than one this
/// runner holds for that id is refused unless [`PairOptions::may_replace`], so a
/// device answering as another cannot write over its token.
///
/// The requests carry no `Authorization`, no `Origin`, and go direct rather than
/// through a configured proxy: the pair answer is the token.
pub async fn pair_with_device(
    base_url: &str,
    store: &DeviceTokenStore,
    options: PairOptions<'_>,
) -> Result<Paired, PairError> {
    let PairOptions {
        code,
        explicit,
        expected_device_id,
        may_replace,
    } = options;
    let client = open_route_client(PAIR_TIMEOUT)?;
    let expected = match expected_device_id {
        Some(id) => Some(id.to_string()),
        None => read_gate(&client, base_url)
            .await?
            .and_then(|gate| gate.device_id),
    };
    let url = format!("{}{PAIR_PATH}", base_url.trim_end_matches('/'));
    let body = match code {
        Some(code) => serde_json::json!({ "code": code }),
        None => serde_json::json!({}),
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| PairError::Unreachable(e.without_url().to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(PairError::AppTooOld);
    }
    // Never echoed into an error: a successful answer carries the token.
    let answer: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| PairError::BadAnswer(format!("HTTP {status} with no JSON body")))?;
    if !status.is_success()
        || answer.get("success").and_then(serde_json::Value::as_bool) != Some(true)
    {
        let text = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        return Err(PairError::Refused {
            status: status.as_u16(),
            code: text("code"),
            message: text("error"),
        });
    }
    let data = answer.get("data");
    let field = |key: &str| {
        data.and_then(|data| data.get(key))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let token =
        field("token").ok_or_else(|| PairError::BadAnswer("no token in the answer".to_string()))?;
    if token.len() > MAX_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(PairError::BadAnswer(
            "the token is not printable ASCII of a sane length".to_string(),
        ));
    }
    let device_id = field("deviceId")
        .ok_or_else(|| PairError::BadAnswer("no deviceId in the answer".to_string()))?
        .to_string();
    let Some(expected) = expected else {
        warn!(answered = %device_id, "UI Bridge device health named no deviceId; nothing kept");
        return Err(PairError::BadAnswer(
            "the device's health names no deviceId, so there is no id to keep its token under"
                .to_string(),
        ));
    };
    if expected != device_id {
        warn!(expected = %expected, answered = %device_id, "UI Bridge pair answer named another deviceId; nothing kept");
        return Err(PairError::BadAnswer(format!(
            "the pair answer names deviceId {device_id}, but the device's health names \
             {expected}, so its token was not kept"
        )));
    }
    if !may_replace
        && store
            .token_for(&device_id)
            .is_some_and(|held| held != token)
    {
        warn!(device_id = %device_id, "UI Bridge pair answer carries a token other than the one held; nothing kept");
        return Err(PairError::TokenHeld { device_id });
    }
    store
        .store(&device_id, token, explicit)
        .map_err(PairError::Store)?;
    info!(device_id = %device_id, explicit, "UI Bridge device paired; its token is stored");
    Ok(Paired { device_id })
}

/// What fetching a token over a fresh local forward came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPairing {
    /// - `paired`: this runner now holds the phone's token, so it can reach the
    ///   phone over Wi-Fi too.
    /// - `not_gated`: the server has no pairing gate. Its UI Bridge predates
    ///   pairing, or it is not a phone.
    /// - `not_local`: the phone did not admit the forward as on the phone itself,
    ///   so it asks for a code; pair with `POST /ui-bridge/devices/{id}/pair`.
    /// - `not_cable`: the forward is not one this runner made to a phone over adb
    ///   or the iOS sidecar (a local emulator's, say, which is never reached over
    ///   the network), so no token is fetched over it.
    /// - `unreachable`: the forward did not answer.
    /// - `failed`: the phone refused or the token could not be kept; see `code`.
    pub status: &'static str,
    /// The phone's own id, which its token is keyed by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_bridge_device_id: Option<String>,
    /// The refusal code, for `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Fetch the token of the phone behind a local forward at `base_url` (an `adb
/// forward`, or the iOS sidecar's), which asks no code of a caller it admits as
/// on the phone itself.
pub async fn pair_over_local_forward(base_url: &str) -> LocalPairing {
    let outcome = |status, ui_bridge_device_id, code| LocalPairing {
        status,
        ui_bridge_device_id,
        code,
    };
    if !super::is_cable_url_now(base_url).await {
        return outcome("not_cable", None, None);
    }
    let gate = match open_route_client(Duration::from_secs(3)) {
        Ok(client) => match read_gate(&client, base_url).await {
            Ok(gate) => gate,
            Err(_) => return outcome("unreachable", None, None),
        },
        Err(_) => return outcome("unreachable", None, None),
    };
    let Some(gate) = gate else {
        return outcome("not_gated", None, None);
    };
    if !gate.admits_as_local() {
        return outcome("not_local", gate.device_id, None);
    }
    let Some(device_id) = gate.device_id.clone() else {
        return outcome("failed", None, Some("PAIRING_BAD_ANSWER".to_string()));
    };
    let options = PairOptions {
        expected_device_id: Some(&device_id),
        ..PairOptions::default()
    };
    match pair_with_device(base_url, &device_tokens(), options).await {
        Ok(paired) => outcome("paired", Some(paired.device_id), None),
        Err(PairError::AppTooOld) => outcome("not_gated", gate.device_id, None),
        Err(e) => outcome("failed", gate.device_id, Some(e.code().to_string())),
    }
}

/// Fetch `device_id`'s token from `base_url` in the background, where the phone
/// admits this runner as on the phone itself and so asks for no code: a cable,
/// which then leaves the same phone reachable over Wi-Fi. Only a cable counts
/// ([`super::is_cable_url_now`]: an adb forward the adb server lists, or an iOS
/// sidecar forward); anything else is skipped, because a phone's own claim to be
/// local is something anything this runner connects to can make. A token this
/// runner already holds is never replaced from here. `only_if_missing` skips a
/// phone this runner already holds a token for. Failures are logged and otherwise
/// ignored; nothing waits on this.
pub fn spawn_token_fetch(base_url: String, device_id: String, only_if_missing: bool) {
    let store = device_tokens();
    if only_if_missing && store.has_token(&device_id) {
        return;
    }
    tokio::spawn(async move {
        if !super::is_cable_url_now(&base_url).await {
            tracing::debug!(device_id = %device_id, "not a cable; no token fetch");
            return;
        }
        let options = PairOptions {
            expected_device_id: Some(&device_id),
            ..PairOptions::default()
        };
        match pair_with_device(&base_url, &store, options).await {
            Ok(_) => {}
            Err(PairError::AppTooOld) => {}
            Err(e) => warn!(
                device_id = %device_id,
                code = e.code(),
                "UI Bridge device token could not be fetched over the local connection: {}",
                e.message()
            ),
        }
    });
}

/// What to tell a caller about a phone's `PAIRING_*` refusal of a request, in place
/// of the phone's own text, which names its own route rather than the runner's.
/// `None` leaves the phone's text as it is.
pub fn explain_refusal(code: &str, device_id: Option<&str>) -> Option<String> {
    let device = device_id
        .map(|id| format!("Device {id}"))
        .unwrap_or_else(|| "The device".to_string());
    match code {
        PAIRING_REQUIRED => Some(pairing_required_message(device_id)),
        PAIRING_TIMESTAMP_REFUSED => Some(format!(
            "{device} refused the request's timestamp even after this runner re-synced to its \
             clock. Check the clocks on the phone and on this machine."
        )),
        "PAIRING_BODY_TOO_LARGE" => Some(format!(
            "{device} refuses a signed request body over 262144 characters (about 256 KiB of \
             ASCII). Send less, or reach the device over a cable (adb forward), where nothing \
             is signed and no limit applies."
        )),
        PAIRING_BACKOFF => Some(format!(
            "{device} is holding this runner off after refusing its signatures, for a wait that \
             grows to 30 s. That usually means the app revoked this runner's pairing: pair \
             again with POST /ui-bridge/devices/{{id}}/pair, {{id}} being the device's row in \
             GET /ui-bridge/devices."
        )),
        "PAIRING_TARGET_TOO_LONG" => Some(format!(
            "{device} refuses a signed request whose path and query exceed 8192 characters. \
             Shorten it, or reach the device over a cable (adb forward)."
        )),
        "PAIRING_UNCONFIGURED" => Some(format!(
            "{device} has not configured pairing, so it accepts only callers on the phone \
             itself: reach it over a cable (adb forward)."
        )),
        _ => None,
    }
}

/// How to pair `device_id`, for a connect or a request the phone refused.
pub fn pairing_required_message(device_id: Option<&str>) -> String {
    let device = device_id
        .map(|id| format!("Device {id}"))
        .unwrap_or_else(|| "The device".to_string());
    format!(
        "{device} requires pairing before this runner can drive it over the network, or it \
         revoked this runner's pairing. Pair with POST /ui-bridge/devices/{{id}}/pair, {{id}} \
         being the device's row in GET /ui-bridge/devices. Over a cable (adb forward) no code is \
         needed. On Wi-Fi, tap Pair in the app's Settings and send the 6-digit code it shows as \
         {{\"code\": \"123456\"}}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ui-bridge-native's published test vector (`pairing.test.ts`, "matches a vector
    // computed by an independent HMAC"). The token is that vector's, not a secret.
    const VECTOR_TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"; // gitleaks:allow
    const VECTOR_NONCE: &str = "0123456789abcdef0123456789abcdef";
    const VECTOR_TS: i64 = 1_798_761_600_000;

    #[test]
    fn signature_matches_the_phones_published_vector() {
        assert_eq!(
            signature_header(
                VECTOR_TOKEN,
                "POST",
                "/ui-bridge/control/element/a%20b/action?x=1&y=%C3%A9",
                VECTOR_TS,
                VECTOR_NONCE,
                "{\"elementId\":\"é\"}".as_bytes(),
            ),
            format!(
                "{VECTOR_TS}.{VECTOR_NONCE}.6be10f9a7c3dec0f0ed248a5b23a7110abb94f30f870d7ac5f706b68f80c7586"
            )
        );
        assert_eq!(
            signature_header(
                VECTOR_TOKEN,
                "GET",
                "/ui-bridge/control/snapshot",
                VECTOR_TS,
                VECTOR_NONCE,
                b"",
            ),
            format!(
                "{VECTOR_TS}.{VECTOR_NONCE}.d856303e4228382376cf7dd3adf5fc190a82d1cb2e0f54eb6fb31fb6b1de02bf"
            )
        );
    }

    #[test]
    fn a_signature_matches_the_phones_header_grammar_and_never_carries_the_token() {
        let store = Arc::new(DeviceTokenStore::in_memory());
        store.store("phone-1", VECTOR_TOKEN, true).unwrap();
        let signer = DeviceSigner::new("phone-1".into(), store, None);
        let header = signer
            .sign("GET", "/ui-bridge/control/snapshot", b"")
            .unwrap();
        let parts: Vec<&str> = header.split('.').collect();
        assert_eq!(parts.len(), 3, "{header}");
        assert!(parts[0].len() <= 16 && parts[0].bytes().all(|b| b.is_ascii_digit()));
        assert!((16..=64).contains(&parts[1].len()));
        assert!(parts[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        assert_eq!(parts[2].len(), 64);
        assert!(parts[2]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert!(!header.contains(VECTOR_TOKEN));
        let again = signer
            .sign("GET", "/ui-bridge/control/snapshot", b"")
            .unwrap();
        assert_ne!(
            parts[1],
            again.split('.').nth(1).unwrap(),
            "every request gets a fresh nonce"
        );
    }

    #[test]
    fn a_signer_reads_the_phones_clock_and_the_current_token() {
        let store = Arc::new(DeviceTokenStore::in_memory());
        let phone_time = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let signer = DeviceSigner::new("phone-2".into(), store.clone(), Some(phone_time));
        assert!(
            signer.sign("GET", "/x", b"").is_none(),
            "no token, no signature"
        );
        assert!(!signer.has_token());

        store.store("phone-2", "first-token", true).unwrap();
        let header = signer.sign("GET", "/x", b"").unwrap();
        let ts: i64 = header.split('.').next().unwrap().parse().unwrap();
        assert!(
            (ts - phone_time).abs() < 60_000,
            "the timestamp is on the phone's clock: {ts} vs {phone_time}"
        );
        let nonce = header.split('.').nth(1).unwrap();
        assert_eq!(
            header,
            signature_header("first-token", "GET", "/x", ts, nonce, b"")
        );

        store.store("phone-2", "second-token", true).unwrap();
        let header = signer.sign("GET", "/x", b"").unwrap();
        let ts: i64 = header.split('.').next().unwrap().parse().unwrap();
        let nonce = header.split('.').nth(1).unwrap();
        assert_eq!(
            header,
            signature_header("second-token", "GET", "/x", ts, nonce, b""),
            "a re-pair takes effect on the next request"
        );
        let other = DeviceSigner::new("phone-3".into(), store, None);
        assert!(
            other.sign("GET", "/x", b"").is_none(),
            "another phone's token is never used"
        );
    }

    #[test]
    fn the_gate_is_read_from_enveloped_and_flat_health() {
        let enveloped = serde_json::json!({
            "success": true,
            "data": {
                "status": "ok",
                "deviceId": "abc",
                "serverTime": 42,
                "pairing": { "required": true, "authenticated": false }
            }
        });
        assert_eq!(
            DeviceGate::from_health(&enveloped),
            Some(DeviceGate {
                device_id: Some("abc".into()),
                server_time_ms: Some(42),
                required: true,
                authenticated: false,
            })
        );
        let flat = serde_json::json!({ "pairing": { "required": false, "authenticated": true } });
        let gate = DeviceGate::from_health(&flat).unwrap();
        assert!(gate.admits_as_local());
        assert_eq!(gate.device_id, None);

        let open_network =
            serde_json::json!({ "pairing": { "required": false, "authenticated": false } });
        assert!(!DeviceGate::from_health(&open_network)
            .unwrap()
            .admits_as_local());

        let web_app = serde_json::json!({ "success": true, "data": { "status": "ok" } });
        assert_eq!(
            DeviceGate::from_health(&web_app),
            None,
            "no gate on a server without one"
        );
    }

    #[test]
    fn a_persisted_store_survives_a_new_process_and_keeps_tokens_apart() {
        let path = crate::test_env::process_scratch_dir("qontinui_test_device_tokens")
            .join("persisted.enc");
        let _ = std::fs::remove_file(&path);
        let store =
            DeviceTokenStore::persisted(EncryptedJsonFile::with_path(path.clone()).unwrap());
        store.store("phone-a", "token-a", false).unwrap();
        store.store("phone-b", "token-b", false).unwrap();

        let reopened =
            DeviceTokenStore::persisted(EncryptedJsonFile::with_path(path.clone()).unwrap());
        assert_eq!(reopened.token_for("phone-a").as_deref(), Some("token-a"));
        assert_eq!(reopened.token_for("phone-b").as_deref(), Some("token-b"));
        assert_eq!(
            reopened.paired_device_ids(),
            vec!["phone-a".to_string(), "phone-b".to_string()]
        );
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains("token-a"),
            "never plaintext on disk"
        );
    }

    #[test]
    fn an_unreadable_token_file_reads_unpaired_and_only_an_explicit_pair_rewrites_it() {
        let path = crate::test_env::process_scratch_dir("qontinui_test_device_tokens")
            .join("unreadable.enc");
        std::fs::write(&path, b"garbage that is not ciphertext at all").unwrap();
        let store =
            DeviceTokenStore::persisted(EncryptedJsonFile::with_path(path.clone()).unwrap());
        assert!(!store.has_token("phone"));
        assert!(
            store.store("phone", "t", false).is_err(),
            "a background write refuses"
        );
        assert!(
            !store.has_token("phone"),
            "memory holds nothing the file refused"
        );
        store.store("phone", "t", true).unwrap();
        assert_eq!(store.token_for("phone").as_deref(), Some("t"));
    }

    #[test]
    fn refusals_name_the_runners_route_not_the_phones() {
        let text = explain_refusal(PAIRING_REQUIRED, Some("phone-9")).unwrap();
        assert!(text.contains("phone-9"));
        assert!(text.contains("POST /ui-bridge/devices/{id}/pair"));
        assert!(!text.contains("POST /ui-bridge/pair "));
        assert!(explain_refusal("PAIRING_BODY_TOO_LARGE", None)
            .unwrap()
            .contains("256 KiB"));
        assert!(explain_refusal("PAIRING_TARGET_TOO_LONG", None)
            .unwrap()
            .contains("8192"));
        assert!(explain_refusal(PAIRING_BACKOFF, Some("phone-1"))
            .unwrap()
            .contains("POST /ui-bridge/devices/{id}/pair"));
        assert_eq!(explain_refusal("SOMETHING_ELSE", None), None);
    }
}
