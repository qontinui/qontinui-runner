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
///
/// This constant is sized for the DEVICE JWT and nothing else. The Cognito
/// access token has its own, much shorter TTL and its own threshold —
/// [`COGNITO_REFRESH_BEFORE_EXPIRY_SECS`]. Reusing this one for Cognito is
/// the bug fixed below: 4_800s exceeds the Cognito token's entire 3_600s
/// lifetime, so the staleness predicate was true the instant a refresh
/// completed and every auth check paid a fresh `refresh_token` grant.
pub const REFRESH_BEFORE_EXPIRY_SECS: i64 = 4 * 60 * 60 / 3;

/// Refresh the Cognito access token once we're within TTL/3 of expiry.
///
/// Cognito mints 1-hour access tokens (`expires_in: 3600` — see
/// `cognito::TokenResponse` and its parse tests). TTL/3 = 1_200s = 20 min,
/// the same one-third convention as [`REFRESH_BEFORE_EXPIRY_SECS`].
///
/// The invariant that matters is `COGNITO_REFRESH_BEFORE_EXPIRY_SECS <
/// COGNITO_ACCESS_TOKEN_TTL_SECS`: a threshold at or above the token's own
/// lifetime makes [`AuthManager::cognito_token_needs_refresh`] permanently
/// true, which turns every `check_auth_status` into a synchronous network
/// round-trip. `cognito_threshold_is_below_token_ttl` pins it.
pub const COGNITO_REFRESH_BEFORE_EXPIRY_SECS: i64 = 60 * 60 / 3;

/// The `expires_in` Cognito returns on both the code-for-token exchange and
/// the refresh grant. Declared here so the threshold above can be pinned
/// against it by a test rather than by a comment.
pub const COGNITO_ACCESS_TOKEN_TTL_SECS: i64 = 3_600;

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
    /// The machine's canonical identity file (`~/.qontinui/machine.json`),
    /// consulted by [`Self::get_device_id`] BEFORE the encrypted cache and
    /// before any mint. `None` only when the home directory is unresolvable
    /// (and, in tests, to exercise the no-machine.json fallback).
    machine_file: Option<std::path::PathBuf>,
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
            machine_file: crate::machine_identity::machine_file_path(),
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
    ///
    /// `machine_file` is `None`, i.e. "this box has no `machine.json`" — tests
    /// must never read (let alone write) the real `~/.qontinui/machine.json`.
    /// Use [`Self::with_storage_and_machine_file`] to exercise the canonical
    /// identity path against a tempdir.
    #[cfg(test)]
    pub fn with_storage(secure_storage: SecureStorage) -> Self {
        Self {
            secure_storage,
            service_name: format!("com.qontinui.runner.test.{}", uuid::Uuid::now_v7()),
            machine_file: None,
        }
    }

    /// [`Self::with_storage`] with an explicit `machine.json` path, so the
    /// canonical-identity branch of [`Self::get_device_id`] is testable
    /// against a tempdir.
    #[cfg(test)]
    pub fn with_storage_and_machine_file(
        secure_storage: SecureStorage,
        machine_file: std::path::PathBuf,
    ) -> Self {
        Self {
            secure_storage,
            service_name: format!("com.qontinui.runner.test.{}", uuid::Uuid::now_v7()),
            machine_file: Some(machine_file),
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

    /// Explicit-acquisition variant of [`Self::store_tokens`]: overwrites a
    /// present-but-unreadable file store from blank rather than refusing. Called
    /// only on the explicit pairing path (`pair::persist_pairing`) — never from
    /// the background device-JWT refresher, which uses [`Self::store_tokens`].
    /// See `SecureStorage::WriteMode` for why the distinction matters.
    pub fn store_tokens_fresh(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        self.secure_storage
            .store_tokens_fresh(access_token, refresh_token)
            .context("Failed to store tokens in secure storage")?;

        // Keychain backup, best effort — identical to store_tokens.
        if let Err(e) = self.store_tokens_in_keychain(access_token, refresh_token) {
            debug!("Could not store tokens in keychain (backup): {}", e);
        }

        info!("Tokens stored successfully in secure storage (fresh/overwrite)");
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

    /// Tri-state probe of the device-JWT (`access_token`) slot.
    ///
    /// NO-DOWNGRADE: [`Self::get_access_token`] returns `Err` both when nothing
    /// was ever stored (genuinely unpaired) and when the store is present but
    /// undecryptable (state UNKNOWN). Callers that turn "no token" into a
    /// capability denial — the relay idle gate, the `device_jwt_present` /
    /// `get_coord_device_token` probes, the coord doctor — must be able to tell
    /// those apart, otherwise a corrupt store renders as "pair this runner
    /// first" at a runner that IS paired, and silently removes it from the
    /// fleet.
    ///
    /// Order matters: the full chain (secure store → keychain) is tried FIRST,
    /// so a legacy keychain-only install still reports `Present`. Only when the
    /// whole chain fails do we inspect the store to classify the failure.
    pub fn probe_access_token(&self) -> crate::secure_storage::StoredTokenRead {
        use crate::secure_storage::StoredTokenRead;
        if let Ok(token) = self.get_access_token() {
            if !token.trim().is_empty() {
                return StoredTokenRead::Present(token);
            }
            return StoredTokenRead::Absent;
        }
        // The chain failed. `Absent` only if the store is genuinely empty /
        // missing; a present-but-unreadable store is UNKNOWN.
        self.secure_storage.read_access_token()
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
    /// Over an unreadable store the file-side clear now REFUSES (it could not
    /// honour "preserve the Cognito session" — a blank rewrite would silently
    /// escalate this into a full sign-out). That is warned, not propagated, and
    /// the logout still sticks: the keychain pair is cleared regardless, and
    /// `SecureStorage::is_interactive_signed_out` fails closed on an unreadable
    /// store, so the status check reports signed-out anyway.
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

    /// Whether the operator explicitly ended the interactive session via a
    /// logout. See `SecureStorage::is_interactive_signed_out` for why this is
    /// tracked separately from credential presence.
    pub fn is_interactive_signed_out(&self) -> bool {
        self.secure_storage.is_interactive_signed_out()
    }

    /// `true` iff the credential store file EXISTS on disk but cannot be
    /// read/decrypted/parsed — the "corrupt / wrong-machine key" case (a machine
    /// rename / disk move / re-image makes the hostname+username-derived AES key
    /// no longer match). Surfaced on the auth status so the LoginScreen can offer
    /// a "reset your credential store" affordance instead of a bare, unexplained
    /// sign-in prompt (`commands::auth::check_auth_status` /
    /// `reset_credential_store`).
    pub fn is_store_present_but_unreadable(&self) -> bool {
        self.secure_storage.is_present_but_unreadable()
    }

    /// Clears the interactive sign-out marker.
    ///
    /// Call this from an EXPLICIT interactive credential acquisition ONLY, and
    /// only once that acquisition has actually been persisted. There are
    /// exactly three such call sites — Cognito sign-in
    /// (`commands::auth::finalize_signed_in`), pair-code redeem
    /// (`commands::web_integration::redeem_pair_code`) and the CLI
    /// `qontinui_profile device pair`. Never call it from a credential WRITER
    /// (`store_tokens` / `store_oauth_tokens` / `pair::persist_pairing`): the
    /// background device-JWT refresher goes through those, and clearing there
    /// would silently un-logout the operator on its next cycle.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be written.
    pub fn clear_interactive_signed_out(&self) -> Result<()> {
        self.secure_storage.clear_interactive_signed_out()
    }

    /// `true` iff the runner holds a *local* signed-in session: a paired coord
    /// device-token JWT (the credential the WS relay presents) and/or a stored
    /// Cognito session.
    ///
    /// Credential PRESENCE, deliberately not credential FRESHNESS. A stale or
    /// even expired device JWT is a refresher problem, never a logout: the
    /// supervised device-JWT refresher re-mints from the Cognito session, and
    /// coord re-verifies on every WS handshake. Gating on freshness here used to
    /// sign the operator out automatically — [`Self::device_jwt_needs_refresh`]
    /// flips to `true` a full [`REFRESH_BEFORE_EXPIRY_SECS`] (80 min) BEFORE the
    /// JWT actually expires, so an install with no stored Cognito refresh token
    /// (legacy pair-code pairing) was reported unauthenticated 80 minutes early
    /// and dropped to the LoginScreen.
    ///
    /// Also deliberately NOT a `/api/v1/auth/users/me` round-trip: the web
    /// backend's `users/me` can return 401/403 for a federated Cognito identity
    /// even though the runner is fully signed in and device-paired, which made a
    /// completed sign-in render the LoginScreen forever.
    /// "Could not read the store" is UNKNOWN, not "no credential" — the two must
    /// not be collapsed. They are handled explicitly and up front here, because
    /// the `.unwrap_or(false)` below would otherwise answer for a case it cannot
    /// see: with the `.enc` unreadable, [`Self::get_access_token`] falls back to
    /// the OS KEYCHAIN and can return a token the store knows nothing about, so
    /// the collapse could just as easily produce a false YES as a false NO. An
    /// unreadable store is resolved the same way
    /// [`SecureStorage::is_interactive_signed_out`] resolves it — fail closed,
    /// one consistent verdict — rather than by whichever slot happened to
    /// answer first.
    pub fn has_local_signed_in_session(&self) -> bool {
        if self.secure_storage.is_present_but_unreadable() {
            warn!(
                "Secure storage present but unreadable — cannot prove a local session either \
                 way. Reporting NO local session (fail-closed, consistent with the interactive \
                 sign-out marker) instead of trusting the keychain fallback. Sign in again to \
                 repair the store."
            );
            return false;
        }

        // Past this point the store is readable, so an `Err` means the slot is
        // genuinely empty — a real NO, not an unknown.
        let has_device_jwt = self
            .get_access_token()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);

        // A stored Cognito session (refresh token present) also means the user
        // signed in — even if the device JWT is momentarily stale and awaiting
        // the refresher's next pair cycle.
        let has_cognito_session = self
            .get_oauth_refresh_token()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);

        has_device_jwt || has_cognito_session
    }

    /// The authoritative "is the operator signed in?" verdict, and the exact
    /// predicate `check_auth_status` reports as `authenticated`.
    ///
    /// An explicit logout WINS over credential presence: the
    /// autonomy-preserving `logout` deliberately keeps the Cognito session (so
    /// background sessions keep running) and immediately re-mints a device JWT,
    /// so without the marker check [`Self::has_local_signed_in_session`] would
    /// report the operator as still signed in and the logout would not stick.
    ///
    /// Conversely, nothing else may report signed-out: no idle timer, no
    /// token-expiry check, no failed backend round-trip. Sign-out is an explicit
    /// user act only.
    pub fn is_interactively_signed_in(&self) -> bool {
        if self.is_interactive_signed_out() {
            info!("Interactive session was explicitly signed out — reporting unauthenticated");
            return false;
        }
        if !self.has_local_signed_in_session() {
            info!("No local signed-in session — user not authenticated");
            return false;
        }
        true
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

    /// Should the encrypted store's cached `device_id` be re-pointed at
    /// `canonical` (the `machine.json` identity)?
    ///
    /// `true` only when the store is READABLE and disagrees. The
    /// present-but-unreadable case is deliberately excluded even though it
    /// "disagrees" — [`SecureStorage::get_device_id`] returns `Err` for both
    /// "nothing cached" and "store undecryptable", while
    /// [`Self::store_device_id`] REFUSES to write over a present-but-unreadable
    /// store (`load_tokens_for_write`, Merge mode). So on a renamed or moved
    /// box — where the hostname+username-derived AES key no longer matches, the
    /// exact scenario the canonical-identity fix exists for — a naive
    /// `get_device_id().ok() != Some(canonical)` test is true FOREVER: every
    /// call logged an `info!`, attempted a doomed write and logged a `warn!`.
    ///
    /// That is a hot path (`commands::workflow_events` constructs a fresh
    /// `AuthManager` and calls `get_device_id` at five sites; the frontend auth
    /// poll adds `check_auth_status`), and the id returned is correct either
    /// way — so the whole loop produced nothing but log noise. Repairing the
    /// store is the operator's explicit re-sign-in path
    /// (`commands::auth::reset_credential_store`), not a background read's job.
    fn should_repoint_device_id_cache(&self, canonical: &str) -> bool {
        if self.secure_storage.is_present_but_unreadable() {
            debug!(
                "Device ID cache not re-pointed: the credential store is present but unreadable \
                 (repair it by signing in again). The canonical machine.json identity is \
                 returned regardless."
            );
            return false;
        }
        self.secure_storage.get_device_id().ok().as_deref() != Some(canonical)
    }

    /// Retrieves the machine's device ID.
    ///
    /// Resolution order — **`machine.json` is canonical and wins outright**:
    ///
    /// 1. `~/.qontinui/machine.json` — the machine's one durable identity,
    ///    minted once at first launch and re-presented forever after.
    /// 2. `auth_tokens.enc` (the encrypted store) — a **cache** of (1), and
    ///    the answer when the machine has no `machine.json` at all.
    /// 3. OS keychain — legacy, migrated into (2) on read.
    /// 4. A fresh UUID v4, minted and stored.
    ///
    /// Step 1 is the fix for a real defect: this method used to start at step
    /// 2 and never read `machine.json`, so the runner carried **two
    /// independent device identities** that converged only after a successful
    /// pair (`pair::persist_pairing` → `store_device_id_fresh`). The
    /// secure-store id is shipped to the web backend by `commands/clipboard`,
    /// `commands/workflow_events` and `commands/auth`, and the store's AES key
    /// derives from hostname+username — so a rename or disk move made it
    /// unreadable and step 4 minted *again*, producing another
    /// `coord.devices` row for the same physical machine. Plan
    /// `2026-08-06-device-identity-is-per-profile-not-per-machine` §0.1.
    ///
    /// # Errors
    ///
    /// Returns an error if storage operations fail.
    pub fn get_device_id(&self) -> Result<String> {
        // 1. Canonical machine identity. Read-only; never mints, never writes
        //    machine.json.
        if let Some(path) = &self.machine_file {
            match crate::machine_identity::read_device_id_at(path) {
                Ok(id) => {
                    // Keep the encrypted store as a CACHE of the canonical id
                    // (best effort — a stale cache must never fail the read).
                    //
                    // Skip the re-point entirely when the store is present but
                    // UNREADABLE. `get_device_id()` returns `Err` for both "no
                    // id cached" and "store undecryptable", and
                    // `store_device_id` REFUSES to write over an unreadable
                    // store (`secure_storage::load_tokens_for_write`, Merge
                    // mode) — so on a renamed/moved box, where the
                    // hostname+username-derived AES key no longer matches, the
                    // divergence test is permanently true and every call logged
                    // an `info!`, attempted a doomed write and logged a `warn!`.
                    // This is a HOT path (`commands::workflow_events` builds a
                    // fresh `AuthManager` per call, plus the frontend auth
                    // poll), and the id returned is already correct, so the only
                    // product of retrying is log noise.
                    if self.should_repoint_device_id_cache(&id) {
                        info!(
                            "Device ID cache re-pointed at canonical machine.json identity: {}",
                            id
                        );
                        if let Err(e) = self.store_device_id(&id) {
                            warn!("Failed to cache canonical device ID: {}", e);
                        }
                    }
                    return Ok(id);
                }
                Err(e) => {
                    debug!(
                        "machine.json unavailable ({}); falling back to the encrypted device-ID store",
                        e
                    );
                }
            }
        }

        // 2. Encrypted file storage (cache / no-machine.json fallback).
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

    /// Explicit-acquisition variant of [`Self::store_oauth_tokens`]: overwrites
    /// a present-but-unreadable file store from blank rather than refusing.
    /// Called only by `finalize_signed_in` step 2 (the first write of an
    /// interactive Cognito sign-in), so it heals an undecryptable `.enc` the
    /// operator is deliberately re-authenticating over. The background
    /// refresher's Cognito write uses [`Self::store_oauth_tokens`].
    pub fn store_oauth_tokens_fresh(
        &self,
        access_token: &str,
        id_token: &str,
        refresh_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        self.secure_storage
            .store_oauth_tokens_fresh(access_token, id_token, refresh_token, expires_at)
            .context("Failed to store Cognito tokens in secure storage")?;
        info!("Cognito tokens stored successfully in secure storage (fresh/overwrite)");
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

    /// Retrieve the device-bound machine key (`dmk_<token>`), if present.
    /// `Ok(None)` when the device was never issued one. Thin pass-through to
    /// [`SecureStorage::get_device_machine_key`] so the device-JWT refresher's
    /// Phase-4b cold-start exchange reads it through the same (test-injectable)
    /// storage as every other credential.
    pub fn get_device_machine_key(&self) -> Result<Option<String>> {
        self.secure_storage.get_device_machine_key()
    }

    /// `true` iff the Cognito access token is missing OR within
    /// [`COGNITO_REFRESH_BEFORE_EXPIRY_SECS`] of expiry. Used by the device-JWT
    /// refresher to refresh the Cognito token *first* when it's stale (so the
    /// subsequent device re-bind presents a fresh user bearer).
    ///
    /// Deliberately NOT [`REFRESH_BEFORE_EXPIRY_SECS`]: that threshold is
    /// TTL/3 of coord's 4-hour DEVICE JWT (4_800s), which is longer than the
    /// Cognito access token lives (3_600s). Borrowing it made this predicate
    /// return `true` even 80ms after a successful refresh, so every
    /// `check_auth_status` forced a blocking Cognito `refresh_token` grant —
    /// which then consumed the caller's whole enrichment budget and left the
    /// UI on its "Checking authentication…" shell.
    pub fn cognito_token_needs_refresh(&self) -> bool {
        match self.secure_storage.get_oauth_expires_at() {
            Some(exp) => chrono::Utc::now().timestamp() + COGNITO_REFRESH_BEFORE_EXPIRY_SECS >= exp,
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

    /// Explicit-acquisition variant of [`Self::store_tenant_device_jwt`]:
    /// overwrites a present-but-unreadable file store from blank rather than
    /// refusing. The FIRST write of the explicit pairing path
    /// (`pair::persist_pairing`), so on an undecryptable `.enc` it heals the
    /// store for the rest of that pairing sequence. The background refresher's
    /// per-tenant write uses [`Self::store_tenant_device_jwt`].
    pub fn store_tenant_device_jwt_fresh(&self, tenant_id: &Uuid, jwt: &str) -> Result<()> {
        self.secure_storage
            .store_tenant_device_jwt_fresh(tenant_id, jwt)
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
    device_bearer_for(None)
}

/// Tenant-selecting variant of [`device_bearer`] — the Phase 8b credential
/// seam (plan `2026-07-02-session-scoped-multi-tenant-device-binding` §D4).
///
/// - `None` → the DEFAULT binding's JWT from the legacy `access_token` slot
///   (byte-identical to the pre-8b [`device_bearer`] behavior, so every
///   unparameterized caller keeps the default slot by construction).
/// - `Some(t)` → that tenant's `device_jwt:<t>` slot. On a slot MISS:
///   - when `t` IS the default binding (per `paired_user.json`), fall back
///     to the legacy `access_token` slot — it holds the same binding's JWT
///     (and is the only slot on a pre-8a install);
///   - otherwise return `None` (send unauthenticated) and warn once per
///     tenant per process. FAIL-SOFT POSTURE (decided here): a slot miss for
///     a non-default tenant must NEVER silently present another tenant's
///     credential — a wrong claim would be attributed cross-tenant coord-side
///     with no observable. Unauthenticated requests instead flow through
///     coord's server-side resolution (explicit payload tenant / sole-binding
///     / legacy-pointer window), which is counted and 422s honestly.
pub fn device_bearer_for(tenant: Option<&Uuid>) -> Option<String> {
    select_device_bearer(&AuthManager::new(), tenant, default_binding_tenant())
}

/// Pure-over-injected-parts core of [`device_bearer_for`] so slot selection
/// is hermetically testable (temp-dir [`SecureStorage::with_path`] +
/// explicit `default_tenant`, no process-global env mutation).
pub(crate) fn select_device_bearer(
    am: &AuthManager,
    tenant: Option<&Uuid>,
    default_tenant: Option<Uuid>,
) -> Option<String> {
    let Some(t) = tenant else {
        return legacy_slot_bearer(am);
    };
    match am.get_tenant_device_jwt(t) {
        Ok(Some(jwt)) if !jwt.trim().is_empty() => return Some(jwt),
        Ok(_) => {}
        Err(e) => {
            debug!("coord data-plane: tenant {t} device-JWT slot read failed ({e})");
        }
    }
    // Slot miss. The default binding may legitimately live only in the
    // legacy slot (pre-8a install, or a pairing that predates per-tenant
    // slots) — that slot IS this tenant's JWT, so fall back to it.
    if default_tenant.as_ref() == Some(t) {
        return legacy_slot_bearer(am);
    }
    warn_once_per_tenant_slot_miss(t);
    None
}

/// Read the legacy `access_token` slot (the DEFAULT binding's JWT) with the
/// original never-fatal posture + once-per-process missing-token warning.
fn legacy_slot_bearer(am: &AuthManager) -> Option<String> {
    match am.get_access_token() {
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

/// Warn once per (tenant, process) that a requested non-default tenant has
/// no device-JWT slot — the heal is pairing this runner for that tenant.
fn warn_once_per_tenant_slot_miss(tenant: &Uuid) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<Uuid>>> =
        std::sync::OnceLock::new();
    let set = WARNED.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let mut guard = set.lock().expect("tenant slot-miss warn set poisoned");
    if guard.insert(*tenant) {
        warn!(
            "coord data-plane: no device-JWT slot for tenant {tenant} — sending that \
             tenant's coord calls unauthenticated (never another tenant's credential); \
             pair this runner for that tenant to authenticate"
        );
    }
}

/// The device's DEFAULT binding tenant, read from `paired_user.json`
/// (v2 `default_tenant_id`, legacy `tenant_id` fallback). Kept as a local
/// minimal reader because `auth` compiles into BOTH the lib and bin crates
/// while `pair` (the canonical v2-aware reader) is lib-only — same
/// documented duplication pattern as the census/backstop `machine.json`
/// readers. `None` on any failure (unpaired runner).
fn default_binding_tenant() -> Option<Uuid> {
    let base = std::env::var("QONTINUI_SECURE_STORAGE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("com.qontinui.runner")))?;
    let bytes = std::fs::read(base.join("paired_user.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value
        .get("default_tenant_id")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("tenant_id").and_then(|v| v.as_str()))?;
    Uuid::parse_str(raw.trim()).ok()
}

/// How many tenant bindings this device holds, per `paired_user.json`.
///
/// The D2 degrade rule keys on this count, so its failure direction is chosen
/// deliberately: **every unreadable state counts as ONE.** A device that cannot
/// state its bindings is not evidence of a multi-tenant device, and the cost of
/// the two errors is wildly asymmetric — under-counting leaves today's
/// behaviour exactly as it is, while over-counting degrades live writes to
/// unauthenticated on a machine that was fine. That is the same priority
/// `coord_mcp::session_tenant_or_refuse` recorded when it refused to fail
/// closed on an `Unpinned` machine: *"the failure mode of refusing too eagerly
/// is an outage on healthy machines, which is strictly worse than the silent
/// default-tenant write on a machine that is genuinely broken."*
///
/// Counts the v2 `bindings` array when present, else the legacy single
/// `tenant_id` entry (mirroring `pair::PairedUserFile::effective_bindings`).
/// Kept as a local minimal reader for the same reason [`default_binding_tenant`]
/// is: `auth` compiles into BOTH the lib and bin crates while `pair` is
/// lib-only.
fn device_binding_count() -> usize {
    let Some(base) = std::env::var("QONTINUI_SECURE_STORAGE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("com.qontinui.runner")))
    else {
        return 1;
    };
    let Ok(bytes) = std::fs::read(base.join("paired_user.json")) else {
        return 1;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return 1;
    };
    binding_count_from_value(&value)
}

/// The parse half of [`device_binding_count`], split out so the v2/legacy
/// asymmetry is testable without touching the filesystem.
///
/// A `bindings` array that is present but EMPTY falls through to the legacy
/// shape rather than reporting zero — `effective_bindings` does the same, and
/// an empty array is an unpaired or half-written file, not a statement that the
/// device holds no tenant.
pub(crate) fn binding_count_from_value(value: &serde_json::Value) -> usize {
    if let Some(arr) = value.get("bindings").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            return arr.len();
        }
    }
    // Legacy single-entry shape: one binding iff it names a tenant at all.
    if value.get("tenant_id").and_then(|v| v.as_str()).is_some() {
        return 1;
    }
    1
}

/// What a call site knows about the tenant that owns the row it is writing —
/// D1's classification key, made typed.
///
/// ## Why a type and not `Option<Uuid>`
///
/// `Option<Uuid>` collapses two outcomes that MUST diverge on a multi-bound
/// device:
///
/// - *"this row has no tenant dimension — it is keyed by `device_id` and the
///   default binding is correct by construction"*, and
/// - *"this row does have an owning tenant and I could not work out which"*.
///
/// Both spell `None`, and `None` presents the DEFAULT binding's credential. So
/// on a device paired to more than one tenant the second case silently writes a
/// row under the wrong tenant — on every route where coord derives ownership
/// from the verified bearer (`ident.require_tenant()`), which has no fallback.
/// That is the entire defect this type closes, and the collapse is why 52 call
/// sites accumulated under a doc comment that already told them not to.
///
/// It is the same fix, one layer down, that [`crate::session::tenant_pin::TenantPin`]
/// made for `machine.json`: separate the legitimate absence from the failure so
/// the fail-closed decision becomes expressible.
///
/// | Variant | Meaning | Credential presented |
/// |---|---|---|
/// | [`TenantScope::Owned`] | the owning tenant is known | that binding's `device_jwt:<t>` slot |
/// | [`TenantScope::Device`] | the bearer carries no tenancy on this route | the default binding's slot |
/// | [`TenantScope::Unresolved`] | the row has an owner the caller could not resolve | the default slot on a single-bound device; **nothing** on a multi-bound one |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantScope {
    /// The owning tenant is known. Present THAT binding's slot, and never
    /// another's — slot-miss posture is [`select_device_bearer`]'s.
    Owned(Uuid),
    /// The bearer carries no tenancy on this route, so the default binding's
    /// credential is correct by construction and stays correct however many
    /// tenants are paired. Two positively-established shapes reach this, and
    /// they are one decision, not two:
    ///
    /// - the row genuinely has no tenant dimension — it is keyed by
    ///   `device_id` alone (heartbeats, device registration, device
    ///   maintenance, the continuation acks); and
    /// - the row HAS a tenant, but coord derives it from a field the caller
    ///   already supplies and never reads the bearer at all (`POST
    ///   /agents/{id}/log` and `/ask-question` resolve it from the path
    ///   `agent_id`; the session-handle register from the handle).
    ///
    /// In both, selecting a different slot changes nothing about where the row
    /// lands — which is exactly what makes the default correct rather than
    /// merely tolerated. What this variant must NEVER mean is "I could not work
    /// it out"; that is [`TenantScope::Unresolved`].
    Device,
    /// This row HAS an owning tenant, and the caller could not resolve it.
    /// Distinct from [`TenantScope::Device`] precisely so the degrade below can
    /// fire without touching the callers for which the default is right.
    Unresolved,
}

impl TenantScope {
    /// Classify a tenant read from a SESSION (the registry's stamped
    /// `intent.tenant_id`, or an intent the caller is holding).
    ///
    /// Absence is [`TenantScope::Unresolved`], never [`TenantScope::Device`]:
    /// a session-owned row always has an owning tenant, so a `None` here is a
    /// resolution FAILURE — the registry no longer holds the session, the id
    /// was never a registry id, or the session was stamped before a default
    /// existed. Calling that `Device` would re-collapse the two outcomes this
    /// type exists to separate.
    pub fn for_session(tenant: Option<Uuid>) -> Self {
        match tenant {
            Some(t) => TenantScope::Owned(t),
            None => TenantScope::Unresolved,
        }
    }

    /// Classify a tenant read from the DEVICE's own default binding
    /// (`machine.json::active_tenant_id` via
    /// `crate::session::dual_write::resolve_active_tenant_id` /
    /// `fleet::resolve_tenant_id`).
    ///
    /// Absence is [`TenantScope::Device`], not `Unresolved`: the machine
    /// declaring no default IS the legitimate single-tenant shape (the MSI
    /// ships `machine.json` without the field — the same `Unpinned` vs
    /// `Unresolvable` split `crate::session::tenant_pin::TenantPin` draws).
    /// Nothing failed, so nothing should degrade.
    pub fn for_device_default(tenant: Option<Uuid>) -> Self {
        match tenant {
            Some(t) => TenantScope::Owned(t),
            None => TenantScope::Device,
        }
    }

    /// The tenant to DECLARE in a request body, for routes that carry a
    /// `tenant_id` field (D1's rule: populate the field AND present that
    /// tenant's bearer, because fixing only one half fixes only one class).
    ///
    /// `Device` and `Unresolved` both yield `None` — declare nothing and let
    /// coord's server-side resolution decide, which it counts. Writing a
    /// guessed tenant into the body would be strictly worse than the bearer
    /// defect: on a body-derived route it is the row's tenancy, not a metric.
    pub fn declared_tenant(self) -> Option<Uuid> {
        match self {
            TenantScope::Owned(t) => Some(t),
            TenantScope::Device | TenantScope::Unresolved => None,
        }
    }
}

/// Resolve the bearer for a typed scope — the D2 degrade rule, in one place.
///
/// Pure over its injected parts (an [`AuthManager`] over a temp-dir
/// [`SecureStorage::with_path`], an explicit default tenant, an explicit
/// binding count) so every cell of the table below is hermetically testable
/// with no process-global env mutation.
///
/// | Scope | Single-bound device | Multi-bound device |
/// |---|---|---|
/// | `Owned(t)` | `t`'s slot | `t`'s slot |
/// | `Device` | default slot | default slot |
/// | `Unresolved` | default slot — **unchanged** | **unauthenticated** |
///
/// The `Unresolved` row is the whole rule. A blanket *"unresolvable → send
/// nothing"* would regress every correctly-configured single-tenant machine,
/// where the default binding simply IS the owning tenant and today's write is
/// right; conditioning on the binding count collapses the rule to exactly that
/// blanket form on precisely the machines where the hazard is real.
pub(crate) fn select_scoped_bearer(
    am: &AuthManager,
    scope: TenantScope,
    default_tenant: Option<Uuid>,
    binding_count: usize,
) -> Option<String> {
    select_scoped_bearer_lazy(am, scope, default_tenant, || binding_count)
}

/// [`select_scoped_bearer`] with the binding count read on demand.
///
/// Only the `Unresolved` arm consults it, and reading it costs a
/// `paired_user.json` parse (see [`device_binding_count`]). Every outbound
/// coord call goes through this, so the `Owned`/`Device` paths — which are all
/// but a handful of them — must not pay for a fact they never look at. The
/// eager wrapper above is what the hermetic tests drive, because a plain
/// `usize` is the honest shape for a table-driven assertion.
pub(crate) fn select_scoped_bearer_lazy(
    am: &AuthManager,
    scope: TenantScope,
    default_tenant: Option<Uuid>,
    binding_count: impl FnOnce() -> usize,
) -> Option<String> {
    match scope {
        TenantScope::Owned(t) => select_device_bearer(am, Some(&t), default_tenant),
        TenantScope::Device => select_device_bearer(am, None, default_tenant),
        TenantScope::Unresolved => {
            let count = binding_count();
            if count > 1 {
                warn_once_unresolved_on_multi_bound(count);
                return None;
            }
            select_device_bearer(am, None, default_tenant)
        }
    }
}

/// Scope-selecting sibling of [`device_bearer_for`] — the single resolver both
/// transports of the credential seam share.
pub fn device_bearer_scoped(scope: TenantScope) -> Option<String> {
    select_scoped_bearer_lazy(
        &AuthManager::new(),
        scope,
        default_binding_tenant(),
        device_binding_count,
    )
}

/// Warn once per process that an unresolved-tenant write degraded to
/// unauthenticated on a multi-bound device.
///
/// Once per process, not per call: the sites that degrade include periodic
/// loops, and a per-call warning would bury the signal it exists to raise.
fn warn_once_unresolved_on_multi_bound(binding_count: usize) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        warn!(
            "coord data-plane: a tenant-owned write could not resolve its owning tenant on a \
             device holding {binding_count} bindings — sending it UNAUTHENTICATED rather than \
             presenting the default binding's credential, which would attribute the row to the \
             wrong tenant. Coord's server-side resolution decides the outcome and counts it."
        );
    });
}

/// Attach the device-JWT bearer to a coord data-plane request when one is
/// available, otherwise return the builder unchanged. Also drives the
/// auth-coverage metric (the dogfood signal Phase 1 gates on): every call is
/// counted in `DATA_PLANE_TOTAL`, and calls that carried the header in
/// `DATA_PLANE_AUTHED`. A coverage summary is emitted at info level every 25th
/// call (rate-floored to one per minute by [`coverage_log_due`]) so an operator
/// can watch the unpaired→paired transition without a new dependency.
///
/// **Scope.** The counters cover calls routed through this module. Coord writes
/// that deliberately present a different credential are annotated
/// `coord-auth-exempt(<kind>)` at the call site and pinned by `coord_auth_pin`;
/// they are counted by neither term, which is why the summary line names its
/// own scope.
///
/// **This wrapper ASSERTS [`TenantScope::Device`]** — "the bearer carries no
/// tenancy on this route". That is a claim about the route, not a shrug, so
/// every call site must declare which class it is in with a
/// `coord-tenant-scope(<kind>)` annotation that `coord_auth_pin` enforces and
/// counts. A caller whose row does have an owner it could not name states
/// [`TenantScope::Unresolved`] through [`attach_device_auth_for`] instead —
/// that is the distinction the whole type exists for.
pub fn attach_device_auth(rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    attach_device_auth_for(rb, TenantScope::Device)
}

/// The tenant-STATING form of [`attach_device_auth`] — the one seam where a
/// caller says which tenant owns the row it is writing (Phase 8b, plan §D4;
/// Phase 5 of `2026-08-29-runner-work-scoped-writes-default-tenant-credential`).
///
/// **The parameter is a [`TenantScope`], not an `Option<Uuid>`, and that is the
/// whole point.** It used to take `Option<&Uuid>`, where `None` meant two
/// irreconcilable things — *"this route takes no tenancy from the bearer"* and
/// *"this row has an owner and I could not work out who"* — and silently
/// presented the DEFAULT binding's credential for both. On a multi-bound device
/// the second is a cross-tenant write on every route where coord derives
/// ownership from the verified bearer (`ident.require_tenant()`, which has no
/// fallback). A caller can no longer spell that ambiguity: it states
/// [`TenantScope::Device`], [`TenantScope::Owned`] or [`TenantScope::Unresolved`],
/// and the D2 degrade in [`select_scoped_bearer`] does the rest.
///
/// Slot-miss posture is [`device_bearer_for`]'s and is unchanged: never another
/// tenant's credential — degrade to unauthenticated.
pub fn attach_device_auth_for(
    rb: reqwest::RequestBuilder,
    scope: TenantScope,
) -> reqwest::RequestBuilder {
    match count_and_resolve_bearer(scope) {
        Some(token) => rb.header("Authorization", format!("Bearer {token}")),
        None => rb,
    }
}

/// Blocking-client sibling of [`attach_device_auth_for`], for the targets that
/// run without a tokio runtime (`qontinui_profile device init` →
/// `register_with_coord`, and the blocking session/log registrars).
///
/// Deliberately a thin transport adapter over the SAME
/// [`count_and_resolve_bearer`] core rather than a second implementation: the
/// token source, the never-fatal posture and the `DATA_PLANE_TOTAL` /
/// `DATA_PLANE_AUTHED` counters are shared by construction. A duplicated
/// resolver would under-report coverage, and coverage is precisely the signal
/// the plan `2026-08-03-per-instance-device-identity` Phase 3(b) enforcement
/// flip is gated on — a metric that silently omits a caller is worse than no
/// metric, because it reads as 100%.
///
/// The alternative — dragging the CLI onto an async runtime just to reuse the
/// async builder — buys nothing: `reqwest::blocking::RequestBuilder::header`
/// takes the identical header pair, so the adapter is one match arm.
///
/// Takes a [`TenantScope`] for the same reason [`attach_device_auth_for`] does,
/// and NOT as a convenience: its callers post bodies declaring an explicit
/// `tenant_id`, and [`select_device_bearer`]'s fail-soft invariant is that a
/// request for tenant X must never carry tenant Y's credential. A
/// tenant-less blocking helper could not honour that on a multi-bound box —
/// it would silently present the default binding's JWT against a request
/// declaring another tenant. There is no unparameterized twin here precisely
/// so that door cannot be opened by accident.
pub fn attach_device_auth_blocking(
    rb: reqwest::blocking::RequestBuilder,
    scope: TenantScope,
) -> reqwest::blocking::RequestBuilder {
    match count_and_resolve_bearer(scope) {
        Some(token) => rb.header("Authorization", format!("Bearer {token}")),
        None => rb,
    }
}

/// Count one outbound data-plane call and resolve the bearer to present on it.
///
/// The transport-independent core of [`attach_device_auth_for`] and
/// [`attach_device_auth_blocking`]. Returns `None` when no credential is held
/// (unpaired runner, non-default tenant slot miss) or when the D2 degrade
/// fired (an `Unresolved` scope on a multi-bound device) — the caller then
/// sends the request unauthenticated, which coord still accepts.
///
/// The returned token is only ever moved into a request header; it must never
/// reach a log line or a process argument.
fn count_and_resolve_bearer(scope: TenantScope) -> Option<String> {
    let total = DATA_PLANE_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let bearer = device_bearer_scoped(scope);
    if bearer.is_some() {
        DATA_PLANE_AUTHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if total.is_multiple_of(25) && coverage_log_due() {
        let authed = DATA_PLANE_AUTHED.load(std::sync::atomic::Ordering::Relaxed);
        let pct = (authed as f64 / total as f64) * 100.0;
        // The scope qualifier is not decoration. This counter measures calls
        // that pass through THIS module, and the runner also makes coord
        // writes that present a credential this module never resolves — the
        // per-agent JWT, a forwarded acting bearer, the pairing bootstrap
        // (see the `coord-auth-exempt(...)` annotations, pinned by
        // `coord_auth_pin`). Those are counted by NEITHER term. Calling the
        // ratio "coord data-plane auth coverage" claimed the fleet and
        // measured a subset, which is the same defect one level up as the
        // omission this plan closed: a metric whose name overstates its
        // scope reads as 100% of something it never looked at.
        info!(
            "coord data-plane device-JWT coverage: {authed}/{total} ({pct:.0}%) \
             (device-JWT-eligible coord calls only; agent-JWT, forwarded-bearer \
             and pair-bootstrap sites are out of scope by design)"
        );
    }
    bearer
}

/// Minimum wall-clock gap between two coverage summaries, in seconds.
const COVERAGE_LOG_MIN_GAP_SECS: u64 = 60;

/// Epoch-seconds of the last emitted coverage summary. `0` = never.
static COVERAGE_LOG_LAST_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Rate floor under the every-25th-call coverage summary.
///
/// The every-25th trigger alone was calibrated for a low-rate data plane. The
/// tree publisher walks every governed repo on the box on a 60s cadence and
/// posts one request each, so on this machine it alone drives hundreds of
/// data-plane calls per cycle — which would emit the summary a dozen times a
/// minute and bury the signal it exists to provide. Both conditions must hold,
/// so the summary stays a per-25-calls sample on a quiet runner and becomes a
/// per-minute one on a busy one.
///
/// Deliberately NOT a "reset the counter" scheme: `DATA_PLANE_TOTAL` /
/// `DATA_PLANE_AUTHED` are cumulative for the process, because the Phase 3(b)
/// flip predicate is "coverage reached 100%", which a windowed counter cannot
/// answer.
fn coverage_log_due() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = COVERAGE_LOG_LAST_SECS.load(std::sync::atomic::Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < COVERAGE_LOG_MIN_GAP_SECS {
        return false;
    }
    // Compare-exchange so concurrent callers cannot both pass the gap check
    // and double-log; the loser simply skips this summary.
    COVERAGE_LOG_LAST_SECS
        .compare_exchange(
            last,
            now,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_ok()
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

    /// As [`create_test_auth_manager`], but with an explicit `machine.json`
    /// path so the canonical-identity branch is exercised WITHOUT touching the
    /// real `~/.qontinui/machine.json`.
    fn create_test_auth_manager_with_machine_file(
        test_name: &str,
        machine_file: std::path::PathBuf,
    ) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage_and_machine_file(storage, machine_file)
    }

    // ------------------------------------------------------------------
    // ONE device identity per machine — plan
    // `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2(c).
    //
    // `get_device_id` used to keep its OWN id in `auth_tokens.enc` and never
    // read `machine.json`, so the runner shipped two unreconciled identities
    // to two different backends and minted a third whenever the encrypted
    // store became unreadable (its AES key derives from hostname+username).
    // ------------------------------------------------------------------

    /// `machine.json` is consulted BEFORE minting: an empty encrypted store
    /// plus a present `machine.json` must yield the machine.json id, not a
    /// fresh UUID.
    #[test]
    fn device_id_reads_machine_json_before_minting() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json");
        fs::write(
            &machine_file,
            br#"{"device_id":"c79a07d5-0000-4000-8000-00000000000a","hostname":"spaceship"}"#,
        )
        .unwrap();
        let mgr = create_test_auth_manager_with_machine_file(
            "device_id_reads_machine_json_before_minting",
            machine_file,
        );
        assert_eq!(
            mgr.get_device_id().unwrap(),
            "c79a07d5-0000-4000-8000-00000000000a"
        );
    }

    /// A DIVERGENT cached id in `auth_tokens.enc` must lose to `machine.json`
    /// and be re-pointed at it — this is the exact "two identities" state the
    /// old code produced before a successful pair converged them.
    #[test]
    fn machine_json_overrides_a_divergent_secure_storage_cache() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json");
        fs::write(
            &machine_file,
            br#"{"device_id":"c79a07d5-0000-4000-8000-00000000000b","hostname":"spaceship"}"#,
        )
        .unwrap();
        let mgr = create_test_auth_manager_with_machine_file(
            "machine_json_overrides_a_divergent_secure_storage_cache",
            machine_file.clone(),
        );
        // Seed the cache with a DIFFERENT identity (the second-identity bug).
        mgr.store_device_id("deadbeef-0000-4000-8000-00000000ffff")
            .unwrap();

        let id = mgr.get_device_id().unwrap();
        assert_eq!(
            id, "c79a07d5-0000-4000-8000-00000000000b",
            "machine.json is canonical; the encrypted store is only a cache"
        );
        // The cache was re-pointed, so subsequent reads agree even offline.
        assert_eq!(
            mgr.secure_storage.get_device_id().unwrap(),
            "c79a07d5-0000-4000-8000-00000000000b"
        );
        // And the canonical file was NOT rewritten by the read.
        let raw = fs::read_to_string(&machine_file).unwrap();
        assert!(raw.contains("c79a07d5-0000-4000-8000-00000000000b"));
        assert!(!raw.contains("deadbeef"));
    }

    /// A legacy `machine_id`-spelled file is still the canonical identity.
    #[test]
    fn device_id_accepts_legacy_machine_id_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json");
        fs::write(
            &machine_file,
            br#"{"machine_id":"c79a07d5-0000-4000-8000-00000000000c","hostname":"spaceship"}"#,
        )
        .unwrap();
        let mgr = create_test_auth_manager_with_machine_file(
            "device_id_accepts_legacy_machine_id_spelling",
            machine_file,
        );
        assert_eq!(
            mgr.get_device_id().unwrap(),
            "c79a07d5-0000-4000-8000-00000000000c"
        );
    }

    /// Genuinely absent `machine.json` → previous behaviour is preserved: mint
    /// into the encrypted store, and stay stable across calls.
    #[test]
    fn device_id_falls_back_to_mint_when_machine_json_absent() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json"); // never created
        let mgr = create_test_auth_manager_with_machine_file(
            "device_id_falls_back_to_mint_when_machine_json_absent",
            machine_file.clone(),
        );
        let first = mgr.get_device_id().unwrap();
        assert!(Uuid::parse_str(&first).is_ok());
        assert_eq!(mgr.get_device_id().unwrap(), first, "must not re-mint");
        assert!(
            !machine_file.exists(),
            "the auth path must never create machine.json"
        );
    }

    /// On a PRESENT-BUT-UNREADABLE credential store the cache re-point is
    /// skipped, not retried forever.
    ///
    /// `SecureStorage::get_device_id()` is `Err` for both "nothing cached" and
    /// "undecryptable", and `store_device_id` refuses to write over an
    /// unreadable store — so the naive divergence test stayed true on every
    /// call. On a renamed/moved box (the scenario this whole change exists
    /// for) that meant an `info!` + a doomed write + a `warn!` on every one of
    /// `commands::workflow_events`' five `get_device_id` calls and every
    /// frontend auth poll, permanently. The id returned was always correct, so
    /// it was pure noise.
    #[test]
    fn device_id_cache_is_not_repointed_over_an_unreadable_store() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json");
        fs::write(
            &machine_file,
            br#"{"device_id":"c79a07d5-0000-4000-8000-00000000000d","hostname":"spaceship"}"#,
        )
        .unwrap();
        let test_name = "device_id_cache_is_not_repointed_over_an_unreadable_store";
        let mgr = create_test_auth_manager_with_machine_file(test_name, machine_file);
        // A store that EXISTS but cannot be decrypted/parsed — what a machine
        // rename or disk move leaves behind (the AES key derives from
        // hostname+username). Same path the helper handed the manager.
        let store_path = env::temp_dir()
            .join("qontinui_test_auth")
            .join(format!("{test_name}.enc"));
        fs::write(&store_path, b"not-ciphertext-at-all").unwrap();
        assert!(
            mgr.secure_storage.is_present_but_unreadable(),
            "fixture must actually be a present-but-unreadable store"
        );

        assert!(
            !mgr.should_repoint_device_id_cache("c79a07d5-0000-4000-8000-00000000000d"),
            "an unreadable store must not trigger a re-point that can only fail"
        );
        // The read still answers correctly, and does not damage the store.
        assert_eq!(
            mgr.get_device_id().unwrap(),
            "c79a07d5-0000-4000-8000-00000000000d"
        );
        assert_eq!(fs::read(&store_path).unwrap(), b"not-ciphertext-at-all");
    }

    /// The complement: on a READABLE store the re-point still fires when the
    /// cache diverges (or is empty), and stays quiet once it agrees.
    #[test]
    fn device_id_cache_is_repointed_only_while_a_readable_store_diverges() {
        let mgr = create_test_auth_manager("device_id_cache_repoint_readable_store");
        const CANONICAL: &str = "c79a07d5-0000-4000-8000-00000000000e";

        // No store file yet → nothing cached → re-point.
        assert!(mgr.should_repoint_device_id_cache(CANONICAL));

        mgr.store_device_id("deadbeef-0000-4000-8000-00000000ffff")
            .unwrap();
        assert!(
            mgr.should_repoint_device_id_cache(CANONICAL),
            "a divergent readable cache must be re-pointed"
        );

        mgr.store_device_id(CANONICAL).unwrap();
        assert!(
            !mgr.should_repoint_device_id_cache(CANONICAL),
            "an agreeing cache must not be rewritten on every read"
        );
    }

    /// An unreadable/corrupt `machine.json` must not wedge sign-in: fall back
    /// to the cache rather than erroring, but never overwrite the file.
    #[test]
    fn corrupt_machine_json_falls_back_without_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let machine_file = dir.path().join("machine.json");
        fs::write(&machine_file, b"{ this is not json").unwrap();
        let mgr = create_test_auth_manager_with_machine_file(
            "corrupt_machine_json_falls_back_without_overwriting_it",
            machine_file.clone(),
        );
        let id = mgr.get_device_id().unwrap();
        assert!(Uuid::parse_str(&id).is_ok());
        assert_eq!(
            fs::read_to_string(&machine_file).unwrap(),
            "{ this is not json",
            "the corrupt file must be left exactly as found for inspection"
        );
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

/// Tests for the "is the operator signed in?" verdict — the predicate
/// `check_auth_status` reports as `authenticated`, and the whole point of the
/// no-auto-logout work: nothing but an EXPLICIT user act may report signed-out.
#[cfg(test)]
mod signed_in_verdict_tests {
    use super::*;
    use base64::Engine;
    use std::env;
    use std::fs;

    fn create_test_auth_manager(test_name: &str) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth_verdict");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage(storage)
    }

    /// Syntactically-valid (unsigned) JWT with the given `exp`.
    fn jwt_with_exp(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    /// PRESENCE, not freshness. An expired device JWT is a refresher problem,
    /// never a logout — the old freshness gate (`device_jwt_needs_refresh() ==
    /// Ok(false)`) reported an install with no Cognito refresh token as
    /// unauthenticated a full 80 minutes before the JWT actually expired, and
    /// dropped the operator to the LoginScreen.
    #[test]
    fn presence_not_freshness_decides_local_session() {
        let mgr = create_test_auth_manager("presence_not_freshness");
        assert!(
            !mgr.has_local_signed_in_session(),
            "no credentials at all must read as no local session"
        );

        // An EXPIRED device JWT and no Cognito session (legacy pair-code
        // install) is still a signed-in runner.
        let expired = jwt_with_exp(chrono::Utc::now().timestamp() - 10_000);
        mgr.store_tokens(&expired, "").unwrap();
        assert!(
            mgr.device_jwt_needs_refresh().unwrap(),
            "test precondition: this JWT is stale by the refresher's measure"
        );
        assert!(
            mgr.has_local_signed_in_session(),
            "a stale/expired device JWT must NOT read as signed out"
        );

        // A blank slot is not a credential.
        mgr.store_tokens("   ", "").unwrap();
        assert!(
            !mgr.has_local_signed_in_session(),
            "a whitespace-only device JWT must not count as a session"
        );

        // A Cognito session alone (device JWT momentarily absent, e.g. right
        // after a logout kicked the refresher) also means signed in.
        mgr.store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();
        assert!(
            mgr.has_local_signed_in_session(),
            "a stored Cognito session alone must read as signed in"
        );
    }

    /// THE invariant `check_auth_status` short-circuits on. The
    /// autonomy-preserving logout keeps the Cognito session AND immediately
    /// re-mints a device JWT, so credential presence alone would report the
    /// operator as still signed in — only the persisted marker makes the logout
    /// stick, and only an explicit sign-in may clear it.
    #[test]
    fn explicit_logout_beats_credential_presence_until_sign_in() {
        let mgr = create_test_auth_manager("logout_beats_presence");

        mgr.store_tokens("device.jwt", "").unwrap();
        mgr.store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();
        assert!(
            mgr.is_interactively_signed_in(),
            "credentials present and no logout ⇒ signed in"
        );

        // Autonomy-preserving logout.
        mgr.clear_interactive_session().unwrap();
        assert!(
            !mgr.is_interactively_signed_in(),
            "an explicit logout must report signed OUT"
        );
        assert!(
            mgr.has_local_signed_in_session(),
            "…while the autonomy credentials deliberately survive it"
        );

        // The refresher re-mints a device JWT seconds later (this is exactly
        // what `logout_impl`'s kick causes). It must NOT un-logout the operator.
        mgr.store_tokens("device.jwt.reminted", "").unwrap();
        assert!(
            !mgr.is_interactively_signed_in(),
            "a background device-JWT re-mint MUST NOT resurrect the session"
        );
        // Same for the refresher's Cognito refresh cycle.
        mgr.store_oauth_tokens("cog.access2", "cog.id2", "cog.refresh2", 1_700_000_001)
            .unwrap();
        assert!(
            !mgr.is_interactively_signed_in(),
            "a background Cognito refresh MUST NOT resurrect the session"
        );

        // The ONLY un-lockout path — an explicit interactive credential
        // acquisition (Cognito sign-in / pair-code redeem / CLI `device pair`)
        // clears the marker. A regression here is a TOTAL lockout: the operator
        // can sign in successfully and still be held at the LoginScreen.
        mgr.clear_interactive_signed_out().unwrap();
        assert!(
            mgr.is_interactively_signed_in(),
            "signing back in must end the logout"
        );
    }

    /// The full stop-autonomy sign-out reports signed out on BOTH counts, and
    /// re-pairing afterwards restores the session.
    #[test]
    fn full_sign_out_reports_signed_out_and_pairing_restores() {
        let mgr = create_test_auth_manager("full_sign_out_then_pair");
        mgr.store_tokens("device.jwt", "").unwrap();
        mgr.store_oauth_tokens("cog.access", "cog.id", "cog.refresh", 1_700_000_000)
            .unwrap();

        mgr.clear_all_credentials().unwrap();
        assert!(!mgr.has_local_signed_in_session());
        assert!(!mgr.is_interactively_signed_in());

        // CRITICAL-1 shape: pairing (pair-code redeem / CLI `device pair`)
        // writes a device JWT and then clears the marker. Writing the credential
        // WITHOUT clearing the marker leaves the operator dead-ended at the
        // LoginScreen with a perfectly valid, relay-online pairing.
        mgr.store_tokens("device.jwt.from.pair.code", "").unwrap();
        assert!(
            !mgr.is_interactively_signed_in(),
            "test precondition: the credential alone does not clear the marker"
        );
        mgr.clear_interactive_signed_out().unwrap();
        assert!(
            mgr.is_interactively_signed_in(),
            "a pair-code redeem that clears the marker must restore the session"
        );
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

/// Tests for the Phase 8b per-session credential SELECTION seam
/// ([`select_device_bearer`] — the injected core of [`device_bearer_for`] /
/// [`attach_device_auth_for`]). Hermetic: temp-dir storage + explicit
/// `default_tenant`, no env mutation, no network.
#[cfg(test)]
mod bearer_selection_tests {
    use super::*;
    use std::env;
    use std::fs;

    fn create_test_auth_manager(test_name: &str) -> AuthManager {
        let temp_dir = env::temp_dir().join("qontinui_test_auth_bearer_select");
        let storage_path = temp_dir.join(format!("{}.enc", test_name));
        let _ = fs::remove_file(&storage_path);
        let storage = SecureStorage::with_path(storage_path).unwrap();
        AuthManager::with_storage(storage)
    }

    fn tenant(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    /// No tenant in scope → the legacy `access_token` slot (default binding)
    /// — the by-construction guarantee for every unparameterized caller.
    #[test]
    fn no_tenant_selects_legacy_default_slot() {
        let mgr = create_test_auth_manager("no_tenant_default_slot");
        mgr.store_tokens("default.jwt", "").unwrap();
        let a = tenant(0xAA);
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();

        assert_eq!(
            select_device_bearer(&mgr, None, Some(a)).as_deref(),
            Some("default.jwt"),
            "None tenant must read the legacy slot, never a tenant slot"
        );
    }

    /// Session tenant with a populated slot → that slot's JWT, not the
    /// default.
    #[test]
    fn session_tenant_selects_its_own_slot() {
        let mgr = create_test_auth_manager("session_tenant_own_slot");
        mgr.store_tokens("default.jwt", "").unwrap();
        let a = tenant(0xA1);
        let b = tenant(0xB2);
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();
        mgr.store_tenant_device_jwt(&b, "jwt.b").unwrap();

        assert_eq!(
            select_device_bearer(&mgr, Some(&b), Some(a)).as_deref(),
            Some("jwt.b"),
            "a session-scoped call must present the session tenant's slot"
        );
        assert_eq!(
            select_device_bearer(&mgr, Some(&a), Some(a)).as_deref(),
            Some("jwt.a")
        );
    }

    /// DEFAULT-tenant slot miss falls back to the legacy slot — the legacy
    /// slot holds the same binding's JWT (pre-8a installs have only it).
    #[test]
    fn default_tenant_slot_miss_falls_back_to_legacy_slot() {
        let mgr = create_test_auth_manager("default_miss_legacy_fallback");
        mgr.store_tokens("default.jwt", "").unwrap();
        let a = tenant(0xC3);
        // No per-tenant slot stored for `a`, but `a` IS the default binding.
        assert_eq!(
            select_device_bearer(&mgr, Some(&a), Some(a)).as_deref(),
            Some("default.jwt"),
            "default-binding slot miss must fall back to access_token (same binding)"
        );
    }

    /// FAIL-SOFT posture: an unknown (non-default) tenant slot miss yields
    /// NO bearer — never another tenant's credential.
    #[test]
    fn unknown_tenant_slot_miss_sends_unauthenticated() {
        let mgr = create_test_auth_manager("unknown_miss_unauthenticated");
        mgr.store_tokens("default.jwt", "").unwrap();
        let default = tenant(0xD4);
        mgr.store_tenant_device_jwt(&default, "jwt.default")
            .unwrap();
        let stranger = tenant(0xE5);

        assert_eq!(
            select_device_bearer(&mgr, Some(&stranger), Some(default)),
            None,
            "non-default slot miss must degrade to unauthenticated, not cross-tenant"
        );
    }

    /// An empty/whitespace tenant-slot value counts as a miss and follows
    /// the same posture (default → legacy fallback; stranger → None).
    #[test]
    fn empty_slot_value_counts_as_miss() {
        let mgr = create_test_auth_manager("empty_slot_is_miss");
        mgr.store_tokens("default.jwt", "").unwrap();
        let a = tenant(0xF6);
        mgr.store_tenant_device_jwt(&a, "   ").unwrap();

        assert_eq!(
            select_device_bearer(&mgr, Some(&a), Some(a)).as_deref(),
            Some("default.jwt")
        );
        let default = tenant(0x11);
        assert_eq!(select_device_bearer(&mgr, Some(&a), Some(default)), None);
    }

    /// No default binding known (unpaired) + non-default tenant miss →
    /// None; None tenant still degrades to the legacy read (which may
    /// itself be empty → None).
    #[test]
    fn unpaired_runner_degrades_to_none() {
        let mgr = create_test_auth_manager("unpaired_degrades_none");
        let a = tenant(0x22);
        assert_eq!(select_device_bearer(&mgr, Some(&a), None), None);
        assert_eq!(select_device_bearer(&mgr, None, None), None);
    }

    // ========================================================================
    // D2 — the degrade rule, one test per cell of `select_scoped_bearer`'s
    // table. These are the Phase-4 pins: they must stay green while Phases 5
    // and 6 change which scope each call site declares, and they must FAIL if
    // a device-scoped caller is switched to a tenant slot.
    // ========================================================================

    /// `Device` presents the default binding's slot — on a SINGLE-bound device.
    /// The by-construction guarantee for heartbeat / registration / device
    /// maintenance, unchanged from the unparameterized wrapper.
    #[test]
    fn device_scope_presents_default_slot_when_single_bound() {
        let mgr = create_test_auth_manager("scope_device_single_bound");
        let a = tenant(0xA1);
        mgr.store_tokens("default.jwt", "").unwrap();
        assert_eq!(
            select_scoped_bearer(&mgr, TenantScope::Device, Some(a), 1).as_deref(),
            Some("default.jwt")
        );
    }

    /// THE Phase-4 invariant: a device-scoped caller is UNAFFECTED by the
    /// binding count. Pairing a second tenant must not change what a heartbeat
    /// presents — if this test ever fails, a `D`-scope site has been
    /// misclassified as tenant-owned.
    #[test]
    fn device_scope_is_unchanged_on_a_multi_bound_device() {
        let mgr = create_test_auth_manager("scope_device_multi_bound");
        let a = tenant(0xA2);
        let b = tenant(0xB2);
        mgr.store_tokens("default.jwt", "").unwrap();
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();
        mgr.store_tenant_device_jwt(&b, "jwt.b").unwrap();
        for count in [1usize, 2, 7] {
            assert_eq!(
                select_scoped_bearer(&mgr, TenantScope::Device, Some(a), count).as_deref(),
                Some("default.jwt"),
                "device-scoped selection must not vary with binding count ({count})"
            );
        }
    }

    /// `Owned(t)` presents `t`'s slot, not the default's — on both a single-
    /// and a multi-bound device.
    #[test]
    fn owned_scope_presents_that_tenants_slot() {
        let mgr = create_test_auth_manager("scope_owned_slot");
        let a = tenant(0xA3);
        let b = tenant(0xB3);
        mgr.store_tokens("default.jwt", "").unwrap();
        mgr.store_tenant_device_jwt(&b, "jwt.b").unwrap();
        for count in [1usize, 2] {
            assert_eq!(
                select_scoped_bearer(&mgr, TenantScope::Owned(b), Some(a), count).as_deref(),
                Some("jwt.b"),
                "Owned must select the owning tenant's slot (binding count {count})"
            );
        }
    }

    /// `Owned(t)` on a slot MISS for a non-default tenant sends nothing — it
    /// must never fall back to the default binding's credential. Inherited
    /// from `select_device_bearer`; pinned here so the scope layer cannot
    /// quietly reintroduce the substitution.
    #[test]
    fn owned_scope_never_substitutes_another_tenants_credential() {
        let mgr = create_test_auth_manager("scope_owned_slot_miss");
        let a = tenant(0xA4);
        let b = tenant(0xB4);
        mgr.store_tokens("default.jwt", "").unwrap();
        // No slot stored for `b`.
        assert_eq!(
            select_scoped_bearer(&mgr, TenantScope::Owned(b), Some(a), 2),
            None,
            "a slot miss for a non-default tenant must degrade to unauthenticated"
        );
    }

    /// D2's no-regression half: on a SINGLE-bound device an unresolved tenant
    /// still presents the default slot, because there the default binding IS
    /// the owning tenant. A blanket refusal here is the outage D2 exists to
    /// avoid.
    #[test]
    fn unresolved_scope_keeps_default_slot_on_single_bound_device() {
        let mgr = create_test_auth_manager("scope_unresolved_single");
        let a = tenant(0xA5);
        mgr.store_tokens("default.jwt", "").unwrap();
        for count in [0usize, 1] {
            assert_eq!(
                select_scoped_bearer(&mgr, TenantScope::Unresolved, Some(a), count).as_deref(),
                Some("default.jwt"),
                "single-bound (count {count}) must be unchanged"
            );
        }
    }

    /// D2's teeth: on a MULTI-bound device an unresolved tenant degrades to
    /// unauthenticated rather than presenting the default binding's credential,
    /// which coord would attribute to the wrong tenant.
    #[test]
    fn unresolved_scope_degrades_to_unauthenticated_on_multi_bound_device() {
        let mgr = create_test_auth_manager("scope_unresolved_multi");
        let a = tenant(0xA6);
        let b = tenant(0xB6);
        mgr.store_tokens("default.jwt", "").unwrap();
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();
        mgr.store_tenant_device_jwt(&b, "jwt.b").unwrap();
        assert_eq!(
            select_scoped_bearer(&mgr, TenantScope::Unresolved, Some(a), 2),
            None,
            "multi-bound + unresolved must send nothing, never the default binding's JWT"
        );
    }

    /// `Unresolved` and `Device` are the two `None`s the old `Option<Uuid>`
    /// collapsed. This test is the collapse, made visible: they agree on a
    /// single-bound device and DIVERGE on a multi-bound one. If they ever agree
    /// on a multi-bound device, the type has stopped earning its existence.
    #[test]
    fn unresolved_and_device_diverge_exactly_when_multi_bound() {
        let mgr = create_test_auth_manager("scope_two_nones_diverge");
        let a = tenant(0xA7);
        mgr.store_tokens("default.jwt", "").unwrap();
        assert_eq!(
            select_scoped_bearer(&mgr, TenantScope::Unresolved, Some(a), 1),
            select_scoped_bearer(&mgr, TenantScope::Device, Some(a), 1),
            "single-bound: the two former `None`s must behave identically"
        );
        assert_ne!(
            select_scoped_bearer(&mgr, TenantScope::Unresolved, Some(a), 2),
            select_scoped_bearer(&mgr, TenantScope::Device, Some(a), 2),
            "multi-bound: they must diverge — that divergence IS the fix"
        );
    }

    // ---- the constructors: which absence means which ----------------------

    /// `for_session` and `for_device_default` disagree on `None`, and that
    /// disagreement is the entire Phase-5 classification decision made in one
    /// place instead of at 12 call sites. A session-owned row always HAS an
    /// owning tenant, so a missing one is a FAILURE (`Unresolved`, degrades on
    /// a multi-bound device). A device that names no default binding is the
    /// legitimate single-tenant shape, so a missing one is not a failure
    /// (`Device`, never degrades).
    #[test]
    fn the_two_constructors_disagree_on_absence_and_agree_on_presence() {
        let t = tenant(0xC1);
        assert_eq!(TenantScope::for_session(Some(t)), TenantScope::Owned(t));
        assert_eq!(
            TenantScope::for_device_default(Some(t)),
            TenantScope::Owned(t)
        );
        assert_eq!(TenantScope::for_session(None), TenantScope::Unresolved);
        assert_eq!(TenantScope::for_device_default(None), TenantScope::Device);
        assert_ne!(
            TenantScope::for_session(None),
            TenantScope::for_device_default(None),
            "collapsing these back together is the defect Phase 5 closed"
        );
    }

    /// `declared_tenant` is D1's OTHER half: the value a body-derived route
    /// puts on the wire. It must be `Some` only for `Owned` — declaring a
    /// guessed tenant on `/agents/allocate` or `/claims/acquire` would BE the
    /// misattribution, not a metric about one, because there the body IS the
    /// tenancy and coord's extractor rejects nothing.
    #[test]
    fn declared_tenant_is_some_only_for_owned() {
        let t = tenant(0xC2);
        assert_eq!(TenantScope::Owned(t).declared_tenant(), Some(t));
        assert_eq!(TenantScope::Device.declared_tenant(), None);
        assert_eq!(TenantScope::Unresolved.declared_tenant(), None);
    }

    /// The lazy resolver must not read `paired_user.json` for a scope that
    /// never consults the count. Every outbound coord call goes through it, so
    /// an eager read would put a file parse on the hot path for a fact
    /// `Owned`/`Device` do not look at. Proven by a counter that panics if
    /// touched.
    #[test]
    fn only_the_unresolved_arm_reads_the_binding_count() {
        let mgr = create_test_auth_manager("scope_lazy_binding_count");
        let a = tenant(0xC3);
        mgr.store_tokens("default.jwt", "").unwrap();
        mgr.store_tenant_device_jwt(&a, "jwt.a").unwrap();

        for scope in [TenantScope::Device, TenantScope::Owned(a)] {
            let _ = select_scoped_bearer_lazy(&mgr, scope, Some(a), || {
                panic!("{scope:?} must not read the binding count")
            });
        }

        let mut reads = 0usize;
        let _ = select_scoped_bearer_lazy(&mgr, TenantScope::Unresolved, Some(a), || {
            reads += 1;
            1
        });
        assert_eq!(reads, 1, "Unresolved must consult the count exactly once");
    }

    // ---- binding count: the input the degrade rule keys on ----------------

    /// The v2 `bindings` array is the count when present and non-empty.
    #[test]
    fn binding_count_reads_the_v2_bindings_array() {
        let v: serde_json::Value = serde_json::json!({
            "user_id": "u",
            "default_tenant_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bindings": [
                {"tenant_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "user_id": "u"},
                {"tenant_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "user_id": "u"}
            ]
        });
        assert_eq!(binding_count_from_value(&v), 2);
    }

    /// A legacy v1 file naming one tenant counts as one binding.
    #[test]
    fn binding_count_handles_the_legacy_single_entry_shape() {
        let v: serde_json::Value = serde_json::json!({
            "user_id": "u",
            "tenant_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        });
        assert_eq!(binding_count_from_value(&v), 1);
    }

    /// Every degenerate shape counts as ONE, never as many. The asymmetry is
    /// deliberate: under-counting preserves today's behaviour, over-counting
    /// degrades live writes on a healthy machine.
    #[test]
    fn binding_count_fails_toward_one_never_toward_degrading() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({"user_id": "u"}),
            serde_json::json!({"user_id": "u", "bindings": []}),
            serde_json::json!({"bindings": "not-an-array"}),
            serde_json::json!({"tenant_id": 7}),
        ] {
            assert_eq!(
                binding_count_from_value(&v),
                1,
                "degenerate paired_user.json must count as one binding: {v}"
            );
        }
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

    #[test]
    fn cognito_threshold_is_ttl_over_three() {
        // Same one-third convention, but against Cognito's 1h access token.
        assert_eq!(COGNITO_REFRESH_BEFORE_EXPIRY_SECS, 1_200);
        assert_eq!(COGNITO_ACCESS_TOKEN_TTL_SECS, 3_600);
    }

    /// The invariant the whole fix rests on. A threshold at or above the
    /// token's own lifetime makes `cognito_token_needs_refresh` permanently
    /// true. The device-JWT constant (4_800s) violates it, which is exactly
    /// why Cognito may not borrow it.
    #[test]
    fn cognito_threshold_is_below_token_ttl() {
        assert!(
            COGNITO_REFRESH_BEFORE_EXPIRY_SECS < COGNITO_ACCESS_TOKEN_TTL_SECS,
            "a refresh threshold >= the token TTL makes every freshly-minted \
             token instantly stale (threshold={COGNITO_REFRESH_BEFORE_EXPIRY_SECS}, \
             ttl={COGNITO_ACCESS_TOKEN_TTL_SECS})"
        );
        assert!(
            REFRESH_BEFORE_EXPIRY_SECS >= COGNITO_ACCESS_TOKEN_TTL_SECS,
            "regression guard: the device-JWT threshold exceeds the Cognito \
             TTL, so reusing it for Cognito reintroduces the always-stale loop"
        );
    }

    /// The reported symptom, at the unit level: store tokens exactly as
    /// `refresh_cognito_bearer` does after a successful grant
    /// (`expires_at = now + expires_in`) and assert the very next staleness
    /// check says NO. Before the fix this returned `true` immediately, so
    /// every `check_auth_status` forced another blocking Cognito round-trip.
    #[test]
    fn freshly_refreshed_cognito_token_is_not_immediately_stale() {
        let mgr = create_test_auth_manager("freshly_refreshed_cognito_token_is_not_stale");
        let now = chrono::Utc::now().timestamp();
        mgr.store_oauth_tokens(
            "access",
            "id",
            "refresh",
            now + COGNITO_ACCESS_TOKEN_TTL_SECS,
        )
        .unwrap();
        assert!(
            !mgr.cognito_token_needs_refresh(),
            "a token stored one second ago with a full 1h TTL must not report stale"
        );
    }

    #[test]
    fn cognito_token_within_threshold_is_stale() {
        let mgr = create_test_auth_manager("cognito_token_within_threshold_is_stale");
        let now = chrono::Utc::now().timestamp();
        // 10 minutes left — inside the 20-minute threshold.
        mgr.store_oauth_tokens("access", "id", "refresh", now + 10 * 60)
            .unwrap();
        assert!(
            mgr.cognito_token_needs_refresh(),
            "a token inside COGNITO_REFRESH_BEFORE_EXPIRY_SECS must report stale"
        );
    }

    #[test]
    fn expired_cognito_token_is_stale() {
        let mgr = create_test_auth_manager("expired_cognito_token_is_stale");
        let now = chrono::Utc::now().timestamp();
        mgr.store_oauth_tokens("access", "id", "refresh", now - 60)
            .unwrap();
        assert!(
            mgr.cognito_token_needs_refresh(),
            "an already-expired token must report stale"
        );
    }
}
