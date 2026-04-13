/**
 * DagGraphPanel.tsx
 *
 * ReactFlow-based DAG visualization for the YAML workflow editor.
 * Parses node definitions and depends_on relationships into a
 * directed acyclic graph with auto-layout via dagre.
 */

import { useCallback, useMemo, useEffect } from "react";
import {
  ReactFlow,
  MiniMap,
  Controls,
  Background,
  useNodesState,
  useEdgesState,
  BackgroundVariant,
  Position,
  Handle,
  MarkerType,
  type Node,
  type Edge,
  type NodeProps,
} from "@xyflow/react";
import dagre from "@dagrejs/dagre";
import "@xyflow/react/dist/style.css";
import {
  Terminal,
  MessageSquare,
  CheckCircle2,
  Layers,
  ThumbsUp,
  XCircle,
  GitBranch,
  Repeat,
  Loader2,
  AlertTriangle,
} from "lucide-react";

// ============================================================================
// Types
// ============================================================================

export interface ValidationResult {
  valid: boolean;
  name?: string;
  node_count?: number;
  errors?: string[];
}

interface ParsedNode {
  id: string;
  name: string;
  type: string;
  depends_on: string[];
  trigger_rule?: string;
  [key: string]: unknown;
}

interface DagGraphPanelProps {
  yamlContent: string;
  validationResult: ValidationResult | null;
}

// ============================================================================
// Node type -> color + icon
// ============================================================================

const NODE_TYPE_CONFIG: Record<
  string,
  { color: string; label: string; Icon: React.FC<{ className?: string }> }
> = {
  command: { color: "#3b82f6", label: "Command", Icon: Terminal },
  prompt: { color: "#8b5cf6", label: "Prompt", Icon: MessageSquare },
  check: { color: "#f59e0b", label: "Check", Icon: CheckCircle2 },
  "ui-bridge": { color: "#06b6d4", label: "UI Bridge", Icon: Layers },
  approval: { color: "#f97316", label: "Approval", Icon: ThumbsUp },
  cancel: { color: "#ef4444", label: "Cancel", Icon: XCircle },
  "workflow-ref": { color: "#6366f1", label: "Workflow Ref", Icon: GitBranch },
  loop: { color: "#22c55e", label: "Loop", Icon: Repeat },
  default: { color: "#64748b", label: "Step", Icon: Terminal },
};

function getNodeTypeConfig(nodeData: Record<string, unknown>) {
  // Detect type from discriminating fields
  if ("command" in nodeData || nodeData.type === "command") return NODE_TYPE_CONFIG.command;
  if ("prompt" in nodeData || nodeData.type === "prompt") return NODE_TYPE_CONFIG.prompt;
  if ("check" in nodeData || nodeData.type === "check") return NODE_TYPE_CONFIG.check;
  if ("ui_bridge" in nodeData || nodeData.type === "ui-bridge") return NODE_TYPE_CONFIG["ui-bridge"];
  if ("approval" in nodeData || nodeData.type === "approval") return NODE_TYPE_CONFIG.approval;
  if ("cancel" in nodeData || nodeData.type === "cancel") return NODE_TYPE_CONFIG.cancel;
  if ("workflow_ref" in nodeData || nodeData.type === "workflow-ref")
    return NODE_TYPE_CONFIG["workflow-ref"];
  if ("loop" in nodeData || nodeData.type === "loop") return NODE_TYPE_CONFIG.loop;
  const typeKey = String(nodeData.type ?? "");
  return NODE_TYPE_CONFIG[typeKey] ?? NODE_TYPE_CONFIG.default;
}

// ============================================================================
// Custom DAG Node
// ============================================================================

interface DagNodeData {
  label: string;
  nodeType: string;
  color: string;
  triggerRule?: string;
  Icon: React.FC<{ className?: string }>;
  [key: string]: unknown;
}

function DagNode({ data }: NodeProps) {
  const nodeData = data as DagNodeData;
  const { label, color, triggerRule, Icon } = nodeData;

  return (
    <div
      className="relative rounded-lg overflow-hidden shadow-lg min-w-[160px] max-w-[220px]"
      style={{
        background: "#1e293b",
        border: `1px solid ${color}40`,
        borderLeft: `4px solid ${color}`,
      }}
    >
      <Handle
        type="target"
        position={Position.Top}
        className="!bg-transparent !border-2 !w-3 !h-3 !rounded-full"
        style={{ borderColor: color }}
      />

      <div className="px-3 py-2.5">
        <div className="flex items-center gap-2">
          <div
            className="flex-shrink-0 w-5 h-5 rounded flex items-center justify-center"
            style={{ background: `${color}20`, color }}
          >
            <Icon className="w-3 h-3" />
          </div>
          <span className="text-xs font-semibold text-slate-100 truncate leading-tight">{label}</span>
        </div>

        {triggerRule && triggerRule !== "all_success" && (
          <div className="mt-1.5">
            <span
              className="inline-block px-1.5 py-0.5 rounded text-[9px] font-medium"
              style={{ background: `${color}20`, color }}
            >
              {triggerRule}
            </span>
          </div>
        )}
      </div>

      <Handle
        type="source"
        position={Position.Bottom}
        className="!bg-transparent !border-2 !w-3 !h-3 !rounded-full"
        style={{ borderColor: color }}
      />
    </div>
  );
}

const nodeTypes = { dagNode: DagNode };

// ============================================================================
// YAML -> Graph parsing
// ============================================================================

function parseYamlToGraph(yamlContent: string): {
  nodes: Node[];
  edges: Edge[];
} {
  // Dynamically import js-yaml to avoid SSR issues
  let parsedDoc: Record<string, unknown>;

  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const yaml = require("js-yaml") as typeof import("js-yaml");
    parsedDoc = yaml.load(yamlContent) as Record<string, unknown>;
  } catch {
    return { nodes: [], edges: [] };
  }

  if (!parsedDoc || typeof parsedDoc !== "object") return { nodes: [], edges: [] };

  const rawNodes = parsedDoc.nodes as Record<string, unknown> | undefined;
  if (!rawNodes || typeof rawNodes !== "object") return { nodes: [], edges: [] };

  const parsedNodes: ParsedNode[] = Object.entries(rawNodes).map(([id, value]) => {
    const nodeData = (value ?? {}) as Record<string, unknown>;
    const dependsOn = nodeData.depends_on as string[] | string | undefined;

    let depsArray: string[] = [];
    if (Array.isArray(dependsOn)) {
      depsArray = dependsOn.map(String);
    } else if (typeof dependsOn === "string") {
      depsArray = [dependsOn];
    }

    const typeConfig = getNodeTypeConfig(nodeData);

    return {
      id,
      name: String(nodeData.name ?? id),
      type: String(nodeData.type ?? "default"),
      depends_on: depsArray,
      trigger_rule: nodeData.trigger_rule as string | undefined,
      ...nodeData,
      _typeConfig: typeConfig,
    } as ParsedNode & { _typeConfig: typeof NODE_TYPE_CONFIG.default };
  });

  // Run dagre layout
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", ranksep: 80, nodesep: 50 });

  const NODE_WIDTH = 200;
  const NODE_HEIGHT = 60;

  for (const node of parsedNodes) {
    g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
  }

  const edges: Edge[] = [];
  for (const node of parsedNodes) {
    for (const dep of node.depends_on) {
      if (rawNodes[dep] !== undefined) {
        g.setEdge(dep, node.id);
        const typeConfig = (node as ParsedNode & { _typeConfig: typeof NODE_TYPE_CONFIG.default })._typeConfig;
        edges.push({
          id: `${dep}->${node.id}`,
          source: dep,
          target: node.id,
          type: "smoothstep",
          markerEnd: { type: MarkerType.ArrowClosed, color: "#475569" },
          style: { stroke: typeConfig?.color ?? "#475569", strokeWidth: 1.5, opacity: 0.7 },
          animated: false,
        });
      }
    }
  }

  dagre.layout(g);

  const nodes: Node[] = parsedNodes.map((node) => {
    const pos = g.node(node.id);
    const typeConfig = (node as ParsedNode & { _typeConfig: typeof NODE_TYPE_CONFIG.default })._typeConfig;

    return {
      id: node.id,
      type: "dagNode",
      position: { x: pos.x - NODE_WIDTH / 2, y: pos.y - NODE_HEIGHT / 2 },
      data: {
        label: node.name,
        nodeType: node.type,
        color: typeConfig.color,
        triggerRule: node.trigger_rule,
        Icon: typeConfig.Icon,
      } satisfies DagNodeData,
    };
  });

  return { nodes, edges };
}

// ============================================================================
// Main Component
// ============================================================================

export function DagGraphPanel({ yamlContent, validationResult }: DagGraphPanelProps) {
  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);

  const graph = useMemo(() => {
    if (!yamlContent.trim()) return null;
    try {
      return parseYamlToGraph(yamlContent);
    } catch {
      return null;
    }
  }, [yamlContent]);

  useEffect(() => {
    if (graph) {
      setNodes(graph.nodes);
      setEdges(graph.edges);
    }
  }, [graph, setNodes, setEdges]);

  const hasValidationErrors =
    validationResult !== null && !validationResult.valid;
  const isEmpty = !yamlContent.trim();
  const hasNoNodes = graph !== null && graph.nodes.length === 0;

  return (
    <div className="relative w-full h-full" style={{ background: "#0f172a" }}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.3 }}
        minZoom={0.3}
        maxZoom={2}
        proOptions={{ hideAttribution: true }}
      >
        <Background
          variant={BackgroundVariant.Dots}
          gap={20}
          size={1}
          color="#1e293b"
        />
        <Controls
          className="!bottom-4 !left-4 !bg-slate-800 !border-slate-700 !rounded-lg"
          showInteractive={false}
        />
        <MiniMap
          className="!bottom-4 !right-4 !bg-slate-900 !border-slate-700 !rounded-lg"
          nodeColor="#3b82f6"
          maskColor="#0f172a99"
        />
      </ReactFlow>

      {/* Overlay messages */}
      {(isEmpty || hasValidationErrors || hasNoNodes) && (
        <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
          <div className="flex flex-col items-center gap-3 text-center px-8 backdrop-blur-sm bg-black/20 rounded-xl py-8 border border-white/5 pointer-events-auto">
            {isEmpty && (
              <>
                <GitBranch className="w-10 h-10 text-slate-500" />
                <p className="text-sm text-slate-400 font-medium">
                  Write a workflow in the editor to see the graph
                </p>
              </>
            )}
            {!isEmpty && hasValidationErrors && (
              <>
                <AlertTriangle className="w-10 h-10 text-yellow-500/70" />
                <p className="text-sm text-slate-300 font-medium">Fix YAML errors to see the graph</p>
                {validationResult?.errors?.slice(0, 3).map((err, i) => (
                  <p key={i} className="text-xs text-red-400 max-w-xs">
                    {err}
                  </p>
                ))}
              </>
            )}
            {!isEmpty && !hasValidationErrors && hasNoNodes && (
              <>
                <Loader2 className="w-8 h-8 text-slate-500 animate-spin" />
                <p className="text-sm text-slate-400">Parsing workflow...</p>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
