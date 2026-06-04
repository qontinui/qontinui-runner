//! Secure file-based storage for authentication tokens.
//!
//! This module provides encrypted file storage as a reliable alternative to
//! the OS keychain, which has proven unreliable on Windows.
//!
//! Security model:
//! - Tokens are encrypted using AES-256-GCM
//! - Encryption key is derived from machine-specific identifiers
//! - Storage file is placed in the app's data directory
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
use tracing::{debug, info};

/// Service identifier for the storage
const SERVICE_NAME: &str = "com.qontinui.runner";

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
}

/// Secure file-based storage manager.
///
/// Provides encrypted storage for authentication tokens using AES-256-GCM.
/// The encryption key is derived from machine-specific identifiers to ensure
/// tokens can only be read on the same machine.
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

        Ok(Self { storage_path })
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

    /// Saves tokens to encrypted storage.
    fn save_tokens(&self, tokens: &StoredTokens) -> Result<()> {
        let json = serde_json::to_vec(tokens).context("Failed to serialize tokens")?;

        let encrypted = self.encrypt(&json)?;

        fs::write(&self.storage_path, encrypted).context("Failed to write storage file")?;

        debug!("Saved tokens to secure storage");
        Ok(())
    }

    /// Stores both access and refresh tokens.
    pub fn store_tokens(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        let mut tokens = self.load_tokens().unwrap_or_default();
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
    pub fn clear_tokens(&self) -> Result<()> {
        let mut tokens = self.load_tokens().unwrap_or_default();
        tokens.access_token = None;
        tokens.refresh_token = None;
        tokens.oauth_access_token = None;
        tokens.oauth_id_token = None;
        tokens.oauth_refresh_token = None;
        tokens.oauth_expires_at = None;
        self.save_tokens(&tokens)?;
        info!("Tokens cleared from secure file storage");
        Ok(())
    }

    /// Stores the device ID.
    pub fn store_device_id(&self, device_id: &str) -> Result<()> {
        let mut tokens = self.load_tokens().unwrap_or_default();
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
        let mut tokens = self.load_tokens().unwrap_or_default();
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
        let mut tokens = self.load_tokens().unwrap_or_default();
        tokens.oauth_access_token = None;
        tokens.oauth_id_token = None;
        tokens.oauth_refresh_token = None;
        tokens.oauth_expires_at = None;
        self.save_tokens(&tokens)?;
        info!("Cognito (oauth) tokens cleared from secure file storage");
        Ok(())
    }

    /// Deletes the storage file entirely.
    #[allow(dead_code)]
    pub fn delete_storage(&self) -> Result<()> {
        if self.storage_path.exists() {
            fs::remove_file(&self.storage_path).context("Failed to delete storage file")?;
            info!("Secure storage file deleted");
        }
        Ok(())
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

    /// A pre-Phase-5 `StoredTokens` JSON (only the original three keys) must
    /// still deserialize — the new oauth_* fields carry `#[serde(default)]`.
    #[test]
    fn test_legacy_stored_tokens_without_oauth_fields_deserializes() {
        let raw = r#"{"access_token":"a","refresh_token":"r","device_id":"d"}"#;
        let parsed: StoredTokens = serde_json::from_str(raw).expect("legacy shape must decode");
        assert_eq!(parsed.access_token.as_deref(), Some("a"));
        assert!(parsed.oauth_access_token.is_none());
        assert!(parsed.oauth_expires_at.is_none());
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
