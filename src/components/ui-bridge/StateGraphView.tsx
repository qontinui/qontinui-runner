/**
 * StateGraphView.tsx
 *
 * Interactive state graph visualization using ReactFlow.
 * Displays discovered states as nodes and transitions as edges.
 */

import { useCallback, useMemo, useEffect } from "react";
import {
  ReactFlow,
  MiniMap,
  Controls,
  Background,
  useNodesState,
  useEdgesState,
  type Node,
  type Edge,
  type NodeMouseHandler,
  type EdgeMouseHandler,
  BackgroundVariant,
  Position,
  Handle,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { memo } from "react";
import { Circle, Layers, AlertCircle } from "lucide-react";
import type { FingerprintDiscoveryResult } from "./StateDiscoveryPanel";
import type { ElementFingerprint } from "../../types/ui-bridge-types";

// =============================================================================
// Types
// =============================================================================

interface StateGraphViewProps {
  /** Discovery result containing states and transitions */
  discoveryResult: FingerprintDiscoveryResult;
  /** Fingerprint details for enriched display */
  fingerprintDetails: Record<string, ElementFingerprint>;
  /** Callback when a state node is clicked */
  onSelectState?: (stateId: string | null) => void;
  /** Callback when a transition edge is clicked */
  onSelectTransition?: (fromStateId: string, toStateId: string) => void;
  /** Currently selected state ID (optional) */
  selectedStateId?: string | null;
  /** Currently selected transition ID (format: "fromStateId->toStateId") */
  selectedTransitionId?: string | null;
}

interface StateNodeData {
  state: FingerprintDiscoveryResult["states"][0];
  isSelected: boolean;
}

interface StateNodeProps {
  data: StateNodeData;
}

// =============================================================================
// State Type Colors
// =============================================================================

const STATE_TYPE_COLORS = {
  global: {
    bg: "rgba(59, 130, 246, 0.2)", // blue
    border: "#3b82f6",
    text: "#60a5fa",
  },
  modal: {
    bg: "rgba(168, 85, 247, 0.2)", // purple
    border: "#a855f7",
    text: "#c084fc",
  },
  content: {
    bg: "rgba(34, 197, 94, 0.2)", // green
    border: "#22c55e",
    text: "#4ade80",
  },
  mixed: {
    bg: "rgba(113, 113, 122, 0.2)", // gray
    border: "#71717a",
    text: "#a1a1aa",
  },
};

// =============================================================================
// Custom State Node Component
// =============================================================================

function StateNodeComponent({ data }: StateNodeProps) {
  const { state, isSelected } = data;
  const stateType = state.isGlobal ? "global" : state.isModal ? "modal" : "content";
  const colors = STATE_TYPE_COLORS[stateType];

  return (
    <>
      {/* Input Handle */}
      <Handle
        type="target"
        position={Position.Top}
        className="w-2 h-2"
        style={{ background: colors.border }}
      />

      <div
        className="rounded-lg min-w-[140px] max-w-[200px] transition-all duration-200"
        style={{
          backgroundColor: colors.bg,
          border: `2px solid ${colors.border}`,
          boxShadow: isSelected
            ? `0 0 0 3px ${colors.border}40, 0 0 20px ${colors.border}30`
            : undefined,
        }}
      >
        {/* Header */}
        <div
          className="px-3 py-2 rounded-t-md flex items-center gap-2"
          style={{ backgroundColor: `${colors.border}20` }}
        >
          <Circle className="w-2.5 h-2.5" style={{ color: colors.text, fill: colors.text }} />
          <span
            className="text-xs font-medium uppercase tracking-wider"
            style={{ color: colors.text }}
          >
            {stateType}
          </span>
        </div>

        {/* Content */}
        <div className="px-3 py-2 space-y-1">
          <h4 className="font-medium text-white text-sm truncate">{state.name}</h4>

          <div className="flex items-center gap-2 text-xs">
            <span className="flex items-center gap-1 text-muted-foreground">
              <Layers className="w-3 h-3" />
              {state.fingerprintHashes.length}
            </span>

            <span
              className="ml-auto font-mono"
              style={{
                color:
                  state.confidence >= 0.9
                    ? "#4ade80"
                    : state.confidence >= 0.7
                      ? "#facc15"
                      : "#f87171",
              }}
            >
              {(state.confidence * 100).toFixed(0)}%
            </span>
          </div>

          {state.positionZone && (
            <div className="text-[10px] text-muted-foreground truncate">{state.positionZone}</div>
          )}
        </div>
      </div>

      {/* Output Handle */}
      <Handle
        type="source"
        position={Position.Bottom}
        className="w-2 h-2"
        style={{ background: colors.border }}
      />
    </>
  );
}

const StateNode = memo(StateNodeComponent);

// Custom node types
const nodeTypes = {
  stateNode: StateNode,
} as const;

// =============================================================================
// Layout Helper
// =============================================================================

function calculateLayout(
  states: FingerprintDiscoveryResult["states"],
  transitions: FingerprintDiscoveryResult["transitions"],
): Record<string, { x: number; y: number }> {
  const positions: Record<string, { x: number; y: number }> = {};

  if (states.length === 0) return positions;

  // Build adjacency information
  const outgoing = new Map<string, Set<string>>();
  const incoming = new Map<string, Set<string>>();

  states.forEach((s) => {
    outgoing.set(s.stateId, new Set());
    incoming.set(s.stateId, new Set());
  });

  transitions.forEach((t) => {
    outgoing.get(t.fromStateId)?.add(t.toStateId);
    incoming.get(t.toStateId)?.add(t.fromStateId);
  });

  // Group states by type for initial arrangement
  const globalStates = states.filter((s) => s.isGlobal);
  const modalStates = states.filter((s) => s.isModal);
  const contentStates = states.filter((s) => !s.isGlobal && !s.isModal);

  const NODE_WIDTH = 180;
  const NODE_HEIGHT = 100;
  const H_GAP = 80;
  const V_GAP = 60;

  let currentY = 0;

  // Place global states at the top
  if (globalStates.length > 0) {
    const rowWidth = globalStates.length * (NODE_WIDTH + H_GAP) - H_GAP;
    const startX = -rowWidth / 2;
    globalStates.forEach((state, i) => {
      positions[state.stateId] = {
        x: startX + i * (NODE_WIDTH + H_GAP),
        y: currentY,
      };
    });
    currentY += NODE_HEIGHT + V_GAP;
  }

  // Place content states in the middle
  if (contentStates.length > 0) {
    // Arrange in multiple rows if many states
    const maxPerRow = 4;
    const rows = Math.ceil(contentStates.length / maxPerRow);

    contentStates.forEach((state, i) => {
      const row = Math.floor(i / maxPerRow);
      const col = i % maxPerRow;
      const rowSize = Math.min(maxPerRow, contentStates.length - row * maxPerRow);
      const rowWidth = rowSize * (NODE_WIDTH + H_GAP) - H_GAP;
      const startX = -rowWidth / 2;

      positions[state.stateId] = {
        x: startX + col * (NODE_WIDTH + H_GAP),
        y: currentY + row * (NODE_HEIGHT + V_GAP),
      };
    });
    currentY += rows * (NODE_HEIGHT + V_GAP);
  }

  // Place modal states at the bottom
  if (modalStates.length > 0) {
    const rowWidth = modalStates.length * (NODE_WIDTH + H_GAP) - H_GAP;
    const startX = -rowWidth / 2;
    modalStates.forEach((state, i) => {
      positions[state.stateId] = {
        x: startX + i * (NODE_WIDTH + H_GAP),
        y: currentY,
      };
    });
  }

  return positions;
}

// =============================================================================
// Action Type Colors
// =============================================================================

const ACTION_TYPE_COLORS: Record<string, string> = {
  click: "#60a5fa",
  type: "#4ade80",
  hover: "#facc15",
  focus: "#c084fc",
  scroll: "#f97316",
  navigate: "#ec4899",
  default: "#a1a1aa",
};

// =============================================================================
// Main Component
// =============================================================================

export function StateGraphView({
  discoveryResult,
  fingerprintDetails: _fingerprintDetails,
  onSelectState,
  onSelectTransition,
  selectedStateId,
  selectedTransitionId,
}: StateGraphViewProps) {
  // Convert discovery result to nodes and edges
  const { initialNodes, initialEdges } = useMemo(() => {
    const positions = calculateLayout(discoveryResult.states, discoveryResult.transitions);

    const nodes: Node[] = discoveryResult.states.map((state) => ({
      id: state.stateId,
      type: "stateNode",
      position: positions[state.stateId] || { x: 0, y: 0 },
      data: {
        state,
        isSelected: state.stateId === selectedStateId,
      } satisfies StateNodeData,
    }));

    // Aggregate transitions with same from/to
    const transitionMap = new Map<string, { actionTypes: Set<string>; totalCount: number }>();
    discoveryResult.transitions.forEach((t) => {
      const key = `${t.fromStateId}->${t.toStateId}`;
      const existing = transitionMap.get(key);
      if (existing) {
        existing.actionTypes.add(t.actionType);
        existing.totalCount += t.count;
      } else {
        transitionMap.set(key, {
          actionTypes: new Set([t.actionType]),
          totalCount: t.count,
        });
      }
    });

    const edges: Edge[] = [];
    transitionMap.forEach((data, key) => {
      const [fromStateId, toStateId] = key.split("->");
      const actionTypes = Array.from(data.actionTypes);
      const primaryAction = actionTypes[0];
      const color = ACTION_TYPE_COLORS[primaryAction] || ACTION_TYPE_COLORS.default;

      edges.push({
        id: key,
        source: fromStateId,
        target: toStateId,
        label: actionTypes.length > 1 ? `${actionTypes.length} actions` : primaryAction,
        labelStyle: {
          fontSize: 10,
          fontWeight: 500,
          fill: color,
        },
        labelBgStyle: {
          fill: "#1a1a1b",
          fillOpacity: 0.9,
        },
        style: {
          stroke: color,
          strokeWidth: Math.min(2 + Math.log2(data.totalCount + 1), 5),
        },
        markerEnd: {
          type: MarkerType.ArrowClosed,
          color: color,
          width: 20,
          height: 20,
        },
        animated: true,
      });
    });

    return { initialNodes: nodes, initialEdges: edges };
  }, [discoveryResult, selectedStateId]);

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  // Update nodes when selection changes
  useEffect(() => {
    setNodes((nds) =>
      nds.map((node) => ({
        ...node,
        data: {
          ...node.data,
          isSelected: node.id === selectedStateId,
        },
      })),
    );
  }, [selectedStateId, setNodes]);

  // Update edges when transition selection changes
  useEffect(() => {
    setEdges((eds) =>
      eds.map((edge) => {
        const isSelected = edge.id === selectedTransitionId;
        if (isSelected) {
          return {
            ...edge,
            style: {
              ...edge.style,
              strokeWidth: 4,
              filter: "drop-shadow(0 0 6px currentColor)",
            },
            animated: true,
          };
        }
        // Reset to default stroke width (the original width is stored in initialEdges)
        return {
          ...edge,
          style: {
            ...edge.style,
            strokeWidth: 2, // Default width for non-selected edges
            filter: undefined,
          },
        };
      }),
    );
  }, [selectedTransitionId, setEdges]);

  // Sync nodes/edges when discovery result changes
  useEffect(() => {
    setNodes(initialNodes);
    setEdges(initialEdges);
  }, [initialNodes, initialEdges, setNodes, setEdges]);

  const handleNodeClick: NodeMouseHandler = useCallback(
    (_event, node) => {
      onSelectState?.(node.id);
    },
    [onSelectState],
  );

  const handleEdgeClick: EdgeMouseHandler = useCallback(
    (_event, edge) => {
      const [fromStateId, toStateId] = edge.id.split("->");
      onSelectTransition?.(fromStateId, toStateId);
    },
    [onSelectTransition],
  );

  const handlePaneClick = useCallback(() => {
    onSelectState?.(null);
  }, [onSelectState]);

  if (discoveryResult.states.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <AlertCircle className="w-8 h-8 opacity-50" />
        <p>No states discovered yet</p>
        <p className="text-xs">Run state discovery to visualize the state graph</p>
      </div>
    );
  }

  return (
    <div className="w-full h-full min-h-[400px]">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeClick={handleNodeClick}
        onEdgeClick={handleEdgeClick}
        onPaneClick={handlePaneClick}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.2, maxZoom: 1.5 }}
        defaultEdgeOptions={{
          style: { strokeWidth: 2 },
          animated: true,
        }}
        proOptions={{ hideAttribution: true }}
      >
        <Controls className="!bg-card !border-border/50 !rounded-lg [&>button]:!bg-card [&>button]:!border-border/30 [&>button]:!fill-muted-foreground [&>button:hover]:!bg-muted/50" />
        <MiniMap
          className="!bg-card !rounded-lg !border-border/50"
          nodeColor={(node) => {
            const data = node.data as unknown as StateNodeData | undefined;
            if (!data?.state) return "#71717a";
            const stateType = data.state.isGlobal
              ? "global"
              : data.state.isModal
                ? "modal"
                : "content";
            return STATE_TYPE_COLORS[stateType].border;
          }}
          maskColor="rgba(0, 0, 0, 0.5)"
        />
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
      </ReactFlow>

      {/* Legend */}
      <div className="absolute bottom-4 left-4 bg-card/90 border border-border/50 rounded-lg p-3 space-y-2">
        <div className="text-xs font-medium text-muted-foreground mb-2">State Types</div>
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2 text-xs">
            <div
              className="w-3 h-3 rounded"
              style={{ backgroundColor: STATE_TYPE_COLORS.global.border }}
            />
            <span className="text-muted-foreground">Global</span>
          </div>
          <div className="flex items-center gap-2 text-xs">
            <div
              className="w-3 h-3 rounded"
              style={{ backgroundColor: STATE_TYPE_COLORS.modal.border }}
            />
            <span className="text-muted-foreground">Modal</span>
          </div>
          <div className="flex items-center gap-2 text-xs">
            <div
              className="w-3 h-3 rounded"
              style={{ backgroundColor: STATE_TYPE_COLORS.content.border }}
            />
            <span className="text-muted-foreground">Content</span>
          </div>
        </div>
      </div>
    </div>
  );
}

export default StateGraphView;
