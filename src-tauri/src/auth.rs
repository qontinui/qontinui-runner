//! Authentication manager for secure token storage and device management.
//!
//! This module provides secure storage of authentication tokens using the OS keychain,
//! as well as device ID management for runner registration.

use anyhow::{Context, Result};
use keyring::Entry;
use tracing::{error, info};
use uuid::Uuid;

/// Service name used for keychain entries
const SERVICE_NAME: &str = "com.qontinui.runner";

/// Manages authentication tokens and device ID storage using OS keychain.
///
/// The AuthManager provides secure storage for:
/// - Access tokens (JWT)
/// - Refresh tokens
/// - Device ID (persistent UUID)
///
/// All data is stored in the OS-provided secure credential storage:
/// - macOS: Keychain
/// - Windows: Credential Manager
/// - Linux: Secret Service API / libsecret
pub struct AuthManager {
    service_name: String,
}

impl AuthManager {
    /// Creates a new AuthManager instance with the default service name.
    pub fn new() -> Self {
        Self {
            service_name: SERVICE_NAME.to_string(),
        }
    }

    /// Stores both access and refresh tokens in the keychain.
    ///
    /// # Arguments
    ///
    /// * `access_token` - JWT access token
    /// * `refresh_token` - JWT refresh token
    ///
    /// # Errors
    ///
    /// Returns an error if the keychain operations fail.
    pub fn store_tokens(&self, access_token: &str, refresh_token: &str) -> Result<()> {
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

        info!("Tokens stored successfully in keychain");
        Ok(())
    }

    /// Retrieves the access token from the keychain.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is not found or keychain access fails.
    pub fn get_access_token(&self) -> Result<String> {
        let entry = Entry::new(&self.service_name, "access_token")
            .context("Failed to create keychain entry for access token")?;
        entry
            .get_password()
            .context("Failed to retrieve access token from keychain")
    }

    /// Retrieves the refresh token from the keychain.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is not found or keychain access fails.
    pub fn get_refresh_token(&self) -> Result<String> {
        let entry = Entry::new(&self.service_name, "refresh_token")
            .context("Failed to create keychain entry for refresh token")?;
        entry
            .get_password()
            .context("Failed to retrieve refresh token from keychain")
    }

    /// Clears both access and refresh tokens from the keychain.
    ///
    /// This is typically called during logout. Errors during deletion are logged
    /// but not propagated, as the tokens may already be deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if keychain entry creation fails.
    pub fn clear_tokens(&self) -> Result<()> {
        let entry_access = Entry::new(&self.service_name, "access_token")
            .context("Failed to create keychain entry for access token")?;
        let entry_refresh = Entry::new(&self.service_name, "refresh_token")
            .context("Failed to create keychain entry for refresh token")?;

        // Ignore errors if tokens don't exist
        if let Err(e) = entry_access.delete_password() {
            error!("Failed to delete access token (may not exist): {}", e);
        }
        if let Err(e) = entry_refresh.delete_password() {
            error!("Failed to delete refresh token (may not exist): {}", e);
        }

        info!("Tokens cleared from keychain");
        Ok(())
    }

    /// Stores the device ID in the keychain.
    ///
    /// # Arguments
    ///
    /// * `device_id` - UUID representing this device
    ///
    /// # Errors
    ///
    /// Returns an error if the keychain operations fail.
    pub fn store_device_id(&self, device_id: &str) -> Result<()> {
        let entry = Entry::new(&self.service_name, "device_id")
            .context("Failed to create keychain entry for device_id")?;
        entry
            .set_password(device_id)
            .context("Failed to store device_id in keychain")?;

        info!("Device ID stored in keychain: {}", device_id);
        Ok(())
    }

    /// Retrieves or generates a device ID.
    ///
    /// If a device ID is already stored, it is returned. Otherwise, a new UUID v4
    /// is generated, stored, and returned.
    ///
    /// # Errors
    ///
    /// Returns an error if keychain operations fail.
    pub fn get_device_id(&self) -> Result<String> {
        let entry = Entry::new(&self.service_name, "device_id")
            .context("Failed to create keychain entry for device_id")?;

        match entry.get_password() {
            Ok(id) => {
                info!("Retrieved existing device ID: {}", id);
                Ok(id)
            }
            Err(_) => {
                // Generate new device ID
                let new_id = Uuid::new_v4().to_string();
                info!("Generated new device ID: {}", new_id);
                self.store_device_id(&new_id)?;
                Ok(new_id)
            }
        }
    }

    /// Checks if the user is authenticated by verifying token existence.
    ///
    /// Returns true if both access and refresh tokens exist in the keychain.
    pub fn has_tokens(&self) -> bool {
        self.get_access_token().is_ok() && self.get_refresh_token().is_ok()
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
        let device_id2 = auth_manager.get_device_id().unwrap();
        assert_eq!(device_id1, device_id2, "Device ID should be persistent");
    }
}
