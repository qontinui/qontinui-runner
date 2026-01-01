/**
 * Context Types for AI Task Guidance
 *
 * Contexts are reusable knowledge snippets that provide domain-specific guidance
 * to AI tasks. This file defines the TypeScript types for the runner frontend.
 *
 * The core Context type is imported from @qontinui/schemas. Runner-specific
 * extensions (scope, enabled, usage stats) are defined here.
 */

// Import base types from qontinui-schemas
// Note: These will be available after the schema package is updated
// For now, we define compatible local types

// ============================================================================
// Core Types (matching qontinui-schemas)
// ============================================================================

/**
 * Rules for automatically including a context in AI tasks.
 *
 * When an AI task is created, the runner evaluates these rules to determine
 * which contexts should be automatically included. Multiple rules are OR'd
 * together (any match triggers inclusion).
 */
export interface ContextAutoInclude {
  /** Keywords in task prompt that trigger inclusion (case-insensitive) */
  taskMentions?: string[] | null;
  /** Action types in loaded config that trigger inclusion (e.g., 'CLICK', 'FIND') */
  actionTypes?: string[] | null;
  /** Regex patterns in recent logs that trigger inclusion */
  errorPatterns?: string[] | null;
  /** Glob patterns for files being worked on (e.g., '*.rs', 'src/api/**') */
  filePatterns?: string[] | null;
}

/**
 * AI context for providing domain knowledge to AI tasks.
 *
 * Contexts are markdown documents that get injected into AI task prompts
 * to provide relevant background knowledge, coding standards, architectural
 * guidance, or debugging tips.
 */
export interface Context {
  /** Unique identifier (UUID v4 or prefixed like 'ctx-schema-flow') */
  id: string;
  /** Human-readable name for display */
  name: string;
  /** Markdown content injected into AI prompts */
  content: string;
  /** Category for organization (e.g., 'architecture', 'debugging', 'philosophy') */
  category?: string | null;
  /** Tags for flexible grouping and search */
  tags?: string[];
  /** Rules for automatic inclusion in AI tasks */
  autoInclude?: ContextAutoInclude | null;
  /** ISO 8601 creation timestamp */
  createdAt: string;
  /** ISO 8601 last modification timestamp */
  modifiedAt: string;
}

// ============================================================================
// Runner-Specific Extensions
// ============================================================================

/**
 * Scope of a context - where it's stored and who can access it.
 */
export type ContextScope = "project" | "user" | "builtin";

/**
 * Runner-specific metadata for a context.
 *
 * This extends the core Context with fields specific to the runner:
 * - enabled: allows disabling without deleting
 * - useCount: tracks popularity
 * - lastUsedAt: for sorting by recency
 */
export interface ContextMetadata {
  /** Reference to the context ID */
  contextId: string;
  /** Whether this context is enabled (can be disabled without deleting) */
  enabled: boolean;
  /** Number of times this context has been used */
  useCount: number;
  /** ISO 8601 timestamp of last use */
  lastUsedAt?: string | null;
}

/**
 * A context with its runner-specific metadata combined.
 * Used for display in the UI.
 */
export interface ContextWithMetadata extends Context {
  /** The scope of this context */
  scope: ContextScope;
  /** Whether this context is enabled */
  enabled: boolean;
  /** Number of times this context has been used */
  useCount: number;
  /** ISO 8601 timestamp of last use */
  lastUsedAt?: string | null;
}

// ============================================================================
// API Request/Response Types
// ============================================================================

/**
 * Request to create a new context
 */
export interface CreateContextRequest {
  name: string;
  content: string;
  category?: string | null;
  tags?: string[];
  autoInclude?: ContextAutoInclude | null;
}

/**
 * Request to update an existing context
 */
export interface UpdateContextRequest {
  name?: string;
  content?: string;
  category?: string | null;
  tags?: string[];
  autoInclude?: ContextAutoInclude | null;
}

/**
 * Context selection state for AI tasks
 */
export interface ContextSelection {
  /** IDs of selected contexts */
  selectedIds: string[];
  /** Whether to use auto-detection */
  autoDetect: boolean;
}

/**
 * Result of auto-detection evaluation
 */
export interface AutoDetectResult {
  /** Context ID */
  contextId: string;
  /** Context name for display */
  contextName: string;
  /** Why this context was auto-detected */
  reason: "taskMention" | "actionType" | "errorPattern" | "filePattern" | string;
  /** The specific trigger that matched */
  matchedTrigger: string;
}

// ============================================================================
// UI State Types
// ============================================================================

/**
 * Filter/sort options for the context list
 */
export interface ContextFilterOptions {
  /** Filter by scope */
  scope?: ContextScope | "all";
  /** Filter by category */
  category?: string | "all";
  /** Search query (matches name, content, tags) */
  searchQuery?: string;
  /** Sort by field */
  sortBy?: "name" | "createdAt" | "modifiedAt" | "useCount" | "lastUsedAt";
  /** Sort direction */
  sortDirection?: "asc" | "desc";
  /** Only show enabled contexts */
  enabledOnly?: boolean;
}

/**
 * State for the context editor dialog
 */
export interface ContextEditorState {
  /** Whether the editor is open */
  isOpen: boolean;
  /** The context being edited (null for new context) */
  context: ContextWithMetadata | null;
  /** Whether this is a new context or editing existing */
  mode: "create" | "edit" | "view";
  /** The scope to create in (for new contexts) */
  createScope?: "project" | "user";
}
