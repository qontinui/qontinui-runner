/**
 * ApiSurfaceGraph — ReactFlow layered visualization of the API surface.
 *
 * Layer 1 (top): Frontend callers
 * Layer 2: Tauri Commands + MCP Routes
 * Layer 3: PgDb Methods + Python Events
 * Layer 4 (bottom): Clorinde Queries + DB Tables
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
import * as dagre from "@dagrejs/dagre";
import "@xyflow/react/dist/style.css";
import type { ApiSurface } from "./types";
import { NODE_COLORS, EDGE_STYLES, type NodeCategory } from "./types";

// ─── Props ───────────────────────────────────────────────────────────────────

interface Props {
  surface: ApiSurface;
  showOrphans: boolean;
  layerFilter: Set<NodeCategory>;
  searchQuery: string;
  onNodeSelect: (node: { type: string; name: string } | null) => void;
}

// ─── Custom Node ─────────────────────────────────────────────────────────────

interface SurfaceNodeData {
  label: string;
  category: NodeCategory;
  sublabel?: string;
  isOrphan: boolean;
  dimmed: boolean;
  callerCount: number;
  [key: string]: unknown;
}

const SurfaceNode = memo(({ data }: { data: SurfaceNodeData }) => {
  const colors = NODE_COLORS[data.category];
  return (
    <div
      className="rounded-lg transition-opacity duration-200"
      style={{
        width: 220,
        background: colors.bg,
        border: `2px solid ${data.isOrphan ? "#ef4444" : colors.border}`,
        opacity: data.dimmed ? 0.15 : 1,
        boxShadow: data.isOrphan ? "0 0 8px rgba(239,68,68,0.3)" : undefined,
      }}
    >
      <Handle type="target" position={Position.Top} style={{ background: colors.border }} />
      <div className="px-3 py-2">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span
            className="w-2 h-2 rounded-full shrink-0"
            style={{ backgroundColor: data.isOrphan ? "#ef4444" : colors.border }}
          />
          <span
            className="text-[10px] font-medium uppercase tracking-wider"
            style={{ color: colors.text }}
          >
            {data.category.replace(/_/g, " ")}
          </span>
          {data.callerCount > 0 && (
            <span className="ml-auto text-[9px] text-zinc-500">{data.callerCount} callers</span>
          )}
        </div>
        <div className="text-xs font-semibold text-zinc-200 truncate">{data.label}</div>
        {data.sublabel && (
          <div className="text-[10px] text-zinc-500 truncate mt-0.5">{data.sublabel}</div>
        )}
      </div>
      <Handle type="source" position={Position.Bottom} style={{ background: colors.border }} />
    </div>
  );
});
SurfaceNode.displayName = "SurfaceNode";

const nodeTypes = { surfaceNode: SurfaceNode };

// ─── Layout ──────────────────────────────────────────────────────────────────

const NODE_W = 220;
const NODE_H = 65;

interface GraphItem {
  id: string;
  category: NodeCategory;
  label: string;
  sublabel?: string;
  isOrphan: boolean;
  callerCount: number;
}

function buildGraphData(
  surface: ApiSurface,
  showOrphans: boolean,
  layerFilter: Set<NodeCategory>,
  searchQuery: string,
) {
  const items: GraphItem[] = [];
  const orphanNames = new Set(surface.orphans.map((o) => `${o.endpointType}:${o.name}`));
  const q = searchQuery.toLowerCase();

  const matchSearch = (name: string, file?: string) =>
    !q || name.toLowerCase().includes(q) || (file?.toLowerCase().includes(q) ?? false);

  // Collect unique frontend callers
  if (layerFilter.has("frontend")) {
    const frontendFiles = new Set<string>();
    for (const cmd of surface.tauriCommands) {
      for (const c of cmd.callers) frontendFiles.add(c.file);
    }
    for (const route of surface.mcpRoutes) {
      for (const c of route.callers) frontendFiles.add(c.file);
    }
    for (const file of frontendFiles) {
      if (matchSearch(file)) {
        items.push({
          id: `frontend:${file}`,
          category: "frontend",
          label: file.split("/").pop() ?? file,
          sublabel: file,
          isOrphan: false,
          callerCount: 0,
        });
      }
    }
  }

  if (layerFilter.has("tauri_command")) {
    for (const cmd of surface.tauriCommands) {
      const isOrphan = orphanNames.has(`tauri_command:${cmd.name}`);
      if (!showOrphans && isOrphan) continue;
      if (!matchSearch(cmd.name, cmd.file)) continue;
      items.push({
        id: `tauri_command:${cmd.name}`,
        category: "tauri_command",
        label: cmd.name,
        sublabel: cmd.file,
        isOrphan,
        callerCount: cmd.callers.length,
      });
    }
  }

  if (layerFilter.has("mcp_route")) {
    for (const route of surface.mcpRoutes) {
      const key = `${route.method} ${route.path}`;
      const isOrphan = orphanNames.has(`mcp_route:${key}`);
      if (!showOrphans && isOrphan) continue;
      if (!matchSearch(route.path, route.file)) continue;
      items.push({
        id: `mcp_route:${route.path}`,
        category: "mcp_route",
        label: `${route.method} ${route.path}`,
        sublabel: route.handler,
        isOrphan,
        callerCount: route.callers.length,
      });
    }
  }

  if (layerFilter.has("ui_bridge_route")) {
    for (const route of surface.uiBridgeRoutes) {
      if (!matchSearch(route.path, route.file)) continue;
      items.push({
        id: `ui_bridge_route:${route.path}`,
        category: "ui_bridge_route",
        label: `${route.method} ${route.path}`,
        sublabel: route.file,
        isOrphan: false,
        callerCount: 0,
      });
    }
  }

  if (layerFilter.has("pg_method")) {
    for (const method of surface.pgMethods) {
      const isOrphan = orphanNames.has(`pg_method:${method.name}`);
      if (!showOrphans && isOrphan) continue;
      if (!matchSearch(method.name, method.file)) continue;
      items.push({
        id: `pg_method:${method.name}`,
        category: "pg_method",
        label: method.name,
        sublabel: method.file,
        isOrphan,
        callerCount: method.callers.length,
      });
    }
  }

  if (layerFilter.has("python_event")) {
    for (const event of surface.pythonEvents) {
      const isOrphan = orphanNames.has(`python_event:${event.value}`);
      if (!showOrphans && isOrphan) continue;
      if (!matchSearch(event.value, event.file)) continue;
      items.push({
        id: `python_event:${event.value}`,
        category: "python_event",
        label: event.name,
        sublabel: event.value,
        isOrphan,
        callerCount: event.interceptedBy.length,
      });
    }
  }

  if (layerFilter.has("clorinde_query")) {
    for (const query of surface.clorindeQueries) {
      const isOrphan = orphanNames.has(`clorinde_query:${query.name}`);
      if (!showOrphans && isOrphan) continue;
      if (!matchSearch(query.name, query.file)) continue;
      items.push({
        id: `clorinde_query:${query.name}`,
        category: "clorinde_query",
        label: query.name,
        sublabel: query.table ?? undefined,
        isOrphan,
        callerCount: query.wrapper ? 1 : 0,
      });
    }
  }

  if (layerFilter.has("db_table")) {
    for (const table of surface.dbTables) {
      if (!matchSearch(table.name, table.file)) continue;
      items.push({
        id: `db_table:${table.name}`,
        category: "db_table",
        label: table.name,
        sublabel: `${table.columnCount} columns`,
        isOrphan: false,
        callerCount: 0,
      });
    }
  }

  // Build edges from connections
  const itemIds = new Set(items.map((i) => i.id));
  const edges: Array<{ source: string; target: string; type: string }> = [];

  for (const conn of surface.connections) {
    const sourceId = `${conn.fromType}:${conn.fromName}`;
    const targetId = `${conn.toType}:${conn.toName}`;
    if (itemIds.has(sourceId) && itemIds.has(targetId)) {
      edges.push({ source: sourceId, target: targetId, type: conn.connectionType });
    }
  }

  // Also link clorinde queries → db tables
  for (const query of surface.clorindeQueries) {
    if (query.table) {
      const sourceId = `clorinde_query:${query.name}`;
      const targetId = `db_table:${query.table}`;
      if (itemIds.has(sourceId) && itemIds.has(targetId)) {
        edges.push({ source: sourceId, target: targetId, type: "queries" });
      }
    }
  }

  return { items, edges };
}

function layoutGraph(
  items: GraphItem[],
  graphEdges: Array<{ source: string; target: string; type: string }>,
) {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", ranksep: 80, nodesep: 30 });

  for (const item of items) {
    g.setNode(item.id, { width: NODE_W, height: NODE_H });
  }

  for (const edge of graphEdges) {
    g.setEdge(edge.source, edge.target);
  }

  dagre.layout(g);

  const nodes: Node[] = items.map((item) => {
    const pos = g.node(item.id);
    return {
      id: item.id,
      type: "surfaceNode",
      position: { x: (pos?.x ?? 0) - NODE_W / 2, y: (pos?.y ?? 0) - NODE_H / 2 },
      data: {
        label: item.label,
        category: item.category,
        sublabel: item.sublabel,
        isOrphan: item.isOrphan,
        dimmed: false,
        callerCount: item.callerCount,
      } satisfies SurfaceNodeData,
    };
  });

  const edges: Edge[] = graphEdges.map((e, i) => {
    const style = EDGE_STYLES[e.type] ?? { stroke: "#71717a", animated: false };
    return {
      id: `edge-${i}`,
      source: e.source,
      target: e.target,
      label: e.type,
      labelStyle: { fontSize: 8, fill: "#94a3b8" },
      labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
      animated: style.animated,
      style: {
        stroke: style.stroke,
        strokeWidth: 1.5,
        strokeDasharray: style.dasharray,
      },
      markerEnd: {
        type: MarkerType.ArrowClosed,
        width: 12,
        height: 12,
        color: style.stroke,
      },
    };
  });

  return { nodes, edges };
}

// ─── Connected node helpers ──────────────────────────────────────────────────

function getConnectedIds(nodeId: string, edges: Edge[]): Set<string> {
  const connected = new Set<string>();
  connected.add(nodeId);
  for (const e of edges) {
    if (e.source === nodeId) connected.add(e.target);
    if (e.target === nodeId) connected.add(e.source);
  }
  return connected;
}

// ─── Component ───────────────────────────────────────────────────────────────

function ApiSurfaceGraphInner({
  surface,
  showOrphans,
  layerFilter,
  searchQuery,
  onNodeSelect,
}: Props) {
  const { items, edges: graphEdges } = useMemo(
    () => buildGraphData(surface, showOrphans, layerFilter, searchQuery),
    [surface, showOrphans, layerFilter, searchQuery],
  );

  const { nodes: layoutNodes, edges: layoutEdges } = useMemo(
    () => layoutGraph(items, graphEdges),
    [items, graphEdges],
  );

  const prevLayoutNodesRef = useRef(layoutNodes);
  const prevLayoutEdgesRef = useRef(layoutEdges);
  const [nodes, setNodes, onNodesChange] = useNodesState(layoutNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(layoutEdges);
  const [, setHoveredId] = useState<string | null>(null);

  // Sync layout during render when data changes
  if (prevLayoutNodesRef.current !== layoutNodes) {
    prevLayoutNodesRef.current = layoutNodes;
    setNodes(layoutNodes);
  }
  if (prevLayoutEdgesRef.current !== layoutEdges) {
    prevLayoutEdgesRef.current = layoutEdges;
    setEdges(layoutEdges);
  }

  const onNodeMouseEnter = useCallback(
    (_: unknown, node: Node) => {
      setHoveredId(node.id);
      const connected = getConnectedIds(node.id, edges);
      setNodes((nds) =>
        nds.map((n) => ({
          ...n,
          data: { ...n.data, dimmed: !connected.has(n.id) },
        })),
      );
    },
    [edges, setNodes],
  );

  const onNodeMouseLeave = useCallback(() => {
    setHoveredId(null);
    setNodes((nds) => nds.map((n) => ({ ...n, data: { ...n.data, dimmed: false } })));
  }, [setNodes]);

  const onNodeClick = useCallback(
    (_: unknown, node: Node) => {
      const [type, ...rest] = node.id.split(":");
      onNodeSelect({ type, name: rest.join(":") });
    },
    [onNodeSelect],
  );

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      onNodeMouseEnter={onNodeMouseEnter}
      onNodeMouseLeave={onNodeMouseLeave}
      onNodeClick={onNodeClick}
      nodeTypes={nodeTypes}
      fitView
      fitViewOptions={{ padding: 0.15 }}
      minZoom={0.1}
      maxZoom={2}
      proOptions={{ hideAttribution: true }}
    >
      <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#27272a" />
      <Controls className="!bg-zinc-800 !border-zinc-700 [&>button]:!bg-zinc-800 [&>button]:!border-zinc-700 [&>button]:!text-zinc-400" />
      <MiniMap
        nodeStrokeWidth={3}
        nodeColor={(n) => {
          const cat = (n.data as SurfaceNodeData)?.category;
          return NODE_COLORS[cat]?.border ?? "#71717a";
        }}
        style={{ background: "#18181b", border: "1px solid #3f3f46" }}
      />
    </ReactFlow>
  );
}

export const ApiSurfaceGraph = memo(ApiSurfaceGraphInner);
