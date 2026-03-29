/**
 * CoverageGapPanel
 *
 * Displays coverage gap analysis: a table of pages sorted by gap severity,
 * and an interactive ReactFlow graph where nodes represent pages colored
 * by coverage score.
 */

import { useState, useMemo, useCallback, Fragment, useRef } from "react";
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
} from "@xyflow/react";
import dagre from "dagre";
import "@xyflow/react/dist/style.css";
import {
  Shield,
  AlertTriangle,
  CheckCircle,
  XCircle,
  ChevronDown,
  ChevronRight,
  BarChart3,
  TestTube,
} from "lucide-react";
import type {
  CoverageGap,
  CoverageAnalysisResult,
  GapDetail,
} from "@/lib/development-intelligence/coverage-analyzer";
import {
  getCoverageTierColor,
  getCoverageTier,
} from "@/lib/development-intelligence/coverage-analyzer";

interface CoverageGapPanelProps {
  data: CoverageAnalysisResult | null;
  loading: boolean;
  onRefresh: () => void;
}

// =============================================================================
// Coverage Node for ReactFlow
// =============================================================================

const NODE_W = 180;
const NODE_H = 60;

function CoverageNode({ data }: { data: { label: string; score: number; assertions: number; gaps: number } }) {
  const color = getCoverageTierColor(data.score);
  return (
    <div
      className="rounded-lg border px-3 py-2 text-xs shadow-sm"
      style={{
        background: `${color}15`,
        borderColor: color,
        minWidth: NODE_W,
      }}
    >
      <Handle type="target" position={Position.Left} className="!bg-border" />
      <div className="font-semibold truncate">{data.label}</div>
      <div className="flex items-center gap-2 mt-1 text-muted-foreground">
        <span style={{ color }}>{Math.round(data.score * 100)}%</span>
        <span>{data.assertions} assertions</span>
        {data.gaps > 0 && (
          <span className="text-destructive">{data.gaps} gaps</span>
        )}
      </div>
      <Handle type="source" position={Position.Right} className="!bg-border" />
    </div>
  );
}

const nodeTypes = { coverageNode: CoverageNode };

// =============================================================================
// Graph builder
// =============================================================================

function buildGraph(gaps: CoverageGap[]): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "LR", ranksep: 80, nodesep: 40 });

  const nodes: Node[] = gaps.map((gap) => {
    g.setNode(gap.page, { width: NODE_W, height: NODE_H });
    return {
      id: gap.page,
      type: "coverageNode",
      position: { x: 0, y: 0 },
      data: {
        label: gap.page,
        score: gap.coverageScore,
        assertions: gap.specAssertionCount,
        gaps: gap.gaps.length,
      },
    };
  });

  dagre.layout(g);

  for (const node of nodes) {
    const pos = g.node(node.id);
    if (pos) {
      node.position = { x: pos.x - NODE_W / 2, y: pos.y - NODE_H / 2 };
    }
  }

  return { nodes, edges: [] };
}

// =============================================================================
// Gap Detail Row
// =============================================================================

function GapDetailRow({ gap }: { gap: GapDetail }) {
  const severityIcon =
    gap.severity === "critical" ? (
      <XCircle className="w-3.5 h-3.5 text-destructive" />
    ) : gap.severity === "warning" ? (
      <AlertTriangle className="w-3.5 h-3.5 text-yellow-500" />
    ) : (
      <Shield className="w-3.5 h-3.5 text-muted-foreground" />
    );

  return (
    <div className="flex items-center gap-2 py-1 px-2 text-xs">
      {severityIcon}
      <span className="font-medium">{gap.specGroupName}</span>
      <span className="text-muted-foreground">({gap.category})</span>
      <span className="text-muted-foreground ml-auto">
        {gap.assertionCount} assertions
      </span>
    </div>
  );
}

// =============================================================================
// Main Panel
// =============================================================================

export function CoverageGapPanel({
  data,
  loading,
  onRefresh,
}: CoverageGapPanelProps) {
  const [view, setView] = useState<"table" | "graph">("table");
  const [expandedPage, setExpandedPage] = useState<string | null>(null);

  const { nodes, edges } = useMemo(
    () => (data ? buildGraph(data.gaps) : { nodes: [], edges: [] }),
    [data],
  );

  const [flowNodes, setFlowNodes, onNodesChange] = useNodesState(nodes);
  const [flowEdges, setFlowEdges, onEdgesChange] = useEdgesState(edges);

  const prevDerivedRef = useRef({ nodes, edges });
  if (prevDerivedRef.current.nodes !== nodes) {
    prevDerivedRef.current.nodes = nodes;
    setFlowNodes(nodes);
  }
  if (prevDerivedRef.current.edges !== edges) {
    prevDerivedRef.current.edges = edges;
    setFlowEdges(edges);
  }

  const toggleExpand = useCallback(
    (page: string) =>
      setExpandedPage((prev) => (prev === page ? null : page)),
    [],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-muted-foreground text-sm">
        Analyzing coverage gaps...
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex flex-col items-center justify-center h-64 text-muted-foreground text-sm gap-2">
        <TestTube className="w-8 h-8" />
        <span>No coverage data. Click refresh to analyze.</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Summary bar */}
      <div className="flex items-center gap-4 px-4 py-3 border-b bg-card">
        <div className="flex items-center gap-1.5">
          <BarChart3 className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Coverage Analysis</span>
        </div>
        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span>
            {data.summary.totalPages} pages
          </span>
          <span className="text-emerald-500">
            {data.summary.wellCovered} covered
          </span>
          <span className="text-yellow-500">
            {data.summary.partiallyCovered} partial
          </span>
          <span className="text-destructive">
            {data.summary.uncovered} uncovered
          </span>
          <span>
            Avg: {Math.round(data.summary.averageCoverageScore * 100)}%
          </span>
        </div>
        <div className="ml-auto flex items-center gap-1">
          <button
            className={`px-2 py-1 text-xs rounded ${view === "table" ? "bg-primary text-primary-foreground" : "bg-muted"}`}
            onClick={() => setView("table")}
          >
            Table
          </button>
          <button
            className={`px-2 py-1 text-xs rounded ${view === "graph" ? "bg-primary text-primary-foreground" : "bg-muted"}`}
            onClick={() => setView("graph")}
          >
            Graph
          </button>
        </div>
      </div>

      {/* Content */}
      {view === "table" ? (
        <div className="flex-1 overflow-auto">
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-card border-b">
              <tr>
                <th className="text-left px-3 py-2 font-medium">Page</th>
                <th className="text-right px-3 py-2 font-medium">
                  Spec Assertions
                </th>
                <th className="text-right px-3 py-2 font-medium">Tests</th>
                <th className="text-right px-3 py-2 font-medium">Coverage</th>
                <th className="text-left px-3 py-2 font-medium">Top Gap</th>
              </tr>
            </thead>
            <tbody>
              {data.gaps.map((gap) => {
                const tier = getCoverageTier(gap.coverageScore);
                const tierColor = getCoverageTierColor(gap.coverageScore);
                const isExpanded = expandedPage === gap.page;
                const topGap = gap.gaps[0];

                return (
                  <Fragment key={gap.page}>
                    <tr
                      className="border-b hover:bg-muted/50 cursor-pointer"
                      onClick={() => toggleExpand(gap.page)}
                    >
                      <td className="px-3 py-2 flex items-center gap-1.5">
                        {isExpanded ? (
                          <ChevronDown className="w-3.5 h-3.5" />
                        ) : (
                          <ChevronRight className="w-3.5 h-3.5" />
                        )}
                        <span className="font-medium">{gap.page}</span>
                      </td>
                      <td className="text-right px-3 py-2">
                        {gap.specAssertionCount}
                        {gap.criticalAssertionCount > 0 && (
                          <span className="text-destructive ml-1">
                            ({gap.criticalAssertionCount} critical)
                          </span>
                        )}
                      </td>
                      <td className="text-right px-3 py-2">
                        {gap.testFileCount} files / {gap.testAssertionCount}{" "}
                        asserts
                      </td>
                      <td className="text-right px-3 py-2">
                        <span
                          className="inline-block px-1.5 py-0.5 rounded text-xs font-medium"
                          style={{
                            color: tierColor,
                            background: `${tierColor}20`,
                          }}
                        >
                          {Math.round(gap.coverageScore * 100)}%
                        </span>
                      </td>
                      <td className="px-3 py-2 text-muted-foreground truncate max-w-[200px]">
                        {topGap
                          ? `${topGap.specGroupName} (${topGap.category})`
                          : tier === "good"
                            ? "Well covered"
                            : "-"}
                      </td>
                    </tr>
                    {isExpanded && gap.gaps.length > 0 && (
                      <tr>
                        <td colSpan={5} className="bg-muted/30 px-6 py-2">
                          <div className="text-xs font-medium mb-1">
                            Uncovered spec groups:
                          </div>
                          {gap.gaps.map((g) => (
                            <GapDetailRow key={g.specGroupId} gap={g} />
                          ))}
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="flex-1">
          <ReactFlow
            nodes={flowNodes}
            edges={flowEdges}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            nodeTypes={nodeTypes}
            fitView
            minZoom={0.3}
            maxZoom={2}
            proOptions={{ hideAttribution: true }}
          >
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
            <Controls showInteractive={false} />
            <MiniMap
              nodeColor={(n) =>
                getCoverageTierColor((n.data?.score as number) ?? 0)
              }
              maskColor="rgba(0,0,0,0.1)"
            />
          </ReactFlow>
        </div>
      )}
    </div>
  );
}
