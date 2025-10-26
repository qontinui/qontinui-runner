/**
 * Event Tree Utility Module
 *
 * Provides utilities for building and managing hierarchical event tree structures.
 * This module enables efficient tree operations including building, searching, toggling,
 * and querying tree nodes in a purely functional manner.
 *
 * @module eventTree
 */

import { HierarchicalEvent } from '../types/events';

/**
 * Generic tree node interface representing a single node in a hierarchical tree.
 *
 * @template T - The type of data stored in the node
 *
 * @property id - Unique identifier for the node
 * @property data - The data payload of type T stored in this node
 * @property children - Array of child nodes (empty array if leaf node)
 * @property parent - Reference to parent node (null for root nodes)
 * @property isExpanded - Whether the node is currently expanded in the UI
 *
 * @example
 * ```typescript
 * const node: TreeNode<string> = {
 *   id: 'node-1',
 *   data: 'Example Node',
 *   children: [],
 *   parent: null,
 *   isExpanded: true
 * };
 * ```
 */
export interface TreeNode<T> {
  id: string;
  data: T;
  children: TreeNode<T>[];
  parent: TreeNode<T> | null;
  isExpanded: boolean;
}

/**
 * Builds a hierarchical tree structure from a flat array of events.
 *
 * This function takes an array of events with parent-child relationships defined
 * by `parent_id` and constructs a tree structure. Events are organized by their
 * hierarchical relationships, with root nodes (parent_id === null) at the top level.
 * Children are sorted by timestamp in ascending order.
 *
 * Algorithm:
 * 1. Create a map of event ID to TreeNode for O(1) lookup
 * 2. Group events by parent_id
 * 3. Build tree structure by linking children to parents
 * 4. Sort children by timestamp
 * 5. Return array of root nodes
 *
 * @param events - Flat array of hierarchical events to organize into a tree
 * @returns Array of root tree nodes representing the top-level of the hierarchy
 *
 * @example
 * ```typescript
 * const events: HierarchicalEvent[] = [
 *   { id: '1', hierarchy: { parent_id: null }, timestamp: 1000, ... },
 *   { id: '2', hierarchy: { parent_id: '1' }, timestamp: 1100, ... },
 *   { id: '3', hierarchy: { parent_id: '1' }, timestamp: 1050, ... },
 * ];
 *
 * const tree = buildEventTree(events);
 * // Returns:
 * // [
 * //   {
 * //     id: '1',
 * //     data: { id: '1', ... },
 * //     children: [
 * //       { id: '3', ... },  // sorted by timestamp (1050)
 * //       { id: '2', ... }   // sorted by timestamp (1100)
 * //     ],
 * //     parent: null,
 * //     isExpanded: true
 * //   }
 * // ]
 * ```
 */
export function buildEventTree(
  events: HierarchicalEvent[]
): TreeNode<HierarchicalEvent>[] {
  // Map of event ID to TreeNode for efficient lookup
  const nodeMap = new Map<string, TreeNode<HierarchicalEvent>>();

  // Map of parent ID to array of child events for grouping
  const childrenByParent = new Map<string | null, HierarchicalEvent[]>();

  // First pass: Create all nodes and group by parent_id
  for (const event of events) {
    // Create node for this event
    nodeMap.set(event.id, {
      id: event.id,
      data: event,
      children: [],
      parent: null,
      isExpanded: true, // Default to expanded for better UX
    });

    // Group event by parent_id
    const parentId = event.hierarchy.parent_id;
    if (!childrenByParent.has(parentId)) {
      childrenByParent.set(parentId, []);
    }
    childrenByParent.get(parentId)!.push(event);
  }

  // Second pass: Build parent-child relationships
  for (const [parentId, children] of childrenByParent) {
    if (parentId === null) {
      continue; // Skip root nodes for now
    }

    const parentNode = nodeMap.get(parentId);
    if (!parentNode) {
      continue; // Parent not found, skip
    }

    // Sort children by timestamp (ascending)
    const sortedChildren = [...children].sort(
      (a, b) => a.timestamp - b.timestamp
    );

    // Add children to parent node
    for (const childEvent of sortedChildren) {
      const childNode = nodeMap.get(childEvent.id);
      if (childNode) {
        childNode.parent = parentNode;
        parentNode.children.push(childNode);
      }
    }
  }

  // Return root nodes (parent_id === null), sorted by timestamp
  const rootEvents = childrenByParent.get(null) || [];
  const sortedRoots = [...rootEvents].sort((a, b) => a.timestamp - b.timestamp);

  return sortedRoots
    .map((event) => nodeMap.get(event.id))
    .filter((node): node is TreeNode<HierarchicalEvent> => node !== undefined);
}

/**
 * Recursively searches a tree for a node with the specified ID.
 *
 * Performs a depth-first search traversal of the tree to find a node
 * matching the given ID. Returns the first matching node found, or null
 * if no node with that ID exists in the tree.
 *
 * Time Complexity: O(n) where n is the total number of nodes in the tree
 * Space Complexity: O(h) where h is the height of the tree (recursion stack)
 *
 * @template T - The type of data stored in the tree nodes
 * @param tree - Array of root nodes to search
 * @param id - The ID of the node to find
 * @returns The matching TreeNode or null if not found
 *
 * @example
 * ```typescript
 * const tree: TreeNode<HierarchicalEvent>[] = buildEventTree(events);
 * const node = findNode(tree, 'event-123');
 *
 * if (node) {
 *   console.log('Found node:', node.data);
 * } else {
 *   console.log('Node not found');
 * }
 * ```
 */
export function findNode<T>(
  tree: TreeNode<T>[],
  id: string
): TreeNode<T> | null {
  for (const node of tree) {
    // Check if current node matches
    if (node.id === id) {
      return node;
    }

    // Recursively search children
    if (node.children.length > 0) {
      const found = findNode(node.children, id);
      if (found) {
        return found;
      }
    }
  }

  return null;
}

/**
 * Toggles the expansion state of a node with the specified ID.
 *
 * Creates a new tree with the target node's `isExpanded` state toggled,
 * maintaining immutability. All other nodes remain unchanged. This function
 * is safe for use in React state updates and other immutable data flows.
 *
 * The function performs a deep clone of the affected path from root to target
 * node, preserving structural sharing for unaffected branches.
 *
 * @template T - The type of data stored in the tree nodes
 * @param tree - Array of root nodes
 * @param id - The ID of the node to toggle
 * @returns New tree array with the toggled node (immutable update)
 *
 * @example
 * ```typescript
 * const [tree, setTree] = useState<TreeNode<HierarchicalEvent>[]>([]);
 *
 * // Toggle a node's expanded state
 * const newTree = toggleNode(tree, 'event-123');
 * setTree(newTree);
 *
 * // Test immutability
 * const original = buildEventTree(events);
 * const toggled = toggleNode(original, 'event-1');
 * console.log(original === toggled); // false (different reference)
 * console.log(original[0] === toggled[0]); // depends on whether node-1 is in first root
 * ```
 */
export function toggleNode<T>(
  tree: TreeNode<T>[],
  id: string
): TreeNode<T>[] {
  return tree.map((node) => toggleNodeRecursive(node, id));
}

/**
 * Helper function for toggleNode that recursively clones and toggles a node.
 *
 * @template T - The type of data stored in the tree nodes
 * @param node - Current node being processed
 * @param id - The ID of the node to toggle
 * @returns New node with toggled state if ID matches, or cloned node with updated children
 */
function toggleNodeRecursive<T>(node: TreeNode<T>, id: string): TreeNode<T> {
  // If this is the target node, toggle its isExpanded state
  if (node.id === id) {
    return {
      ...node,
      isExpanded: !node.isExpanded,
      children: node.children, // Keep same children reference
    };
  }

  // If this node has children, recursively process them
  if (node.children.length > 0) {
    const newChildren = node.children.map((child) =>
      toggleNodeRecursive(child, id)
    );

    // Check if any child changed
    const childrenChanged = newChildren.some(
      (newChild, index) => newChild !== node.children[index]
    );

    // Only create new node if children changed
    if (childrenChanged) {
      return {
        ...node,
        children: newChildren,
      };
    }
  }

  // No changes needed, return original node
  return node;
}

/**
 * Collects all node IDs where isExpanded is true.
 *
 * Recursively traverses the tree and builds a Set of IDs for all nodes
 * that are currently in an expanded state. This is useful for:
 * - Persisting expansion state
 * - Restoring UI state after data refresh
 * - Efficient lookup of expanded nodes (O(1) with Set)
 *
 * @template T - The type of data stored in the tree nodes
 * @param tree - Array of root nodes to traverse
 * @returns Set of node IDs that are expanded
 *
 * @example
 * ```typescript
 * const tree = buildEventTree(events);
 * const expandedIds = getAllExpandedIds(tree);
 *
 * // Check if a specific node is expanded
 * if (expandedIds.has('event-123')) {
 *   console.log('Node event-123 is expanded');
 * }
 *
 * // Count expanded nodes
 * console.log(`${expandedIds.size} nodes are expanded`);
 *
 * // Save to localStorage
 * localStorage.setItem('expandedNodes', JSON.stringify([...expandedIds]));
 *
 * // Restore from localStorage
 * const saved = JSON.parse(localStorage.getItem('expandedNodes') || '[]');
 * const savedSet = new Set(saved);
 * ```
 */
export function getAllExpandedIds<T>(tree: TreeNode<T>[]): Set<string> {
  const expandedIds = new Set<string>();

  function traverse(node: TreeNode<T>): void {
    if (node.isExpanded) {
      expandedIds.add(node.id);
    }

    // Recursively traverse children
    for (const child of node.children) {
      traverse(child);
    }
  }

  // Traverse all root nodes
  for (const root of tree) {
    traverse(root);
  }

  return expandedIds;
}

/**
 * Unit Test Examples (for documentation purposes)
 *
 * These examples demonstrate how to test the eventTree utilities.
 * In a real project, these would be in a separate test file (e.g., eventTree.test.ts)
 *
 * @example
 * ```typescript
 * import { describe, it, expect } from 'vitest';
 * import {
 *   buildEventTree,
 *   findNode,
 *   toggleNode,
 *   getAllExpandedIds,
 *   TreeNode
 * } from './eventTree';
 * import { HierarchicalEvent } from '../types/events';
 *
 * describe('buildEventTree', () => {
 *   it('should build a tree with root nodes', () => {
 *     const events: HierarchicalEvent[] = [
 *       {
 *         id: '1',
 *         timestamp: 1000,
 *         hierarchy: { parent_id: null, nesting_level: 0, workflow_name: null, is_expandable: false },
 *         action_id: 'a1',
 *         action_type: 'click',
 *         success: true,
 *         attempts: 1,
 *         config: {},
 *         typed_text: null,
 *         error: null,
 *         reason: null
 *       }
 *     ];
 *
 *     const tree = buildEventTree(events);
 *     expect(tree).toHaveLength(1);
 *     expect(tree[0].id).toBe('1');
 *     expect(tree[0].parent).toBeNull();
 *   });
 *
 *   it('should build nested parent-child relationships', () => {
 *     const events: HierarchicalEvent[] = [
 *       {
 *         id: '1',
 *         timestamp: 1000,
 *         hierarchy: { parent_id: null, nesting_level: 0, workflow_name: null, is_expandable: true },
 *         workflow_id: 'w1',
 *         workflow_name: 'Test Workflow',
 *         is_workflow: true
 *       },
 *       {
 *         id: '2',
 *         timestamp: 1100,
 *         hierarchy: { parent_id: '1', nesting_level: 1, workflow_name: 'Test Workflow', is_expandable: false },
 *         action_id: 'a1',
 *         action_type: 'click',
 *         success: true,
 *         attempts: 1,
 *         config: {},
 *         typed_text: null,
 *         error: null,
 *         reason: null
 *       }
 *     ];
 *
 *     const tree = buildEventTree(events);
 *     expect(tree).toHaveLength(1);
 *     expect(tree[0].children).toHaveLength(1);
 *     expect(tree[0].children[0].id).toBe('2');
 *     expect(tree[0].children[0].parent).toBe(tree[0]);
 *   });
 *
 *   it('should sort children by timestamp', () => {
 *     const events: HierarchicalEvent[] = [
 *       {
 *         id: '1',
 *         timestamp: 1000,
 *         hierarchy: { parent_id: null, nesting_level: 0, workflow_name: null, is_expandable: true },
 *         workflow_id: 'w1',
 *         workflow_name: 'Test',
 *         is_workflow: true
 *       },
 *       {
 *         id: '3',
 *         timestamp: 1200,
 *         hierarchy: { parent_id: '1', nesting_level: 1, workflow_name: 'Test', is_expandable: false },
 *         action_id: 'a2',
 *         action_type: 'click',
 *         success: true,
 *         attempts: 1,
 *         config: {},
 *         typed_text: null,
 *         error: null,
 *         reason: null
 *       },
 *       {
 *         id: '2',
 *         timestamp: 1100,
 *         hierarchy: { parent_id: '1', nesting_level: 1, workflow_name: 'Test', is_expandable: false },
 *         action_id: 'a1',
 *         action_type: 'click',
 *         success: true,
 *         attempts: 1,
 *         config: {},
 *         typed_text: null,
 *         error: null,
 *         reason: null
 *       }
 *     ];
 *
 *     const tree = buildEventTree(events);
 *     expect(tree[0].children[0].id).toBe('2'); // Earlier timestamp
 *     expect(tree[0].children[1].id).toBe('3'); // Later timestamp
 *   });
 * });
 *
 * describe('findNode', () => {
 *   it('should find root node', () => {
 *     const tree: TreeNode<string>[] = [
 *       { id: '1', data: 'root', children: [], parent: null, isExpanded: true }
 *     ];
 *
 *     const found = findNode(tree, '1');
 *     expect(found).not.toBeNull();
 *     expect(found?.id).toBe('1');
 *   });
 *
 *   it('should find nested node', () => {
 *     const child: TreeNode<string> = {
 *       id: '2',
 *       data: 'child',
 *       children: [],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     const root: TreeNode<string> = {
 *       id: '1',
 *       data: 'root',
 *       children: [child],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     child.parent = root;
 *
 *     const found = findNode([root], '2');
 *     expect(found).not.toBeNull();
 *     expect(found?.id).toBe('2');
 *   });
 *
 *   it('should return null for non-existent node', () => {
 *     const tree: TreeNode<string>[] = [
 *       { id: '1', data: 'root', children: [], parent: null, isExpanded: true }
 *     ];
 *
 *     const found = findNode(tree, 'non-existent');
 *     expect(found).toBeNull();
 *   });
 * });
 *
 * describe('toggleNode', () => {
 *   it('should toggle node expansion state', () => {
 *     const tree: TreeNode<string>[] = [
 *       { id: '1', data: 'root', children: [], parent: null, isExpanded: true }
 *     ];
 *
 *     const newTree = toggleNode(tree, '1');
 *     expect(newTree[0].isExpanded).toBe(false);
 *   });
 *
 *   it('should maintain immutability', () => {
 *     const tree: TreeNode<string>[] = [
 *       { id: '1', data: 'root', children: [], parent: null, isExpanded: true }
 *     ];
 *
 *     const newTree = toggleNode(tree, '1');
 *     expect(newTree).not.toBe(tree);
 *     expect(tree[0].isExpanded).toBe(true); // Original unchanged
 *     expect(newTree[0].isExpanded).toBe(false); // New is toggled
 *   });
 *
 *   it('should toggle nested node', () => {
 *     const child: TreeNode<string> = {
 *       id: '2',
 *       data: 'child',
 *       children: [],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     const root: TreeNode<string> = {
 *       id: '1',
 *       data: 'root',
 *       children: [child],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     child.parent = root;
 *
 *     const newTree = toggleNode([root], '2');
 *     expect(newTree[0].children[0].isExpanded).toBe(false);
 *   });
 * });
 *
 * describe('getAllExpandedIds', () => {
 *   it('should return empty set for collapsed tree', () => {
 *     const tree: TreeNode<string>[] = [
 *       { id: '1', data: 'root', children: [], parent: null, isExpanded: false }
 *     ];
 *
 *     const expandedIds = getAllExpandedIds(tree);
 *     expect(expandedIds.size).toBe(0);
 *   });
 *
 *   it('should collect all expanded node IDs', () => {
 *     const child: TreeNode<string> = {
 *       id: '2',
 *       data: 'child',
 *       children: [],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     const root: TreeNode<string> = {
 *       id: '1',
 *       data: 'root',
 *       children: [child],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     child.parent = root;
 *
 *     const expandedIds = getAllExpandedIds([root]);
 *     expect(expandedIds.size).toBe(2);
 *     expect(expandedIds.has('1')).toBe(true);
 *     expect(expandedIds.has('2')).toBe(true);
 *   });
 *
 *   it('should only include expanded nodes', () => {
 *     const child1: TreeNode<string> = {
 *       id: '2',
 *       data: 'child1',
 *       children: [],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     const child2: TreeNode<string> = {
 *       id: '3',
 *       data: 'child2',
 *       children: [],
 *       parent: null,
 *       isExpanded: false
 *     };
 *     const root: TreeNode<string> = {
 *       id: '1',
 *       data: 'root',
 *       children: [child1, child2],
 *       parent: null,
 *       isExpanded: true
 *     };
 *     child1.parent = root;
 *     child2.parent = root;
 *
 *     const expandedIds = getAllExpandedIds([root]);
 *     expect(expandedIds.size).toBe(2);
 *     expect(expandedIds.has('1')).toBe(true);
 *     expect(expandedIds.has('2')).toBe(true);
 *     expect(expandedIds.has('3')).toBe(false);
 *   });
 * });
 * ```
 */
