/**
 * Type definitions for API credentials
 *
 * Credentials are used to authenticate API requests. The actual secret values
 * are stored in encrypted secure storage, while metadata is stored in the database.
 */

// =============================================================================
// Credential Types
// =============================================================================

/**
 * Types of credentials supported for API authentication.
 */
export type CredentialType = "bearer_token" | "basic_auth" | "api_key" | "oauth2";

/**
 * Storage type for credentials.
 * - secure: Encrypted file storage (persisted)
 * - session: Memory only (cleared on restart)
 */
export type CredentialStorageType = "secure" | "session";

// =============================================================================
// Credential Metadata (stored in database)
// =============================================================================

/**
 * API credential metadata stored in the database.
 * Actual secret values are stored separately in secure storage.
 */
export interface ApiCredential {
  /** Unique identifier */
  id: string;
  /** Display name for the credential */
  name: string;
  /** Type of credential */
  credential_type: CredentialType;
  /** Where the credential is stored */
  storage_type: CredentialStorageType;

  // OAuth2 specific fields
  /** Token endpoint URL for OAuth2 token refresh */
  token_endpoint?: string;
  /** Client ID for OAuth2 */
  client_id?: string;

  // Timestamps
  /** When the credential was created */
  created_at: string;
  /** When the credential was last updated */
  updated_at: string;
  /** When the credential expires (for tokens) */
  expires_at?: string;
}

// =============================================================================
// Credential Values (stored in secure storage, never in database)
// =============================================================================

/**
 * Credential secret values.
 * These are never stored in the database - only in encrypted secure storage.
 */
export interface CredentialValue {
  /** For bearer_token type */
  access_token?: string;

  /** For basic_auth type */
  username?: string;
  password?: string;

  /** For api_key type */
  api_key?: string;
  /** Header name for API key (e.g., "X-API-Key", "Authorization") */
  header_name?: string;

  /** For oauth2 type */
  refresh_token?: string;
}

// =============================================================================
// Credential Operations
// =============================================================================

/**
 * Request to create a new credential.
 */
export interface CreateCredentialRequest {
  name: string;
  credential_type: CredentialType;
  storage_type?: CredentialStorageType;
  value: CredentialValue;

  // OAuth2 specific
  token_endpoint?: string;
  client_id?: string;
  client_secret?: string;
}

/**
 * Request to update an existing credential.
 */
export interface UpdateCredentialRequest {
  name?: string;
  value?: CredentialValue;

  // OAuth2 specific
  token_endpoint?: string;
  client_id?: string;
  client_secret?: string;
}

/**
 * Result of a credential operation.
 */
export interface CredentialOperationResult {
  success: boolean;
  credential?: ApiCredential;
  error?: string;
}

/**
 * Result of listing credentials.
 */
export interface ListCredentialsResult {
  success: boolean;
  credentials: ApiCredential[];
  error?: string;
}

// =============================================================================
// Credential Application (for API requests)
// =============================================================================

/**
 * How to apply a credential to an HTTP request.
 */
export interface AppliedCredential {
  /** Header name to set */
  header_name: string;
  /** Header value to set */
  header_value: string;
}

/**
 * Get the header application for a credential.
 */
export function getCredentialHeader(
  credentialType: CredentialType,
  value: CredentialValue,
): AppliedCredential | null {
  switch (credentialType) {
    case "bearer_token":
      if (value.access_token) {
        return {
          header_name: "Authorization",
          header_value: `Bearer ${value.access_token}`,
        };
      }
      break;

    case "basic_auth":
      if (value.username && value.password) {
        const encoded = btoa(`${value.username}:${value.password}`);
        return {
          header_name: "Authorization",
          header_value: `Basic ${encoded}`,
        };
      }
      break;

    case "api_key":
      if (value.api_key) {
        return {
          header_name: value.header_name || "X-API-Key",
          header_value: value.api_key,
        };
      }
      break;

    case "oauth2":
      if (value.access_token) {
        return {
          header_name: "Authorization",
          header_value: `Bearer ${value.access_token}`,
        };
      }
      break;
  }

  return null;
}
