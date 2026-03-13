import { useMemo, useCallback, useEffect, memo } from "react";
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
import dagre from "dagre";
import "@xyflow/react/dist/style.css";
import type { ComponentNode, ComponentEdge } from "@/types/architecture";

// =============================================================================
// Health color mapping
// =============================================================================

function healthColor(score: number): { bg: string; border: string; text: string } {
  if (score > 0.7) return { bg: "rgba(34,197,94,0.18)", border: "#22c55e", text: "#86efac" };
  if (score > 0.4) return { bg: "rgba(245,158,11,0.18)", border: "#f59e0b", text: "#fcd34d" };
  return { bg: "rgba(239,68,68,0.18)", border: "#ef4444", text: "#fca5a5" };
}

function basename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

function parentDir(path: string): string {
  const parts = path.split("/");
  if (parts.length <= 1) return "";
  return parts[parts.length - 2];
}

// =============================================================================
// Custom node
// =============================================================================

interface HealthNodeData {
  node: ComponentNode;
  [key: string]: unknown;
}

const HealthNode = memo(({ data }: { data: HealthNodeData }) => {
  const n = data.node;
  const colors = healthColor(n.health_score);
  const activity = n.fix_count + n.causal_involvement_count;
  const width = Math.min(220, Math.max(140, 140 + activity * 8));
  const dir = parentDir(n.component_path);

  return (
    <div
      style={{
        width,
        background: colors.bg,
        border: `1.5px solid ${colors.border}`,
        borderRadius: 8,
        padding: "8px 10px",
      }}
    >
      <Handle type="target" position={Position.Top} style={{ background: colors.border }} />
      <div className="flex items-center gap-1.5 mb-1">
        {dir && (
          <span
            className="text-[9px] px-1 py-0.5 rounded"
            style={{ background: "rgba(255,255,255,0.08)", color: "#94a3b8" }}
          >
            {dir}
          </span>
        )}
        <span className="text-xs font-medium truncate" style={{ color: colors.text }}>
          {basename(n.component_path)}
        </span>
      </div>
      <div className="flex items-center gap-2 text-[10px]" style={{ color: "#94a3b8" }}>
        <span>{n.fix_count} fixes</span>
        <span className="opacity-40">|</span>
        <span>{Math.round(n.health_score * 100)}%</span>
      </div>
      <Handle type="source" position={Position.Bottom} style={{ background: colors.border }} />
    </div>
  );
});
HealthNode.displayName = "HealthNode";

const nodeTypes = { healthNode: HealthNode };

// =============================================================================
// Layout
// =============================================================================

function layoutGraph(
  nodes: ComponentNode[],
  edges: ComponentEdge[],
): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", ranksep: 80, nodesep: 40 });

  // Node dimensions scale with activity
  for (const n of nodes) {
    const activity = n.fix_count + n.causal_involvement_count;
    const width = Math.min(220, Math.max(140, 140 + activity * 8));
    g.setNode(n.component_path, { width, height: 60 });
  }

  for (const e of edges) {
    g.setEdge(e.source, e.target);
  }

  dagre.layout(g);

  const flowNodes: Node[] = nodes.map((n) => {
    const pos = g.node(n.component_path);
    return {
      id: n.component_path,
      type: "healthNode",
      position: { x: (pos?.x ?? 0) - 70, y: (pos?.y ?? 0) - 30 },
      data: { node: n } satisfies HealthNodeData,
    };
  });

  const flowEdges: Edge[] = edges.map((e, i) => {
    const isImpact = e.relationship_type === "impacts";
    const strokeWidth = Math.min(4, Math.max(1, e.strength));
    return {
      id: `edge-${i}`,
      source: e.source,
      target: e.target,
      label: e.relationship_type.replace(/_/g, " "),
      labelStyle: { fontSize: 9, fill: "#94a3b8" },
      labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
      style: {
        stroke: isImpact ? "#f97316" : "#7aa2f7",
        strokeWidth,
      },
      markerEnd: {
        type: MarkerType.ArrowClosed,
        width: 14,
        height: 14,
        color: isImpact ? "#f97316" : "#7aa2f7",
      },
    };
  });

  return { nodes: flowNodes, edges: flowEdges };
}

// =============================================================================
// Component
// =============================================================================

interface Props {
  nodes: ComponentNode[];
  edges: ComponentEdge[];
  onNodeClick?: (componentPath: string) => void;
}

export function ArchitectureGraphPanel({ nodes: rawNodes, edges: rawEdges, onNodeClick }: Props) {
  const { nodes: initialNodes, edges: initialEdges } = useMemo(
    () => layoutGraph(rawNodes, rawEdges),
    [rawNodes, rawEdges],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  // Sync when data changes (workflow switch, rebuild, refresh)
  useEffect(() => {
    setNodes(initialNodes);
    setEdges(initialEdges);
  }, [initialNodes, initialEdges, setNodes, setEdges]);

  const handleNodeClick = useCallback(
    (_: React.MouseEvent, node: Node) => {
      onNodeClick?.(node.id);
    },
    [onNodeClick],
  );

  return (
    <div className="w-full h-full relative" style={{ minHeight: 400 }}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeClick={handleNodeClick}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.2, maxZoom: 1.5 }}
        proOptions={{ hideAttribution: true }}
      >
        <Controls className="!bg-card !border-border/50 !rounded-lg [&>button]:!bg-card [&>button]:!border-border/30 [&>button]:!fill-muted-foreground [&>button:hover]:!bg-muted/50" />
        <MiniMap
          nodeStrokeWidth={3}
          style={{ background: "#1a1b26", border: "1px solid rgba(255,255,255,0.1)" }}
          maskColor="rgba(0,0,0,0.6)"
        />
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
      </ReactFlow>

      {/* Legend */}
      <div className="absolute bottom-12 left-2 bg-card/90 border border-border/50 rounded-lg p-2 space-y-1 z-10 text-[10px]">
        <div className="flex items-center gap-1.5">
          <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#22c55e" }} />
          <span className="text-muted-foreground">Healthy (&gt;70%)</span>
        </div>
        <div className="flex items-center gap-1.5">
          <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#f59e0b" }} />
          <span className="text-muted-foreground">Moderate (40-70%)</span>
        </div>
        <div className="flex items-center gap-1.5">
          <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#ef4444" }} />
          <span className="text-muted-foreground">Problematic (&lt;40%)</span>
        </div>
        <div className="mt-1 pt-1 border-t border-border/30 space-y-1">
          <div className="flex items-center gap-1.5">
            <span className="w-4 h-0.5" style={{ background: "#f97316" }} />
            <span className="text-muted-foreground">impacts</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="w-4 h-0.5" style={{ background: "#7aa2f7" }} />
            <span className="text-muted-foreground">co-changes</span>
          </div>
        </div>
      </div>
    </div>
  );
}
