/**
 * Tree Event Types and Interfaces
 *
 * Defines the event data structures for the new tree-based execution architecture.
 * These types correspond to events emitted by the backend's ExecutionTree system.
 *
 * @module treeEvents
 */

/**
 * Type of nodes in the execution tree
 */
export type NodeType = "workflow" | "action" | "transition";

/**
 * Status of a node in the execution tree
 */
export type NodeStatus = "pending" | "running" | "success" | "failed";

/**
 * Event types for tree events
 */
export type TreeEventType =
  | "workflow_started"
  | "workflow_completed"
  | "workflow_failed"
  | "action_started"
  | "action_completed"
  | "action_failed";

/**
 * Metadata for a node in the execution tree
 * Contains action-specific configuration and properties
 */
export interface NodeMetadata {
  /** Action configuration data */
  config?: Record<string, any>;

  /** Whether this action can have child nodes (e.g., GO_TO_STATE, RUN_WORKFLOW) */
  is_expandable?: boolean;

  /** Whether this action is inline (no children) */
  is_inline?: boolean;

  /** Runtime data from action execution */
  runtime?: {
    // TYPE actions
    typed_text?: string;
    character_count?: number;

    // FIND/IF actions
    image_id?: string;
    found?: boolean;
    confidence?: number;
    location?: { x: number; y: number };
    dimensions?: { w: number; h: number };
    match_method?: string;
    // For multiple matches
    top_matches?: Array<{
      confidence: number;
      location: { x: number; y: number };
      dimensions: { w: number; h: number };
    }>;

    // CLICK actions
    clicked_at?: { x: number; y: number };
    button?: string;
    target_type?: string;

    // GO_TO_STATE actions
    source_states?: string[];
    target_states?: string[];
    targets_reached?: string[];
    transitions_executed?: string[];
    already_at_target?: boolean;

    // IF actions
    condition_passed?: boolean;
    branch_taken?: string;

    // RUN_WORKFLOW actions
    workflow_name?: string;
    workflow_status?: string;
  };

  /** State context */
  state_context?: {
    active_before: string[];
    active_after: string[];
    changed: boolean;
    activated: string[];
    deactivated: string[];
  };

  /** Precise timing */
  timing?: {
    start_time: string; // ISO 8601
    end_time: string;
    duration_ms: number;
  };

  /** Execution outcome */
  outcome?: {
    success: boolean;
    error?: string;
    retry_count: number;
  };

  /** Screenshot references */
  screenshot_reference?: string;
  visual_debug_reference?: string;

  /** Additional metadata fields */
  [key: string]: any;
}

/**
 * Path element representing a single node in the path from root to current node
 * Used for breadcrumb display and navigation
 */
export interface PathElement {
  /** Unique identifier for this path element */
  id: string;

  /** Display name of this element */
  name: string;

  /** Type of this element */
  type: string;
}

/**
 * Node data structure as emitted by the backend
 * Represents a single node in the execution tree
 */
export interface TreeNode {
  /** Unique identifier for this node */
  id: string;

  /** Type of node (workflow or action) */
  node_type: NodeType;

  /** Display name for this node */
  name: string;

  /** Timestamp when this node was created (Unix epoch in seconds) */
  timestamp: number;

  /** Timestamp when this node completed (Unix epoch in seconds) */
  end_timestamp?: number;

  /** Duration in seconds (calculated from end_timestamp - timestamp) */
  duration?: number;

  /** ID of the parent node, or null if this is a root node */
  parent_id: string | null;

  /** Current execution status */
  status: NodeStatus;

  /** Node metadata (action config, expandability, etc.) */
  metadata: NodeMetadata;

  /** Error message if status is "failed" */
  error?: string;
}

/**
 * Complete tree event data structure
 * This is the primary event type emitted by the backend's tree-based execution system
 */
export interface TreeEventData {
  /** Event type identifier */
  type: "tree_event";

  /** Specific tree event type */
  event_type: TreeEventType;

  /** The node this event is about */
  node: TreeNode;

  /** Path from root to this node (for breadcrumb display) */
  path: PathElement[];

  /** Timestamp when this event was emitted (Unix epoch) */
  timestamp: number;

  /** Additional event-specific data */
  [key: string]: any;
}

/**
 * Display node structure used by the frontend
 * Extended version of TreeNode with references for tree rendering
 */
export interface DisplayNode {
  /** Unique identifier for this node */
  id: string;

  /** Type of node (workflow or action) */
  type: NodeType;

  /** Display name for this node */
  name: string;

  /** Timestamp when this node was created (Unix epoch in seconds) */
  timestamp: number;

  /** Timestamp when this node completed (Unix epoch in seconds) */
  end_timestamp?: number;

  /** Duration in seconds (calculated from end_timestamp - timestamp) */
  duration?: number;

  /** Current execution status */
  status: NodeStatus;

  /** Node metadata (action config, expandability, etc.) */
  metadata: NodeMetadata;

  /** Error message if status is "failed" */
  error?: string;

  /** Child nodes in the tree */
  children: DisplayNode[];

  /** Reference to parent node, or null if this is a root */
  parent: DisplayNode | null;

  /** Whether this node should be expanded in the UI (default: true) */
  isExpanded: boolean;

  /** Nesting level in the tree (0 for root, 1 for first level children, etc.) */
  level?: number;
}
