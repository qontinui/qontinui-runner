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
    #[cfg(test)]
    pub fn with_storage(secure_storage: SecureStorage) -> Self {
        Self {
            secure_storage,
            service_name: SERVICE_NAME.to_string(),
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
        // Try file storage first (primary)
        match self.secure_storage.get_access_token() {
            Ok(token) => {
                debug!("Retrieved access token from secure storage");
                return Ok(token);
            }
            Err(e) => {
                debug!("Access token not in secure storage: {}", e);
            }
        }

        // Fall back to keychain (for migration)
        match self.get_access_token_from_keychain() {
            Ok(token) => {
                info!("Retrieved access token from keychain (migrating to secure storage)");
                // Migrate: try to get refresh token too and store both in secure storage
                if let Ok(refresh) = self.get_refresh_token_from_keychain() {
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

    /// Clears all tokens from both storages.
    ///
    /// This is typically called during logout.
    ///
    /// # Errors
    ///
    /// Returns an error if clearing fails.
    pub fn clear_tokens(&self) -> Result<()> {
        // Clear from file storage
        if let Err(e) = self.secure_storage.clear_tokens() {
            warn!("Failed to clear tokens from secure storage: {}", e);
        }

        // Also clear from keychain (best effort)
        if let Err(e) = self.clear_tokens_from_keychain() {
            debug!("Failed to clear tokens from keychain: {}", e);
        }

        info!("Tokens cleared");
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
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            debug!("device_jwt_needs_refresh: token is not a 3-segment JWT (likely legacy opaque) — treating as needs-refresh");
            return Ok(true);
        }
        // Try URL_SAFE_NO_PAD first (the JWT spec mandates no-padding), but
        // accept URL_SAFE defensively too.
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]));
        let payload_bytes = match payload_bytes {
            Ok(b) => b,
            Err(e) => {
                debug!(
                    "device_jwt_needs_refresh: middle segment base64-decode failed ({e}) \
                     — treating as needs-refresh"
                );
                return Ok(true);
            }
        };
        let claim: JwtExpClaim = match serde_json::from_slice(&payload_bytes) {
            Ok(c) => c,
            Err(e) => {
                debug!(
                    "device_jwt_needs_refresh: payload JSON-decode failed ({e}) \
                     — treating as needs-refresh"
                );
                return Ok(true);
            }
        };
        let now = chrono::Utc::now().timestamp();
        Ok(now + REFRESH_BEFORE_EXPIRY_SECS >= claim.exp)
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

        // Clear and verify
        auth_manager.clear_tokens().unwrap();
        assert!(!auth_manager.has_tokens());
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
