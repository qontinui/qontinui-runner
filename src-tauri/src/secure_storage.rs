//! Secure file-based storage for authentication tokens.
//!
//! This module provides encrypted file storage as a reliable alternative to
//! the OS keychain, which has proven unreliable on Windows.
//!
//! Security model:
//! - Tokens are encrypted using AES-256-GCM
//! - Encryption key is derived from machine-specific identifiers
//! - Storage file is placed in the app's data directory

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
#[derive(Debug, Serialize, Deserialize, Default)]
struct StoredTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    device_id: Option<String>,
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
        let data_dir = dirs::data_local_dir()
            .context("Failed to get local data directory")?
            .join(SERVICE_NAME);

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

    /// Clears all tokens from storage.
    pub fn clear_tokens(&self) -> Result<()> {
        let mut tokens = self.load_tokens().unwrap_or_default();
        tokens.access_token = None;
        tokens.refresh_token = None;
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

    /// Checks if tokens exist in storage.
    pub fn has_tokens(&self) -> bool {
        match self.load_tokens() {
            Ok(tokens) => tokens.access_token.is_some() && tokens.refresh_token.is_some(),
            Err(_) => false,
        }
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
