/**
 * Saved API Request Types
 *
 * Types for saved API request templates in the Library.
 * These templates can be reused across workflows.
 */

import type {
  HttpMethod,
  ApiContentType,
  ApiVariableExtraction,
  ApiAssertion,
} from "./unified-workflow";

/**
 * A saved API request template in the Library
 */
export interface SavedApiRequest {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the request */
  name: string;
  /** Optional description of what this request does */
  description: string;
  /** Category for organization */
  category: string;
  /** Tags for filtering/searching */
  tags: string[];

  // Request configuration
  /** HTTP method */
  method: HttpMethod;
  /** URL (may contain {{variables}}) */
  url: string;
  /** Request headers */
  headers: Record<string, string>;
  /** Request body */
  body?: string;
  /** Content type for the body */
  body_content_type?: ApiContentType;
  /** Timeout in milliseconds */
  timeout_ms: number;
  /** Whether to follow redirects */
  follow_redirects: boolean;

  /** Variable extractions from response */
  variable_extractions: ApiVariableExtraction[];
  /** Assertions for response verification */
  assertions: ApiAssertion[];

  /** Credential ID for authentication */
  credential_id?: string;

  /** ISO 8601 timestamp of creation */
  created_at: string;
  /** ISO 8601 timestamp of last modification */
  updated_at: string;
}

/**
 * Request body for creating a new saved API request
 */
export interface CreateSavedApiRequestRequest {
  name: string;
  description?: string;
  category?: string;
  tags?: string[];
  method: HttpMethod;
  url: string;
  headers?: Record<string, string>;
  body?: string;
  body_content_type?: ApiContentType;
  timeout_ms?: number;
  follow_redirects?: boolean;
  variable_extractions?: ApiVariableExtraction[];
  assertions?: ApiAssertion[];
  credential_id?: string;
}

/**
 * Request body for updating a saved API request
 */
export interface UpdateSavedApiRequestRequest {
  name?: string;
  description?: string;
  category?: string;
  tags?: string[];
  method?: HttpMethod;
  url?: string;
  headers?: Record<string, string>;
  body?: string;
  body_content_type?: ApiContentType;
  timeout_ms?: number;
  follow_redirects?: boolean;
  variable_extractions?: ApiVariableExtraction[];
  assertions?: ApiAssertion[];
  credential_id?: string;
}
