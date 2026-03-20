/**
 * DeveloperDependencyGraph
 *
 * Renders an interactive ReactFlow dependency graph of features from an
 * SdkArchitectureGraph. Nodes represent features, edges represent
 * dependency relationships (requires/extends/uses).
 *
 * Interactions:
 *   - Hover a node to dim unconnected nodes/edges
 *   - Click a node to open a detail sidebar
 *
 * Layout uses dagre with left-to-right ranking.
 */

import { memo, useMemo, useState, useCallback, useRef } from "react";
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
import type {
  SdkArchitectureGraph,
  SdkArchitectureNode,
  SdkArchitectureEdge,
} from "@/types/architecture";

// =============================================================================
// Priority color system
// =============================================================================

type PriorityKey = "core" | "critical" | "important" | "high" | "medium" | "nice-to-have" | "low";

const PRIORITY_COLORS: Record<
  PriorityKey | "default",
  { bg: string; border: string; dot: string }
> = {
  core: { bg: "rgba(59,130,246,0.15)", border: "#3b82f6", dot: "#3b82f6" },
  critical: { bg: "rgba(59,130,246,0.15)", border: "#3b82f6", dot: "#3b82f6" },
  important: { bg: "rgba(34,197,94,0.15)", border: "#22c55e", dot: "#22c55e" },
  high: { bg: "rgba(34,197,94,0.15)", border: "#22c55e", dot: "#22c55e" },
  medium: { bg: "rgba(245,158,11,0.15)", border: "#f59e0b", dot: "#f59e0b" },
  "nice-to-have": { bg: "rgba(113,113,122,0.15)", border: "#71717a", dot: "#71717a" },
  low: { bg: "rgba(113,113,122,0.15)", border: "#71717a", dot: "#71717a" },
  default: { bg: "rgba(113,113,122,0.15)", border: "#71717a", dot: "#71717a" },
};

function priorityColor(priority?: string) {
  if (!priority) return PRIORITY_COLORS.default;
  return PRIORITY_COLORS[priority as PriorityKey] ?? PRIORITY_COLORS.default;
}

// =============================================================================
// Edge style system
// =============================================================================

type DepEdgeType = "requires" | "extends" | "uses";

const EDGE_STYLE_MAP: Record<
  DepEdgeType,
  { stroke: string; strokeWidth: number; strokeDasharray?: string; animated: boolean }
> = {
  requires: { stroke: "#3b82f6", strokeWidth: 2, animated: true },
  extends: { stroke: "#a855f7", strokeWidth: 1.5, strokeDasharray: "6 3", animated: false },
  uses: { stroke: "#71717a", strokeWidth: 1.5, strokeDasharray: "2 3", animated: false },
};

// =============================================================================
// Status indicator
// =============================================================================

function StatusDot({ status }: { status?: string }) {
  let color = "#71717a";
  if (status === "active" || status === "complete" || status === "done") color = "#22c55e";
  else if (status === "in-progress" || status === "wip") color = "#f59e0b";
  else if (status === "planned" || status === "todo") color = "#3b82f6";
  else if (status === "deprecated" || status === "removed") color = "#ef4444";

  return (
    <span
      className="inline-block w-1.5 h-1.5 rounded-full flex-shrink-0"
      style={{ backgroundColor: color }}
      title={status ?? "unknown"}
    />
  );
}

// =============================================================================
// Custom node component
// =============================================================================

interface FeatureNodeData {
  feature: SdkArchitectureNode;
  dimmed: boolean;
  [key: string]: unknown;
}

const FeatureNode = memo(({ data }: { data: FeatureNodeData }) => {
  const feature = data.feature;
  const colors = priorityColor(feature.priority);

  return (
    <div
      className="rounded-lg transition-opacity duration-200"
      style={{
        width: 200,
        background: colors.bg,
        border: `2px solid ${colors.border}`,
        opacity: data.dimmed ? 0.2 : 1,
      }}
    >
      <Handle type="target" position={Position.Left} style={{ background: colors.border }} />
      <div className="px-3 py-2.5">
        <div className="flex items-center gap-1.5 mb-1">
          <span
            className="w-2 h-2 rounded-full flex-shrink-0"
            style={{ backgroundColor: colors.dot }}
          />
          <span className="text-xs font-semibold text-white truncate flex-1">{feature.label}</span>
          <StatusDot status={feature.status} />
        </div>
        {feature.priority && (
          <span className="text-[10px] text-zinc-400 capitalize">{feature.priority}</span>
        )}
      </div>
      <Handle type="source" position={Position.Right} style={{ background: colors.border }} />
    </div>
  );
});
FeatureNode.displayName = "FeatureNode";

const nodeTypes = { featureNode: FeatureNode };

// =============================================================================
// Dagre layout (LR)
// =============================================================================

const NODE_W = 200;
const NODE_H = 60;

function layoutGraph(
  features: SdkArchitectureNode[],
  edges: SdkArchitectureEdge[],
): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "LR", ranksep: 100, nodesep: 40 });

  for (const f of features) {
    g.setNode(f.id, { width: NODE_W, height: NODE_H });
  }

  const featureIds = new Set(features.map((f) => f.id));

  const processedEdges: Array<
    SdkArchitectureEdge & { resolvedSource: string; resolvedTarget: string }
  > = [];
  for (const e of edges) {
    // Edge source/target already include the "feature:" prefix (matching node IDs)
    const source = e.source;
    const target = e.target;
    if (featureIds.has(source) && featureIds.has(target)) {
      g.setEdge(source, target);
      processedEdges.push({ ...e, resolvedSource: source, resolvedTarget: target });
    }
  }

  dagre.layout(g);

  const flowNodes: Node[] = features.map((f) => {
    const pos = g.node(f.id);
    return {
      id: f.id,
      type: "featureNode",
      position: { x: (pos?.x ?? 0) - NODE_W / 2, y: (pos?.y ?? 0) - NODE_H / 2 },
      data: { feature: f, dimmed: false } satisfies FeatureNodeData,
    };
  });

  const flowEdges: Edge[] = processedEdges.map((e, i) => {
    const edgeType = e.type as DepEdgeType;
    const style = EDGE_STYLE_MAP[edgeType] ?? EDGE_STYLE_MAP.uses;
    return {
      id: `dep-edge-${i}`,
      source: e.resolvedSource,
      target: e.resolvedTarget,
      label: e.label ?? e.type,
      labelStyle: { fontSize: 9, fill: "#94a3b8" },
      labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
      animated: style.animated,
      style: {
        stroke: style.stroke,
        strokeWidth: style.strokeWidth,
        strokeDasharray: style.strokeDasharray,
      },
      markerEnd: {
        type: MarkerType.ArrowClosed,
        width: 14,
        height: 14,
        color: style.stroke,
      },
    };
  });

  return { nodes: flowNodes, edges: flowEdges };
}

// =============================================================================
// Connection helpers
// =============================================================================

/** Returns the set of node IDs directly connected to the given node. */
function getConnectedIds(nodeId: string, edges: Edge[]): Set<string> {
  const connected = new Set<string>();
  connected.add(nodeId);
  for (const e of edges) {
    if (e.source === nodeId) connected.add(e.target);
    if (e.target === nodeId) connected.add(e.source);
  }
  return connected;
}

/** Returns edges directly connected to the given node. */
function getConnectedEdgeIds(nodeId: string, edges: Edge[]): Set<string> {
  const ids = new Set<string>();
  for (const e of edges) {
    if (e.source === nodeId || e.target === nodeId) ids.add(e.id);
  }
  return ids;
}

// =============================================================================
// Detail sidebar
// =============================================================================

interface DetailSidebarProps {
  feature: SdkArchitectureNode;
  depsOut: Array<{ id: string; label: string; type: string }>;
  depsIn: Array<{ id: string; label: string; type: string }>;
  onClose: () => void;
}

function DetailSidebar({ feature, depsOut, depsIn, onClose }: DetailSidebarProps) {
  const colors = priorityColor(feature.priority);

  return (
    <div className="w-72 bg-card border-l border-border/50 overflow-y-auto flex-shrink-0">
      {/* Header */}
      <div className="flex items-start justify-between p-4 border-b border-border/30">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <span
              className="w-2.5 h-2.5 rounded-full flex-shrink-0"
              style={{ backgroundColor: colors.dot }}
            />
            <h3 className="text-sm font-semibold text-white truncate">{feature.label}</h3>
          </div>
          <div className="flex items-center gap-2 text-[11px]">
            {feature.priority && (
              <span
                className="px-1.5 py-0.5 rounded capitalize"
                style={{
                  background: colors.bg,
                  color: colors.dot,
                  border: `1px solid ${colors.border}`,
                }}
              >
                {feature.priority}
              </span>
            )}
            {feature.status && (
              <span className="flex items-center gap-1 text-zinc-400">
                <StatusDot status={feature.status} />
                {feature.status}
              </span>
            )}
          </div>
        </div>
        <button
          onClick={onClose}
          className="p-1 rounded hover:bg-muted/50 text-zinc-400 hover:text-white transition-colors flex-shrink-0"
          aria-label="Close detail sidebar"
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path
              d="M3 3L11 11M11 3L3 11"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>

      <div className="p-4 space-y-4">
        {/* Description */}
        {feature.description && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Description
            </h4>
            <p className="text-xs text-zinc-300 leading-relaxed">{feature.description}</p>
          </div>
        )}

        {/* Entry Points */}
        {feature.entryPoints && feature.entryPoints.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Entry Points
            </h4>
            <div className="space-y-0.5">
              {feature.entryPoints.map((ep, i) => (
                <div
                  key={`${ep}-${i}`}
                  className="text-[11px] font-mono text-zinc-400 bg-muted/30 rounded px-1.5 py-0.5 truncate"
                  title={ep}
                >
                  {ep}
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Tech Used */}
        {feature.techUsed && feature.techUsed.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Tech Used
            </h4>
            <div className="flex flex-wrap gap-1">
              {feature.techUsed.map((tech, i) => (
                <span
                  key={`${tech}-${i}`}
                  className="text-[10px] px-1.5 py-0.5 rounded-full bg-muted/50 text-zinc-300 border border-border/30"
                >
                  {tech}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Dependencies Out */}
        {depsOut.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Dependencies Out
            </h4>
            <div className="space-y-1">
              {depsOut.map((dep) => (
                <div key={dep.id} className="flex items-center gap-1.5 text-xs">
                  <span className="text-zinc-500">{dep.type}</span>
                  <span className="text-zinc-300">{dep.label}</span>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Dependencies In */}
        {depsIn.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Dependencies In
            </h4>
            <div className="space-y-1">
              {depsIn.map((dep) => (
                <div key={dep.id} className="flex items-center gap-1.5 text-xs">
                  <span className="text-zinc-500">{dep.type}</span>
                  <span className="text-zinc-300">{dep.label}</span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Main component
// =============================================================================

interface Props {
  project: SdkArchitectureGraph;
}

export function DeveloperDependencyGraph({ project }: Props) {
  const features = useMemo(
    () => project.nodes.filter((n) => n.type === "feature"),
    [project.nodes],
  );

  const dataKey = useMemo(
    () =>
      features.map((f) => f.id).join(",") +
      "|" +
      project.edges.map((e) => `${e.source}-${e.target}`).join(","),
    [features, project.edges],
  );

  if (features.length === 0) {
    return (
      <div className="flex items-center justify-center h-48 text-sm text-zinc-500 italic">
        No feature nodes found in architecture graph
      </div>
    );
  }

  return (
    <DeveloperDependencyGraphFlow
      key={dataKey}
      features={features}
      allNodes={project.nodes}
      edges={project.edges}
    />
  );
}

/** Inner component that owns ReactFlow interactive state; remounted via key when data changes. */
function DeveloperDependencyGraphFlow({
  features,
  allNodes,
  edges: rawEdges,
}: {
  features: SdkArchitectureNode[];
  allNodes: SdkArchitectureNode[];
  edges: SdkArchitectureEdge[];
}) {
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
  const [selectedNode, setSelectedNode] = useState<SdkArchitectureNode | null>(null);

  const { nodes: initialNodes, edges: initialEdges } = useMemo(
    () => layoutGraph(features, rawEdges),
    [features, rawEdges],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  // Sync when data changes
  const prevInitialNodesRef = useRef(initialNodes);
  const prevInitialEdgesRef = useRef(initialEdges);
  if (prevInitialNodesRef.current !== initialNodes) {
    prevInitialNodesRef.current = initialNodes;
    setNodes(initialNodes);
  }
  if (prevInitialEdgesRef.current !== initialEdges) {
    prevInitialEdgesRef.current = initialEdges;
    setEdges(initialEdges);
  }

  // Build a lookup map: node id -> SdkArchitectureNode
  // Use a ref to stabilize the lookup so handleNodeClick doesn't cause ReactFlow re-renders
  const nodeById = useMemo(() => {
    const map = new Map<string, SdkArchitectureNode>();
    for (const n of allNodes) {
      map.set(n.id, n);
    }
    return map;
  }, [allNodes]);

  const nodeByIdRef = useRef(nodeById);
  nodeByIdRef.current = nodeById;

  // Apply hover dimming
  const displayNodes = useMemo(() => {
    if (!hoveredNodeId) {
      return nodes.map((n) => ({
        ...n,
        data: { ...n.data, dimmed: false },
      }));
    }
    const connected = getConnectedIds(hoveredNodeId, edges);
    return nodes.map((n) => ({
      ...n,
      data: { ...n.data, dimmed: !connected.has(n.id) },
    }));
  }, [nodes, edges, hoveredNodeId]);

  const displayEdges = useMemo(() => {
    if (!hoveredNodeId) {
      return edges.map((e) => ({ ...e, style: { ...e.style, opacity: 1 } }));
    }
    const connectedEdges = getConnectedEdgeIds(hoveredNodeId, edges);
    return edges.map((e) => ({
      ...e,
      style: { ...e.style, opacity: connectedEdges.has(e.id) ? 1 : 0.1 },
    }));
  }, [edges, hoveredNodeId]);

  const handleNodeMouseEnter = useCallback((_: React.MouseEvent, node: Node) => {
    setHoveredNodeId(node.id);
  }, []);

  const handleNodeMouseLeave = useCallback(() => {
    setHoveredNodeId(null);
  }, []);

  const handleNodeClick = useCallback((_: React.MouseEvent, node: Node) => {
    const feature = nodeByIdRef.current.get(node.id);
    if (feature) setSelectedNode(feature);
  }, []);

  const handleCloseDetail = useCallback(() => {
    setSelectedNode(null);
  }, []);

  // Compute dependency lists for sidebar
  const { depsOut, depsIn } = useMemo(() => {
    if (!selectedNode) return { depsOut: [], depsIn: [] };

    const out: Array<{ id: string; label: string; type: string }> = [];
    const inbound: Array<{ id: string; label: string; type: string }> = [];

    for (const e of rawEdges) {
      // Edge source/target already include the "feature:" prefix (matching node IDs)
      if (e.source === selectedNode.id) {
        const targetNode = nodeById.get(e.target);
        if (targetNode) {
          out.push({ id: e.target, label: targetNode.label, type: e.type });
        }
      }
      if (e.target === selectedNode.id) {
        const sourceNode = nodeById.get(e.source);
        if (sourceNode) {
          inbound.push({ id: e.source, label: sourceNode.label, type: e.type });
        }
      }
    }

    return { depsOut: out, depsIn: inbound };
  }, [selectedNode, rawEdges, nodeById]);

  return (
    <div className="flex w-full h-full" style={{ minHeight: 500 }}>
      {/* Graph area */}
      <div className="flex-1 relative">
        <ReactFlow
          nodes={displayNodes}
          edges={displayEdges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onNodeMouseEnter={handleNodeMouseEnter}
          onNodeMouseLeave={handleNodeMouseLeave}
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
            nodeColor={(node) => {
              const d = node.data as unknown as FeatureNodeData | undefined;
              return priorityColor(d?.feature?.priority).dot;
            }}
            maskColor="rgba(0,0,0,0.6)"
          />
          <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
        </ReactFlow>

        {/* Legend */}
        <div className="absolute bottom-12 left-2 bg-card/90 border border-border/50 rounded-lg p-2.5 space-y-1.5 z-10">
          <div className="text-[10px] font-semibold text-zinc-400 mb-1">Priority</div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#3b82f6" }} />
            <span className="text-zinc-400">Core / Critical</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#22c55e" }} />
            <span className="text-zinc-400">Important / High</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#f59e0b" }} />
            <span className="text-zinc-400">Medium</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <span className="w-2.5 h-2.5 rounded-full" style={{ background: "#71717a" }} />
            <span className="text-zinc-400">Nice-to-have / Low</span>
          </div>

          <div className="text-[10px] font-semibold text-zinc-400 mt-2 mb-1 pt-1 border-t border-border/30">
            Edges
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <div className="w-4 h-0 border-t-2 border-[#3b82f6]" />
            <span className="text-zinc-400">Requires</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <div className="w-4 h-0 border-t-2 border-dashed border-[#a855f7]" />
            <span className="text-zinc-400">Extends</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <div className="w-4 h-0 border-t-2 border-dotted border-[#71717a]" />
            <span className="text-zinc-400">Uses</span>
          </div>
        </div>
      </div>

      {/* Detail sidebar */}
      {selectedNode && (
        <DetailSidebar
          feature={selectedNode}
          depsOut={depsOut}
          depsIn={depsIn}
          onClose={handleCloseDetail}
        />
      )}
    </div>
  );
}
