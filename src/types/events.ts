/**
 * Event type definitions for hierarchical action logging
 *
 * This module defines TypeScript interfaces for representing action execution
 * and workflow events in a hierarchical structure, supporting nested workflows
 * and parent-child relationships between actions.
 */

/**
 * Metadata describing the hierarchical position and context of an event
 * within the action execution tree.
 */
export interface HierarchyMetadata {
  /**
   * ID of the parent event (workflow or action) in the hierarchy.
   * Null for top-level events.
   */
  parent_id: string | null;

  /**
   * Zero-based nesting depth in the hierarchy.
   * 0 = top-level, 1 = first nested level, etc.
   */
  nesting_level: number;

  /**
   * Name of the workflow this event belongs to.
   * Null for events not part of a workflow.
   */
  workflow_name: string | null;

  /**
   * Whether this event can be expanded to show child events.
   * True for workflows and composite actions with children.
   */
  is_expandable: boolean;
}

/**
 * Event representing the execution of a single action.
 * Contains all execution details including success status, configuration,
 * and error information.
 */
export interface ActionExecutionEvent {
  /**
   * Unique identifier for this event, used as React key.
   */
  id: string;

  /**
   * Unix timestamp (milliseconds) when the action was executed.
   */
  timestamp: number;

  /**
   * Unique identifier for the action instance.
   */
  action_id: string;

  /**
   * Type of action executed (e.g., "click", "type", "navigate").
   */
  action_type: string;

  /**
   * Whether the action executed successfully.
   */
  success: boolean;

  /**
   * Number of execution attempts made for this action.
   */
  attempts: number;

  /**
   * Configuration object passed to the action.
   * Structure varies by action_type.
   */
  config: Record<string, unknown>;

  /**
   * Text that was typed during the action execution.
   * Populated for typing actions, null otherwise.
   */
  typed_text?: string | null;

  /**
   * Text that was typed (alternative field name).
   */
  text?: string;

  /**
   * Error message if the action failed.
   * Null if success is true.
   */
  error: string | null;

  /**
   * Human-readable reason for action failure or success details.
   * Null if not provided.
   */
  reason: string | null;

  /**
   * Hierarchical metadata for this action event.
   */
  hierarchy: HierarchyMetadata;

  /**
   * Flag indicating this is not a workflow event (always false).
   * Used for type discrimination in union types.
   */
  is_workflow: false;

  // Action-specific optional fields

  /**
   * Target states for GO_TO_STATE actions.
   */
  target_states?: string[];

  /**
   * Workflow ID for RUN_WORKFLOW actions.
   */
  workflow_id?: string;

  /**
   * Workflow name for RUN_WORKFLOW actions.
   */
  workflow_name?: string;

  /**
   * X coordinate for CLICK/MOUSE_MOVE actions.
   */
  x?: number;

  /**
   * Y coordinate for CLICK/MOUSE_MOVE actions.
   */
  y?: number;

  /**
   * Image ID for image-based actions (CLICK, FIND, etc.).
   */
  image_id?: string;

  /**
   * Whether image was found (for FIND actions).
   */
  found?: boolean;

  /**
   * Number of matches found (for FIND actions).
   */
  matches?: number;

  /**
   * Duration for WAIT actions (in milliseconds).
   */
  duration?: number;

  /**
   * Condition result for IF actions (true/false).
   */
  condition_result?: boolean;
}

/**
 * Base interface for workflow-related events.
 * Contains common fields shared by workflow start and completion events.
 */
export interface WorkflowEvent {
  /**
   * Unique identifier for this event, used as React key.
   */
  id: string;

  /**
   * Unix timestamp (milliseconds) when the workflow event occurred.
   */
  timestamp: number;

  /**
   * Unique identifier for the workflow instance.
   */
  workflow_id: string;

  /**
   * Human-readable name of the workflow.
   */
  workflow_name: string;

  /**
   * Flag indicating this is a workflow event (always true).
   * Used for type discrimination in union types.
   */
  is_workflow: boolean;

  /**
   * Hierarchical metadata for this workflow event.
   */
  hierarchy: HierarchyMetadata;
}

/**
 * Event emitted when a workflow begins execution.
 * Marks the start of a group of related actions.
 */
export interface WorkflowStartedEvent extends WorkflowEvent {
  // Inherits all fields from WorkflowEvent
  // Optional success field that gets added when workflow completes
  success?: boolean;
}

/**
 * Event emitted when a workflow completes execution.
 * Indicates whether all actions in the workflow succeeded.
 */
export interface WorkflowCompletedEvent extends WorkflowEvent {
  /**
   * Whether the workflow completed successfully.
   * False if any action within the workflow failed.
   */
  success: boolean;
}

/**
 * Union type representing any event that can occur in the hierarchical
 * action execution system.
 *
 * Use type guards or discriminated union patterns to narrow the type:
 * - Check `is_workflow` to distinguish workflow events from action events
 * - Check `success` presence to distinguish WorkflowCompletedEvent from WorkflowStartedEvent
 */
export type HierarchicalEvent =
  | ActionExecutionEvent
  | WorkflowStartedEvent
  | WorkflowCompletedEvent;
