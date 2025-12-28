/**
 * Execution Tree Manager
 *
 * Manages the frontend representation of the execution tree by processing
 * tree events emitted by the backend. Builds and maintains the tree structure
 * incrementally as events arrive.
 *
 * This manager follows the Single Responsibility Principle by focusing solely
 * on tree construction and maintenance. It does not handle UI rendering or
 * event emission - those are separate concerns.
 *
 * @module ExecutionTreeManager
 */

import { TreeEventData, DisplayNode, NodeType, NodeStatus } from "../types/treeEvents";
import type { ExtendedNodeMetadata, NodeMetadata } from "../types/treeEvents";

/**
 * Manages the execution tree structure for the frontend.
 *
 * This class provides incremental tree construction from backend events,
 * maintaining parent-child relationships and ensuring consistency.
 *
 * Key design principles:
 * - Nodes are created on-demand when events arrive
 * - Parent-child links are established immediately
 * - Nodes are updated in-place when status changes
 * - No post-hoc tree building - everything is incremental
 *
 * @example
 * ```typescript
 * const treeManager = new ExecutionTreeManager();
 *
 * // Handle events as they arrive
 * const node1 = treeManager.handleTreeEvent(workflowStartedEvent);
 * const node2 = treeManager.handleTreeEvent(actionStartedEvent);
 * const node3 = treeManager.handleTreeEvent(actionCompletedEvent);
 *
 * // Get the tree structure
 * const roots = treeManager.getRoots();
 * ```
 */
export class ExecutionTreeManager {
  /**
   * Map of node ID to DisplayNode for O(1) lookups
   * @private
   */
  private nodes = new Map<string, DisplayNode>();

  /**
   * Array of root nodes (nodes without parents)
   * @private
   */
  private roots: DisplayNode[] = [];

  /**
   * Handles a tree event from the backend.
   *
   * This is the primary entry point for processing backend events. It creates
   * new nodes or updates existing ones based on the event type.
   *
   * Node creation/update logic:
   * - If the node doesn't exist, create it and link to parent
   * - If the node exists, update its status and error fields
   *
   * Parent linking:
   * - If parent_id is present, look up parent and add as child
   * - If parent not found, log warning (should never happen with proper backend)
   * - If no parent_id, add to roots array
   *
   * @param event - The tree event to process
   * @returns The DisplayNode that was created or updated
   */
  handleTreeEvent(event: TreeEventData): DisplayNode {
    const { node: nodeData, event_type } = event;

    console.log(
      `[TreeManager] Handling ${event_type} for ${nodeData.node_type} "${nodeData.name}" (id: ${nodeData.id})`,
    );

    // Get or create node
    let displayNode = this.nodes.get(nodeData.id);

    if (!displayNode) {
      // Create new node
      console.log(
        `[TreeManager] Creating new node: ${nodeData.id}, parent: ${nodeData.parent_id || "null"}`,
      );
      displayNode = this.createDisplayNode(nodeData);
      this.nodes.set(nodeData.id, displayNode);

      // Link to parent or add as root
      this.linkNodeToParent(displayNode, nodeData.parent_id);
    } else {
      // Update existing node status
      console.log(`[TreeManager] Updating node ${nodeData.id} status: ${nodeData.status}`);
      this.updateNodeStatus(displayNode, nodeData);
    }

    console.log(`[TreeManager] Total nodes: ${this.nodes.size}, roots: ${this.roots.length}`);
    return displayNode;
  }

  /**
   * Creates a new DisplayNode from backend node data.
   *
   * Sets default values:
   * - isExpanded: true (all nodes expanded by default)
   * - children: empty array
   * - parent: null (will be set by linkNodeToParent)
   *
   * Preserves all metadata fields from backend including:
   * - runtime: Action-specific execution data
   * - state_context: State change information
   * - screenshot_reference: Visual reference paths
   * - timing: Detailed timing data
   * - outcome: Success/failure information
   *
   * @private
   * @param nodeData - Backend node data
   * @returns Newly created DisplayNode
   */
  private createDisplayNode(nodeData: {
    id: string;
    node_type: NodeType;
    name: string;
    timestamp: number;
    end_timestamp?: number;
    duration?: number;
    status: NodeStatus;
    metadata?: ExtendedNodeMetadata | NodeMetadata | Record<string, unknown>;
    error?: string;
  }): DisplayNode {
    // Flatten execution_record metadata into top-level metadata
    // Backend sends: metadata.execution_record.metadata.runtime
    // Frontend expects: metadata.runtime
    const flattenedMetadata: ExtendedNodeMetadata = { ...(nodeData.metadata || {}) };
    const executionRecord = flattenedMetadata.execution_record;
    if (
      executionRecord &&
      typeof executionRecord === "object" &&
      "metadata" in executionRecord &&
      executionRecord.metadata
    ) {
      // Merge execution_record.metadata fields into top level
      const recordMetadata = executionRecord.metadata as Record<string, unknown>;
      Object.assign(flattenedMetadata, recordMetadata);
    }

    return {
      id: nodeData.id,
      node_type: nodeData.node_type,
      name: nodeData.name,
      timestamp: nodeData.timestamp,
      end_timestamp: nodeData.end_timestamp,
      duration: nodeData.duration,
      status: nodeData.status,
      metadata: flattenedMetadata,
      error: nodeData.error,
      children: [],
      parent: null,
      isExpanded: true, // Default to expanded for better UX
    };
  }

  /**
   * Links a node to its parent or adds it as a root.
   *
   * This method handles the critical parent-child relationship establishment.
   * With the new tree-based backend, parents should always exist before children,
   * so a missing parent indicates a bug.
   *
   * Defensive handling:
   * - If parentId equals node.id (self-parent), treat as root
   * - If parent not found, treat as root (defensive against backend bugs)
   *
   * @private
   * @param node - The node to link
   * @param parentId - ID of the parent node, or null if this is a root
   */
  private linkNodeToParent(node: DisplayNode, parentId: string | null): void {
    // Check for self-parent bug (node trying to be its own parent)
    if (parentId && parentId === node.id) {
      console.warn(
        `[ExecutionTreeManager] Node ${node.id} has itself as parent. ` +
          `This is a backend bug. Treating as root node.`,
      );
      this.roots.push(node);
      return;
    }

    if (parentId) {
      const parent = this.nodes.get(parentId);

      if (parent) {
        // Link child to parent
        node.parent = parent;
        parent.children.push(node);
      } else {
        // Parent not found - treat as root (defensive against backend bugs)
        console.warn(
          `[ExecutionTreeManager] Parent node ${parentId} not found for node ${node.id}. ` +
            `This indicates a bug in the backend event emission order. Treating as root.`,
        );
        this.roots.push(node);
      }
    } else {
      // No parent - this is a root node
      this.roots.push(node);
    }
  }

  /**
   * Updates an existing node's status and error fields.
   *
   * Called when we receive completion/failure events for nodes that already exist.
   * Only updates mutable fields - immutable fields like id, name, type remain unchanged.
   *
   * Performs deep merge of metadata to preserve new fields from action_completed events:
   * - runtime: Execution details (typed_text, match_summary, clicked_location, etc.)
   * - state_context: State change information
   * - screenshot_reference: Visual debug references
   * - timing: Detailed timing data
   * - outcome: Retry count and other outcome data
   *
   * @private
   * @param node - The node to update
   * @param nodeData - New node data from the backend
   */
  private updateNodeStatus(
    node: DisplayNode,
    nodeData: {
      status: NodeStatus;
      end_timestamp?: number;
      duration?: number;
      error?: string;
      metadata?: ExtendedNodeMetadata | NodeMetadata | Record<string, unknown>;
    },
  ): void {
    node.status = nodeData.status;
    node.end_timestamp = nodeData.end_timestamp;
    node.duration = nodeData.duration;
    node.error = nodeData.error;

    // Deep merge metadata to preserve new fields from action_completed events
    if (nodeData.metadata) {
      // Flatten execution_record metadata before merging
      const flattenedMetadata: ExtendedNodeMetadata = { ...nodeData.metadata };
      const executionRecord = flattenedMetadata.execution_record;
      if (
        executionRecord &&
        typeof executionRecord === "object" &&
        "metadata" in executionRecord &&
        executionRecord.metadata
      ) {
        const recordMetadata = executionRecord.metadata as Record<string, unknown>;
        Object.assign(flattenedMetadata, recordMetadata);
      }

      node.metadata = this.deepMergeMetadata(node.metadata, flattenedMetadata);
    }
  }

  /**
   * Deep merges metadata objects, preserving nested structures.
   *
   * Handles special fields that come from action_completed events:
   * - runtime: Action-specific execution data
   * - state_context: State change information
   * - Preserves all other fields from both objects
   *
   * @private
   * @param existing - Existing metadata from node creation
   * @param incoming - New metadata from update event
   * @returns Merged metadata object
   */
  private deepMergeMetadata(
    existing: ExtendedNodeMetadata,
    incoming: ExtendedNodeMetadata,
  ): ExtendedNodeMetadata {
    const merged: ExtendedNodeMetadata = { ...existing };

    // Merge each field from incoming metadata
    for (const [key, value] of Object.entries(incoming)) {
      if (value === undefined) {
        continue; // Skip undefined values
      }

      // For nested objects (runtime, state_context), merge instead of replace
      if (
        typeof value === "object" &&
        value !== null &&
        !Array.isArray(value) &&
        typeof merged[key] === "object" &&
        merged[key] !== null &&
        !Array.isArray(merged[key])
      ) {
        merged[key] = { ...merged[key], ...value };
      } else {
        // For primitives, arrays, and new fields, replace
        merged[key] = value;
      }
    }

    return merged;
  }

  /**
   * Gets all root nodes in the tree.
   *
   * Root nodes are nodes without parents (typically top-level workflows).
   * This is the entry point for rendering the tree in the UI.
   *
   * @returns Array of root DisplayNodes
   */
  getRoots(): DisplayNode[] {
    return this.roots;
  }

  /**
   * Gets a specific node by ID.
   *
   * Useful for looking up nodes when handling events that reference them,
   * or for implementing features like "jump to node" in the UI.
   *
   * @param id - The node ID to look up
   * @returns The DisplayNode if found, undefined otherwise
   */
  getNode(id: string): DisplayNode | undefined {
    return this.nodes.get(id);
  }

  /**
   * Clears all tree data.
   *
   * Resets the manager to initial state. Useful when starting a new execution
   * or when the user manually clears the display.
   *
   * This removes all nodes and roots, allowing garbage collection of the tree.
   */
  clear(): void {
    this.nodes.clear();
    this.roots = [];
  }

  /**
   * Gets all nodes as a flat array.
   *
   * Useful for debugging or implementing search functionality.
   *
   * @returns Array of all DisplayNodes in the tree
   */
  getAllNodes(): DisplayNode[] {
    return Array.from(this.nodes.values());
  }

  /**
   * Gets the total number of nodes in the tree.
   *
   * @returns The count of nodes
   */
  getNodeCount(): number {
    return this.nodes.size;
  }

  /**
   * Checks if a node exists in the tree.
   *
   * @param id - The node ID to check
   * @returns True if the node exists, false otherwise
   */
  hasNode(id: string): boolean {
    return this.nodes.has(id);
  }

  /**
   * Gets all action nodes in chronological order (by timestamp).
   *
   * This method traverses the entire tree and returns only action nodes
   * (excluding workflow nodes), sorted by their start timestamp. Each
   * action node includes its nesting level for display purposes.
   *
   * Used by ActionLogTable to display a flat, chronological list of
   * all actions that have been executed.
   *
   * @returns Array of DisplayNodes representing actions, with level property set
   */
  public getFlattenedActions(): DisplayNode[] {
    const actions: DisplayNode[] = [];

    // Recursive traversal function
    const traverse = (node: DisplayNode, level: number) => {
      if (node.node_type === "action") {
        // Set the level property directly on the node
        // This preserves the original node with all its references intact
        node.level = level;
        actions.push(node);
      }

      // Traverse children at next level
      node.children.forEach((child) => traverse(child, level + 1));
    };

    // Start traversal from all root nodes
    this.getRoots().forEach((root) => traverse(root, 0));

    // Sort by timestamp (chronological order)
    return actions.sort((a, b) => a.timestamp - b.timestamp);
  }
}
