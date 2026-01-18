/**
 * Type definitions for saved prompts in the prompt library.
 *
 * Prompts can be saved and reused across workflows. Each prompt has a unique ID,
 * content, and optional configuration for AI provider/model overrides.
 */

/**
 * A saved prompt from the prompt library.
 */
export interface SavedPrompt {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the prompt */
  name: string;
  /** Optional description of what this prompt does */
  description: string;
  /** The actual prompt content to send to the AI */
  content: string;
  /** Category for organization (e.g., "Development", "Testing", "Deployment") */
  category: string;
  /** Tags for filtering/searching */
  tags: string[];
  /** Maximum number of sessions (null = unlimited) */
  max_sessions: number | null;
  /** AI provider override (e.g., "claude_cli", "gemini_api") */
  provider?: string | null;
  /** AI model override (e.g., "gemini-3-flash-preview", "claude-sonnet-4") */
  model?: string | null;
  /** Whether this prompt requires the orchestrator for planning and verification */
  requires_orchestrator?: boolean;
  /** Goal description for the orchestrator */
  orchestrator_goal?: string | null;
  /** Maximum iterations for the orchestrator */
  orchestrator_max_iterations?: number | null;
  /** ISO 8601 timestamp of creation */
  created_at: string;
  /** ISO 8601 timestamp of last modification */
  modified_at: string;
}

/**
 * Request to create a new saved prompt.
 */
export interface CreateSavedPromptRequest {
  name: string;
  description?: string;
  content: string;
  category?: string;
  tags?: string[];
  max_sessions?: number | null;
  provider?: string | null;
  model?: string | null;
  requires_orchestrator?: boolean;
  orchestrator_goal?: string | null;
  orchestrator_max_iterations?: number | null;
}

/**
 * Request to update an existing saved prompt.
 */
export interface UpdateSavedPromptRequest {
  name?: string;
  description?: string;
  content?: string;
  category?: string;
  tags?: string[];
  max_sessions?: number | null;
  provider?: string | null;
  model?: string | null;
  requires_orchestrator?: boolean;
  orchestrator_goal?: string | null;
  orchestrator_max_iterations?: number | null;
}
