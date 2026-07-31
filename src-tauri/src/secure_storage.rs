//! Encrypted file-based storage for authentication tokens.
//!
//! This module provides encrypted file storage as a reliable alternative to
//! the OS keychain, which has proven unreliable on Windows.
//!
//! # Security model — read this before relying on it
//!
//! Tokens are encrypted with AES-256-GCM, but the key is DERIVED FROM PUBLIC,
//! LOCALLY-READABLE INPUTS (hostname ‖ service name ‖ a static string ‖
//! username — see [`SecureStorage::derive_key`]). Any process that can read
//! the file and knows (or guesses) the hostname + username can re-derive the
//! key. The honest guarantees are therefore:
//!
//! - **Same-machine/user binding**: the ciphertext only decrypts trivially on
//!   this host under this username; a file exfiltrated off-box does not
//!   decrypt without also knowing those identifiers (obfuscation, not proof
//!   against a determined attacker — the identifiers are low-entropy).
//! - **Tamper evidence**: AES-GCM authentication means a modified file fails
//!   to decrypt rather than yielding attacker-controlled token values.
//! - **NOT at-rest confidentiality against a local reader**: a local process
//!   running as the same user (or any process that can read the file plus the
//!   public inputs) can recover the plaintext. Do not describe this store as
//!   protecting tokens from local malware.
//!
//! ## Why the key is not bound to an OS secret (DPAPI / Keychain / keyring)
//!
//! Considered and deliberately not done (2026-07-17 credential-hygiene plan,
//! Task 7):
//! - Confidentiality against a *same-user* local process — the gap called out
//!   above — is not attainable via DPAPI or a keyring either: any same-user
//!   process can call `CryptUnprotectData` / read the keyring entry, so the
//!   binding would not close the stated gap, only the off-box one.
//! - The OS keychain's unreliability on Windows is the documented reason this
//!   module exists; putting the *decryption key* for the runner's identity
//!   credential behind it would reintroduce that failure mode into the
//!   boot-critical path (and headless Linux runners often have no
//!   secret-service at all).
//! - Re-keying live stores in the field risks stranding paired runners.
//!
//! The compensating control is filesystem-level: every write of the store is
//! owner-only (`0600` / protected owner-only DACL via [`crate::fs_perms`]),
//! which is what actually stops OTHER local users reading it.
//!
//! Additional properties:
//! - Storage file is placed in the app's data directory
//! - The file is written owner-only (Unix `0600`; Windows protected DACL)
//!
//! ## Stored value format (Phase 3 Unified Devices Registry)
//!
//! Prior to Phase 3, the `access_token` slot held a `qontinui_runner_<random>`
//! opaque bearer string minted by the web backend's
//! `POST /api/v1/runners/tokens`. Phase 3 retires that endpoint in favour of
//! `qontinui_profile device pair`, which mints a coord-issued device-token
//! JWT and OVERWRITES the same `access_token` slot. The slot name is
//! preserved so existing readers (`AuthManager::get_access_token`) need no
//! changes. The `refresh_token` slot is unused under the new flow (the
//! device JWT lifecycle is coord-managed); pair writes an empty string.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Service identifier for the storage
const SERVICE_NAME: &str = "com.qontinui.runner";

/// Key prefix for the per-tenant device-JWT slots (session-scoped
/// multi-tenant Phase 1, plan 2026-07-02). A slot for tenant
/// `aaaa…` is stored under the map key `device_jwt:aaaa…`.
const TENANT_DEVICE_JWT_PREFIX: &str = "device_jwt:";

/// Storage file name
const STORAGE_FILE: &str = "auth_tokens.enc";

/// Stored token data structure
///
/// ## Token slots (Phase 5 unified-Cognito-identity)
///
/// - `access_token` / `refresh_token`: the **coord device-token JWT** slot.
///   `access_token` holds the coord-minted device JWT (read by the WS relay
///   via `AuthManager::get_access_token`); `refresh_token` is unused for the
///   device-JWT flow (coord owns its lifecycle).
/// - `oauth_access_token` / `oauth_id_token` / `oauth_refresh_token`: the
///   **Cognito user-token** slots, written by the RFC-8252 PKCE sign-in
///   (`cognito::store_cognito_tokens`). These are kept distinct from the
///   device-JWT slot so the relay keeps using the device JWT while
///   user-facing calls (and the device→user re-bind) use the Cognito token.
///   `oauth_expires_at` is the absolute unix-seconds expiry of the Cognito
///   access token, used by the refresher to decide staleness.
///
/// All new fields carry `#[serde(default)]` so a pre-Phase-5 `auth_tokens.enc`
/// (only the first three keys) still deserializes.
#[derive(Debug, Serialize, Deserialize, Default)]
struct StoredTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    device_id: Option<String>,
    #[serde(default)]
    oauth_access_token: Option<String>,
    #[serde(default)]
    oauth_id_token: Option<String>,
    #[serde(default)]
    oauth_refresh_token: Option<String>,
    #[serde(default)]
    oauth_expires_at: Option<i64>,
    /// Persisted coord-mcp loopback proxy nonces: nonce → workdir it was
    /// provisioned into (plan 2026-06-13 Phase 3b). The nonce is a
    /// **loopback-only** proxy key (127.0.0.1) — NOT a bearer — so persisting it
    /// at rest leaks nothing exploitable off-box, and it is the only way an
    /// already-running agent's already-read `.mcp.json` keeps validating across a
    /// runner rebuild/restart (the MCP client never re-reads the file). Carries
    /// `#[serde(default)]` so a pre-Phase-3b `.enc` deserializes.
    #[serde(default)]
    coord_mcp_nonces: std::collections::HashMap<String, String>,
    /// Per-machine API key for the dev-environment capture agent
    /// (`mk_<token>`), minted ONCE by the qontinui-web enroll endpoint
    /// (`POST /api/v1/devenv/agent/enroll`). Sent as the `X-Machine-Key`
    /// header on every config push. This is a bearer credential (NOT a
    /// loopback-only nonce), so it lives in the encrypted store alongside the
    /// device JWT. `#[serde(default)]` keeps a pre-env-agent `.enc`
    /// deserializing.
    #[serde(default)]
    agent_machine_key: Option<String>,
    /// Per-tenant coord device-JWT slots (session-scoped multi-tenant
    /// plan 2026-07-02; shipped un-gated as of Phase 8a). Keys are
    /// `device_jwt:<tenant_id>`; values are the coord-minted device JWTs
    /// for that tenant binding, written by `pair::persist_pairing` and
    /// kept fresh by the refresher's per-slot pass. The legacy
    /// `access_token` slot is COMPLETELY UNTOUCHED by these — it keeps
    /// holding the DEFAULT binding's JWT so every unmodified consumer keeps
    /// working during the compat window. `#[serde(default)]` keeps
    /// every pre-Phase-1 `.enc` deserializing. BTreeMap so enumeration is
    /// deterministic.
    ///
    /// ORTHOGONAL to `device_machine_key` below: these N slots are the
    /// per-tenant device JWTs the refresher's per-slot pass keeps fresh,
    /// whereas the machine key is the DEVICE-scoped cold-start credential
    /// used to re-mint the DEFAULT binding's JWT (`access_token` slot) when
    /// no device JWT exists at all. Both survive the credential-model merge.
    #[serde(default)]
    tenant_device_jwts: std::collections::BTreeMap<String, String>,
    /// Long-lived, device-bound machine key (`dmk_<token>`) minted by the
    /// qontinui-web backend (mirror of `agent_machine_key`, but for the
    /// DEVICE rather than the env-capture agent). Sent as the
    /// `X-Device-Machine-Key` header to web's
    /// `POST /api/v1/devices/{device_id}/machine-credential/exchange`, which
    /// exchanges it for a fresh device JWT with NO user session — the final
    /// cold-start recovery path for a runner offline longer than BOTH the
    /// device-JWT TTL and the Cognito refresh-token window (plan
    /// 2026-07-02-runner-device-machine-key-cold-start Phase 4). This is a
    /// higher-privilege bearer than `agent_machine_key`, so a full sign-out
    /// (`clear_tokens`) MUST wipe it while the autonomy-preserving
    /// `clear_interactive_session` PRESERVES it. `#[serde(default)]` keeps a
    /// pre-dmk `.enc` deserializing.
    ///
    /// DEVICE-scoped (one per device), unlike `tenant_device_jwts` above
    /// which is one JWT per bound tenant. A successful machine-key exchange
    /// seeds the DEFAULT binding's JWT (the `access_token` slot); the
    /// per-tenant slots are refreshed independently by the per-slot pass.
    #[serde(default)]
    device_machine_key: Option<String>,
    /// Whether the operator has explicitly logged out of the INTERACTIVE
    /// session while leaving the autonomy credentials in place.
    ///
    /// This exists because "signed in?" is otherwise derived purely from
    /// credential PRESENCE (see `AuthManager::has_local_signed_in_session`),
    /// and the autonomy-preserving logout
    /// ([`Self::clear_interactive_session`]) deliberately keeps the Cognito
    /// session so the device-JWT refresher can re-mint. Without this flag that
    /// logout would not stick: the next status re-check would see the retained
    /// `oauth_refresh_token` and flip the UI straight back to signed-in.
    ///
    /// Set by both logout paths. Cleared ONLY by an EXPLICIT interactive
    /// credential acquisition, of which there are exactly three:
    ///
    ///   1. `commands::auth::finalize_signed_in` — Cognito Hosted-UI PKCE and
    ///      password sign-in (both converge there),
    ///   2. `commands::web_integration::redeem_pair_code` — pair-code redeem
    ///      from Settings (also allowlisted over the UI-Bridge HTTP surface),
    ///   3. `qontinui_profile device pair` — the CLI pairing subcommand.
    ///
    /// Each of those clears it AFTER the pairing has actually been persisted,
    /// so a sign-in that fails partway cannot un-logout the operator.
    ///
    /// Notably NOT cleared by [`Self::store_tokens`], [`Self::store_oauth_tokens`]
    /// or `pair::persist_pairing`: the background device-JWT refresher writes
    /// those slots on every refresh cycle (`mcp::device_jwt_refresher`), so
    /// clearing it there would silently un-logout the operator minutes after
    /// they logged out — the exact invariant this marker exists to protect.
    ///
    /// DOWNGRADE HAZARD: this key is unknown to any runner build that predates
    /// it. An old binary reading a new `.enc` drops the key on its next write
    /// (serde has no `flatten`-capture here), so downgrading a logged-out
    /// install silently un-logs-out the operator. Re-running the logout on the
    /// old build is not possible (it has no marker); the recovery is to upgrade
    /// again and log out, or use the full sign-out which wipes the credentials
    /// themselves.
    #[serde(default)]
    interactive_signed_out: bool,
}

/// Write posture for a read-modify-write over a possibly-unreadable store.
///
/// Every credential writer here is read-modify-write: load the whole struct,
/// change one slot, save it back. When the store is present-but-UNREADABLE the
/// two postures diverge:
///
/// - [`WriteMode::Merge`] REFUSES (`Err`). The posture for every BACKGROUND
///   writer (the device-JWT refresher's per-slot / oauth / legacy passes): a
///   blank rewrite there would silently destroy sibling credential slots
///   (including the Cognito `oauth_refresh_token` that keeps autonomy alive)
///   while reporting success.
/// - [`WriteMode::Fresh`] starts from a blank [`StoredTokens`] instead of
///   refusing. Reserved for the EXPLICIT, user/agent-initiated
///   credential-acquisition writes (Cognito sign-in, pair-code redeem, CLI
///   `device pair`). There the operator is deliberately re-establishing
///   credentials and the old encrypted bytes are already cryptographically
///   dead on this machine — the AES key derives from hostname + username, so a
///   machine rename / disk move / re-image produces an undecryptable `.enc`
///   that ONLY a fresh sign-in can heal. Refusing there dead-ended the operator
///   at the LoginScreen with no in-app way back. A BACKGROUND refresh must never
///   use this.
///
/// The two only differ on the present-but-unreadable path; on a readable store
/// (or a genuinely absent one) `Fresh` behaves identically to `Merge`, so an
/// explicit-path write never discards slots it could have preserved — it only
/// discards bytes that were already unreadable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WriteMode {
    /// Refuse over a present-but-unreadable store (background writers).
    Merge,
    /// Overwrite a present-but-unreadable store from blank (explicit
    /// credential acquisition only).
    Fresh,
}

/// Outcome of reading a credential slot out of the encrypted store.
///
/// NO-DOWNGRADE: `Absent` ("definitively nothing stored") and `Unreadable`
/// ("the store exists but we could not decrypt/parse it") are DIFFERENT facts.
/// Flattening them into a single "no token" is what let a locked or corrupt
/// store present as an unpaired / signed-out runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredTokenRead {
    Present(String),
    Absent,
    Unreadable(String),
}

/// Encrypted file-based storage manager.
///
/// Provides encrypted storage for authentication tokens using AES-256-GCM.
/// The encryption key is derived from machine-specific identifiers to BIND
/// tokens to this machine/user. See the module docs for the honest guarantee:
/// this is same-machine binding + tamper evidence, NOT confidentiality
/// against a local same-user reader.
pub struct SecureStorage {
    storage_path: PathBuf,
}

impl SecureStorage {
    /// Creates a new SecureStorage instance.
    ///
    /// The storage file will be created in the app's data directory.
    pub fn new() -> Result<Self> {
        let data_dir = std::env::var("QONTINUI_SECURE_STORAGE_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::data_local_dir().map(|d| d.join(SERVICE_NAME)))
            .context("Failed to get data directory")?;

        // Ensure directory exists
        fs::create_dir_all(&data_dir).context("Failed to create data directory")?;

        let storage_path = data_dir.join(STORAGE_FILE);
        debug!("SecureStorage initialized at: {:?}", storage_path);

        let storage = Self { storage_path };
        // Best-effort boot sweep of crash-orphaned atomic-write temp files
        // (older than 5 min so we never race a live writer). Never fatal.
        storage.sweep_stale_temp_files(std::time::Duration::from_secs(5 * 60));
        Ok(storage)
    }

    /// Creates a SecureStorage instance with a custom storage path.
    ///
    /// This is primarily used for testing to ensure test isolation.
    #[cfg(test)]
    pub fn with_path(storage_path: PathBuf) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).context("Failed to create data directory")?;
        }
        Ok(Self { storage_path })
    }

    /// Derives an encryption key from machine-specific identifiers.
    ///
    /// Uses hostname and a salt to create a deterministic key that's
    /// unique to this machine.
    fn derive_key(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();

        // Add machine-specific identifiers
        if let Ok(hostname) = hostname::get() {
            hasher.update(hostname.to_string_lossy().as_bytes());
        }

        // Add service name as salt
        hasher.update(SERVICE_NAME.as_bytes());

        // Add a static component for additional entropy
        hasher.update(b"qontinui-runner-secure-storage-v1");

        // Get username if available
        if let Ok(user) = std::env::var("USERNAME") {
            hasher.update(user.as_bytes());
        } else if let Ok(user) = std::env::var("USER") {
            hasher.update(user.as_bytes());
        }

        let result = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&result);
        key
    }

    /// Encrypts data using AES-256-GCM.
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key = self.derive_key();
        let cipher = Aes256Gcm::new_from_slice(&key).context("Failed to create cipher")?;

        // Generate random nonce
        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        // Prepend nonce to ciphertext
        let mut result = nonce_bytes.to_vec();
        result.extend(ciphertext);

        Ok(result)
    }

    /// Decrypts data using AES-256-GCM.
    fn decrypt(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        if encrypted.len() < 12 {
            anyhow::bail!("Invalid encrypted data: too short");
        }

        let key = self.derive_key();
        let cipher = Aes256Gcm::new_from_slice(&key).context("Failed to create cipher")?;

        // Extract nonce and ciphertext
        let (nonce_bytes, ciphertext) = encrypted.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        // Decrypt
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        Ok(plaintext)
    }

    /// Loads tokens from encrypted storage.
    fn load_tokens(&self) -> Result<StoredTokens> {
        if !self.storage_path.exists() {
            debug!("Storage file does not exist, returning empty tokens");
            return Ok(StoredTokens::default());
        }

        let encrypted = fs::read(&self.storage_path).context("Failed to read storage file")?;

        let decrypted = self.decrypt(&encrypted)?;

        let tokens: StoredTokens =
            serde_json::from_slice(&decrypted).context("Failed to parse stored tokens")?;

        debug!("Loaded tokens from secure storage");
        Ok(tokens)
    }

    /// Saves tokens to encrypted storage — ATOMICALLY, then owner-only.
    ///
    /// This must never be a plain `fs::write`. That is truncate-then-write, so
    /// a reader racing a writer can observe a zero-length or half-written file;
    /// `decrypt` then bails with "Invalid encrypted data: too short" and the
    /// caller sees a corrupt store that never existed. The device-JWT refresher
    /// rewrites this file roughly every 5 minutes, so that race is not
    /// theoretical, and a crash or power loss mid-write makes it DURABLE.
    ///
    /// That matters more since [`Self::is_interactive_signed_out`] fails CLOSED
    /// on an unreadable-but-present store: a torn write would be read as "the
    /// operator logged out" and bounce them to the LoginScreen — a brand-new
    /// automatic logout manufactured by the guard against automatic logouts.
    /// The fail-closed posture is only sound because this write is atomic.
    ///
    /// `atomic_write` is the same helper `settings.rs` and `claude_accounts.rs`
    /// use (temp file → fsync → rename; `MoveFileExW` with
    /// `MOVEFILE_REPLACE_EXISTING` on Windows, so the swap is atomic on NTFS
    /// just like POSIX rename).
    ///
    /// The file is ALSO owner-only (Unix `0600`, Windows protected owner-only
    /// DACL), and specifically owner-only *from the moment it exists* —
    /// `atomic_write_owner_only` hardens the temp file before the rename.
    ///
    /// Both properties are required and the ordering is not interchangeable:
    ///
    /// - `fs_perms::write_owner_only` alone is a plain truncate-then-write, so
    ///   it would trade the torn-write bug above for the permission fix.
    /// - `atomic_write` followed by a permission fix leaves the ciphertext
    ///   world-readable for the whole write (the temp is created with the
    ///   default umask), and — because the rename installs a NEW inode — a
    ///   failure of that post-hoc fix would leave a previously-`0600` store at
    ///   `0644`, silently de-hardening a file that was already safe.
    ///
    /// Hardening failure therefore FAILS the write rather than warning: the
    /// alternative is publishing a readable credential store while reporting
    /// success. Note the AES-GCM key derives from hostname / service / username
    /// — all inputs another LOCAL user can reproduce — so "it's encrypted" is
    /// not a substitute for the file mode against the local-reader threat this
    /// hardening exists for.
    fn save_tokens(&self, tokens: &StoredTokens) -> Result<()> {
        let json = serde_json::to_vec(tokens).context("Failed to serialize tokens")?;

        let encrypted = self.encrypt(&json)?;

        crate::fs_atomic::atomic_write_owner_only(&self.storage_path, &encrypted)
            .context("Failed to write storage file")?;

        debug!("Saved tokens to secure storage");
        Ok(())
    }

    /// `true` iff the store file EXISTS but cannot be read/decrypted/parsed.
    ///
    /// The discriminator between "first run" (absent → a blank store is the
    /// correct starting point) and "corrupt / wrong-machine key" (present but
    /// unreadable → every slot is unknown, and pretending it is blank destroys
    /// credentials). [`Self::load_tokens`] returns `Ok(default)` for the
    /// former and `Err` only for the latter.
    pub fn is_present_but_unreadable(&self) -> bool {
        self.store_file_exists() && self.load_tokens().is_err()
    }

    /// Loads the current store for a read-modify-write, REFUSING to fabricate a
    /// blank one over a present-but-unreadable file ([`WriteMode::Merge`]).
    ///
    /// Every writer here is read-modify-write: it loads the whole struct,
    /// changes one slot, and saves it back. `load_tokens().unwrap_or_default()`
    /// therefore turned any transient or permanent read failure into a silent
    /// wipe of EVERY OTHER SLOT — including `oauth_refresh_token`, the only
    /// credential the device-JWT refresher can self-recover from — while
    /// returning `Ok(())`. The worst shape was `clear_interactive_signed_out`
    /// on a successful sign-in destroying all credentials and reporting
    /// success.
    ///
    /// Mirrors the posture `AuthManager::get_access_token` already takes: a
    /// malformed-but-present store is never overwritten, so the corruption is
    /// surfaced (and left intact for forensics) rather than masked.
    ///
    /// The EXPLICIT credential-acquisition writers instead call
    /// [`Self::load_tokens_for_write_mode`] with [`WriteMode::Fresh`], which
    /// starts from blank rather than refusing — see that method and `WriteMode`.
    fn load_tokens_for_write(&self) -> Result<StoredTokens> {
        self.load_tokens_for_write_mode(WriteMode::Merge)
    }

    /// [`Self::load_tokens_for_write`] with an explicit posture.
    ///
    /// On a present-but-unreadable store, [`WriteMode::Merge`] refuses (`Err`)
    /// and [`WriteMode::Fresh`] returns a blank [`StoredTokens`] so the explicit
    /// caller can rewrite the (cryptographically-dead) store from scratch. On a
    /// readable or genuinely-absent store the two are identical.
    fn load_tokens_for_write_mode(&self, mode: WriteMode) -> Result<StoredTokens> {
        match self.load_tokens() {
            Ok(tokens) => Ok(tokens),
            Err(e) if self.store_file_exists() => match mode {
                WriteMode::Merge => Err(anyhow::anyhow!(
                    "secure storage at {} is present but unreadable ({e}); refusing to overwrite \
                     it — a blank rewrite would destroy every other credential slot (including \
                     the Cognito refresh token that keeps autonomous sessions running). An \
                     explicit sign-in / pairing may overwrite it; a background refresh may not.",
                    self.storage_path.display()
                )),
                WriteMode::Fresh => {
                    warn!(
                        "secure storage at {} is present but unreadable ({e}); an EXPLICIT \
                         credential-acquisition write is rebuilding it from a blank store. The \
                         prior encrypted bytes were undecryptable on this machine (the AES key \
                         derives from hostname + username, so a machine rename / disk move \
                         produces exactly this) and are discarded — a background refresh would \
                         have refused here instead.",
                        self.storage_path.display()
                    );
                    Ok(StoredTokens::default())
                }
            },
            // File genuinely absent (raced away between the two checks): a
            // fresh store is the correct starting point.
            Err(_) => Ok(StoredTokens::default()),
        }
    }

    /// Stores both access and refresh tokens. Background/default posture:
    /// refuses over a present-but-unreadable store ([`WriteMode::Merge`]).
    pub fn store_tokens(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        self.store_tokens_mode(access_token, refresh_token, WriteMode::Merge)
    }

    /// Explicit-acquisition variant of [`Self::store_tokens`]: overwrites a
    /// present-but-unreadable store from blank rather than refusing. Only the
    /// explicit pairing path (`pair::persist_pairing`) calls this.
    pub fn store_tokens_fresh(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        self.store_tokens_mode(access_token, refresh_token, WriteMode::Fresh)
    }

    fn store_tokens_mode(
        &self,
        access_token: &str,
        refresh_token: &str,
        mode: WriteMode,
    ) -> Result<()> {
        let mut tokens = self.load_tokens_for_write_mode(mode)?;
        tokens.access_token = Some(access_token.to_string());
        tokens.refresh_token = Some(refresh_token.to_string());
        self.save_tokens(&tokens)?;
        info!("Tokens stored in secure file storage");
        Ok(())
    }

    /// Retrieves the access token.
    pub fn get_access_token(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .access_token
            .ok_or_else(|| anyhow::anyhow!("Access token not found in storage"))
    }

    /// Retrieves the refresh token.
    pub fn get_refresh_token(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .refresh_token
            .ok_or_else(|| anyhow::anyhow!("Refresh token not found in storage"))
    }

    /// Clears all tokens from storage (device-JWT slot AND the Cognito
    /// user-token slots). `device_id` is preserved — it is a stable local
    /// identifier, not a credential.
    ///
    /// This is the FULL wipe — it destroys `oauth_refresh_token`, the only
    /// credential the device-JWT refresher can self-recover from. After this
    /// call the runner's autonomous terminal sessions can no longer re-mint a
    /// device JWT and will park until an interactive re-login. Use
    /// [`Self::clear_interactive_session`] for a default logout that should
    /// keep autonomy alive.
    ///
    /// The ONE writer that deliberately keeps `unwrap_or_default()` instead of
    /// [`Self::load_tokens_for_write`]: destroying every credential slot IS the
    /// operation, so an unreadable store must not block it. `device_id` is the
    /// only thing lost relative to a readable run, and it is a regenerable local
    /// identifier, not a credential. (Every other writer refuses, because for
    /// them a blank rewrite is silent credential loss, not the point.)
    pub fn clear_tokens(&self) -> Result<()> {
        let mut tokens = self.load_tokens().unwrap_or_default();
        tokens.access_token = None;
        tokens.refresh_token = None;
        tokens.oauth_access_token = None;
        tokens.oauth_id_token = None;
        tokens.oauth_refresh_token = None;
        tokens.oauth_expires_at = None;
        // Full sign-out also destroys every per-tenant device-JWT slot —
        // they are bearer credentials just like the default-binding JWT.
        // (`clear_interactive_session` deliberately preserves them, same as
        // it preserves the other autonomy credentials.)
        tokens.tenant_device_jwts.clear();
        // Full sign-out & stop-autonomy: drop the long-lived autonomy
        // credentials too (both the device machine key and the env-agent
        // machine key). `clear_interactive_session` below deliberately does
        // NOT touch these, so autonomy survives a default logout.
        tokens.device_machine_key = None;
        tokens.agent_machine_key = None;
        // Belt-and-braces: with every credential gone the presence check
        // already reports signed-out, but keep the flag consistent so a
        // partially-failed wipe can't leave the UI showing signed-in.
        tokens.interactive_signed_out = true;
        self.save_tokens(&tokens)?;
        info!("Tokens cleared from secure file storage");
        Ok(())
    }

    /// Clears ONLY the device-JWT pair (`access_token` / `refresh_token`),
    /// PRESERVING the four Cognito (`oauth_*`) slots, the per-tenant
    /// device-JWT slots, and `device_id`.
    ///
    /// This is the autonomy-preserving clear used by a default logout: the
    /// device JWT is dropped, but the long-lived `oauth_refresh_token` (and the
    /// rest of the Cognito session) is left intact so the supervised device-JWT
    /// refresher can immediately re-mint a fresh device JWT and keep the
    /// runner's autonomous terminal sessions running. Contrast with
    /// [`Self::clear_tokens`], which wipes everything.
    pub fn clear_interactive_session(&self) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.access_token = None;
        tokens.refresh_token = None;
        // oauth_* slots, device_id, and the long-lived autonomy machine keys
        // (device_machine_key / agent_machine_key) are intentionally preserved
        // so the refresher can re-mint a device JWT after a default logout.
        //
        // Because those credentials survive, the presence-based signed-in check
        // would otherwise report the operator as still signed in on the next
        // status re-check. Mark the interactive session as deliberately ended so
        // the logout sticks while autonomy keeps running.
        tokens.interactive_signed_out = true;
        self.save_tokens(&tokens)?;
        info!(
            "Device-JWT pair cleared from secure file storage (Cognito session preserved for \
             autonomous refresh)"
        );
        Ok(())
    }

    /// Whether the operator explicitly ended the interactive session.
    ///
    /// FAIL-CLOSED on an unreadable-but-PRESENT store. `load_tokens()` returns
    /// `Ok(default)` when the file is absent (first run — genuinely never
    /// logged out, so `false` is correct), and `Err` only when a file that DOES
    /// exist cannot be read/decrypted/parsed. That second case must report
    /// `true`, because the marker is not the only thing an unreadable `.enc`
    /// hides: `AuthManager::get_access_token` falls back to the OS KEYCHAIN,
    /// which the device-JWT refresher re-populates right after the
    /// autonomy-preserving logout. Reporting `false` there would let the
    /// presence check read that keychain token and silently sign the operator
    /// back in — resurrecting a session they explicitly ended. The `.enc` key
    /// derives from hostname + USERNAME, so "present but undecryptable" is a
    /// real, already-handled scenario (see the `store_present` warn! in
    /// `AuthManager::get_access_token`), not a hypothetical.
    ///
    /// Why failing closed is SAFE here (and not the automatic logout this whole
    /// change removes): the read is PURE. Reporting signed-out tears down no
    /// live session — the autonomous daemons keep running off the device JWT the
    /// refresher already holds (in memory and in the OS keychain), so nothing is
    /// revoked. The token is NOT necessarily dead: in the hostname-change case
    /// the keychain copy is still perfectly usable (the keychain is keyed per
    /// OS-user, not by the `.enc`'s hostname-derived key). That is precisely why
    /// we must NOT fail open — trusting that still-usable keychain token in this
    /// read would silently resurrect a session the operator explicitly ended,
    /// indistinguishable from genuine corruption. The only cost of failing
    /// closed is a re-sign-in prompt, which is recoverable; the cost of failing
    /// open is an unrevocable, invisible un-logout.
    ///
    /// Failing closed treats an unreadable store as a REAL signal, which is only
    /// justified because [`Self::save_tokens`] is ATOMIC (temp file → rename):
    /// a partial/torn write is impossible, so an unreadable store is durable
    /// corruption, never a transient artifact of a reader racing the refresher's
    /// ~5-minute rewrite. Under the old plain `fs::write` (truncate-then-write)
    /// that race could momentarily expose a zero-length file and this branch
    /// would have manufactured a spurious logout. If `save_tokens` ever stops
    /// being atomic, this must stop failing closed.
    pub fn is_interactive_signed_out(&self) -> bool {
        match self.load_tokens() {
            Ok(t) => t.interactive_signed_out,
            Err(e) => {
                if self.store_file_exists() {
                    warn!(
                        "Secure storage present but unreadable ({e}) — treating the interactive \
                         session as SIGNED OUT (fail-closed) so a keychain-backed credential \
                         cannot resurrect an ended session. Sign in again to repair the store."
                    );
                    true
                } else {
                    // No store file at all: nothing was ever logged out.
                    debug!("No secure-storage file ({e}) — no interactive sign-out recorded");
                    false
                }
            }
        }
    }

    /// Clears the interactive sign-out marker. Called ONLY by the three
    /// explicit interactive credential-acquisition paths (Cognito sign-in,
    /// pair-code redeem, CLI `device pair`) once the acquisition has actually
    /// been persisted — see the `interactive_signed_out` field docs.
    pub fn clear_interactive_signed_out(&self) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.interactive_signed_out = false;
        self.save_tokens(&tokens)?;
        debug!("Interactive sign-out marker cleared (user signed back in)");
        Ok(())
    }

    /// Stores the device ID. Background/default posture: refuses over a
    /// present-but-unreadable store.
    pub fn store_device_id(&self, device_id: &str) -> Result<()> {
        self.store_device_id_mode(device_id, WriteMode::Merge)
    }

    /// Explicit-acquisition variant of [`Self::store_device_id`]: overwrites a
    /// present-but-unreadable store from blank rather than refusing. Called on
    /// the explicit pairing path (`pair::persist_pairing`).
    pub fn store_device_id_fresh(&self, device_id: &str) -> Result<()> {
        self.store_device_id_mode(device_id, WriteMode::Fresh)
    }

    fn store_device_id_mode(&self, device_id: &str, mode: WriteMode) -> Result<()> {
        let mut tokens = self.load_tokens_for_write_mode(mode)?;
        tokens.device_id = Some(device_id.to_string());
        self.save_tokens(&tokens)?;
        info!("Device ID stored in secure file storage: {}", device_id);
        Ok(())
    }

    /// Retrieves the device ID.
    pub fn get_device_id(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .device_id
            .ok_or_else(|| anyhow::anyhow!("Device ID not found in storage"))
    }

    /// Returns whether the encrypted storage file is present on disk.
    ///
    /// Distinguishes "no store yet" (first-run / never paired) from "store
    /// present but unreadable" (a `load_tokens()` parse/decrypt failure on an
    /// existing file). Callers use this to avoid masking a malformed-but-present
    /// store: a parse failure with the file PRESENT must not trigger a keychain
    /// re-migration that overwrites the `.enc` file. See
    /// `AuthManager::get_access_token`.
    pub fn store_file_exists(&self) -> bool {
        self.storage_path.exists()
    }

    /// Tri-state read of the device-JWT (`access_token`) slot.
    ///
    /// [`Self::get_access_token`] returns `Err` for BOTH "nothing was ever
    /// stored" and "the store is present but undecryptable", which is exactly
    /// the collapse of "unknown" into "denied" that made an unreadable store
    /// render as "this runner was never paired". This read keeps them apart.
    pub fn read_access_token(&self) -> StoredTokenRead {
        if !self.store_file_exists() {
            return StoredTokenRead::Absent;
        }
        match self.load_tokens() {
            Ok(tokens) => match tokens.access_token.filter(|t| !t.trim().is_empty()) {
                Some(t) => StoredTokenRead::Present(t),
                None => StoredTokenRead::Absent,
            },
            Err(e) => StoredTokenRead::Unreadable(e.to_string()),
        }
    }

    /// Checks if tokens exist in storage.
    pub fn has_tokens(&self) -> bool {
        match self.load_tokens() {
            Ok(tokens) => tokens.access_token.is_some() && tokens.refresh_token.is_some(),
            Err(_) => false,
        }
    }

    /// Stores the Cognito user tokens (Phase 5 unified-Cognito-identity).
    ///
    /// Writes the `oauth_access_token` / `oauth_id_token` /
    /// `oauth_refresh_token` / `oauth_expires_at` slots, leaving the coord
    /// device-JWT (`access_token`) slot untouched.
    pub fn store_oauth_tokens(
        &self,
        access_token: &str,
        id_token: &str,
        refresh_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        self.store_oauth_tokens_mode(
            access_token,
            id_token,
            refresh_token,
            expires_at,
            WriteMode::Merge,
        )
    }

    /// Explicit-acquisition variant of [`Self::store_oauth_tokens`]: overwrites
    /// a present-but-unreadable store from blank rather than refusing. Called
    /// only by `finalize_signed_in` step 2 (the first write of an interactive
    /// Cognito sign-in), so it can heal an undecryptable `.enc` the operator is
    /// deliberately re-authenticating over. The background refresher's Cognito
    /// write uses the plain (Merge) method.
    pub fn store_oauth_tokens_fresh(
        &self,
        access_token: &str,
        id_token: &str,
        refresh_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        self.store_oauth_tokens_mode(
            access_token,
            id_token,
            refresh_token,
            expires_at,
            WriteMode::Fresh,
        )
    }

    fn store_oauth_tokens_mode(
        &self,
        access_token: &str,
        id_token: &str,
        refresh_token: &str,
        expires_at: i64,
        mode: WriteMode,
    ) -> Result<()> {
        let mut tokens = self.load_tokens_for_write_mode(mode)?;
        tokens.oauth_access_token = Some(access_token.to_string());
        tokens.oauth_id_token = Some(id_token.to_string());
        tokens.oauth_refresh_token = Some(refresh_token.to_string());
        tokens.oauth_expires_at = Some(expires_at);
        self.save_tokens(&tokens)?;
        info!("Cognito (oauth) tokens stored in secure file storage");
        Ok(())
    }

    /// Retrieves the Cognito access token.
    pub fn get_oauth_access_token(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .oauth_access_token
            .ok_or_else(|| anyhow::anyhow!("Cognito access token not found in storage"))
    }

    /// Retrieves the Cognito id token.
    pub fn get_oauth_id_token(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .oauth_id_token
            .ok_or_else(|| anyhow::anyhow!("Cognito id token not found in storage"))
    }

    /// Retrieves the Cognito refresh token.
    pub fn get_oauth_refresh_token(&self) -> Result<String> {
        let tokens = self.load_tokens()?;
        tokens
            .oauth_refresh_token
            .ok_or_else(|| anyhow::anyhow!("Cognito refresh token not found in storage"))
    }

    /// Retrieves the Cognito access-token expiry (absolute unix seconds),
    /// if present.
    pub fn get_oauth_expires_at(&self) -> Option<i64> {
        self.load_tokens().ok().and_then(|t| t.oauth_expires_at)
    }

    /// Clears only the Cognito (oauth) token slots, leaving the device-JWT
    /// slot intact. Used on Cognito sign-out.
    pub fn clear_oauth_tokens(&self) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.oauth_access_token = None;
        tokens.oauth_id_token = None;
        tokens.oauth_refresh_token = None;
        tokens.oauth_expires_at = None;
        self.save_tokens(&tokens)?;
        info!("Cognito (oauth) tokens cleared from secure file storage");
        Ok(())
    }

    /// Persist the full coord-mcp loopback proxy nonce map (nonce → workdir),
    /// replacing any prior persisted set (plan 2026-06-13 Phase 3b). Called by
    /// the in-memory `PROXY_NONCES` registry on every mint/eviction so the
    /// durable copy tracks the live set. Best-effort: a write failure is
    /// surfaced to the caller, which logs and continues (the in-memory map
    /// remains authoritative for this process lifetime).
    pub fn store_coord_mcp_nonces(
        &self,
        nonces: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.coord_mcp_nonces = nonces.clone();
        self.save_tokens(&tokens)?;
        Ok(())
    }

    /// Load the persisted coord-mcp loopback proxy nonce map (nonce → workdir).
    /// Returns an empty map when the store is absent / unreadable / pre-Phase-3b
    /// — a missing nonce simply 401s and the next provisioning re-mints, so an
    /// empty restore is the safe default (today's in-memory-only behavior).
    pub fn load_coord_mcp_nonces(&self) -> std::collections::HashMap<String, String> {
        self.load_tokens()
            .map(|t| t.coord_mcp_nonces)
            .unwrap_or_default()
    }

    /// Store the dev-environment capture agent's per-machine API key
    /// (`mk_<token>`). Minted ONCE by the enroll endpoint; overwrites any
    /// prior key. Leaves all other slots untouched.
    pub fn store_agent_machine_key(&self, key: &str) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.agent_machine_key = Some(key.to_string());
        self.save_tokens(&tokens)?;
        info!("env-agent machine key stored in secure file storage");
        Ok(())
    }

    /// Retrieve the dev-environment capture agent's per-machine API key, if
    /// present. `Ok(None)` when the machine has never enrolled (vs an `Err`
    /// only on a decrypt/parse failure of an existing store).
    pub fn get_agent_machine_key(&self) -> Result<Option<String>> {
        let tokens = self.load_tokens()?;
        Ok(tokens.agent_machine_key)
    }

    /// Clear the dev-environment capture agent's per-machine API key, leaving
    /// all other slots intact. Used when re-enrolling or unenrolling.
    pub fn clear_agent_machine_key(&self) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.agent_machine_key = None;
        self.save_tokens(&tokens)?;
        info!("env-agent machine key cleared from secure file storage");
        Ok(())
    }

    // ========================================================================
    // Per-tenant device-JWT slots — session-scoped multi-tenant Phase 1
    // (plan 2026-07-02-session-scoped-multi-tenant-device-binding, D4).
    //
    // One slot per tenant binding, keyed `device_jwt:<tenant_id>`. The legacy
    // `access_token` slot is never read or written by these helpers — it keeps
    // holding the DEFAULT binding's JWT. Like the oauth_* slots, the keychain
    // backup is intentionally NOT mirrored here: the encrypted file is the
    // source of truth (the keychain proved unreliable on Windows; see module
    // docs).
    // ========================================================================

    /// Map key for a tenant's device-JWT slot: `device_jwt:<tenant_id>`.
    fn tenant_device_jwt_key(tenant_id: &uuid::Uuid) -> String {
        format!("{TENANT_DEVICE_JWT_PREFIX}{tenant_id}")
    }

    /// Store (or overwrite) the device JWT for one tenant binding. Leaves the
    /// legacy `access_token` slot and every other slot untouched. Background/
    /// default posture: refuses over a present-but-unreadable store.
    pub fn store_tenant_device_jwt(&self, tenant_id: &uuid::Uuid, jwt: &str) -> Result<()> {
        self.store_tenant_device_jwt_mode(tenant_id, jwt, WriteMode::Merge)
    }

    /// Explicit-acquisition variant of [`Self::store_tenant_device_jwt`]:
    /// overwrites a present-but-unreadable store from blank rather than
    /// refusing. This is the FIRST write of `pair::persist_pairing`, so on an
    /// undecryptable `.enc` it heals the store and every subsequent write in the
    /// same explicit pairing sequence then merges over the now-readable store.
    /// The background refresher's per-tenant write uses the plain (Merge) method.
    pub fn store_tenant_device_jwt_fresh(&self, tenant_id: &uuid::Uuid, jwt: &str) -> Result<()> {
        self.store_tenant_device_jwt_mode(tenant_id, jwt, WriteMode::Fresh)
    }

    fn store_tenant_device_jwt_mode(
        &self,
        tenant_id: &uuid::Uuid,
        jwt: &str,
        mode: WriteMode,
    ) -> Result<()> {
        let mut tokens = self.load_tokens_for_write_mode(mode)?;
        tokens
            .tenant_device_jwts
            .insert(Self::tenant_device_jwt_key(tenant_id), jwt.to_string());
        self.save_tokens(&tokens)?;
        info!("Per-tenant device JWT stored for tenant {tenant_id}");
        Ok(())
    }

    /// Retrieve the device JWT for one tenant binding. `Ok(None)` when no
    /// slot exists for that tenant (vs an `Err` only on a decrypt/parse
    /// failure of an existing store).
    pub fn get_tenant_device_jwt(&self, tenant_id: &uuid::Uuid) -> Result<Option<String>> {
        let tokens = self.load_tokens()?;
        Ok(tokens
            .tenant_device_jwts
            .get(&Self::tenant_device_jwt_key(tenant_id))
            .cloned())
    }

    /// Remove one tenant's device-JWT slot, leaving all other slots (incl.
    /// the legacy `access_token`) intact. Idempotent.
    pub fn clear_tenant_device_jwt(&self, tenant_id: &uuid::Uuid) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens
            .tenant_device_jwts
            .remove(&Self::tenant_device_jwt_key(tenant_id));
        self.save_tokens(&tokens)?;
        info!("Per-tenant device JWT cleared for tenant {tenant_id}");
        Ok(())
    }

    /// Enumerate the tenant ids that currently have a device-JWT slot, in
    /// deterministic (BTreeMap key) order. Unreadable stores and malformed
    /// keys yield an empty / filtered list — enumeration is never fatal.
    pub fn list_tenant_device_jwt_tenants(&self) -> Vec<uuid::Uuid> {
        self.load_tokens()
            .map(|t| {
                t.tenant_device_jwts
                    .keys()
                    .filter_map(|k| k.strip_prefix(TENANT_DEVICE_JWT_PREFIX))
                    .filter_map(|s| uuid::Uuid::parse_str(s).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Store the device-bound machine key (`dmk_<token>`). Minted by the
    /// qontinui-web backend (at pairing or via an explicit mint), overwrites
    /// any prior key. Leaves all other slots untouched. Mirror of
    /// [`Self::store_agent_machine_key`].
    pub fn store_device_machine_key(&self, key: &str) -> Result<()> {
        self.store_device_machine_key_mode(key, WriteMode::Merge)
    }

    /// Explicit-acquisition variant of [`Self::store_device_machine_key`]:
    /// overwrites a present-but-unreadable store from blank rather than
    /// refusing. Called (best-effort) on the explicit pairing path
    /// (`pair::persist_pairing`) when the web response auto-minted a `dmk_`.
    pub fn store_device_machine_key_fresh(&self, key: &str) -> Result<()> {
        self.store_device_machine_key_mode(key, WriteMode::Fresh)
    }

    fn store_device_machine_key_mode(&self, key: &str, mode: WriteMode) -> Result<()> {
        let mut tokens = self.load_tokens_for_write_mode(mode)?;
        tokens.device_machine_key = Some(key.to_string());
        self.save_tokens(&tokens)?;
        info!("device machine key stored in secure file storage");
        Ok(())
    }

    /// Retrieve the device-bound machine key, if present. `Ok(None)` when the
    /// device has never been issued a `dmk_` (vs an `Err` only on a
    /// decrypt/parse failure of an existing store). Mirror of
    /// [`Self::get_agent_machine_key`].
    pub fn get_device_machine_key(&self) -> Result<Option<String>> {
        let tokens = self.load_tokens()?;
        Ok(tokens.device_machine_key)
    }

    /// Clear the device-bound machine key, leaving all other slots intact.
    /// Used on revocation / re-issue. Mirror of
    /// [`Self::clear_agent_machine_key`].
    pub fn clear_device_machine_key(&self) -> Result<()> {
        let mut tokens = self.load_tokens_for_write()?;
        tokens.device_machine_key = None;
        self.save_tokens(&tokens)?;
        info!("device machine key cleared from secure file storage");
        Ok(())
    }

    /// Deletes the storage file entirely.
    ///
    /// The reset affordance behind the "your credential store is corrupt" banner
    /// (`commands::auth::reset_credential_store`): when the `.enc` is
    /// present-but-unreadable, deleting it turns the next launch back into a
    /// clean first-run (absent store ⇒ no interactive-sign-out marker, sign-in
    /// writes succeed) so the operator can sign in again from the LoginScreen.
    pub fn delete_storage(&self) -> Result<()> {
        if self.storage_path.exists() {
            fs::remove_file(&self.storage_path).context("Failed to delete storage file")?;
            info!("Secure storage file deleted");
        }
        Ok(())
    }

    /// Best-effort sweep of crash-orphaned `auth_tokens.enc.tmp.<...>` temp
    /// files left next to the store. [`Self::save_tokens`] writes to a temp file
    /// then renames it over the store; a crash or power loss between create and
    /// rename can strand a partial temp. It is harmless (readers only ever open
    /// the real store path) but accumulates, so we unlink temps older than
    /// `min_age` on startup. Never fatal — every IO error is swallowed so a
    /// sweep failure can't block runner boot.
    fn sweep_stale_temp_files(&self, min_age: std::time::Duration) {
        let Some(dir) = self.storage_path.parent() else {
            return;
        };
        // Derive the prefix from the ACTUAL store file name (not the
        // `STORAGE_FILE` constant): `atomic_write` names temps
        // `<store-file-name>.tmp.<...>`, and the store file name is
        // customizable via `with_path` (tests) even though production uses
        // `STORAGE_FILE`.
        let Some(store_name) = self.storage_path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let prefix = format!("{store_name}.tmp.");
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(&prefix) {
                continue;
            }
            // Only unlink temps that are demonstrably old — never race a
            // concurrent writer's in-flight temp (same process or a sibling
            // runner). A temp with an unreadable mtime is left alone.
            let old_enough = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough {
                let path = entry.path();
                match fs::remove_file(&path) {
                    Ok(()) => info!("Swept stale secure-storage temp file: {}", path.display()),
                    Err(e) => debug!("Could not sweep temp file {}: {e}", path.display()),
                }
            }
        }
    }
}

impl Default for SecureStorage {
    fn default() -> Self {
        Self::new().expect("Failed to create SecureStorage")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Create an isolated storage instance for testing.
    /// Each test gets its own unique storage file to avoid test interference.
    fn create_test_storage(test_name: &str) -> SecureStorage {
        let temp_dir = env::temp_dir().join("qontinui_test_storage");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        // Clean up any existing file from previous test runs
        let _ = fs::remove_file(&storage_path);
        SecureStorage::with_path(storage_path).unwrap()
    }

    #[test]
    fn test_encrypt_decrypt() {
        let storage = create_test_storage("test_encrypt_decrypt");
        let plaintext = b"Hello, World!";

        let encrypted = storage.encrypt(plaintext).unwrap();
        let decrypted = storage.decrypt(&encrypted).unwrap();

        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_store_and_retrieve_tokens() {
        let storage = create_test_storage("test_store_and_retrieve_tokens");

        // Store tokens
        storage.store_tokens("test_access", "test_refresh").unwrap();

        // Retrieve tokens
        assert_eq!(storage.get_access_token().unwrap(), "test_access");
        assert_eq!(storage.get_refresh_token().unwrap(), "test_refresh");

        // Clear tokens
        storage.clear_tokens().unwrap();
        assert!(storage.get_access_token().is_err());
        assert!(storage.get_refresh_token().is_err());
    }

    #[test]
    fn test_oauth_tokens_round_trip_and_isolation() {
        let storage = create_test_storage("test_oauth_tokens_round_trip");

        // Device-JWT slot.
        storage.store_tokens("device.jwt.here", "").unwrap();
        // Cognito user-token slots.
        storage
            .store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();

        // Both slots coexist — Cognito write must not clobber the device JWT.
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt.here");
        assert_eq!(storage.get_oauth_access_token().unwrap(), "cog.access");
        assert_eq!(storage.get_oauth_id_token().unwrap(), "cog.id");
        assert_eq!(storage.get_oauth_refresh_token().unwrap(), "cog.refresh");
        assert_eq!(storage.get_oauth_expires_at(), Some(1_700_000_000));

        // Clearing only the oauth slots leaves the device JWT intact.
        storage.clear_oauth_tokens().unwrap();
        assert!(storage.get_oauth_access_token().is_err());
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt.here");
    }

    /// `clear_interactive_session` drops ONLY the device-JWT pair and PRESERVES
    /// the Cognito session (`oauth_refresh_token` et al.) so the refresher can
    /// re-mint autonomously, whereas the full `clear_tokens` wipe destroys the
    /// `oauth_refresh_token` too. This is the core Phase-1 invariant.
    #[test]
    fn test_interactive_clear_preserves_oauth_full_clear_wipes() {
        let storage = create_test_storage("test_interactive_vs_full_clear");

        // Seed both the device-JWT pair and a Cognito session.
        storage
            .store_tokens("device.jwt", "device.refresh")
            .unwrap();
        storage
            .store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();

        // Interactive clear: device JWT gone, Cognito session intact.
        storage.clear_interactive_session().unwrap();
        assert!(
            storage.get_access_token().is_err(),
            "interactive clear must drop the device JWT"
        );
        assert!(
            storage.get_refresh_token().is_err(),
            "interactive clear must drop the device refresh token"
        );
        assert_eq!(
            storage.get_oauth_refresh_token().unwrap(),
            "cog.refresh",
            "interactive clear MUST preserve oauth_refresh_token (the autonomy credential)"
        );
        assert_eq!(storage.get_oauth_access_token().unwrap(), "cog.access");
        assert_eq!(storage.get_oauth_expires_at(), Some(1_700_000_000));

        // Re-seed and prove the full wipe destroys the Cognito session too.
        storage
            .store_oauth_tokens("cog.access2", "cog.id2", "cog.refresh2", 1_700_000_001)
            .unwrap();
        storage.clear_tokens().unwrap();
        assert!(
            storage.get_oauth_refresh_token().is_err(),
            "full clear_tokens MUST wipe oauth_refresh_token"
        );
        assert!(storage.get_oauth_access_token().is_err());
        assert_eq!(storage.get_oauth_expires_at(), None);
    }

    /// The interactive sign-out marker is what makes the autonomy-preserving
    /// logout STICK. Because `clear_interactive_session` deliberately keeps the
    /// Cognito session, the presence-based "signed in?" check would otherwise
    /// see the retained `oauth_refresh_token` and flip the UI back to
    /// signed-in on the next status re-check.
    #[test]
    fn test_interactive_signed_out_marker_lifecycle() {
        let storage = create_test_storage("test_interactive_signed_out_marker");

        // A fresh store has never been logged out. An unreadable/absent store
        // must never invent a logout the operator did not ask for.
        assert!(
            !storage.is_interactive_signed_out(),
            "a fresh store must not report a sign-out"
        );

        storage.store_tokens("device.jwt", "").unwrap();
        storage
            .store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();
        assert!(
            !storage.is_interactive_signed_out(),
            "storing credentials must not set the sign-out marker"
        );

        // Autonomy-preserving logout: marker set, Cognito session preserved.
        storage.clear_interactive_session().unwrap();
        assert!(
            storage.is_interactive_signed_out(),
            "clear_interactive_session must mark the interactive session ended"
        );
        assert_eq!(
            storage.get_oauth_refresh_token().unwrap(),
            "cog.refresh",
            "the autonomy credential must survive the logout that sets the marker"
        );

        // THE LOAD-BEARING CASE: the background device-JWT refresher writes the
        // oauth_* slots on every Cognito refresh cycle. If that write cleared
        // the marker, the refresher would silently un-logout the operator
        // minutes after they logged out. Only an interactive sign-in may clear
        // it (`finalize_signed_in` calls `clear_interactive_signed_out`).
        storage
            .store_oauth_tokens("cog.access2", "cog.id2", "cog.refresh2", 1_700_000_001)
            .unwrap();
        assert!(
            storage.is_interactive_signed_out(),
            "a refresher oauth write MUST NOT clear the interactive sign-out marker"
        );

        // Signing back in is the only thing that ends the logout.
        storage.clear_interactive_signed_out().unwrap();
        assert!(
            !storage.is_interactive_signed_out(),
            "an interactive sign-in must clear the marker"
        );

        // The full stop-autonomy wipe also marks the session signed out, so a
        // partially-failed wipe cannot leave the UI showing signed-in.
        storage.clear_tokens().unwrap();
        assert!(
            storage.is_interactive_signed_out(),
            "clear_tokens must also mark the interactive session ended"
        );
    }

    /// An unreadable-but-PRESENT store must FAIL CLOSED: report signed-out.
    ///
    /// The failure mode this guards: after the autonomy-preserving logout the
    /// device-JWT refresher immediately re-mints and writes the device JWT to
    /// BOTH the `.enc` and the OS keychain. If the `.enc` then becomes
    /// undecryptable (its key derives from hostname + USERNAME, so this is a
    /// real, already-handled scenario), `AuthManager::get_access_token` still
    /// serves the keychain copy. A marker read that returned `false` on that
    /// error would therefore resurrect a session the operator explicitly ended.
    #[test]
    fn test_unreadable_store_reports_signed_out_fail_closed() {
        let storage = create_test_storage("test_unreadable_store_fail_closed");

        // No file at all → genuinely never logged out.
        assert!(
            !storage.store_file_exists(),
            "fresh test storage must have no file"
        );
        assert!(
            !storage.is_interactive_signed_out(),
            "an ABSENT store is a first run, not a logout"
        );

        // Seed a real store, then corrupt it in place (garbage that cannot be
        // AES-GCM decrypted) — the "present but undecryptable" case.
        storage.store_tokens("device.jwt", "").unwrap();
        assert!(storage.store_file_exists());
        fs::write(&storage.storage_path, b"not-a-valid-aes-gcm-ciphertext").unwrap();
        assert!(
            storage.load_tokens().is_err(),
            "the corrupted store must fail to load (test precondition)"
        );

        assert!(
            storage.is_interactive_signed_out(),
            "a PRESENT but unreadable store must fail CLOSED (report signed out) so a \
             keychain-backed credential cannot resurrect an ended session"
        );
    }

    /// A write must never expose a truncated store to a concurrent reader.
    ///
    /// `save_tokens` used a plain `fs::write` (truncate-then-write). The
    /// device-JWT refresher rewrites this file about every 5 minutes, and
    /// `is_interactive_signed_out` FAILS CLOSED on an unreadable-but-present
    /// store — so one torn read would bounce the operator to the LoginScreen:
    /// an automatic logout manufactured by the guard against automatic logouts.
    ///
    /// The reader here asserts the invariant directly: at no instant may the
    /// store be present-and-unreadable while a writer is hammering it.
    #[test]
    fn test_save_tokens_is_atomic_under_a_concurrent_reader() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let storage = Arc::new(create_test_storage("test_save_tokens_atomic"));
        // Seed so the file exists for the whole run — a missing file is a
        // legitimate state and would not prove anything.
        storage.store_tokens("device.jwt.seed", "").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let writer_storage = Arc::clone(&storage);
        let writer_stop = Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            for i in 0..100 {
                writer_storage
                    .store_tokens(&format!("device.jwt.{i}"), "")
                    .expect("write must succeed");
            }
            writer_stop.store(true, Ordering::SeqCst);
        });

        // At least one read happens even if the writer wins the race to finish.
        let mut reads = 0usize;
        loop {
            assert!(
                !storage.is_present_but_unreadable(),
                "a concurrent reader observed a TORN store — save_tokens is not atomic. \
                 This is the failure that turns a routine refresher write into a spurious \
                 logout (is_interactive_signed_out fails closed on an unreadable store)."
            );
            reads += 1;
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
        writer.join().expect("writer thread panicked");
        assert!(reads > 0, "the reader never ran");

        // And the store is intact afterwards.
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt.99");
    }

    /// The atomic write leaves no `.tmp.<nanos>` debris next to the store.
    #[test]
    fn test_save_tokens_leaves_no_temp_file_behind() {
        let storage = create_test_storage("test_save_tokens_no_temp_debris");
        storage.store_tokens("device.jwt", "").unwrap();
        storage.store_tokens("device.jwt.2", "").unwrap();

        let dir = storage.storage_path.parent().unwrap();
        let stem = storage.storage_path.file_name().unwrap().to_string_lossy();
        let debris: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(&format!("{stem}.tmp.")))
            .collect();
        assert!(debris.is_empty(), "temp files left behind: {debris:?}");
    }

    /// A read-modify-write over a present-but-unreadable store must FAIL, not
    /// silently rewrite a blank store.
    ///
    /// Every writer is read-modify-write, so `load_tokens().unwrap_or_default()`
    /// turned an unreadable store into a wipe of EVERY OTHER SLOT — including
    /// `oauth_refresh_token`, the only credential the refresher can self-recover
    /// from — while returning `Ok(())`. The worst shape: signing in calls
    /// `clear_interactive_signed_out`, which would destroy all credentials and
    /// report success.
    #[test]
    fn test_writers_refuse_to_blank_an_unreadable_store() {
        let storage = create_test_storage("test_writers_refuse_unreadable");
        storage.store_tokens("device.jwt", "").unwrap();
        storage
            .store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();

        let corrupt = b"not-a-valid-aes-gcm-ciphertext".to_vec();
        fs::write(&storage.storage_path, &corrupt).unwrap();

        for (name, result) in [
            ("store_tokens", storage.store_tokens("new.jwt", "")),
            (
                "store_oauth_tokens",
                storage.store_oauth_tokens("a", "b", "c", 1),
            ),
            ("store_device_id", storage.store_device_id("dev-1")),
            (
                "clear_interactive_signed_out",
                storage.clear_interactive_signed_out(),
            ),
            (
                "clear_interactive_session",
                storage.clear_interactive_session(),
            ),
            (
                "store_agent_machine_key",
                storage.store_agent_machine_key("mk_x"),
            ),
        ] {
            assert!(
                result.is_err(),
                "{name} must REFUSE to write over a present-but-unreadable store \
                 (a blank rewrite silently destroys every other credential slot)"
            );
        }

        // The corrupt bytes are left byte-identical — the corruption is
        // surfaced, not masked (same posture as AuthManager::get_access_token).
        assert_eq!(
            fs::read(&storage.storage_path).unwrap(),
            corrupt,
            "a refused write must leave the store untouched"
        );

        // …with ONE deliberate exception: the full wipe. Destroying every slot
        // IS the operation there, so an unreadable store must not block it.
        storage
            .clear_tokens()
            .expect("clear_tokens must still succeed");
        assert!(
            storage.is_interactive_signed_out(),
            "the full wipe still records the sign-out"
        );
        assert!(storage.get_oauth_refresh_token().is_err());
    }

    /// The EXPLICIT credential-acquisition writers (`*_fresh`) must OVERWRITE a
    /// present-but-unreadable store instead of refusing — the regression this
    /// branch's finalize/pair path hit: with an undecryptable `.enc` (hostname
    /// change / disk move), every re-auth write refused and the operator could
    /// never sign back in from the LoginScreen. Their Merge siblings still
    /// refuse (proven above); this asserts the Fresh variants heal.
    #[test]
    fn test_fresh_writers_overwrite_an_unreadable_store() {
        let storage = create_test_storage("test_fresh_writers_overwrite_unreadable");
        storage.store_tokens("old.jwt", "").unwrap();

        // Corrupt it in place — present but undecryptable.
        fs::write(&storage.storage_path, b"not-a-valid-aes-gcm-ciphertext").unwrap();
        assert!(storage.load_tokens().is_err(), "test precondition");

        // The Merge variant still refuses (background posture unchanged).
        assert!(
            storage.store_oauth_tokens("a", "b", "c", 1).is_err(),
            "the background (Merge) oauth write must still refuse over an unreadable store"
        );

        // A Cognito sign-in heals with the Fresh oauth write, then merges the
        // pairing writes over the now-readable store — the finalize_signed_in
        // sequence.
        storage
            .store_oauth_tokens_fresh("cog.a", "cog.i", "cog.r", 1_700_000_000)
            .expect("store_oauth_tokens_fresh must overwrite an unreadable store");
        let tenant = uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        storage
            .store_tenant_device_jwt_fresh(&tenant, "jwt.tenant")
            .expect("store_tenant_device_jwt_fresh must succeed");
        storage
            .store_tokens_fresh("device.jwt", "")
            .expect("store_tokens_fresh must succeed");
        storage
            .store_device_id_fresh("dev-1")
            .expect("store_device_id_fresh must succeed");
        storage
            .store_device_machine_key_fresh("dmk_1")
            .expect("store_device_machine_key_fresh must succeed");

        // Every slot written across the healed sequence survives — the Fresh
        // writes discarded ONLY the dead bytes, then merged on the readable store.
        assert_eq!(storage.get_oauth_refresh_token().unwrap(), "cog.r");
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant).unwrap().as_deref(),
            Some("jwt.tenant")
        );
        assert_eq!(storage.get_device_id().unwrap(), "dev-1");
        assert_eq!(
            storage.get_device_machine_key().unwrap().as_deref(),
            Some("dmk_1")
        );
        assert!(
            !storage.is_present_but_unreadable(),
            "the store must be readable after the explicit heal"
        );
    }

    /// On a READABLE store, a Fresh write must behave exactly like a Merge write
    /// — it preserves every sibling slot, discarding nothing. (Fresh only
    /// diverges on the present-but-unreadable path.)
    #[test]
    fn test_fresh_writer_preserves_siblings_on_a_readable_store() {
        let storage = create_test_storage("test_fresh_preserves_readable");
        storage.store_tokens("device.jwt", "").unwrap();
        storage
            .store_oauth_tokens("cog.a", "cog.i", "cog.r", 1_700_000_000)
            .unwrap();

        // A Fresh write over a perfectly readable store must not blank oauth.
        storage.store_tokens_fresh("device.jwt.v2", "").unwrap();
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt.v2");
        assert_eq!(
            storage.get_oauth_refresh_token().unwrap(),
            "cog.r",
            "a Fresh write over a readable store must preserve sibling slots"
        );
    }

    /// The boot sweep unlinks crash-orphaned `*.enc.tmp.<...>` files older than
    /// the age threshold, but leaves the real store and any recent temp alone.
    #[test]
    fn test_sweep_stale_temp_files() {
        let storage = create_test_storage("test_sweep_temps");
        storage.store_tokens("device.jwt", "").unwrap();
        let dir = storage.storage_path.parent().unwrap();
        let stem = storage
            .storage_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        // Two temps matching the real store's `.tmp.` prefix.
        let a = dir.join(format!("{stem}.tmp.111"));
        let b = dir.join(format!("{stem}.tmp.222"));
        fs::write(&a, b"orphaned").unwrap();
        fs::write(&b, b"in-flight").unwrap();

        // A generous threshold spares fresh temps (never race a live writer).
        storage.sweep_stale_temp_files(std::time::Duration::from_secs(3600));
        assert!(a.exists(), "a temp younger than the threshold must survive");
        assert!(b.exists());

        // Let them age past a small threshold (a real elapse, not a 0-second
        // threshold — filesystem mtime resolution can round an mtime slightly
        // ahead of a just-captured `now`, which would spuriously spare a
        // 0-second sweep). Then both qualify and are swept; the real store,
        // which never matches the `.tmp.` prefix, must remain.
        std::thread::sleep(std::time::Duration::from_millis(60));
        storage.sweep_stale_temp_files(std::time::Duration::from_millis(20));
        assert!(!a.exists(), "a temp older than the threshold must be swept");
        assert!(!b.exists());
        assert!(
            storage.storage_path.exists(),
            "the real store must never be swept"
        );
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");
    }

    /// A pre-Phase-5 `StoredTokens` JSON (only the original three keys) must
    /// still deserialize — the new oauth_* fields carry `#[serde(default)]`.
    #[test]
    fn test_legacy_stored_tokens_without_oauth_fields_deserializes() {
        let raw = r#"{"access_token":"a","refresh_token":"r","device_id":"d"}"#;
        let parsed: StoredTokens = serde_json::from_str(raw).expect("legacy shape must decode");
        assert_eq!(parsed.access_token.as_deref(), Some("a"));
        assert!(parsed.oauth_access_token.is_none());
        assert!(parsed.oauth_expires_at.is_none());
        // A store written before the marker existed must decode as NOT
        // signed-out — an upgrade may never invent a logout for an operator
        // whose install predates the flag.
        assert!(
            !parsed.interactive_signed_out,
            "a legacy store must default to NOT interactively signed out"
        );
    }

    #[test]
    fn test_agent_machine_key_round_trip_and_isolation() {
        let storage = create_test_storage("test_agent_machine_key");

        // Absent before any store.
        assert!(storage.get_agent_machine_key().unwrap().is_none());

        // A device JWT in the access_token slot must NOT be clobbered by the
        // machine-key write (slot isolation).
        storage.store_tokens("device.jwt", "").unwrap();
        storage.store_agent_machine_key("mk_abc123").unwrap();

        assert_eq!(
            storage.get_agent_machine_key().unwrap().as_deref(),
            Some("mk_abc123")
        );
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");

        // Clearing only the machine key leaves the device JWT intact.
        storage.clear_agent_machine_key().unwrap();
        assert!(storage.get_agent_machine_key().unwrap().is_none());
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");
    }

    /// Per-tenant device-JWT slots: round-trip + enumeration + legacy-slot
    /// isolation (session-scoped multi-tenant Phase 1). Writing/clearing a
    /// tenant slot must NEVER mutate the legacy `access_token` slot, and
    /// vice versa.
    #[test]
    fn test_tenant_device_jwt_slots_round_trip_and_isolation() {
        let storage = create_test_storage("test_tenant_device_jwt_slots");
        let tenant_a = uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let tenant_b = uuid::Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();

        // Legacy slot seeded first — it must survive everything below.
        storage.store_tokens("legacy.default.jwt", "").unwrap();

        // Absent before any store.
        assert!(storage.get_tenant_device_jwt(&tenant_a).unwrap().is_none());
        assert!(storage.list_tenant_device_jwt_tenants().is_empty());

        // Round-trip both tenants.
        storage
            .store_tenant_device_jwt(&tenant_a, "jwt.for.a")
            .unwrap();
        storage
            .store_tenant_device_jwt(&tenant_b, "jwt.for.b")
            .unwrap();
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant_a).unwrap().as_deref(),
            Some("jwt.for.a")
        );
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant_b).unwrap().as_deref(),
            Some("jwt.for.b")
        );
        let mut listed = storage.list_tenant_device_jwt_tenants();
        listed.sort();
        assert_eq!(listed, vec![tenant_a, tenant_b]);

        // Overwrite is in-place (no duplicate slots).
        storage
            .store_tenant_device_jwt(&tenant_a, "jwt.for.a.v2")
            .unwrap();
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant_a).unwrap().as_deref(),
            Some("jwt.for.a.v2")
        );
        assert_eq!(storage.list_tenant_device_jwt_tenants().len(), 2);

        // LEGACY-SLOT PRESERVATION: none of the tenant-slot writes touched it.
        assert_eq!(storage.get_access_token().unwrap(), "legacy.default.jwt");

        // Clearing one tenant leaves the other + the legacy slot intact.
        storage.clear_tenant_device_jwt(&tenant_a).unwrap();
        assert!(storage.get_tenant_device_jwt(&tenant_a).unwrap().is_none());
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant_b).unwrap().as_deref(),
            Some("jwt.for.b")
        );
        assert_eq!(storage.get_access_token().unwrap(), "legacy.default.jwt");
        // Idempotent clear.
        storage.clear_tenant_device_jwt(&tenant_a).unwrap();

        // And the reverse direction: a legacy-slot write leaves tenant slots
        // untouched.
        storage.store_tokens("legacy.default.jwt.v2", "").unwrap();
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant_b).unwrap().as_deref(),
            Some("jwt.for.b")
        );
    }

    /// The FULL wipe (`clear_tokens`) destroys the per-tenant slots (they are
    /// bearer credentials); the autonomy-preserving interactive clear keeps
    /// them, exactly like it keeps the oauth_* credentials.
    #[test]
    fn test_tenant_device_jwt_slots_clear_semantics() {
        let storage = create_test_storage("test_tenant_device_jwt_clear");
        let tenant = uuid::Uuid::parse_str("cccccccc-cccc-4ccc-8ccc-cccccccccccc").unwrap();

        storage.store_tokens("legacy.jwt", "").unwrap();
        storage
            .store_tenant_device_jwt(&tenant, "jwt.tenant")
            .unwrap();

        // Interactive clear: legacy pair dropped, tenant slots preserved.
        storage.clear_interactive_session().unwrap();
        assert!(storage.get_access_token().is_err());
        assert_eq!(
            storage.get_tenant_device_jwt(&tenant).unwrap().as_deref(),
            Some("jwt.tenant")
        );

        // Full wipe: tenant slots destroyed too.
        storage.clear_tokens().unwrap();
        assert!(storage.get_tenant_device_jwt(&tenant).unwrap().is_none());
        assert!(storage.list_tenant_device_jwt_tenants().is_empty());
    }

    /// A pre-Phase-1 `StoredTokens` JSON (no `tenant_device_jwts` key) must
    /// still deserialize, with an empty slot map.
    #[test]
    fn test_legacy_stored_tokens_without_tenant_slots_deserializes() {
        let raw = r#"{"access_token":"a","refresh_token":"r","device_id":"d"}"#;
        let parsed: StoredTokens = serde_json::from_str(raw).expect("legacy shape must decode");
        assert!(parsed.tenant_device_jwts.is_empty());
    }

    #[test]
    fn test_device_machine_key_round_trip_and_isolation() {
        let storage = create_test_storage("test_device_machine_key");

        // Absent before any store.
        assert!(storage.get_device_machine_key().unwrap().is_none());

        // A device JWT in the access_token slot must NOT be clobbered by the
        // device-machine-key write (slot isolation).
        storage.store_tokens("device.jwt", "").unwrap();
        storage.store_device_machine_key("dmk_abc123").unwrap();

        assert_eq!(
            storage.get_device_machine_key().unwrap().as_deref(),
            Some("dmk_abc123")
        );
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");

        // Clearing only the device machine key leaves the device JWT intact.
        storage.clear_device_machine_key().unwrap();
        assert!(storage.get_device_machine_key().unwrap().is_none());
        assert_eq!(storage.get_access_token().unwrap(), "device.jwt");
    }

    /// Credential-split invariant (plan 2026-07-02 Phase 4): the FULL wipe
    /// (`clear_tokens`) MUST drop `device_machine_key` (a "sign out & stop
    /// autonomy" must drop the autonomy credential), while the
    /// autonomy-preserving interactive clear (`clear_interactive_session`)
    /// MUST keep it so the refresher can still cold-start-recover.
    #[test]
    fn test_device_machine_key_clear_semantics() {
        let storage = create_test_storage("test_device_machine_key_clear_semantics");

        // Seed a device JWT + a dmk_ (and prove agent_machine_key is wiped too).
        storage
            .store_tokens("device.jwt", "device.refresh")
            .unwrap();
        storage.store_device_machine_key("dmk_keepme").unwrap();
        storage.store_agent_machine_key("mk_keepme").unwrap();

        // Interactive clear: device JWT gone, dmk_ (and mk_) PRESERVED.
        storage.clear_interactive_session().unwrap();
        assert!(
            storage.get_access_token().is_err(),
            "interactive clear must drop the device JWT"
        );
        assert_eq!(
            storage.get_device_machine_key().unwrap().as_deref(),
            Some("dmk_keepme"),
            "interactive clear MUST preserve device_machine_key (the autonomy credential)"
        );
        assert_eq!(
            storage.get_agent_machine_key().unwrap().as_deref(),
            Some("mk_keepme"),
            "interactive clear MUST preserve agent_machine_key"
        );

        // Full wipe: the dmk_ (and mk_) MUST be gone.
        storage.clear_tokens().unwrap();
        assert!(
            storage.get_device_machine_key().unwrap().is_none(),
            "full clear_tokens MUST wipe device_machine_key"
        );
        assert!(
            storage.get_agent_machine_key().unwrap().is_none(),
            "full clear_tokens MUST wipe agent_machine_key"
        );
    }

    /// A pre-dmk `StoredTokens` JSON must still deserialize — the new
    /// `device_machine_key` field carries `#[serde(default)]`.
    #[test]
    fn test_legacy_stored_tokens_without_device_machine_key_deserializes() {
        let raw = r#"{"access_token":"a","refresh_token":"r","device_id":"d","agent_machine_key":"mk_x"}"#;
        let parsed: StoredTokens = serde_json::from_str(raw).expect("legacy shape must decode");
        assert_eq!(parsed.access_token.as_deref(), Some("a"));
        assert_eq!(parsed.agent_machine_key.as_deref(), Some("mk_x"));
        assert!(parsed.device_machine_key.is_none());
    }

    #[test]
    fn test_device_id() {
        let storage = create_test_storage("test_device_id");

        // Use a valid UUID format
        let test_uuid = "550e8400-e29b-41d4-a716-446655440000";
        storage.store_device_id(test_uuid).unwrap();

        // Verify the device ID was stored and can be retrieved
        let retrieved = storage.get_device_id().unwrap();

        // The retrieved value should be the UUID we stored
        assert_eq!(retrieved, test_uuid);

        // Also verify it's a valid UUID
        assert!(
            uuid::Uuid::parse_str(&retrieved).is_ok(),
            "Device ID should be a valid UUID"
        );
    }
}
