/**
 * DependencyGraphPanel
 *
 * Canvas panel type "DependencyGraph" — renders an interactive dependency
 * graph between workflow steps using @xyflow/react.
 *
 * Data schema:
 * {
 *   nodes: [{ id, label, type, phase, is_referenced, cost_category? }],
 *   edges: [{ source, target, label?, edge_type }]
 * }
 *
 * Phase -> color mapping:
 *   setup        #3b82f6  (blue)
 *   verification #22c55e  (green)
 *   agentic      #f59e0b  (amber)
 *   completion   #a855f7  (purple)
 *
 * Unreferenced nodes get a dashed border and muted opacity.
 *
 * Edge styles:
 *   explicit_depends_on  solid
 *   implicit_reference   dashed
 *   setup_provides       dotted
 */

import { memo, useMemo } from "react";
import {
  ReactFlow,
  MiniMap,
  Controls,
  Background,
  useNodesState,
  useEdgesState,
  type Node,
  type Edge,
  BackgroundVariant,
  Position,
  Handle,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { CanvasPanelComponentProps } from "./types";

// ============================================================================
// Phase color system
// ============================================================================

type Phase = "setup" | "verification" | "agentic" | "completion";

const PHASE_COLORS: Record<Phase | "default", { bg: string; border: string; text: string }> = {
  setup: { bg: "rgba(59,130,246,0.15)", border: "#3b82f6", text: "#93c5fd" },
  verification: { bg: "rgba(34,197,94,0.15)", border: "#22c55e", text: "#86efac" },
  agentic: { bg: "rgba(245,158,11,0.15)", border: "#f59e0b", text: "#fcd34d" },
  completion: { bg: "rgba(168,85,247,0.15)", border: "#a855f7", text: "#d8b4fe" },
  default: { bg: "rgba(113,113,122,0.15)", border: "#71717a", text: "#d4d4d8" },
};

// ============================================================================
// Dependency Node Data types
// ============================================================================

interface DepNode {
  id: string;
  label: string;
  type: string;
  phase: string;
  is_referenced: boolean;
  cost_category?: string;
}

interface DepEdge {
  source: string;
  target: string;
  label?: string;
  edge_type: "explicit_depends_on" | "implicit_reference" | "setup_provides";
}

interface DepNodeData {
  depNode: DepNode;
  [key: string]: unknown;
}

// ============================================================================
// Custom node component
// ============================================================================

function DepNodeComponent({ data }: { data: DepNodeData }) {
  const node = data.depNode;
  const colors = PHASE_COLORS[(node.phase as Phase) ?? "default"] ?? PHASE_COLORS.default;
  const isUnreferenced = !node.is_referenced;

  return (
    <>
      <Handle type="target" position={Position.Top} style={{ background: colors.border }} />
      <div
        className="rounded-lg min-w-[140px] max-w-[200px] transition-all duration-200"
        style={{
          backgroundColor: colors.bg,
          border: `2px ${isUnreferenced ? "dashed" : "solid"} ${colors.border}`,
          opacity: isUnreferenced ? 0.55 : 1,
        }}
      >
        {/* Type badge */}
        <div
          className="px-2.5 py-1.5 rounded-t-md text-[10px] font-semibold uppercase tracking-widest"
          style={{ backgroundColor: `${colors.border}20`, color: colors.text }}
        >
          {node.type}
        </div>
        {/* Label */}
        <div className="px-2.5 py-2">
          <div className="text-xs font-semibold text-white truncate">{node.label}</div>
          {node.cost_category && (
            <div className="text-[10px] text-zinc-400 mt-0.5">{node.cost_category}</div>
          )}
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} style={{ background: colors.border }} />
    </>
  );
}

const MemoizedDepNode = memo(DepNodeComponent);
const nodeTypes = { depNode: MemoizedDepNode } as const;

// ============================================================================
// Layout: arrange nodes top-to-bottom by phase (dagre-style)
// ============================================================================

const PHASE_ORDER: Array<Phase | string> = ["setup", "verification", "agentic", "completion"];

function layoutNodes(nodes: DepNode[]): Record<string, { x: number; y: number }> {
  const positions: Record<string, { x: number; y: number }> = {};
  const NODE_W = 190;
  const NODE_H = 90;
  const H_GAP = 60;
  const V_GAP = 70;

  // Group by phase
  const byPhase = new Map<string, DepNode[]>();
  const unknownPhase: DepNode[] = [];

  for (const n of nodes) {
    const phase = n.phase ?? "completion";
    if (PHASE_ORDER.includes(phase)) {
      const arr = byPhase.get(phase) ?? [];
      arr.push(n);
      byPhase.set(phase, arr);
    } else {
      unknownPhase.push(n);
    }
  }

  if (unknownPhase.length > 0) {
    const arr = byPhase.get("completion") ?? [];
    byPhase.set("completion", [...arr, ...unknownPhase]);
  }

  let currentY = 0;
  for (const phase of PHASE_ORDER) {
    const phaseNodes = byPhase.get(phase);
    if (!phaseNodes || phaseNodes.length === 0) continue;

    const rowWidth = phaseNodes.length * (NODE_W + H_GAP) - H_GAP;
    const startX = -rowWidth / 2;
    phaseNodes.forEach((n, i) => {
      positions[n.id] = { x: startX + i * (NODE_W + H_GAP), y: currentY };
    });
    currentY += NODE_H + V_GAP;
  }

  return positions;
}

// ============================================================================
// Edge type -> line style
// ============================================================================

type DepEdgeType = "explicit_depends_on" | "implicit_reference" | "setup_provides";

const EDGE_STYLES: Record<
  DepEdgeType,
  { stroke: string; strokeWidth: number; strokeDasharray?: string }
> = {
  explicit_depends_on: { stroke: "#7aa2f7", strokeWidth: 2 },
  implicit_reference: { stroke: "#9ca3af", strokeWidth: 1.5, strokeDasharray: "6 3" },
  setup_provides: { stroke: "#22c55e", strokeWidth: 1.5, strokeDasharray: "2 3" },
};

const EDGE_COLORS: Record<DepEdgeType, string> = {
  explicit_depends_on: "#7aa2f7",
  implicit_reference: "#9ca3af",
  setup_provides: "#22c55e",
};

// ============================================================================
// Main component
// ============================================================================

export function DependencyGraphPanel({ data }: CanvasPanelComponentProps) {
  const rawNodes = useMemo(() => (data.nodes as DepNode[] | undefined) ?? [], [data.nodes]);
  const rawEdges = useMemo(() => (data.edges as DepEdge[] | undefined) ?? [], [data.edges]);

  // Stable key that changes when data changes, forcing ReactFlow to remount
  // with fresh useNodesState/useEdgesState. This replaces the anti-pattern of
  // syncing derived state via useEffect.
  const dataKey = useMemo(
    () =>
      rawNodes.map((n) => n.id).join(",") +
      "|" +
      rawEdges.map((e) => `${e.source}-${e.target}`).join(","),
    [rawNodes, rawEdges],
  );

  if (rawNodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-48 text-sm text-zinc-500 italic">
        No dependency nodes found
      </div>
    );
  }

  return <DependencyGraphFlow key={dataKey} rawNodes={rawNodes} rawEdges={rawEdges} />;
}

/** Inner component that owns ReactFlow interactive state; remounted via key when data changes. */
function DependencyGraphFlow({ rawNodes, rawEdges }: { rawNodes: DepNode[]; rawEdges: DepEdge[] }) {
  const { initialNodes, initialEdges } = useMemo(() => {
    const positions = layoutNodes(rawNodes);

    const nodes: Node[] = rawNodes.map((n) => ({
      id: n.id,
      type: "depNode",
      position: positions[n.id] ?? { x: 0, y: 0 },
      data: { depNode: n } satisfies DepNodeData,
    }));

    const edges: Edge[] = rawEdges.map((e, i) => {
      const edgeType = e.edge_type as DepEdgeType;
      const style = EDGE_STYLES[edgeType] ?? EDGE_STYLES.explicit_depends_on;
      const color = EDGE_COLORS[edgeType] ?? EDGE_COLORS.explicit_depends_on;

      return {
        id: `edge-${i}`,
        source: e.source,
        target: e.target,
        label: e.label,
        labelStyle: { fontSize: 10, fill: "#94a3b8" },
        labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
        style,
        markerEnd: {
          type: MarkerType.ArrowClosed,
          width: 16,
          height: 16,
          color,
        },
      };
    });

    return { initialNodes: nodes, initialEdges: edges };
  }, [rawNodes, rawEdges]);

  const [nodes, , onNodesChange] = useNodesState(initialNodes);
  const [edges, , onEdgesChange] = useEdgesState(initialEdges);

  return (
    <div className="w-full relative" style={{ height: 420 }}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.2, maxZoom: 1.5 }}
        proOptions={{ hideAttribution: true }}
      >
        <Controls className="!bg-card !border-border/50 !rounded-lg [&>button]:!bg-card [&>button]:!border-border/30 [&>button]:!fill-muted-foreground [&>button:hover]:!bg-muted/50" />
        <MiniMap
          className="!bg-card !rounded-lg !border-border/50"
          nodeColor={(node) => {
            const d = node.data as unknown as DepNodeData | undefined;
            const phase = (d?.depNode?.phase ?? "default") as Phase;
            return (PHASE_COLORS[phase] ?? PHASE_COLORS.default).border;
          }}
          maskColor="rgba(0,0,0,0.5)"
        />
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
      </ReactFlow>

      {/* Legend */}
      <div className="absolute bottom-12 left-2 bg-card/90 border border-border/50 rounded-lg p-2.5 space-y-1.5 z-10">
        <div className="text-[10px] font-semibold text-zinc-400 mb-1">Phases</div>
        {(Object.entries(PHASE_COLORS) as [string, { border: string }][])
          .filter(([k]) => k !== "default")
          .map(([phase, colors]) => (
            <div key={phase} className="flex items-center gap-1.5 text-[10px]">
              <div className="w-2.5 h-2.5 rounded-xs" style={{ backgroundColor: colors.border }} />
              <span className="text-zinc-400 capitalize">{phase}</span>
            </div>
          ))}
        <div className="text-[10px] font-semibold text-zinc-400 mt-2 mb-1 pt-1 border-t border-border/30">
          Edges
        </div>
        <div className="flex items-center gap-1.5 text-[10px]">
          <div className="w-4 h-0 border-t-2 border-[#7aa2f7]" />
          <span className="text-zinc-400">Depends on</span>
        </div>
        <div className="flex items-center gap-1.5 text-[10px]">
          <div className="w-4 h-0 border-t-2 border-dashed border-[#9ca3af]" />
          <span className="text-zinc-400">Implicit ref</span>
        </div>
        <div className="flex items-center gap-1.5 text-[10px]">
          <div className="w-4 h-0 border-t-2 border-dotted border-[#22c55e]" />
          <span className="text-zinc-400">Setup provides</span>
        </div>
      </div>
    </div>
  );
}
