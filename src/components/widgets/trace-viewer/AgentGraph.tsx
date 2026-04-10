import { useCallback, useMemo } from "react";
import { ReactFlow, Background, Controls, type Node, type Edge, Position } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import * as dagre from "@dagrejs/dagre";
import type { TraceSpan } from "./types";
import { formatDuration } from "./trace-utils";

interface AgentGraphProps {
  spans: TraceSpan[];
  selectedSpanId?: string;
  onSelectSpan?: (span: TraceSpan | null) => void;
  height?: number;
}

/** Safely read a string attribute from a span. */
function getAttr(span: TraceSpan, key: string): string | undefined {
  const val = span.attributes?.[key];
  return typeof val === "string" ? val : undefined;
}

/** Build graph nodes and edges from agent spans. */
function buildAgentGraph(spans: TraceSpan[]) {
  const agentPhases = spans.filter((s) => s.name.includes("agent.phase"));
  const agentMessages = spans.filter((s) => s.name.includes("agent.message"));

  const nodes: Node[] = agentPhases.map((span, i) => {
    const role = getAttr(span, "agent.role") || getAttr(span, "agent.id") || `Agent ${i + 1}`;
    const status = span.success === false || span.error ? "error" : "success";

    return {
      id: span.span_id,
      position: { x: 0, y: 0 }, // dagre will set this
      data: {
        label: role,
        span,
        status,
        duration: span.duration_ms,
        tokens: (span.input_tokens ?? 0) + (span.output_tokens ?? 0),
      },
      type: "default",
      style: {
        background: status === "error" ? "rgba(239, 68, 68, 0.15)" : "rgba(34, 197, 94, 0.15)",
        border: `2px solid ${status === "error" ? "#ef4444" : "#22c55e"}`,
        color: "#e4e4e7",
        borderRadius: "8px",
        padding: "8px 12px",
        fontSize: "12px",
        minWidth: "140px",
        textAlign: "center" as const,
      },
    };
  });

  // Build edges from message spans
  const edges: Edge[] = [];
  for (const msg of agentMessages) {
    const sourceId = getAttr(msg, "agent.source_id");
    const targetId = getAttr(msg, "agent.target_id");
    const msgType = getAttr(msg, "agent.message_type") || "data";

    if (sourceId && targetId) {
      // Find node IDs that match source/target agent IDs
      const sourceNode = agentPhases.find((s) => getAttr(s, "agent.id") === sourceId);
      const targetNode = agentPhases.find((s) => getAttr(s, "agent.id") === targetId);

      if (sourceNode && targetNode) {
        edges.push({
          id: msg.span_id,
          source: sourceNode.span_id,
          target: targetNode.span_id,
          label: msgType,
          animated: true,
          style: { stroke: "#9ca3af" },
          labelStyle: { fontSize: 10, fill: "#9ca3af" },
        });
      }
    }
  }

  // If no message edges found, create edges from sequential relationships
  if (edges.length === 0 && nodes.length > 1) {
    const sorted = [...agentPhases].sort((a, b) =>
      (a.start_ts || "").localeCompare(b.start_ts || ""),
    );
    for (let i = 0; i < sorted.length - 1; i++) {
      edges.push({
        id: `seq-${i}`,
        source: sorted[i].span_id,
        target: sorted[i + 1].span_id,
        animated: true,
        style: { stroke: "#9ca3af" },
      });
    }
  }

  return { nodes, edges };
}

/** Apply dagre layout to position nodes. */
function layoutGraph(nodes: Node[], edges: Edge[]) {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", nodesep: 50, ranksep: 80 });

  for (const node of nodes) {
    g.setNode(node.id, { width: 160, height: 60 });
  }
  for (const edge of edges) {
    g.setEdge(edge.source, edge.target);
  }

  dagre.layout(g);

  return nodes.map((node) => {
    const pos = g.node(node.id);
    return {
      ...node,
      position: { x: pos.x - 80, y: pos.y - 30 },
      sourcePosition: Position.Bottom,
      targetPosition: Position.Top,
    };
  });
}

export function AgentGraph({ spans, selectedSpanId, onSelectSpan, height = 400 }: AgentGraphProps) {
  const { nodes: rawNodes, edges } = useMemo(() => buildAgentGraph(spans), [spans]);
  const nodes = useMemo(() => layoutGraph(rawNodes, edges), [rawNodes, edges]);

  const hasAgentSpans = nodes.length > 0;

  const onNodeClick = useCallback(
    (_: React.MouseEvent, node: Node) => {
      const span = node.data?.span as TraceSpan | undefined;
      onSelectSpan?.(span ?? null);
    },
    [onSelectSpan],
  );

  if (!hasAgentSpans) {
    return (
      <div
        className="flex items-center justify-center text-muted-foreground text-sm"
        style={{ height }}
      >
        No agent pipeline spans found in this trace.
      </div>
    );
  }

  // Add labels with duration and token info
  const labeledNodes = nodes.map((node) => ({
    ...node,
    data: {
      ...node.data,
      label: (
        <div className="text-center">
          <div className="font-medium text-xs">{node.data.label as string}</div>
          {node.data.duration != null && (
            <div className="text-[10px] text-muted-foreground">
              {formatDuration(node.data.duration as number)}
              {(node.data.tokens as number) > 0 &&
                ` \u00b7 ${(node.data.tokens as number).toLocaleString()} tok`}
            </div>
          )}
        </div>
      ),
    },
    selected: (node.data?.span as TraceSpan | undefined)?.span_id === selectedSpanId,
  }));

  return (
    <div style={{ height }}>
      <ReactFlow
        nodes={labeledNodes}
        edges={edges}
        onNodeClick={onNodeClick}
        fitView
        fitViewOptions={{ padding: 0.2 }}
        nodesDraggable={false}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}
