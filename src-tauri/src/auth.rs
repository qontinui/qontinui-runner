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
use keyring::Entry;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Service name used for keychain entries (legacy)
const SERVICE_NAME: &str = "com.qontinui.runner";

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
        if let Ok(entry) = Entry::new(&self.service_name, "device_id") {
            let _ = entry.set_password(device_id);
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
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_id_generation() {
        let auth_manager = AuthManager::new();
        let device_id = auth_manager.get_device_id().unwrap();
        assert!(!device_id.is_empty());
        assert!(Uuid::parse_str(&device_id).is_ok());
    }

    #[test]
    fn test_device_id_persistence() {
        let auth_manager = AuthManager::new();
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
        let auth_manager = AuthManager::new();

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
