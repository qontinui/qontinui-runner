/**
 * LineageTreeView.tsx
 *
 * Displays a visual tree of checkpoint replay lineage.
 * Shows original task at root with replay branches as child nodes.
 */

import { useState, useMemo, useCallback } from "react";
import {
  GitBranch,
  RefreshCw,
  AlertTriangle,
  Circle,
  Bookmark,
  X,
  ZoomIn,
  ZoomOut,
  Maximize2,
} from "lucide-react";
import { useLineageTree } from "./useCheckpointData";
import type { LineageTree, LineageNode } from "../../types/checkpoint";

interface LineageTreeViewProps {
  /** Task ID to show lineage for */
  taskId: string;
  /** Callback when a node is clicked to navigate to that task */
  onNavigateToTask?: (taskId: string) => void;
  /** Whether to show in modal/expanded mode */
  expanded?: boolean;
  /** Callback to close expanded mode */
  onClose?: () => void;
}

/** Node dimensions for tree layout */
const NODE_WIDTH = 180;
const NODE_HEIGHT = 80;
const HORIZONTAL_SPACING = 60;
const VERTICAL_SPACING = 40;

/** Tree node with calculated position */
interface PositionedNode extends LineageNode {
  x: number;
  y: number;
  children: PositionedNode[];
}

/**
 * Build a hierarchical tree structure from flat lineage nodes
 */
function buildTree(lineage: LineageTree): PositionedNode | null {
  if (lineage.nodes.length === 0) return null;

  // Create a map of nodes by task ID
  const nodeMap = new Map<string, PositionedNode>();
  lineage.nodes.forEach((node) => {
    nodeMap.set(node.task_id, { ...node, x: 0, y: 0, children: [] });
  });

  // Find root and build parent-child relationships
  let root: PositionedNode | null = null;
  nodeMap.forEach((node) => {
    if (node.parent_task_id) {
      const parent = nodeMap.get(node.parent_task_id);
      if (parent) {
        parent.children.push(node);
      }
    } else {
      root = node;
    }
  });

  // If no root found by parent_task_id, use the root_task_id
  if (!root) {
    root = nodeMap.get(lineage.root_task_id) || null;
  }

  return root;
}

/**
 * Calculate positions for each node in the tree
 */
function layoutTree(root: PositionedNode): { root: PositionedNode; width: number; height: number } {
  let maxWidth = 0;
  let maxHeight = 0;

  // Calculate subtree widths
  function getSubtreeWidth(node: PositionedNode): number {
    if (node.children.length === 0) {
      return NODE_WIDTH;
    }
    const childrenWidth = node.children.reduce(
      (sum, child) => sum + getSubtreeWidth(child) + HORIZONTAL_SPACING,
      -HORIZONTAL_SPACING,
    );
    return Math.max(NODE_WIDTH, childrenWidth);
  }

  // Position nodes
  function position(node: PositionedNode, x: number, y: number): void {
    node.x = x;
    node.y = y;
    maxWidth = Math.max(maxWidth, x + NODE_WIDTH);
    maxHeight = Math.max(maxHeight, y + NODE_HEIGHT);

    if (node.children.length === 0) return;

    // Sort children by creation time
    node.children.sort(
      (a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime(),
    );

    // Calculate starting x for children
    const totalWidth = node.children.reduce(
      (sum, child) => sum + getSubtreeWidth(child) + HORIZONTAL_SPACING,
      -HORIZONTAL_SPACING,
    );
    let childX = x + NODE_WIDTH / 2 - totalWidth / 2;

    // Position each child
    node.children.forEach((child) => {
      const childWidth = getSubtreeWidth(child);
      position(child, childX + childWidth / 2 - NODE_WIDTH / 2, y + NODE_HEIGHT + VERTICAL_SPACING);
      childX += childWidth + HORIZONTAL_SPACING;
    });
  }

  position(root, 0, 0);

  // Normalize positions (shift to start at 0)
  function normalize(node: PositionedNode, minX: number): void {
    node.x -= minX;
    node.children.forEach((child) => normalize(child, minX));
  }

  // Find minimum x
  function findMinX(node: PositionedNode): number {
    let minX = node.x;
    node.children.forEach((child) => {
      minX = Math.min(minX, findMinX(child));
    });
    return minX;
  }

  const minX = findMinX(root);
  normalize(root, minX);
  maxWidth -= minX;

  return { root, width: maxWidth, height: maxHeight };
}

/** Render a single tree node */
function TreeNode({
  node,
  onSelect,
  isSelected,
}: {
  node: PositionedNode;
  onSelect: (taskId: string) => void;
  isSelected: boolean;
}) {
  const isRoot = node.depth === 0;
  const isCurrent = node.is_current;

  // Determine node color based on state
  const getBorderColor = () => {
    if (isCurrent) return "border-cyan-500";
    if (isRoot) return "border-purple-500";
    return "border-gray-600";
  };

  const getGlowClass = () => {
    if (isCurrent) return "shadow-[0_0_12px_rgba(6,182,212,0.4)]";
    if (isRoot) return "shadow-[0_0_12px_rgba(168,85,247,0.4)]";
    return "";
  };

  return (
    <g
      transform={`translate(${node.x}, ${node.y})`}
      onClick={() => onSelect(node.task_id)}
      className="cursor-pointer"
    >
      {/* Node background */}
      <foreignObject width={NODE_WIDTH} height={NODE_HEIGHT}>
        <div
          className={`
            h-full p-3 rounded-lg border-2 transition-all
            bg-gray-800 hover:bg-gray-750
            ${getBorderColor()}
            ${getGlowClass()}
            ${isSelected ? "ring-2 ring-white ring-opacity-50" : ""}
          `}
        >
          {/* Header with icons */}
          <div className="flex items-center gap-2 mb-1">
            {isRoot ? (
              <Circle className="w-3 h-3 text-purple-400 fill-purple-400" />
            ) : (
              <GitBranch className="w-3 h-3 text-cyan-400" />
            )}
            <span className="text-xs font-medium text-gray-400">
              {isRoot ? "Original" : `Depth ${node.depth}`}
            </span>
            {isCurrent && (
              <span className="ml-auto text-xs px-1.5 py-0.5 bg-cyan-500/20 text-cyan-400 rounded">
                Current
              </span>
            )}
          </div>

          {/* Task ID (truncated) */}
          <div className="text-sm font-mono text-white truncate" title={node.task_id}>
            {node.task_id.length > 16
              ? `${node.task_id.slice(0, 8)}...${node.task_id.slice(-6)}`
              : node.task_id}
          </div>

          {/* Checkpoint info */}
          {node.checkpoint_id && (
            <div className="flex items-center gap-1 mt-1 text-xs text-gray-400">
              <Bookmark className="w-3 h-3" />
              <span className="truncate" title={node.checkpoint_id}>
                {node.checkpoint_id.slice(0, 8)}...
              </span>
            </div>
          )}
        </div>
      </foreignObject>
    </g>
  );
}

/** Render connection lines between nodes */
function TreeConnections({ root }: { root: PositionedNode }) {
  const lines: React.ReactElement[] = [];

  function drawConnections(node: PositionedNode): void {
    const parentCenterX = node.x + NODE_WIDTH / 2;
    const parentBottomY = node.y + NODE_HEIGHT;

    node.children.forEach((child, _index) => {
      const childCenterX = child.x + NODE_WIDTH / 2;
      const childTopY = child.y;

      // Create curved path
      const midY = (parentBottomY + childTopY) / 2;
      const path = `
        M ${parentCenterX} ${parentBottomY}
        C ${parentCenterX} ${midY}, ${childCenterX} ${midY}, ${childCenterX} ${childTopY}
      `;

      lines.push(
        <path
          key={`${node.task_id}-${child.task_id}`}
          d={path}
          fill="none"
          stroke="url(#lineGradient)"
          strokeWidth="2"
          strokeLinecap="round"
          className="transition-all"
        />,
      );

      // Recurse for children
      drawConnections(child);
    });
  }

  drawConnections(root);

  return <>{lines}</>;
}

/** Collect all nodes from tree for rendering */
function collectNodes(node: PositionedNode): PositionedNode[] {
  return [node, ...node.children.flatMap(collectNodes)];
}

export function LineageTreeView({
  taskId,
  onNavigateToTask,
  expanded = false,
  onClose,
}: LineageTreeViewProps) {
  const { lineage, isLoading, error } = useLineageTree(taskId);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0 });

  // Build and layout tree
  const treeData = useMemo(() => {
    if (!lineage) return null;
    const root = buildTree(lineage);
    if (!root) return null;
    return layoutTree(root);
  }, [lineage]);

  // Calculate SVG dimensions with padding
  const svgPadding = 40;
  const svgWidth = treeData ? treeData.width + svgPadding * 2 : 400;
  const svgHeight = treeData ? treeData.height + svgPadding * 2 : 200;

  const handleNodeSelect = useCallback(
    (nodeTaskId: string) => {
      setSelectedNode(nodeTaskId);
      if (onNavigateToTask) {
        onNavigateToTask(nodeTaskId);
      }
    },
    [onNavigateToTask],
  );

  const handleZoomIn = () => setZoom((z) => Math.min(z + 0.25, 2));
  const handleZoomOut = () => setZoom((z) => Math.max(z - 0.25, 0.5));
  const handleResetView = () => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  };

  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return; // Only left click
    setIsDragging(true);
    setDragStart({ x: e.clientX - pan.x, y: e.clientY - pan.y });
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!isDragging) return;
    setPan({
      x: e.clientX - dragStart.x,
      y: e.clientY - dragStart.y,
    });
  };

  const handleMouseUp = () => {
    setIsDragging(false);
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-8">
        <RefreshCw className="w-6 h-6 animate-spin text-gray-400" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center p-8">
        <div className="text-center">
          <AlertTriangle className="w-8 h-8 text-red-500 mx-auto mb-2" />
          <p className="text-gray-400 text-sm">{error}</p>
        </div>
      </div>
    );
  }

  if (!lineage || lineage.nodes.length === 0) {
    return (
      <div className="flex items-center justify-center p-8">
        <div className="text-center">
          <GitBranch className="w-8 h-8 text-gray-600 mx-auto mb-2" />
          <p className="text-gray-400 text-sm">No lineage data available</p>
          <p className="text-gray-500 text-xs mt-1">
            This task has not been replayed from a checkpoint
          </p>
        </div>
      </div>
    );
  }

  if (!treeData) {
    return (
      <div className="flex items-center justify-center p-8">
        <div className="text-center">
          <AlertTriangle className="w-8 h-8 text-yellow-500 mx-auto mb-2" />
          <p className="text-gray-400 text-sm">Could not build lineage tree</p>
        </div>
      </div>
    );
  }

  const allNodes = collectNodes(treeData.root);

  const content = (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-gray-700">
        <div className="flex items-center gap-2">
          <GitBranch className="w-5 h-5 text-cyan-500" />
          <span className="font-medium text-white">Replay Lineage</span>
          <span className="text-xs text-gray-400">
            {lineage.nodes.length} task{lineage.nodes.length !== 1 ? "s" : ""}
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={handleZoomOut}
            className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Zoom Out"
          >
            <ZoomOut className="w-4 h-4" />
          </button>
          <span className="text-xs text-gray-400 px-2">{Math.round(zoom * 100)}%</span>
          <button
            onClick={handleZoomIn}
            className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Zoom In"
          >
            <ZoomIn className="w-4 h-4" />
          </button>
          <button
            onClick={handleResetView}
            className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors ml-2"
            title="Reset View"
          >
            <Maximize2 className="w-4 h-4" />
          </button>
          {onClose && (
            <button
              onClick={onClose}
              className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors ml-2"
              title="Close"
            >
              <X className="w-4 h-4" />
            </button>
          )}
        </div>
      </div>

      {/* Tree View */}
      <div
        className="flex-1 overflow-hidden bg-gray-900/50"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
        style={{ cursor: isDragging ? "grabbing" : "grab" }}
      >
        <svg
          width="100%"
          height="100%"
          viewBox={`0 0 ${svgWidth} ${svgHeight}`}
          style={{
            transform: `scale(${zoom}) translate(${pan.x / zoom}px, ${pan.y / zoom}px)`,
            transformOrigin: "center center",
          }}
        >
          <defs>
            {/* Gradient for connection lines */}
            <linearGradient id="lineGradient" x1="0%" y1="0%" x2="0%" y2="100%">
              <stop offset="0%" stopColor="#a855f7" stopOpacity="0.6" />
              <stop offset="100%" stopColor="#06b6d4" stopOpacity="0.6" />
            </linearGradient>
          </defs>

          <g transform={`translate(${svgPadding}, ${svgPadding})`}>
            {/* Connection lines (rendered first, behind nodes) */}
            <TreeConnections root={treeData.root} />

            {/* Nodes */}
            {allNodes.map((node) => (
              <TreeNode
                key={node.task_id}
                node={node}
                onSelect={handleNodeSelect}
                isSelected={selectedNode === node.task_id}
              />
            ))}
          </g>
        </svg>
      </div>

      {/* Legend */}
      <div className="flex items-center gap-4 px-3 py-2 border-t border-gray-700 text-xs text-gray-400">
        <div className="flex items-center gap-1.5">
          <Circle className="w-3 h-3 text-purple-400 fill-purple-400" />
          <span>Original Task</span>
        </div>
        <div className="flex items-center gap-1.5">
          <GitBranch className="w-3 h-3 text-cyan-400" />
          <span>Replay Branch</span>
        </div>
        <div className="flex items-center gap-1.5">
          <div className="w-3 h-3 rounded border-2 border-cyan-500 shadow-[0_0_6px_rgba(6,182,212,0.4)]" />
          <span>Current Task</span>
        </div>
      </div>
    </div>
  );

  if (expanded) {
    return (
      <div className="fixed inset-0 z-modal-backdrop bg-black/70 flex items-center justify-center p-8">
        <div className="w-full max-w-4xl h-[80vh] bg-gray-800 rounded-lg border border-gray-700 shadow-xl overflow-hidden">
          {content}
        </div>
      </div>
    );
  }

  return <div className="h-full">{content}</div>;
}

export default LineageTreeView;
