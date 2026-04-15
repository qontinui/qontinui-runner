/**
 * Tree Event Types and Interfaces
 *
 * Re-exports from @qontinui/shared-types/tree-events plus a few
 * runner-specific extensions. Single source of truth:
 * qontinui-schemas/rust/src/tree_events.rs.
 *
 * @module treeEvents
 */

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
  // These are string-literal union types now (see CoordinateSystem note in
  // ./geometry.ts), not runtime TS enums. All callers use them as types.
  NodeType,
  NodeStatus,
  TreeEventType,
  ActionType,
} from "@qontinui/shared-types/tree-events";

// Import types we need to extend
import type {
  NodeType as SchemaNodeType,
  NodeStatus as SchemaNodeStatus,
  NodeMetadata as SchemaNodeMetadata,
} from "@qontinui/shared-types/tree-events";

/**
 * Extended NodeMetadata that allows additional properties for backward
 * compatibility. The backend may send additional fields not declared in the
 * canonical Rust type.
 */
export interface ExtendedNodeMetadata extends SchemaNodeMetadata {
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
 * TreeEventData — alias for TreeEvent for backward compatibility.
 */
export type { TreeEvent as TreeEventData } from "@qontinui/shared-types/tree-events";

/**
 * Display node structure used by the runner frontend. Extends the canonical
 * DisplayNode with runner-specific properties.
 *
 * Differences from the canonical schema:
 * - `parent`: Reference to parent DisplayNode for in-memory navigation
 * - `isExpanded`: camelCase for React convention (schema uses `is_expanded`)
 * - `metadata`: Uses ExtendedNodeMetadata for backward compatibility
 */
export interface DisplayNode {
  id: string;
  node_type: SchemaNodeType;
  name: string;
  timestamp: number;
  end_timestamp?: number | null;
  duration?: number | null;
  status: SchemaNodeStatus;
  metadata: ExtendedNodeMetadata;
  error?: string | null;
  children: DisplayNode[];
  parent: DisplayNode | null;
  /** Whether this node should be expanded in the UI (default: true). */
  isExpanded: boolean;
  /** Nesting level in the tree (0 for root, 1 for first-level children). */
  level?: number;
}

/**
 * Create a DisplayNode from a TreeNode or partial node data.
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
