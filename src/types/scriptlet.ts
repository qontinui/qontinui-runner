/**
 * Scriptlet Types
 *
 * Scriptlets are reusable text snippets that capture learnings from AI debugging sessions.
 * They can be inserted into Playwright script descriptions to provide context and guidance.
 */

/**
 * A reusable text snippet for script descriptions
 */
export interface Scriptlet {
  /** Unique identifier (UUID v4) */
  id: string;

  /** Short descriptive name */
  name: string;

  /** The actual text content to insert */
  content: string;

  /** Category for organization (e.g., "Login", "Navigation", "Forms") */
  category: string;

  /** Tags for flexible grouping */
  tags: string[];

  /** AI loop IDs that were used to generate this scriptlet */
  source_log_ids?: string[];

  /** Creation timestamp (ISO 8601) */
  created_at: string;

  /** Last modification timestamp (ISO 8601) */
  modified_at: string;
}

/**
 * Request to create a new scriptlet
 */
export interface CreateScriptletRequest {
  name: string;
  content: string;
  category?: string;
  tags?: string[];
  source_log_ids?: string[];
}

/**
 * Request to update an existing scriptlet
 */
export interface UpdateScriptletRequest {
  name?: string;
  content?: string;
  category?: string;
  tags?: string[];
}
