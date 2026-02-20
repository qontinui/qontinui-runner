/**
 * Prompt Snippet Types
 *
 * Prompt snippets are reusable text snippets that capture learnings from AI debugging sessions.
 * They can be inserted into Playwright test descriptions to provide context and guidance.
 */

/**
 * A reusable text snippet for test descriptions
 */
export interface PromptSnippet {
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

  /** AI loop IDs that were used to generate this prompt snippet */
  source_log_ids?: string[];

  /** Creation timestamp (ISO 8601) */
  created_at: string;

  /** Last modification timestamp (ISO 8601) */
  modified_at: string;
}

/**
 * Request to create a new prompt snippet
 */
export interface CreatePromptSnippetRequest {
  name: string;
  content: string;
  category?: string;
  tags?: string[];
  source_log_ids?: string[];
}

/**
 * Request to update an existing prompt snippet
 */
export interface UpdatePromptSnippetRequest {
  name?: string;
  content?: string;
  category?: string;
  tags?: string[];
}
