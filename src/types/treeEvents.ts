/**
 * Tree Event Types and Interfaces
 *
 * Re-exports types from qontinui-schemas and adds runner-specific extensions.
 * This provides a single import point for all tree event types in the runner.
 *
 * @module treeEvents
 */

// Re-export all types from the shared schema
export type {
  MatchLocation,
  TopMatch,
  RuntimeData,
  StateContext,
  TimingInfo,
  Outcome,
  NodeMetadata,
  TreeNode,
  PathElement,
  TreeEvent,
  TreeEventCreate,
  TreeEventResponse,
  TreeEventListResponse,
  ExecutionTreeResponse,
} from "@qontinui/schemas/tree_events";

// Re-export enums (values, not just types)
export { NodeType, NodeStatus, TreeEventType, ActionType } from "@qontinui/schemas/tree_events";

// Import types we need to extend
import type {
  NodeType as SchemaNodeType,
  NodeStatus as SchemaNodeStatus,
  NodeMetadata as SchemaNodeMetadata,
  RuntimeData as SchemaRuntimeData,
} from "@qontinui/schemas/tree_events";

/**
 * Extended NodeMetadata that allows additional properties for backward compatibility.
 * The backend may send additional fields not defined in the schema.
 */
export interface ExtendedNodeMetadata extends SchemaNodeMetadata {
  // Allow additional properties for flexibility
  [key: string]: unknown;

  // Legacy fields that may be flattened from runtime
  typedText?: string;
  target_states?: string[];
  confidence?: number;

  // Execution record (flattened metadata from backend)
  execution_record?: {
    metadata?: Record<string, unknown>;
    [key: string]: unknown;
  };
}

/**
 * TreeEventData - alias for TreeEvent for backward compatibility
 * This is the primary event type emitted by the backend's tree-based execution system
 */
export type { TreeEvent as TreeEventData } from "@qontinui/schemas/tree_events";

/**
 * Display node structure used by the runner frontend.
 * Extends the schema's DisplayNode with runner-specific properties.
 *
 * Key differences from schema:
 * - `parent`: Reference to parent DisplayNode for navigation
 * - `isExpanded`: camelCase naming for React convention
 * - `metadata`: Uses ExtendedNodeMetadata for backward compatibility
 */
export interface DisplayNode {
  /** Unique identifier for this node */
  id: string;

  /** Type of node (workflow, action, or transition) */
  node_type: SchemaNodeType;

  /** Display name for this node */
  name: string;

  /** Timestamp when this node was created (Unix epoch in seconds) */
  timestamp: number;

  /** Timestamp when this node completed (Unix epoch in seconds) */
  end_timestamp?: number | null;

  /** Duration in seconds (calculated from end_timestamp - timestamp) */
  duration?: number | null;

  /** Current execution status */
  status: SchemaNodeStatus;

  /** Node metadata (action config, expandability, etc.) */
  metadata: ExtendedNodeMetadata;

  /** Error message if status is "failed" */
  error?: string | null;

  /** Child nodes in the tree */
  children: DisplayNode[];

  /** Reference to parent node, or null if this is a root */
  parent: DisplayNode | null;

  /** Whether this node should be expanded in the UI (default: true) */
  isExpanded: boolean;

  /** Nesting level in the tree (0 for root, 1 for first level children, etc.) */
  level?: number;
}

/**
 * Create a DisplayNode from a TreeNode or partial node data
 */
export function createDisplayNode(
  node: {
    id: string;
    node_type: SchemaNodeType;
    name: string;
    timestamp: number;
    end_timestamp?: number | null;
    duration?: number | null;
    status: SchemaNodeStatus;
    metadata?: ExtendedNodeMetadata | SchemaNodeMetadata | Record<string, unknown>;
    error?: string | null;
  },
  parent: DisplayNode | null = null,
  level: number = 0,
): DisplayNode {
  return {
    id: node.id,
    node_type: node.node_type,
    name: node.name,
    timestamp: node.timestamp,
    end_timestamp: node.end_timestamp,
    duration: node.duration,
    status: node.status,
    metadata: (node.metadata || {}) as ExtendedNodeMetadata,
    error: node.error,
    children: [],
    parent,
    isExpanded: true,
    level,
  };
}
