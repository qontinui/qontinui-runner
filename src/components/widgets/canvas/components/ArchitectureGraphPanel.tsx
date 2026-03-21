/**
 * ArchitectureGraphPanel
 *
 * Canvas panel type "ArchitectureGraph" — renders an interactive project
 * architecture diagram using @xyflow/react.
 *
 * Data schema:
 * {
 *   nodes: [{ id, label, layer, description?, changed? }],
 *   edges: [{ source, target, label?, type? }]
 * }
 *
 * Layer → color mapping:
 *   frontend  #3b82f6  (blue)
 *   backend   #22c55e  (green)
 *   runner    #f97316  (orange)
 *   python    #a855f7  (purple)
 *   shared    #71717a  (gray)
 *
 * Changed nodes get an amber glow border: #f59e0b
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
// Layer color system
// ============================================================================

type Layer = "frontend" | "backend" | "runner" | "python" | "shared";

const LAYER_COLORS: Record<Layer | "default", { bg: string; border: string; text: string }> = {
  frontend: { bg: "rgba(59,130,246,0.15)", border: "#3b82f6", text: "#93c5fd" },
  backend: { bg: "rgba(34,197,94,0.15)", border: "#22c55e", text: "#86efac" },
  runner: { bg: "rgba(249,115,22,0.15)", border: "#f97316", text: "#fdba74" },
  python: { bg: "rgba(168,85,247,0.15)", border: "#a855f7", text: "#d8b4fe" },
  shared: { bg: "rgba(113,113,122,0.15)", border: "#71717a", text: "#d4d4d8" },
  default: { bg: "rgba(113,113,122,0.15)", border: "#71717a", text: "#d4d4d8" },
};

const CHANGED_BORDER = "#f59e0b";
const CHANGED_GLOW = "0 0 0 2px #f59e0b80, 0 0 12px #f59e0b40";

// ============================================================================
// Architecture Node Data types
// ============================================================================

interface ArchNode {
  id: string;
  label: string;
  layer: Layer | string;
  description?: string;
  changed?: boolean;
}

interface ArchEdge {
  source: string;
  target: string;
  label?: string;
  type?: "dependency" | "api" | string;
}

interface ArchNodeData {
  archNode: ArchNode;
  [key: string]: unknown;
}

// ============================================================================
// Custom node component
// ============================================================================

function ArchNodeComponent({ data }: { data: ArchNodeData }) {
  const node = data.archNode;
  const colors = LAYER_COLORS[(node.layer as Layer) ?? "default"] ?? LAYER_COLORS.default;
  const isChanged = !!node.changed;

  return (
    <>
      <Handle type="target" position={Position.Top} style={{ background: colors.border }} />
      <div
        className="rounded-lg min-w-[140px] max-w-[200px] transition-all duration-200"
        style={{
          backgroundColor: colors.bg,
          border: `2px solid ${isChanged ? CHANGED_BORDER : colors.border}`,
          boxShadow: isChanged ? CHANGED_GLOW : undefined,
        }}
      >
        {/* Layer badge */}
        <div
          className="px-2.5 py-1.5 rounded-t-md text-[10px] font-semibold uppercase tracking-widest"
          style={{ backgroundColor: `${colors.border}20`, color: colors.text }}
        >
          {node.layer ?? "component"}
        </div>
        {/* Label */}
        <div className="px-2.5 py-2">
          <div className="text-xs font-semibold text-white truncate">{node.label}</div>
          {node.description && (
            <div className="text-[10px] text-zinc-400 mt-0.5 line-clamp-2">{node.description}</div>
          )}
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} style={{ background: colors.border }} />
    </>
  );
}

const MemoizedArchNode = memo(ArchNodeComponent);
const nodeTypes = { archNode: MemoizedArchNode } as const;

// ============================================================================
// Layout: arrange nodes in layer rows
// ============================================================================

const LAYER_ORDER: Array<Layer | string> = ["frontend", "runner", "backend", "python", "shared"];

function layoutNodes(nodes: ArchNode[]): Record<string, { x: number; y: number }> {
  const positions: Record<string, { x: number; y: number }> = {};
  const NODE_W = 190;
  const NODE_H = 90;
  const H_GAP = 60;
  const V_GAP = 70;

  // Group by layer
  const byLayer = new Map<string, ArchNode[]>();
  const unknownLayer: ArchNode[] = [];

  for (const n of nodes) {
    const layer = n.layer ?? "shared";
    if (LAYER_ORDER.includes(layer)) {
      const arr = byLayer.get(layer) ?? [];
      arr.push(n);
      byLayer.set(layer, arr);
    } else {
      unknownLayer.push(n);
    }
  }

  if (unknownLayer.length > 0) {
    const arr = byLayer.get("shared") ?? [];
    byLayer.set("shared", [...arr, ...unknownLayer]);
  }

  let currentY = 0;
  for (const layer of LAYER_ORDER) {
    const layerNodes = byLayer.get(layer);
    if (!layerNodes || layerNodes.length === 0) continue;

    const rowWidth = layerNodes.length * (NODE_W + H_GAP) - H_GAP;
    const startX = -rowWidth / 2;
    layerNodes.forEach((n, i) => {
      positions[n.id] = { x: startX + i * (NODE_W + H_GAP), y: currentY };
    });
    currentY += NODE_H + V_GAP;
  }

  return positions;
}

// ============================================================================
// Edge type → line style
// ============================================================================

function edgeStyle(type?: string) {
  if (type === "api") return { stroke: "#f97316", strokeWidth: 2 };
  return { stroke: "#7aa2f7", strokeWidth: 1.5 };
}

// ============================================================================
// Main component
// ============================================================================

export function ArchitectureGraphPanel({ data }: CanvasPanelComponentProps) {
  const rawNodes = useMemo(() => (data.nodes as ArchNode[] | undefined) ?? [], [data.nodes]);
  const rawEdges = useMemo(() => (data.edges as ArchEdge[] | undefined) ?? [], [data.edges]);

  const { initialNodes, initialEdges } = useMemo(() => {
    const positions = layoutNodes(rawNodes);

    const nodes: Node[] = rawNodes.map((n) => ({
      id: n.id,
      type: "archNode",
      position: positions[n.id] ?? { x: 0, y: 0 },
      data: { archNode: n } satisfies ArchNodeData,
    }));

    const edges: Edge[] = rawEdges.map((e, i) => ({
      id: `edge-${i}`,
      source: e.source,
      target: e.target,
      label: e.label,
      labelStyle: { fontSize: 10, fill: "#94a3b8" },
      labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
      style: edgeStyle(e.type),
      markerEnd: {
        type: MarkerType.ArrowClosed,
        width: 16,
        height: 16,
        color: e.type === "api" ? "#f97316" : "#7aa2f7",
      },
    }));

    return { initialNodes: nodes, initialEdges: edges };
  }, [rawNodes, rawEdges]);

  const [nodes, , onNodesChange] = useNodesState(initialNodes);
  const [edges, , onEdgesChange] = useEdgesState(initialEdges);

  if (rawNodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-48 text-sm text-zinc-500 italic">
        No architecture nodes found
      </div>
    );
  }

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
            const d = node.data as unknown as ArchNodeData | undefined;
            const layer = (d?.archNode?.layer ?? "shared") as Layer;
            return (LAYER_COLORS[layer] ?? LAYER_COLORS.default).border;
          }}
          maskColor="rgba(0,0,0,0.5)"
        />
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
      </ReactFlow>

      {/* Legend */}
      <div className="absolute bottom-12 left-2 bg-card/90 border border-border/50 rounded-lg p-2.5 space-y-1.5 z-10">
        <div className="text-[10px] font-semibold text-zinc-400 mb-1">Layers</div>
        {(Object.entries(LAYER_COLORS) as [string, { border: string }][])
          .filter(([k]) => k !== "default")
          .map(([layer, colors]) => (
            <div key={layer} className="flex items-center gap-1.5 text-[10px]">
              <div className="w-2.5 h-2.5 rounded-xs" style={{ backgroundColor: colors.border }} />
              <span className="text-zinc-400 capitalize">{layer}</span>
            </div>
          ))}
        <div className="flex items-center gap-1.5 text-[10px] mt-1 pt-1 border-t border-border/30">
          <div className="w-2.5 h-2.5 rounded-xs" style={{ backgroundColor: CHANGED_BORDER }} />
          <span className="text-zinc-400">Changed</span>
        </div>
      </div>
    </div>
  );
}
