import { useRef, useMemo } from "react";
import {
  ReactFlow,
  MiniMap,
  Controls,
  Background,
  BackgroundVariant,
  useNodesState,
  useEdgesState,
  Handle,
  Position,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { FileText, Code2, Plug, Database, Component } from "lucide-react";
import {
  buildSessionGraph,
  type RecapNodeData,
} from "@/lib/session-recap/dependency-graph-builder";
import type { SessionRecap } from "@/lib/session-recap/types";

interface Props {
  recap: SessionRecap;
}

// ============================================================================
// Custom Node
// ============================================================================

const NODE_TYPE_ICONS: Record<string, typeof FileText> = {
  file: FileText,
  type: Code2,
  endpoint: Plug,
  database: Database,
  component: Component,
};

function RecapNode({ data }: NodeProps) {
  const nd = data as unknown as RecapNodeData;
  const Icon = NODE_TYPE_ICONS[nd.nodeType] ?? FileText;
  const isSmall = nd.nodeType === "type";

  return (
    <>
      <Handle type="target" position={Position.Top} className="!bg-border !w-1.5 !h-1.5" />
      <div
        className={`
          flex items-center gap-1.5 rounded-md border shadow-sm
          ${isSmall ? "px-2 py-1 text-[10px]" : "px-2.5 py-2 text-[11px]"}
          ${nd.changeType === "created" ? "border-green-500/40 bg-green-500/5" : "border-border/60 bg-card"}
        `}
        style={{ borderLeftColor: nd.color, borderLeftWidth: 3 }}
      >
        <Icon className="w-3 h-3 shrink-0" style={{ color: nd.color }} />
        <span className="truncate max-w-[160px] font-medium">{nd.label}</span>
        {nd.detail && (
          <span className="text-[9px] text-muted-foreground ml-auto shrink-0">{nd.detail}</span>
        )}
      </div>
      <Handle type="source" position={Position.Bottom} className="!bg-border !w-1.5 !h-1.5" />
    </>
  );
}

const nodeTypes = { recapNode: RecapNode };

// ============================================================================
// Graph View
// ============================================================================

export function SessionGraphView({ recap }: Props) {
  const { nodes: initialNodes, edges: initialEdges } = useMemo(
    () => buildSessionGraph(recap),
    [recap],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  const prevInitialRef = useRef({ nodes: initialNodes, edges: initialEdges });
  if (
    prevInitialRef.current.nodes !== initialNodes ||
    prevInitialRef.current.edges !== initialEdges
  ) {
    prevInitialRef.current = { nodes: initialNodes, edges: initialEdges };
    setNodes(initialNodes);
    setEdges(initialEdges);
  }

  if (initialNodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-sm text-muted-foreground">
        No dependency graph to display
      </div>
    );
  }

  return (
    <div className="h-full w-full">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        nodeTypes={nodeTypes}
        fitView
        minZoom={0.1}
        maxZoom={2}
        proOptions={{ hideAttribution: true }}
      >
        <Background
          variant={BackgroundVariant.Dots}
          gap={16}
          size={1}
          color="hsl(var(--border)/0.3)"
        />
        <Controls className="!bg-card !border-border !shadow-md" />
        <MiniMap
          nodeColor={(node) => {
            const nd = node.data as unknown as RecapNodeData;
            return nd?.color ?? "#6B7280";
          }}
          className="!bg-card/80 !border-border"
        />
      </ReactFlow>
    </div>
  );
}
