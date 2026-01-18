/**
 * Type definitions for saved shell commands in the shell command library.
 *
 * Shell commands can be saved and reused across workflows. Each command has a unique ID,
 * the command to execute, and optional configuration for working directory and timeout.
 */

/**
 * A saved shell command from the library.
 */
export interface SavedShellCommand {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the command */
  name: string;
  /** Optional description of what this command does */
  description?: string;
  /** The actual shell command to execute */
  command: string;
  /** Working directory for command execution */
  working_directory?: string;
  /** Timeout in seconds (default: 60) */
  timeout_seconds: number;
  /** Whether to fail the workflow if the command fails */
  fail_on_error: boolean;
  /** Category for organization */
  category?: string;
  /** Tags for filtering/searching */
  tags: string[];
  /** Whether the command is enabled */
  enabled: boolean;
  /** ISO 8601 timestamp of creation */
  created_at: string;
  /** ISO 8601 timestamp of last modification */
  updated_at: string;
}

/**
 * Request to create a new saved shell command.
 */
export interface CreateSavedShellCommandRequest {
  name: string;
  description?: string;
  command: string;
  working_directory?: string;
  timeout_seconds?: number;
  fail_on_error?: boolean;
  category?: string;
  tags?: string[];
}

/**
 * Request to update an existing saved shell command.
 */
export interface UpdateSavedShellCommandRequest {
  name?: string;
  description?: string;
  command?: string;
  working_directory?: string;
  timeout_seconds?: number;
  fail_on_error?: boolean;
  category?: string;
  tags?: string[];
  enabled?: boolean;
}
