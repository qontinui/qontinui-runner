/**
 * Transforms a SessionRecap into ReactFlow nodes + edges with Dagre layout.
 *
 * Layers (top to bottom):
 *   Frontend (TypeScript/React)
 *     ↓ Tauri IPC
 *   Backend (Rust)
 *     ↓ Queries
 *   Database (PostgreSQL / SQL)
 *     ↓
 *   Python Bridge
 */

import type { Node, Edge } from "@xyflow/react";
import { MarkerType, Position } from "@xyflow/react";
import dagre from "dagre";
import type { SessionRecap, FileChange, DependencyEdge } from "./types";

// ============================================================================
// Constants
// ============================================================================

const LANGUAGE_COLORS: Record<string, string> = {
  rust: "#F97316", // orange
  typescript: "#3B82F6", // blue
  javascript: "#EAB308", // yellow
  python: "#22C55E", // green
  sql: "#A855F7", // purple
  json: "#6B7280", // gray
  other: "#9CA3AF",
};

const NODE_WIDTH = 220;
const NODE_HEIGHT = 50;
const TYPE_NODE_WIDTH = 160;
const TYPE_NODE_HEIGHT = 36;

// ============================================================================
// Node types
// ============================================================================

export type RecapNodeType = "file" | "type" | "endpoint" | "database" | "component";

export interface RecapNodeData extends Record<string, unknown> {
  label: string;
  nodeType: RecapNodeType;
  language: string;
  category: string;
  repo: string;
  changeType?: string;
  linesAdded?: number;
  linesRemoved?: number;
  color: string;
  detail?: string;
}

// ============================================================================
// Build graph
// ============================================================================

export function buildSessionGraph(recap: SessionRecap): {
  nodes: Node<RecapNodeData>[];
  edges: Edge[];
} {
  const nodes: Node<RecapNodeData>[] = [];
  const edges: Edge[] = [];
  const nodeIdSet = new Set<string>();

  const allFiles = [...recap.files_created, ...recap.files_modified, ...recap.files_deleted];

  // --- File nodes ---
  for (const file of allFiles) {
    const id = fileNodeId(file);
    if (nodeIdSet.has(id)) continue;
    nodeIdSet.add(id);

    nodes.push({
      id,
      type: "recapNode",
      position: { x: 0, y: 0 },
      data: {
        label: shortPath(file.path),
        nodeType: "file",
        language: file.language,
        category: file.category,
        repo: file.repo,
        changeType: file.change_type,
        linesAdded: file.lines_added,
        linesRemoved: file.lines_removed,
        color: LANGUAGE_COLORS[file.language] ?? LANGUAGE_COLORS.other,
      },
    });
  }

  // --- Type definition nodes ---
  for (const td of recap.types_defined) {
    const id = `type-${td.repo}-${td.name}`;
    if (nodeIdSet.has(id)) continue;
    nodeIdSet.add(id);

    const parentId = `file-${td.repo}-${td.file}`;
    nodes.push({
      id,
      type: "recapNode",
      position: { x: 0, y: 0 },
      data: {
        label: td.name,
        nodeType: "type",
        language: td.language,
        category: td.kind,
        repo: td.repo,
        color: LANGUAGE_COLORS[td.language] ?? LANGUAGE_COLORS.other,
        detail: td.kind,
      },
    });

    if (nodeIdSet.has(parentId)) {
      edges.push({
        id: `e-${parentId}-${id}`,
        source: parentId,
        target: id,
        type: "smoothstep",
        animated: false,
        style: { stroke: "#94a3b8", strokeWidth: 1, strokeDasharray: "4 2" },
      });
    }
  }

  // --- Endpoint nodes ---
  for (const ep of recap.endpoints_added) {
    const id = `ep-${ep.repo}-${ep.method}-${ep.path}`;
    if (nodeIdSet.has(id)) continue;
    nodeIdSet.add(id);

    const parentId = `file-${ep.repo}-${ep.file}`;
    nodes.push({
      id,
      type: "recapNode",
      position: { x: 0, y: 0 },
      data: {
        label: `${ep.method} ${ep.path}`,
        nodeType: "endpoint",
        language: "rust",
        category: "backend",
        repo: ep.repo,
        color: "#F59E0B",
        detail: ep.method,
      },
    });

    if (nodeIdSet.has(parentId)) {
      edges.push({
        id: `e-${parentId}-${id}`,
        source: parentId,
        target: id,
        type: "smoothstep",
        animated: false,
        style: { stroke: "#f59e0b", strokeWidth: 1.5 },
      });
    }
  }

  // --- Database nodes ---
  for (const db of recap.database_changes) {
    const id = `db-${db.table_name}`;
    if (nodeIdSet.has(id)) continue;
    nodeIdSet.add(id);

    nodes.push({
      id,
      type: "recapNode",
      position: { x: 0, y: 0 },
      data: {
        label: db.table_name,
        nodeType: "database",
        language: "sql",
        category: "database",
        repo: db.repo,
        color: LANGUAGE_COLORS.sql,
        detail: db.change_type,
      },
    });
  }

  // --- Component nodes ---
  for (const comp of recap.ui_components) {
    const id = `comp-${comp.repo}-${comp.name}`;
    if (nodeIdSet.has(id)) continue;
    nodeIdSet.add(id);

    const parentId = `file-${comp.repo}-${comp.file}`;
    nodes.push({
      id,
      type: "recapNode",
      position: { x: 0, y: 0 },
      data: {
        label: comp.name,
        nodeType: "component",
        language: "typescript",
        category: "frontend",
        repo: comp.repo,
        color: "#3B82F6",
        detail: comp.component_type,
      },
    });

    if (nodeIdSet.has(parentId)) {
      edges.push({
        id: `e-${parentId}-${id}`,
        source: parentId,
        target: id,
        type: "smoothstep",
        animated: false,
        style: { stroke: "#3b82f6", strokeWidth: 1 },
      });
    }
  }

  // --- Dependency edges ---
  for (const dep of recap.dependency_graph) {
    const sourceId = findFileNode(dep.from_file, nodeIdSet);
    const targetId = findFileNode(dep.to_file, nodeIdSet);
    if (!sourceId || !targetId) continue;

    const edgeId = `dep-${sourceId}-${targetId}`;
    edges.push({
      id: edgeId,
      source: sourceId,
      target: targetId,
      type: "smoothstep",
      animated: dep.cross_language,
      label: edgeLabel(dep),
      markerEnd: { type: MarkerType.ArrowClosed, width: 12, height: 12 },
      style: {
        stroke: dep.cross_language ? "#F97316" : "#64748b",
        strokeWidth: dep.cross_language ? 2 : 1.5,
        strokeDasharray: dep.cross_language ? "6 3" : undefined,
      },
    });
  }

  // --- Dagre layout ---
  return applyDagreLayout(nodes, edges);
}

// ============================================================================
// Layout
// ============================================================================

function applyDagreLayout(
  nodes: Node<RecapNodeData>[],
  edges: Edge[],
): { nodes: Node<RecapNodeData>[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", ranksep: 80, nodesep: 30 });

  for (const node of nodes) {
    const isSmall = node.data.nodeType === "type";
    g.setNode(node.id, {
      width: isSmall ? TYPE_NODE_WIDTH : NODE_WIDTH,
      height: isSmall ? TYPE_NODE_HEIGHT : NODE_HEIGHT,
    });
  }

  for (const edge of edges) {
    g.setEdge(edge.source, edge.target);
  }

  dagre.layout(g);

  const laid = nodes.map((node) => {
    const pos = g.node(node.id);
    const isSmall = node.data.nodeType === "type";
    const w = isSmall ? TYPE_NODE_WIDTH : NODE_WIDTH;
    const h = isSmall ? TYPE_NODE_HEIGHT : NODE_HEIGHT;
    return {
      ...node,
      position: { x: pos.x - w / 2, y: pos.y - h / 2 },
      sourcePosition: Position.Bottom,
      targetPosition: Position.Top,
    };
  });

  return { nodes: laid, edges };
}

// ============================================================================
// Helpers
// ============================================================================

function fileNodeId(file: FileChange): string {
  return `file-${file.repo}-${file.path}`;
}

function shortPath(path: string): string {
  const parts = path.split("/");
  if (parts.length <= 2) return path;
  return `.../${parts.slice(-2).join("/")}`;
}

function findFileNode(key: string, nodeIdSet: Set<string>): string | null {
  // key is "repo/path" — node IDs are "file-{repo}-{path}" where path keeps slashes
  const slashIdx = key.indexOf("/");
  if (slashIdx === -1) return null;
  const repo = key.substring(0, slashIdx);
  const path = key.substring(slashIdx + 1);
  const id = `file-${repo}-${path}`;
  if (nodeIdSet.has(id)) return id;
  // Fallback: match by filename suffix
  const filename = path.split("/").pop() ?? "";
  if (filename) {
    for (const nid of nodeIdSet) {
      if (nid.startsWith("file-") && nid.endsWith(filename)) {
        return nid;
      }
    }
  }
  return null;
}

function edgeLabel(dep: DependencyEdge): string | undefined {
  switch (dep.relationship) {
    case "tauri_ipc":
      return "Tauri IPC";
    case "python_bridge":
      return "Python Bridge";
    case "cross_language":
      return "Cross-lang";
    default:
      return undefined;
  }
}
