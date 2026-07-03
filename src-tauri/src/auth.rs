//! Authentication manager for secure token storage and device management.
//!
//! This module provides secure storage of authentication tokens using encrypted
//! file storage as the primary mechanism, with OS keychain as a fallback.
//!
//! The file-based storage was implemented because the Windows Credential Manager
//! (via the keyring crate) proved unreliable - tokens would be stored successfully
//! but become unreadable after ~14 minutes, causing unexpected logouts.

use crate::secure_storage::SecureStorage;
use anyhow::{Context, Result};
use base64::Engine;
use keyring::Entry;
use serde::Deserialize;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Service name used for keychain entries (legacy)
const SERVICE_NAME: &str = "com.qontinui.runner";

/// Refresh the device-JWT once we're within TTL/3 of expiry.
///
/// Coord mints 4-hour device-JWTs (14_400s). TTL/3 = 4_800s = 80 min.
/// The refresher loop calls `device_jwt_needs_refresh` to decide whether
/// to call `pair_with_auth_token` and replace the stored JWT.
pub const REFRESH_BEFORE_EXPIRY_SECS: i64 = 4 * 60 * 60 / 3;

/// Minimal projection of a device-JWT payload — only the `exp` claim is
/// consulted by the refresher. Signature is NOT verified here; the
/// AuthManager just stored this token after a successful pair handshake.
#[derive(Debug, Deserialize)]
struct JwtExpClaim {
    exp: i64,
}

/// Leeway (seconds) applied when deciding whether a device-JWT is expired
/// for *attach* purposes. A small clock-skew margin so a token that's a few
/// seconds from expiry is treated as already-dead rather than attached and
/// 401'd a moment later by the relay.
const EXPIRY_LEEWAY_SECS: i64 = 30;

/// Decode the `exp` (unix seconds) claim from a JWT *without* verifying its
/// signature. Returns `None` when the input is not a 3-segment JWT or the
/// payload fails to base64/JSON-decode (e.g. a legacy opaque
/// `qontinui_runner_<random>` bearer).
///
/// This is the single decode site shared by [`AuthManager::device_jwt_needs_refresh`]
/// (refresher staleness), the SDK-connect expired-token guard, and the
/// `/auth/freshness` introspection route. Signature verification is
/// intentionally out of scope — coord re-verifies the JWT on every WS
/// handshake; the only thing read here is the unverified `exp` for staleness
/// decisions and operator introspection.
pub(crate) fn decode_jwt_exp(token: &str) -> Option<i64> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // Try URL_SAFE_NO_PAD first (the JWT spec mandates no-padding), but
    // accept URL_SAFE defensively too.
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    let claim: JwtExpClaim = serde_json::from_slice(&payload_bytes).ok()?;
    Some(claim.exp)
}

/// Returns `true` iff `token` is a JWT whose `exp` is already in the past
/// (with [`EXPIRY_LEEWAY_SECS`] of leeway). A non-JWT / undecodable token
/// returns `false` — callers that want a shape check should use
/// [`looks_like_jwt`] first; this helper answers only "is this decodable
/// JWT past its expiry?" and never claims an opaque token is expired.
pub(crate) fn jwt_is_expired(token: &str) -> bool {
    match decode_jwt_exp(token) {
        Some(exp) => {
            let now = chrono::Utc::now().timestamp();
            now - EXPIRY_LEEWAY_SECS >= exp
        }
        None => false,
    }
}

/// When `QONTINUI_DISABLE_KEYCHAIN` is set, keychain reads return an error
/// (callers fall back to file storage) and keychain writes are no-ops. The
/// keychain path is best-effort migration backup; file storage is the source
/// of truth. On macOS CI, `keyring::Entry::get_password()` blocks indefinitely
/// waiting for a Keychain user-permission dialog that never resolves — three
/// auth tests would hang past the 90-min step timeout. Setting this env var
/// in CI bypasses the dialog cleanly.
fn keychain_enabled() -> bool {
    std::env::var_os("QONTINUI_DISABLE_KEYCHAIN").is_none()
}

/// Feature flag for the session-scoped multi-tenant device-JWT model
/// (plan 2026-07-02-session-scoped-multi-tenant-device-binding, Phase 1).
///
/// `QONTINUI_MULTI_TENANT_JWT=1` turns on the flag-gated dark paths (the
/// refresher's per-tenant slot pass). DEFAULT OFF: unset, empty, or any
/// value other than `1` leaves every behavior byte-identical to today.
/// Read from the environment on each call so a long-lived process picks up
/// nothing stale — the check is a trivial env read.
pub fn multi_tenant_jwt_enabled() -> bool {
    multi_tenant_jwt_flag_from(std::env::var("QONTINUI_MULTI_TENANT_JWT").ok().as_deref())
}

/// Pure core of [`multi_tenant_jwt_enabled`]: only the exact value `1`
/// (after trimming) enables the flag. Factored out so the gate is testable
/// without mutating process-global env vars (racy across parallel tests).
pub(crate) fn multi_tenant_jwt_flag_from(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim), Some("1"))
}

/// Manages authentication tokens and device ID storage.
///
/// The AuthManager provides secure storage for:
/// - Access tokens (JWT)
/// - Refresh tokens
/// - Device ID (persistent UUID)
///
/// Storage strategy:
/// 1. Primary: Encrypted file storage (reliable on all platforms)
/// 2. Fallback: OS keychain (for migration from existing installations)
pub struct AuthManager {
    secure_storage: SecureStorage,
    service_name: String,
}

impl AuthManager {
    /// Creates a new AuthManager instance.
    pub fn new() -> Self {
        let secure_storage = SecureStorage::new().unwrap_or_else(|e| {
            error!("Failed to create SecureStorage: {}, using default", e);
            SecureStorage::default()
        });

        Self {
            secure_storage,
            service_name: SERVICE_NAME.to_string(),
        }
    }

    /// Creates an AuthManager with a custom SecureStorage for testing.
    ///
    /// Uses a UNIQUE per-instance keychain `service_name` so parallel tests do
    /// NOT collide on the single process-/OS-global keychain slot. The file
    /// store is already isolated via [`SecureStorage::with_path`], but the
    /// keychain backup (`store_tokens_in_keychain` / the `get_access_token`
    /// fallback) writes/reads `Entry::new(service_name, "access_token")` — a
    /// FIXED slot under the real `SERVICE_NAME`. Two parallel tests that both
    /// store/clear tokens would otherwise stomp that one shared slot, so a test
    /// expecting an empty slot reads a sibling's keychain token (the flaky
    /// `needs_refresh_when_no_token` / `test_token_storage` cross-test
    /// pollution). A per-test service name makes the keychain backup as isolated
    /// as the file store.
    #[cfg(test)]
    pub fn with_storage(secure_storage: SecureStorage) -> Self {
        Self {
            secure_storage,
            service_name: format!("com.qontinui.runner.test.{}", uuid::Uuid::now_v7()),
        }
    }

    /// Stores both access and refresh tokens.
    ///
    /// Uses encrypted file storage as the primary mechanism.
    ///
    /// # Arguments
    ///
    /// * `access_token` - JWT access token
    /// * `refresh_token` - JWT refresh token
    ///
    /// # Errors
    ///
    /// Returns an error if storage operations fail.
    pub fn store_tokens(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        // Store in encrypted file storage (primary)
        self.secure_storage
            .store_tokens(access_token, refresh_token)
            .context("Failed to store tokens in secure storage")?;

        // Also try to store in keychain (backup, best effort)
        if let Err(e) = self.store_tokens_in_keychain(access_token, refresh_token) {
            debug!("Could not store tokens in keychain (backup): {}", e);
        }

        info!("Tokens stored successfully in secure storage");
        Ok(())
    }

    /// Stores tokens in the OS keychain (legacy/backup).
    fn store_tokens_in_keychain(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        if !keychain_enabled() {
            debug!("keychain disabled via QONTINUI_DISABLE_KEYCHAIN, skipping store");
            return Ok(());
        }
        let entry_access = Entry::new(&self.service_name, "access_token")
            .context("Failed to create keychain entry for access token")?;
        let entry_refresh = Entry::new(&self.service_name, "refresh_token")
            .context("Failed to create keychain entry for refresh token")?;

        entry_access
            .set_password(access_token)
            .context("Failed to store access token in keychain")?;
        entry_refresh
            .set_password(refresh_token)
            .context("Failed to store refresh token in keychain")?;

        debug!("Tokens also stored in keychain (backup)");
        Ok(())
    }

    /// Retrieves the access token.
    ///
    /// First tries encrypted file storage, then falls back to keychain.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is not found in any storage.
    pub fn get_access_token(&self) -> Result<String> {
        // Try file storage first (primary).
        //
        // Distinguish two failure modes (item 5 of the auth-friction plan):
        //   - file ABSENT  → first-run / never-paired; keychain fallback +
        //     migration (overwrite the `.enc`) is the correct bootstrap.
        //   - file PRESENT but load failed → the store is malformed (parse /
        //     decrypt error). We STILL fall back to the keychain for an
        //     in-memory token (availability), but we must NOT overwrite the
        //     `.enc` file — doing so would mask the corruption (no forensics)
        //     and resurrect potentially-stale keychain tokens over a present
        //     store. So we suppress the migrate-write in this case.
        let store_present = self.secure_storage.store_file_exists();
        match self.secure_storage.get_access_token() {
            Ok(token) => {
                debug!("Retrieved access token from secure storage");
                return Ok(token);
            }
            Err(e) => {
                if store_present {
                    warn!(
                        "Secure storage present but access token could not be loaded \
                         (malformed/undecryptable store): {}. Falling back to keychain WITHOUT \
                         overwriting the .enc file (left intact for forensics).",
                        e
                    );
                } else {
                    debug!("Access token not in secure storage (no store file): {}", e);
                }
            }
        }

        // Fall back to keychain (for migration).
        match self.get_access_token_from_keychain() {
            Ok(token) => {
                info!("Retrieved access token from keychain");
                // Only migrate (overwrite the `.enc`) when the store file is
                // ABSENT — first-run bootstrap. When the store is present but
                // unreadable, leave it untouched (see above).
                if store_present {
                    warn!(
                        "Skipping keychain→secure-storage migration: a malformed store file is \
                         present and must not be overwritten."
                    );
                } else if let Ok(refresh) = self.get_refresh_token_from_keychain() {
                    if let Err(e) = self.secure_storage.store_tokens(&token, &refresh) {
                        warn!("Failed to migrate tokens to secure storage: {}", e);
                    } else {
                        info!("Tokens migrated to secure storage");
                    }
                }
                Ok(token)
            }
            Err(e) => {
                debug!("Access token not in keychain: {}", e);
                Err(anyhow::anyhow!("Access token not found in any storage"))
            }
        }
    }

    /// Retrieves access token from keychain (legacy).
    fn get_access_token_from_keychain(&self) -> Result<String> {
        if !keychain_enabled() {
            return Err(anyhow::anyhow!(
                "keychain disabled via QONTINUI_DISABLE_KEYCHAIN"
            ));
        }
        let entry = Entry::new(&self.service_name, "access_token")
            .context("Failed to create keychain entry for access token")?;
        entry
            .get_password()
            .context("Failed to retrieve access token from keychain")
    }

    /// Retrieves the refresh token.
    ///
    /// First tries encrypted file storage, then falls back to keychain.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is not found in any storage.
    pub fn get_refresh_token(&self) -> Result<String> {
        // Try file storage first (primary)
        match self.secure_storage.get_refresh_token() {
            Ok(token) => {
                debug!("Retrieved refresh token from secure storage");
                return Ok(token);
            }
            Err(e) => {
                debug!("Refresh token not in secure storage: {}", e);
            }
        }

        // Fall back to keychain (for migration)
        match self.get_refresh_token_from_keychain() {
            Ok(token) => {
                info!("Retrieved refresh token from keychain");
                Ok(token)
            }
            Err(e) => {
                debug!("Refresh token not in keychain: {}", e);
                Err(anyhow::anyhow!("Refresh token not found in any storage"))
            }
        }
    }

    /// Retrieves refresh token from keychain (legacy).
    fn get_refresh_token_from_keychain(&self) -> Result<String> {
        if !keychain_enabled() {
            return Err(anyhow::anyhow!(
                "keychain disabled via QONTINUI_DISABLE_KEYCHAIN"
            ));
        }
        let entry = Entry::new(&self.service_name, "refresh_token")
            .context("Failed to create keychain entry for refresh token")?;
        entry
            .get_password()
            .context("Failed to retrieve refresh token from keychain")
    }

    /// Clears ALL credentials from both storages — the device-JWT pair AND
    /// the Cognito (`oauth_*`) session, including the long-lived
    /// `oauth_refresh_token`.
    ///
    /// This is the explicit full sign-out. It destroys the only credential the
    /// device-JWT refresher can self-recover from, so it STOPS the runner's
    /// autonomous terminal sessions (they cannot re-mint a device JWT until an
    /// interactive re-login). For a default logout that should keep autonomy
    /// running, use [`Self::clear_interactive_session`].
    ///
    /// # Errors
    ///
    /// Returns an error if clearing fails.
    pub fn clear_all_credentials(&self) -> Result<()> {
        // Clear from file storage
        if let Err(e) = self.secure_storage.clear_tokens() {
            warn!("Failed to clear tokens from secure storage: {}", e);
        }

        // Also clear from keychain (best effort). The keychain only ever holds
        // the device-JWT pair (the oauth_* slots are file-only — see the module
        // comment at the Cognito section), so this deletes the access_token /
        // refresh_token entries.
        if let Err(e) = self.clear_tokens_from_keychain() {
            debug!("Failed to clear tokens from keychain: {}", e);
        }

        info!("All credentials cleared (device JWT + Cognito session)");
        Ok(())
    }

    /// Clears ONLY the interactive device-JWT session, PRESERVING the Cognito
    /// (`oauth_*`) session so the device-JWT refresher can immediately re-mint
    /// a fresh device JWT and keep autonomous terminal sessions running.
    ///
    /// This is the autonomy-preserving clear used by a default logout. Only the
    /// `access_token` / `refresh_token` slots (file + keychain) are dropped; the
    /// `oauth_refresh_token` (and the rest of the Cognito session) is left
    /// intact. Contrast with [`Self::clear_all_credentials`].
    ///
    /// The keychain only stores the device-JWT pair (oauth_* are file-only), so
    /// the keychain clear here is exactly the same `clear_tokens_from_keychain`
    /// helper and never touches a Cognito entry.
    ///
    /// # Errors
    ///
    /// Returns an error if clearing fails.
    pub fn clear_interactive_session(&self) -> Result<()> {
        // Clear ONLY the device-JWT pair from file storage; preserve oauth_*.
        if let Err(e) = self.secure_storage.clear_interactive_session() {
            warn!(
                "Failed to clear interactive device-JWT session from secure storage: {}",
                e
            );
        }

        // Keychain only holds the device-JWT pair, so this is the same helper.
        if let Err(e) = self.clear_tokens_from_keychain() {
            debug!("Failed to clear device-JWT pair from keychain: {}", e);
        }

        info!("Interactive device-JWT session cleared (Cognito session preserved for autonomous refresh)");
        Ok(())
    }

    /// Clears tokens from keychain (legacy).
    fn clear_tokens_from_keychain(&self) -> Result<()> {
        if !keychain_enabled() {
            return Ok(());
        }
        let entry_access = Entry::new(&self.service_name, "access_token")
            .context("Failed to create keychain entry for access token")?;
        let entry_refresh = Entry::new(&self.service_name, "refresh_token")
            .context("Failed to create keychain entry for refresh token")?;

        // Ignore errors if tokens don't exist
        let _ = entry_access.delete_credential();
        let _ = entry_refresh.delete_credential();

        Ok(())
    }

    /// Stores the device ID.
    ///
    /// # Arguments
    ///
    /// * `device_id` - UUID representing this device
    ///
    /// # Errors
    ///
    /// Returns an error if storage operations fail.
    pub fn store_device_id(&self, device_id: &str) -> Result<()> {
        // Store in file storage
        self.secure_storage
            .store_device_id(device_id)
            .context("Failed to store device_id in secure storage")?;

        // Also store in keychain (backup, best effort)
        if keychain_enabled() {
            if let Ok(entry) = Entry::new(&self.service_name, "device_id") {
                let _ = entry.set_password(device_id);
            }
        }

        info!("Device ID stored: {}", device_id);
        Ok(())
    }

    /// Retrieves or generates a device ID.
    ///
    /// If a device ID is already stored, it is returned. Otherwise, a new UUID v4
    /// is generated, stored, and returned.
    ///
    /// # Errors
    ///
    /// Returns an error if storage operations fail.
    pub fn get_device_id(&self) -> Result<String> {
        // Try file storage first
        if let Ok(id) = self.secure_storage.get_device_id() {
            info!("Retrieved existing device ID from secure storage: {}", id);
            return Ok(id);
        }

        // Try keychain (for migration)
        if keychain_enabled() {
            if let Ok(entry) = Entry::new(&self.service_name, "device_id") {
                if let Ok(id) = entry.get_password() {
                    info!(
                        "Retrieved existing device ID from keychain (migrating): {}",
                        id
                    );
                    // Migrate to file storage
                    if let Err(e) = self.secure_storage.store_device_id(&id) {
                        warn!("Failed to migrate device ID to secure storage: {}", e);
                    }
                    return Ok(id);
                }
            }
        }

        // Generate new device ID
        let new_id = Uuid::new_v4().to_string();
        info!("Generated new device ID: {}", new_id);
        self.store_device_id(&new_id)?;
        Ok(new_id)
    }

    /// Checks if the user is authenticated by verifying token existence.
    ///
    /// Returns true if both access and refresh tokens exist in storage.
    pub fn has_tokens(&self) -> bool {
        // Check file storage first
        if self.secure_storage.has_tokens() {
            return true;
        }

        // Fall back to keychain check
        self.get_access_token_from_keychain().is_ok()
            && self.get_refresh_token_from_keychain().is_ok()
    }

    /// Returns `Ok(true)` iff the device-JWT (stored in the access_token
    /// slot by `pair::persist_pairing`) is missing OR will expire within
    /// [`REFRESH_BEFORE_EXPIRY_SECS`]. Returns `Ok(false)` iff the JWT is
    /// fresh enough.
    ///
    /// Treatment of unparseable tokens: if the slot is non-empty but the
    /// middle segment fails to base64-decode or JSON-decode (likely a
    /// legacy opaque `qontinui_runner_<random>` bearer from before the
    /// device-JWT migration), this returns `Ok(true)` so the refresher
    /// replaces it with a real JWT. Returning Err here would prevent the
    /// refresher from healing legacy installs.
    ///
    /// Signature is intentionally NOT verified — the only consumer is the
    /// refresher loop deciding "should I pair again?". Coord re-verifies
    /// the JWT on every WS handshake; a forged exp at most causes one
    /// extra pair call.
    pub fn device_jwt_needs_refresh(&self) -> Result<bool> {
        let token = match self.get_access_token() {
            Ok(t) => t,
            Err(_) => return Ok(true), // No stored token = needs first pair.
        };
        if token.is_empty() {
            return Ok(true);
        }
        // Unparseable tokens (non-3-segment legacy opaque bearers, or a
        // payload that fails base64/JSON-decode) get treated as needs-refresh
        // so the refresher heals legacy installs.
        match crate::auth::decode_jwt_exp(&token) {
            Some(exp) => {
                let now = chrono::Utc::now().timestamp();
                Ok(now + REFRESH_BEFORE_EXPIRY_SECS >= exp)
            }
            None => {
                debug!(
                    "device_jwt_needs_refresh: access_token is not a decodable JWT \
                     (likely legacy opaque) — treating as needs-refresh"
                );
                Ok(true)
            }
        }
    }

    /// Returns the decoded (unverified) `exp` of the device-JWT in the
    /// `access_token` slot, or `None` if the slot is empty, missing, or holds
    /// a non-decodable (legacy opaque) bearer. Used by the `/auth/freshness`
    /// introspection route to compute an expiry delta without exposing the
    /// token. Reuses the shared [`decode_jwt_exp`] machinery.
    pub fn access_token_exp(&self) -> Option<i64> {
        let token = self.get_access_token().ok()?;
        crate::auth::decode_jwt_exp(&token)
    }

    /// Returns the absolute Cognito (oauth) access-token expiry in unix
    /// seconds, if a Cognito session is present. Thin pass-through to
    /// `SecureStorage::get_oauth_expires_at`.
    pub fn oauth_expires_at(&self) -> Option<i64> {
        self.secure_storage.get_oauth_expires_at()
    }

    // ========================================================================
    // Cognito (oauth) user-token slots — Phase 5 unified-Cognito-identity.
    //
    // These are stored in slots DISTINCT from the coord device-JWT
    // (`access_token`) slot so the WS relay keeps using the device JWT while
    // user-facing calls (and the device→user re-bind) use the Cognito token.
    // Keychain backup is intentionally NOT mirrored here: the keychain path
    // proved unreliable on Windows (see module docs) and the encrypted file
    // is the source of truth.
    // ========================================================================

    /// Store the Cognito user tokens. `expires_at` is the absolute
    /// unix-seconds expiry of the Cognito access token (now + expires_in).
    pub fn store_oauth_tokens(
        &self,
        access_token: &str,
        id_token: &str,
        refresh_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        self.secure_storage
            .store_oauth_tokens(access_token, id_token, refresh_token, expires_at)
            .context("Failed to store Cognito tokens in secure storage")?;
        info!("Cognito tokens stored successfully in secure storage");
        Ok(())
    }

    /// Retrieve the Cognito access token.
    pub fn get_oauth_access_token(&self) -> Result<String> {
        self.secure_storage.get_oauth_access_token()
    }

    /// Retrieve the Cognito id token.
    pub fn get_oauth_id_token(&self) -> Result<String> {
        self.secure_storage.get_oauth_id_token()
    }

    /// Retrieve the Cognito refresh token.
    pub fn get_oauth_refresh_token(&self) -> Result<String> {
        self.secure_storage.get_oauth_refresh_token()
    }

    /// Absolute unix-seconds expiry of the Cognito access token, if stored.
    pub fn get_oauth_expires_at(&self) -> Option<i64> {
        self.secure_storage.get_oauth_expires_at()
    }

    /// `true` iff the Cognito access token is missing OR within
    /// [`REFRESH_BEFORE_EXPIRY_SECS`] of expiry. Used by the device-JWT
    /// refresher to refresh the Cognito token *first* when it's stale (so the
    /// subsequent device re-bind presents a fresh user bearer).
    pub fn cognito_token_needs_refresh(&self) -> bool {
        match self.secure_storage.get_oauth_expires_at() {
            Some(exp) => chrono::Utc::now().timestamp() + REFRESH_BEFORE_EXPIRY_SECS >= exp,
            // No stored expiry → either never signed in via Cognito, or a
            // partial write. Treat as "needs refresh" only if we actually
            // hold a refresh token; otherwise there's nothing to refresh.
            None => self.secure_storage.get_oauth_refresh_token().is_ok(),
        }
    }

    // ========================================================================
    // Per-tenant device-JWT slots — session-scoped multi-tenant Phase 1
    // (plan 2026-07-02-session-scoped-multi-tenant-device-binding, D4).
    //
    // One secure-storage slot per tenant binding, keyed
    // `device_jwt:<tenant_id>`. The legacy `access_token` slot is NEVER
    // read or written by these methods — it keeps holding the DEFAULT
    // binding's JWT, so every unmodified consumer (relay, data-plane bearer,
    // legacy refresher path) keeps working during the compat window. Like
    // the oauth_* slots, the keychain backup is intentionally not mirrored
    // (file store is the source of truth; see module docs).
    // ========================================================================

    /// Store (or overwrite) the device JWT for one tenant binding
    /// (slot `device_jwt:<tenant_id>`). Never touches the legacy
    /// `access_token` slot.
    pub fn store_tenant_device_jwt(&self, tenant_id: &Uuid, jwt: &str) -> Result<()> {
        self.secure_storage
            .store_tenant_device_jwt(tenant_id, jwt)
            .context("Failed to store per-tenant device JWT in secure storage")
    }

    /// Retrieve the device JWT for one tenant binding. `Ok(None)` when no
    /// slot exists for that tenant.
    pub fn get_tenant_device_jwt(&self, tenant_id: &Uuid) -> Result<Option<String>> {
        self.secure_storage.get_tenant_device_jwt(tenant_id)
    }

    /// Remove one tenant's device-JWT slot. Idempotent; never touches the
    /// legacy `access_token` slot or any other tenant's slot.
    pub fn clear_tenant_device_jwt(&self, tenant_id: &Uuid) -> Result<()> {
        self.secure_storage
            .clear_tenant_device_jwt(tenant_id)
            .context("Failed to clear per-tenant device JWT from secure storage")
    }

    /// Enumerate the tenant ids that currently have a device-JWT slot, in
    /// deterministic order. Never fatal — an unreadable store yields an
    /// empty list.
    pub fn list_tenant_device_jwt_tenants(&self) -> Vec<Uuid> {
        self.secure_storage.list_tenant_device_jwt_tenants()
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Device-JWT data-plane bearer — Phase 0 runner↔coord multi-user readiness.
//
// The runner's coord data-plane HTTP calls (session register/heartbeat/state,
// claim acquire/heartbeat/release, agent allocate) were unauthenticated. The
// device-JWT already exists (minted by pairing, stored in the access_token
// slot); these helpers attach it as `Authorization: Bearer <jwt>` on each call
// when present. Coord still accepts anonymous calls, so a missing token (an
// unpaired runner / empty keychain) is NEVER fatal — the request goes out
// exactly as before.
// ============================================================================

/// Total data-plane calls that passed through [`attach_device_auth`].
static DATA_PLANE_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Subset of those that carried the device-JWT bearer header.
static DATA_PLANE_AUTHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Gate so the unpaired-runner warning is logged at most once per process.
static MISSING_TOKEN_WARNED: std::sync::Once = std::sync::Once::new();

/// Returns the stored device-JWT as a bearer string, or `None` when no token
/// is available (unpaired runner, missing keychain entry, storage IO error).
///
/// NEVER fatal and NEVER panics — every failure mode collapses to `None`, and
/// callers fall back to sending the request unauthenticated (coord accepts
/// anonymous data-plane writes). The missing-token case is logged at most once
/// per process via [`MISSING_TOKEN_WARNED`] so the frequent heartbeat path
/// can't spam the log.
///
/// No caching: `AuthManager::get_access_token()` is a cheap local encrypted-
/// file read, and the device-JWT has only a 4-hour TTL (refreshed in place by
/// the refresher loop), so a stale long-lived cache would risk presenting an
/// expired bearer. Reading per-call keeps us always-current.
pub fn device_bearer() -> Option<String> {
    match AuthManager::new().get_access_token() {
        Ok(token) if !token.trim().is_empty() => Some(token),
        Ok(_) => {
            MISSING_TOKEN_WARNED.call_once(|| {
                warn!(
                    "coord data-plane: no device-JWT stored (empty token) — \
                     sending coord calls unauthenticated; pair this runner to authenticate"
                );
            });
            None
        }
        Err(e) => {
            MISSING_TOKEN_WARNED.call_once(|| {
                warn!(
                    "coord data-plane: device-JWT unavailable ({e}) — \
                     sending coord calls unauthenticated; pair this runner to authenticate"
                );
            });
            None
        }
    }
}

/// Attach the device-JWT bearer to a coord data-plane request when one is
/// available, otherwise return the builder unchanged. Also drives the
/// auth-coverage metric (the dogfood signal Phase 1 gates on): every call is
/// counted in `DATA_PLANE_TOTAL`, and calls that carried the header in
/// `DATA_PLANE_AUTHED`. A coverage summary is emitted at info level every 25th
/// call so an operator can watch the unpaired→paired transition without a new
/// dependency.
pub fn attach_device_auth(rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let total = DATA_PLANE_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let rb = match device_bearer() {
        Some(token) => {
            DATA_PLANE_AUTHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            rb.header("Authorization", format!("Bearer {token}"))
        }
        None => rb,
    };
    if total.is_multiple_of(25) {
        let authed = DATA_PLANE_AUTHED.load(std::sync::atomic::Ordering::Relaxed);
        let pct = (authed as f64 / total as f64) * 100.0;
        info!("coord data-plane auth coverage: {authed}/{total} ({pct:.0}%)");
    }
    rb
}

/// Returns true if `s` looks like a JWS Compact Serialization
/// (three base64url segments separated by `.`). Used by Phase 4 of the
/// unified-devices migration to distinguish a real device-JWT from the
/// legacy opaque `qontinui_runner_<random>` bearer that older paired
/// installs have in the access_token slot.
///
/// Does NOT verify the signature or parse claims — that's
/// `device_jwt_needs_refresh`'s job. This is the shallow shape check
/// used at boot to decide whether a refresher kick is warranted.
pub(crate) fn looks_like_jwt(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

#[cfg(test)]
mod looks_like_jwt_tests {
    use super::looks_like_jwt;

    #[test]
    fn legacy_opaque_runner_token_is_not_jwt() {
        assert!(!looks_like_jwt("qontinui_runner_abc123"));
    }

    #[test]
    fn empty_string_is_not_jwt() {
        assert!(!looks_like_jwt(""));
    }

    #[test]
    fn whitespace_is_not_jwt() {
        assert!(!looks_like_jwt("   "));
    }

    #[test]
    fn single_dot_is_not_jwt() {
        assert!(!looks_like_jwt("a.b"));
    }

    #[test]
    fn four_segments_is_not_jwt() {
        assert!(!looks_like_jwt("a.b.c.d"));
    }

    #[test]
    fn jwt_shape_three_segments_passes() {
        assert!(looks_like_jwt("abc.def.ghi"));
    }

    #[test]
    fn jwt_shape_with_url_safe_chars_passes() {
        assert!(looks_like_jwt("eyJ_h-1.eyJa-bc.xy_z"));
    }

    #[test]
    fn jwt_shape_with_invalid_chars_fails() {
        // `+` and `/` are standard base64, NOT URL-safe — a real JWT uses
        // base64url, so a token with these is malformed.
        assert!(!looks_like_jwt("eyJh.eyJh+/.xyz"));
    }
}

#[cfg(test)]
mod jwt_exp_tests {
    use super::{decode_jwt_exp, jwt_is_expired, EXPIRY_LEEWAY_SECS};
    use base64::Engine;

    /// Build a syntactically-valid (unsigned) JWT with the given `exp` claim.
    /// Header/signature are throwaway — only the middle segment is decoded.
    fn jwt_with_exp(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn decodes_exp_from_valid_jwt() {
        let token = jwt_with_exp(1_900_000_000);
        assert_eq!(decode_jwt_exp(&token), Some(1_900_000_000));
    }

    #[test]
    fn decode_returns_none_for_opaque_token() {
        assert_eq!(decode_jwt_exp("qontinui_runner_abc123"), None);
    }

    #[test]
    fn decode_returns_none_for_empty() {
        assert_eq!(decode_jwt_exp(""), None);
        assert_eq!(decode_jwt_exp("   "), None);
    }

    #[test]
    fn decode_returns_none_for_malformed_payload() {
        // Three segments but the middle is not valid base64url JSON.
        assert_eq!(decode_jwt_exp("aaa.!!!.ccc"), None);
        // Valid base64 but not JSON with an `exp` field.
        let not_json = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json");
        assert_eq!(decode_jwt_exp(&format!("aaa.{not_json}.ccc")), None);
    }

    #[test]
    fn expired_token_is_expired() {
        let now = chrono::Utc::now().timestamp();
        // Comfortably past expiry (beyond the leeway window).
        let token = jwt_with_exp(now - EXPIRY_LEEWAY_SECS - 60);
        assert!(jwt_is_expired(&token));
    }

    #[test]
    fn fresh_token_is_not_expired() {
        let now = chrono::Utc::now().timestamp();
        let token = jwt_with_exp(now + 3600);
        assert!(!jwt_is_expired(&token));
    }

    #[test]
    fn malformed_token_is_not_reported_expired() {
        // A non-decodable token must NOT be claimed expired — only a
        // decodable-past-exp JWT counts. (looks_like_jwt is the shape gate.)
        assert!(!jwt_is_expired("qontinui_runner_abc123"));
        assert!(!jwt_is_expired(""));
        assert!(!jwt_is_expired("aaa.!!!.ccc"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    /// Create an isolated AuthManager for testing.
    /// Each test gets its own unique storage file to avoid test interference.
    fn create_test_auth_manager(test_name: &str) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        // Clean up any existing file from previous test runs
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage(storage)
    }

    #[test]
    fn test_device_id_generation() {
        let auth_manager = create_test_auth_manager("test_device_id_generation");
        let device_id = auth_manager.get_device_id().unwrap();
        assert!(!device_id.is_empty());
        assert!(Uuid::parse_str(&device_id).is_ok());
    }

    #[test]
    fn test_device_id_persistence() {
        let auth_manager = create_test_auth_manager("test_device_id_persistence");
        let device_id1 = auth_manager.get_device_id().unwrap();

        // First call should generate a valid UUID
        assert!(!device_id1.is_empty());
        assert!(Uuid::parse_str(&device_id1).is_ok());

        // Second call should return the same ID (from file storage)
        let device_id2 = auth_manager.get_device_id().unwrap();
        assert_eq!(device_id1, device_id2);
    }

    #[test]
    fn test_token_storage() {
        let auth_manager = create_test_auth_manager("test_token_storage");

        // Store tokens
        auth_manager
            .store_tokens("test_access", "test_refresh")
            .unwrap();

        // Retrieve tokens
        assert_eq!(auth_manager.get_access_token().unwrap(), "test_access");
        assert_eq!(auth_manager.get_refresh_token().unwrap(), "test_refresh");

        // Verify has_tokens
        assert!(auth_manager.has_tokens());

        // Clear and verify (full wipe).
        auth_manager.clear_all_credentials().unwrap();
        assert!(!auth_manager.has_tokens());
    }

    /// Item 5: a malformed-but-PRESENT store file must NOT be overwritten by
    /// the keychain-migration fallback. `get_access_token` may fall back to
    /// the keychain for availability, but the `.enc` bytes stay byte-identical
    /// (left intact for forensics) rather than being clobbered with a fresh
    /// store derived from (potentially stale) keychain tokens.
    #[test]
    fn malformed_store_present_is_not_overwritten() {
        // Keep keychain out of the picture so the test is deterministic on
        // every platform: with no keychain entry, get_access_token returns
        // Err — exactly the case where the OLD code would have re-migrated
        // and overwritten the file had a keychain token existed.
        let temp_dir = env::temp_dir().join("qontinui_test_auth");
        let storage_path = temp_dir.join("malformed_store_present_is_not_overwritten.enc");
        let _ = fs::remove_file(&storage_path);
        fs::create_dir_all(&temp_dir).unwrap();

        // Write garbage bytes — present, but neither decryptable nor parseable.
        let garbage = b"this-is-not-a-valid-encrypted-token-store\x00\x01\x02".to_vec();
        fs::write(&storage_path, &garbage).unwrap();

        let storage = SecureStorage::with_path(storage_path.clone()).unwrap();
        let mgr = AuthManager::with_storage(storage);

        // Load attempt: with no keychain token available this errors, but the
        // important invariant is the file-untouched check below.
        let _ = mgr.get_access_token();

        let after = fs::read(&storage_path).expect("store file must still exist");
        assert_eq!(
            after, garbage,
            "malformed store file must be left byte-identical (not overwritten)"
        );

        let _ = fs::remove_file(&storage_path);
    }
}

/// Tests for the per-tenant device-JWT slots + the multi-tenant feature flag
/// (session-scoped multi-tenant Phase 1).
#[cfg(test)]
mod tenant_device_jwt_tests {
    use super::*;
    use std::env;
    use std::fs;

    fn create_test_auth_manager(test_name: &str) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth_tenant_jwt");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage(storage)
    }

    #[test]
    fn flag_is_default_off_and_only_exactly_1_enables() {
        // Default off: unset / empty / junk / "true" / "0" all stay dark.
        assert!(!multi_tenant_jwt_flag_from(None));
        assert!(!multi_tenant_jwt_flag_from(Some("")));
        assert!(!multi_tenant_jwt_flag_from(Some("0")));
        assert!(!multi_tenant_jwt_flag_from(Some("true")));
        assert!(!multi_tenant_jwt_flag_from(Some("yes")));
        // Only the documented value (trimmed) enables.
        assert!(multi_tenant_jwt_flag_from(Some("1")));
        assert!(multi_tenant_jwt_flag_from(Some(" 1 ")));
    }

    #[test]
    fn tenant_slot_round_trip_and_enumeration() {
        let mgr = create_test_auth_manager("tenant_slot_round_trip");
        let a = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let b = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();

        assert!(mgr.get_tenant_device_jwt(&a).unwrap().is_none());
        assert!(mgr.list_tenant_device_jwt_tenants().is_empty());

        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();
        mgr.store_tenant_device_jwt(&b, "jwt.b").unwrap();

        assert_eq!(
            mgr.get_tenant_device_jwt(&a).unwrap().as_deref(),
            Some("jwt.a")
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&b).unwrap().as_deref(),
            Some("jwt.b")
        );
        let mut listed = mgr.list_tenant_device_jwt_tenants();
        listed.sort();
        assert_eq!(listed, vec![a, b]);

        mgr.clear_tenant_device_jwt(&a).unwrap();
        assert!(mgr.get_tenant_device_jwt(&a).unwrap().is_none());
        assert_eq!(mgr.list_tenant_device_jwt_tenants(), vec![b]);
    }

    /// THE Phase-1 invariant: writing per-tenant slots never mutates the
    /// legacy `access_token` slot (which keeps the DEFAULT binding's JWT for
    /// every unmodified consumer), and clearing tenant slots doesn't either.
    #[test]
    fn tenant_slot_writes_never_mutate_legacy_access_token() {
        let mgr = create_test_auth_manager("tenant_slot_legacy_preserved");
        let a = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();

        mgr.store_tokens("default.binding.jwt", "").unwrap();
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "default.binding.jwt",
            "storing a tenant slot must not mutate access_token"
        );

        mgr.store_tenant_device_jwt(&a, "jwt.a.v2").unwrap();
        mgr.clear_tenant_device_jwt(&a).unwrap();
        assert_eq!(
            mgr.get_access_token().unwrap(),
            "default.binding.jwt",
            "overwriting/clearing a tenant slot must not mutate access_token"
        );

        // Reverse direction: a legacy write leaves tenant slots alone.
        mgr.store_tenant_device_jwt(&a, "jwt.a.v3").unwrap();
        mgr.store_tokens("default.binding.jwt.v2", "").unwrap();
        assert_eq!(
            mgr.get_tenant_device_jwt(&a).unwrap().as_deref(),
            Some("jwt.a.v3")
        );
    }
}

/// Tests for `AuthManager::device_jwt_needs_refresh` — Phase 2.1 of the
/// runner unified-devices migration.
#[cfg(test)]
mod device_jwt_tests {
    use super::*;
    use std::env;
    use std::fs;

    /// Each test gets its own isolated storage file so they don't poison
    /// each other.
    fn create_test_auth_manager(test_name: &str) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth_device_jwt");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage(storage)
    }

    /// URL-safe base64 without padding — the JWT spec encoding.
    fn b64url_no_pad(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Mint a synthetic JWT with the given `exp` claim. Signature is not
    /// verified by `device_jwt_needs_refresh`, so a placeholder is fine.
    fn synth_jwt(exp: i64) -> String {
        let header = b64url_no_pad(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url_no_pad(format!("{{\"exp\":{}}}", exp).as_bytes());
        let sig = b64url_no_pad(b"fake-signature-bytes");
        format!("{header}.{payload}.{sig}")
    }

    #[test]
    fn needs_refresh_when_no_token() {
        let mgr = create_test_auth_manager("needs_refresh_when_no_token");
        // Empty slot — never stored anything.
        assert!(
            mgr.device_jwt_needs_refresh().unwrap(),
            "missing token must report needs-refresh"
        );
    }

    #[test]
    fn needs_refresh_when_legacy_opaque_token() {
        let mgr = create_test_auth_manager("needs_refresh_when_legacy_opaque_token");
        mgr.store_tokens("qontinui_runner_abc123", "").unwrap();
        assert!(
            mgr.device_jwt_needs_refresh().unwrap(),
            "legacy opaque token must report needs-refresh so the refresher heals it"
        );
    }

    #[test]
    fn needs_refresh_when_jwt_within_threshold() {
        let mgr = create_test_auth_manager("needs_refresh_when_jwt_within_threshold");
        let now = chrono::Utc::now().timestamp();
        // exp 30 minutes from now — well inside the 80-minute refresh threshold.
        let jwt = synth_jwt(now + 30 * 60);
        mgr.store_tokens(&jwt, "").unwrap();
        assert!(
            mgr.device_jwt_needs_refresh().unwrap(),
            "JWT within REFRESH_BEFORE_EXPIRY_SECS must report needs-refresh"
        );
    }

    #[test]
    fn does_not_need_refresh_when_jwt_fresh() {
        let mgr = create_test_auth_manager("does_not_need_refresh_when_jwt_fresh");
        let now = chrono::Utc::now().timestamp();
        // exp 3 hours from now — comfortably outside the 80-minute threshold.
        let jwt = synth_jwt(now + 3 * 60 * 60);
        mgr.store_tokens(&jwt, "").unwrap();
        assert!(
            !mgr.device_jwt_needs_refresh().unwrap(),
            "JWT with >80min until expiry must NOT report needs-refresh"
        );
    }

    #[test]
    fn needs_refresh_when_jwt_expired() {
        let mgr = create_test_auth_manager("needs_refresh_when_jwt_expired");
        let now = chrono::Utc::now().timestamp();
        // exp 1 hour ago — already expired.
        let jwt = synth_jwt(now - 60 * 60);
        mgr.store_tokens(&jwt, "").unwrap();
        assert!(
            mgr.device_jwt_needs_refresh().unwrap(),
            "expired JWT must report needs-refresh"
        );
    }

    #[test]
    fn refresh_threshold_is_ttl_over_three() {
        // Pin the constant — coord mints 4h JWTs, refresh threshold = TTL/3.
        assert_eq!(REFRESH_BEFORE_EXPIRY_SECS, 4_800);
    }
}
