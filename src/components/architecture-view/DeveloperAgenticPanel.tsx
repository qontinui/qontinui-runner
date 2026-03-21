/**
 * DeveloperAgenticPanel
 *
 * Visualizes the agentic structure of a project: agents as a hierarchy graph
 * (ReactFlow + dagre), feedback loops as a card list, and a decision authority
 * matrix as a compact table.
 *
 * Interactions:
 *   - Hover a node to dim unconnected nodes/edges
 *   - Click a node to open a detail sidebar
 */

import { memo, useMemo, useState, useCallback, useRef, useEffect } from "react";
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
import { Bot, X, Check, CircleStop, Shield, Zap, ArrowRight } from "lucide-react";
import type {
  SdkArchitectureGraph,
  AgenticAgent,
  AgenticFeedbackLoop,
  AgenticDelegationChain,
  AgenticStructure,
  AgentRole,
} from "@/types/architecture";

// =============================================================================
// Role color system
// =============================================================================

const ROLE_COLORS: Record<AgentRole, { bg: string; border: string; accent: string; text: string }> =
  {
    orchestrator: {
      bg: "rgba(59,130,246,0.15)",
      border: "#3b82f6",
      accent: "#3b82f6",
      text: "#93c5fd",
    },
    executor: {
      bg: "rgba(34,197,94,0.15)",
      border: "#22c55e",
      accent: "#22c55e",
      text: "#86efac",
    },
    analyzer: {
      bg: "rgba(168,85,247,0.15)",
      border: "#a855f7",
      accent: "#a855f7",
      text: "#d8b4fe",
    },
    session: {
      bg: "rgba(6,182,212,0.15)",
      border: "#06b6d4",
      accent: "#06b6d4",
      text: "#67e8f9",
    },
    monitor: {
      bg: "rgba(249,115,22,0.15)",
      border: "#f97316",
      accent: "#f97316",
      text: "#fdba74",
    },
  };

const ROLE_COLOR_DEFAULT = {
  bg: "rgba(113,113,122,0.15)",
  border: "#71717a",
  accent: "#71717a",
  text: "#a1a1aa",
};

function roleColor(role?: string) {
  if (!role) return ROLE_COLOR_DEFAULT;
  return ROLE_COLORS[role as AgentRole] ?? ROLE_COLOR_DEFAULT;
}

// =============================================================================
// Feedback loop type colors
// =============================================================================

const LOOP_TYPE_COLORS: Record<string, { bg: string; text: string; border: string }> = {
  inner: { bg: "rgba(59,130,246,0.15)", text: "#93c5fd", border: "#3b82f6" },
  outer: { bg: "rgba(168,85,247,0.15)", text: "#d8b4fe", border: "#a855f7" },
  cross: { bg: "rgba(245,158,11,0.15)", text: "#fcd34d", border: "#f59e0b" },
};

const LOOP_TYPE_DEFAULT = { bg: "rgba(113,113,122,0.15)", text: "#a1a1aa", border: "#71717a" };

function loopTypeColor(type?: string) {
  if (!type) return LOOP_TYPE_DEFAULT;
  return LOOP_TYPE_COLORS[type] ?? LOOP_TYPE_DEFAULT;
}

// =============================================================================
// Role sort order
// =============================================================================

const ROLE_SORT_ORDER: Record<AgentRole, number> = {
  orchestrator: 0,
  executor: 1,
  analyzer: 2,
  session: 3,
  monitor: 4,
};

function roleSortKey(role: string): number {
  return ROLE_SORT_ORDER[role as AgentRole] ?? 99;
}

// =============================================================================
// Custom node component
// =============================================================================

interface AgentNodeData {
  agent: AgenticAgent;
  dimmed: boolean;
  [key: string]: unknown;
}

const AgentNode = memo(({ data }: { data: AgentNodeData }) => {
  const agent = data.agent;
  const colors = roleColor(agent.role);

  return (
    <div
      className="rounded-lg transition-opacity duration-200"
      style={{
        width: 220,
        background: colors.bg,
        border: `2px solid ${colors.border}`,
        opacity: data.dimmed ? 0.2 : 1,
      }}
    >
      <Handle type="target" position={Position.Top} style={{ background: colors.border }} />
      <div className="px-3 py-2">
        <div className="flex items-center gap-1.5 mb-1">
          <span className="text-xs font-semibold text-white truncate flex-1">{agent.name}</span>
          <span
            className="text-[9px] px-1.5 py-0.5 rounded-full capitalize font-medium shrink-0"
            style={{
              background: colors.bg,
              color: colors.text,
              border: `1px solid ${colors.border}`,
            }}
          >
            {agent.role}
          </span>
        </div>
        <div className="flex items-center gap-1 text-[10px] text-zinc-400">
          <Zap className="w-2.5 h-2.5 shrink-0" />
          <span>{agent.capabilities.length} capabilities</span>
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} style={{ background: colors.border }} />
    </div>
  );
});
AgentNode.displayName = "AgentNode";

const nodeTypes = { agentNode: AgentNode };

// =============================================================================
// Dagre layout (TB — top-to-bottom hierarchy)
// =============================================================================

const NODE_W = 220;
const NODE_H = 70;

function layoutAgentGraph(
  agents: AgenticAgent[],
  delegationChains: AgenticDelegationChain[],
): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "TB", ranksep: 80, nodesep: 50 });

  const agentIds = new Set(agents.map((a) => a.id));

  for (const a of agents) {
    g.setNode(a.id, { width: NODE_W, height: NODE_H });
  }

  const flowEdges: Edge[] = [];
  let edgeIdx = 0;

  // Edges from agent.delegatesTo[]
  const delegationChainSet = new Set<string>();
  for (const chain of delegationChains) {
    delegationChainSet.add(`${chain.from}->${chain.to}`);
  }

  for (const agent of agents) {
    if (agent.delegatesTo) {
      for (const targetId of agent.delegatesTo) {
        if (agentIds.has(targetId)) {
          g.setEdge(agent.id, targetId);

          // Check if there's a delegation chain for this edge
          const chainKey = `${agent.id}->${targetId}`;
          const chain = delegationChainSet.has(chainKey)
            ? delegationChains.find((c) => c.from === agent.id && c.to === targetId)
            : undefined;

          flowEdges.push({
            id: `agent-edge-${edgeIdx++}`,
            source: agent.id,
            target: targetId,
            label: chain?.via,
            labelStyle: { fontSize: 9, fill: "#94a3b8" },
            labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
            animated: !!chain,
            style: {
              stroke: chain ? "#f59e0b" : "#64748b",
              strokeWidth: chain ? 2 : 1.5,
            },
            markerEnd: {
              type: MarkerType.ArrowClosed,
              width: 14,
              height: 14,
              color: chain ? "#f59e0b" : "#64748b",
            },
          });
        }
      }
    }
  }

  // Edges from delegationChains that aren't already covered by delegatesTo
  for (const chain of delegationChains) {
    if (!agentIds.has(chain.from) || !agentIds.has(chain.to)) continue;

    const alreadyExists = flowEdges.some((e) => e.source === chain.from && e.target === chain.to);
    if (!alreadyExists) {
      g.setEdge(chain.from, chain.to);
      flowEdges.push({
        id: `chain-edge-${edgeIdx++}`,
        source: chain.from,
        target: chain.to,
        label: chain.via,
        labelStyle: { fontSize: 9, fill: "#94a3b8" },
        labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
        animated: true,
        style: {
          stroke: "#f59e0b",
          strokeWidth: 2,
        },
        markerEnd: {
          type: MarkerType.ArrowClosed,
          width: 14,
          height: 14,
          color: "#f59e0b",
        },
      });
    }
  }

  dagre.layout(g);

  const flowNodes: Node[] = agents.map((a) => {
    const pos = g.node(a.id);
    return {
      id: a.id,
      type: "agentNode",
      position: { x: (pos?.x ?? 0) - NODE_W / 2, y: (pos?.y ?? 0) - NODE_H / 2 },
      data: { agent: a, dimmed: false } satisfies AgentNodeData,
    };
  });

  return { nodes: flowNodes, edges: flowEdges };
}

// =============================================================================
// Connection helpers (same pattern as DeveloperDependencyGraph)
// =============================================================================

function getConnectedIds(nodeId: string, edges: Edge[]): Set<string> {
  const connected = new Set<string>();
  connected.add(nodeId);
  for (const e of edges) {
    if (e.source === nodeId) connected.add(e.target);
    if (e.target === nodeId) connected.add(e.source);
  }
  return connected;
}

function getConnectedEdgeIds(nodeId: string, edges: Edge[]): Set<string> {
  const ids = new Set<string>();
  for (const e of edges) {
    if (e.source === nodeId || e.target === nodeId) ids.add(e.id);
  }
  return ids;
}

// =============================================================================
// Agent detail sidebar
// =============================================================================

interface AgentDetailSidebarProps {
  agent: AgenticAgent;
  agents: AgenticAgent[];
  feedbackLoops: AgenticFeedbackLoop[];
  onClose: () => void;
}

function AgentDetailSidebar({ agent, agents, feedbackLoops, onClose }: AgentDetailSidebarProps) {
  const colors = roleColor(agent.role);

  const agentById = useMemo(() => {
    const map = new Map<string, AgenticAgent>();
    for (const a of agents) map.set(a.id, a);
    return map;
  }, [agents]);

  const participatingLoops = useMemo(
    () => feedbackLoops.filter((loop) => loop.agents.includes(agent.id)),
    [feedbackLoops, agent.id],
  );

  return (
    <div className="w-80 bg-card border-l border-border/50 overflow-y-auto shrink-0">
      {/* Header */}
      <div className="flex items-start justify-between p-4 border-b border-border/30">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <h3 className="text-sm font-semibold text-white truncate">{agent.name}</h3>
          </div>
          <span
            className="text-[10px] px-1.5 py-0.5 rounded-full capitalize font-medium inline-block"
            style={{
              background: colors.bg,
              color: colors.text,
              border: `1px solid ${colors.border}`,
            }}
          >
            {agent.role}
          </span>
        </div>
        <button
          onClick={onClose}
          className="p-1 rounded hover:bg-muted/50 text-zinc-400 hover:text-white transition-colors shrink-0"
          aria-label="Close detail sidebar"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      <div className="p-4 space-y-4">
        {/* Description */}
        {agent.description && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1">
              Description
            </h4>
            <p className="text-xs text-zinc-300 leading-relaxed">{agent.description}</p>
          </div>
        )}

        {/* Capabilities */}
        {agent.capabilities.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1.5">
              Capabilities
            </h4>
            <div className="flex flex-wrap gap-1">
              {agent.capabilities.map((cap, i) => (
                <span
                  key={`${cap}-${i}`}
                  className="text-[10px] px-1.5 py-0.5 rounded-full bg-muted/50 text-zinc-300 border border-border/30"
                >
                  {cap}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Decision Authority */}
        {agent.decisionAuthority && agent.decisionAuthority.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1.5 flex items-center gap-1">
              <Shield className="w-3 h-3" />
              Decision Authority
            </h4>
            <div className="flex flex-wrap gap-1">
              {agent.decisionAuthority.map((auth, i) => (
                <span
                  key={`${auth}-${i}`}
                  className="text-[10px] px-1.5 py-0.5 rounded-full border"
                  style={{
                    background: "rgba(245,158,11,0.15)",
                    color: "#fcd34d",
                    borderColor: "rgba(245,158,11,0.3)",
                  }}
                >
                  {auth}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Triggers */}
        {agent.triggers && agent.triggers.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1.5 flex items-center gap-1">
              <Zap className="w-3 h-3" />
              Triggers
            </h4>
            <div className="flex flex-wrap gap-1">
              {agent.triggers.map((trigger, i) => (
                <span
                  key={`${trigger}-${i}`}
                  className="text-[10px] px-1.5 py-0.5 rounded-full bg-muted/50 text-zinc-300 border border-border/30"
                >
                  {trigger}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Delegates To */}
        {agent.delegatesTo && agent.delegatesTo.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1.5 flex items-center gap-1">
              <ArrowRight className="w-3 h-3" />
              Delegates To
            </h4>
            <div className="space-y-1">
              {agent.delegatesTo.map((targetId) => {
                const target = agentById.get(targetId);
                const targetColors = roleColor(target?.role);
                return (
                  <div key={targetId} className="flex items-center gap-1.5 text-xs">
                    <span
                      className="w-1.5 h-1.5 rounded-full shrink-0"
                      style={{ backgroundColor: targetColors.accent }}
                    />
                    <span className="text-zinc-300">{target?.name ?? targetId}</span>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {/* Feedback Loops */}
        {participatingLoops.length > 0 && (
          <div>
            <h4 className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 mb-1.5">
              Feedback Loops
            </h4>
            <div className="space-y-1.5">
              {participatingLoops.map((loop) => {
                const ltc = loopTypeColor(loop.type);
                return (
                  <div
                    key={loop.id}
                    className="text-xs rounded px-2 py-1.5 border"
                    style={{ background: ltc.bg, borderColor: ltc.border + "40" }}
                  >
                    <div className="flex items-center gap-1.5">
                      <span className="font-medium text-zinc-200">{loop.name}</span>
                      <span
                        className="text-[9px] px-1 py-0.5 rounded capitalize"
                        style={{
                          color: ltc.text,
                          background: ltc.bg,
                          border: `1px solid ${ltc.border}`,
                        }}
                      >
                        {loop.type}
                      </span>
                    </div>
                    <p className="text-[10px] text-zinc-400 mt-0.5">{loop.exitCondition}</p>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Feedback loops card list (Section 2)
// =============================================================================

interface FeedbackLoopsSectionProps {
  feedbackLoops: AgenticFeedbackLoop[];
  agents: AgenticAgent[];
}

function FeedbackLoopsSection({ feedbackLoops, agents }: FeedbackLoopsSectionProps) {
  const agentById = useMemo(() => {
    const map = new Map<string, AgenticAgent>();
    for (const a of agents) map.set(a.id, a);
    return map;
  }, [agents]);

  if (feedbackLoops.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-xs text-zinc-500 italic">
        No feedback loops defined
      </div>
    );
  }

  return (
    <div className="overflow-y-auto space-y-2 p-3">
      <h3 className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500 mb-2">
        Feedback Loops
      </h3>
      {feedbackLoops.map((loop) => {
        const ltc = loopTypeColor(loop.type);
        return (
          <div
            key={loop.id}
            className="rounded-lg p-3 border"
            style={{ borderColor: "hsl(var(--border) / 0.5)" }}
          >
            {/* Name + type badge */}
            <div className="flex items-center gap-2 mb-1">
              <span className="text-xs font-semibold text-white">{loop.name}</span>
              <span
                className="text-[9px] px-1.5 py-0.5 rounded-full capitalize font-medium"
                style={{ background: ltc.bg, color: ltc.text, border: `1px solid ${ltc.border}` }}
              >
                {loop.type}
              </span>
            </div>

            {/* Description */}
            {loop.description && (
              <p className="text-xs text-muted-foreground mb-2 leading-relaxed">
                {loop.description}
              </p>
            )}

            {/* Participants */}
            <div className="flex items-center gap-1.5 flex-wrap mb-1.5">
              <span className="text-[10px] text-zinc-500 font-medium">Participants:</span>
              {loop.agents.map((agentId) => {
                const agent = agentById.get(agentId);
                const ac = roleColor(agent?.role);
                return (
                  <span
                    key={agentId}
                    className="text-[10px] px-1.5 py-0.5 rounded cursor-default"
                    style={{
                      color: ac.text,
                      background: ac.bg,
                      border: `1px solid ${ac.border}40`,
                    }}
                  >
                    {agent?.name ?? agentId}
                  </span>
                );
              })}
            </div>

            {/* Exit condition */}
            <div className="flex items-start gap-1.5 text-[10px] text-zinc-400">
              <CircleStop className="w-3 h-3 shrink-0 mt-0.5 text-red-400/70" />
              <span>
                <span className="font-medium text-zinc-500">Exit:</span> {loop.exitCondition}
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
}

// =============================================================================
// Decision authority matrix (Section 3)
// =============================================================================

interface DecisionMatrixSectionProps {
  agents: AgenticAgent[];
}

function DecisionMatrixSection({ agents }: DecisionMatrixSectionProps) {
  const { sortedAgents, decisionTypes } = useMemo(() => {
    const allDecisions = new Set<string>();
    for (const agent of agents) {
      if (agent.decisionAuthority) {
        for (const d of agent.decisionAuthority) allDecisions.add(d);
      }
    }

    const sorted = [...agents].sort((a, b) => roleSortKey(a.role) - roleSortKey(b.role));
    return { sortedAgents: sorted, decisionTypes: Array.from(allDecisions).sort() };
  }, [agents]);

  if (decisionTypes.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-xs text-zinc-500 italic">
        No decision authority defined
      </div>
    );
  }

  return (
    <div className="overflow-auto p-3">
      <h3 className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500 mb-2">
        Decision Authority
      </h3>
      <table className="w-full border-collapse">
        <thead>
          <tr>
            <th className="text-[10px] text-zinc-500 font-medium text-left pb-2 pr-2 sticky left-0 bg-card">
              Agent
            </th>
            {decisionTypes.map((dt) => (
              <th
                key={dt}
                className="text-[10px] text-zinc-500 font-medium pb-2 px-1"
                style={{
                  writingMode: decisionTypes.length > 4 ? "vertical-rl" : undefined,
                  transform: decisionTypes.length > 4 ? "rotate(180deg)" : undefined,
                  textAlign: "center",
                  whiteSpace: "nowrap",
                  maxWidth: 80,
                  height: decisionTypes.length > 4 ? 80 : undefined,
                }}
              >
                {dt}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sortedAgents.map((agent) => {
            const ac = roleColor(agent.role);
            const authoritySet = new Set(agent.decisionAuthority ?? []);
            return (
              <tr key={agent.id} className="border-t border-border/20">
                <td className="text-[10px] py-1.5 pr-2 sticky left-0 bg-card">
                  <span style={{ color: ac.text }}>{agent.name}</span>
                </td>
                {decisionTypes.map((dt) => (
                  <td key={dt} className="text-center py-1.5 px-1">
                    {authoritySet.has(dt) ? (
                      <Check className="w-3 h-3 mx-auto text-emerald-400" />
                    ) : null}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// =============================================================================
// Empty state
// =============================================================================

function AgenticEmptyState() {
  return (
    <div className="flex flex-col items-center justify-center h-full gap-3 text-center px-8">
      <Bot className="w-12 h-12 text-muted-foreground/50" />
      <div>
        <p className="text-sm font-medium text-zinc-400">No Agentic Structure</p>
        <p className="text-xs text-muted-foreground mt-1 max-w-md leading-relaxed">
          No agentic patterns discovered in this project. Add an agenticStructure section to the
          architecture spec to visualize agents, feedback loops, and decision authority.
        </p>
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

export function DeveloperAgenticPanel({ project }: Props) {
  const agenticStructure = project.agenticStructure;

  if (!agenticStructure || agenticStructure.agents.length === 0) {
    return <AgenticEmptyState />;
  }

  return <AgenticPanelInner structure={agenticStructure} />;
}

// =============================================================================
// Inner component (owns ReactFlow state)
// =============================================================================

function AgenticPanelInner({ structure }: { structure: AgenticStructure }) {
  const { agents, feedbackLoops, delegationChains } = structure;

  const dataKey = useMemo(
    () =>
      agents.map((a) => a.id).join(",") +
      "|" +
      delegationChains.map((c) => `${c.from}-${c.to}`).join(","),
    [agents, delegationChains],
  );

  return (
    <div className="flex flex-col w-full h-full" style={{ minHeight: 600 }}>
      {/* Section 1: Agent Hierarchy Graph (~60% height) */}
      <div className="flex-[3] min-h-0">
        <AgentHierarchyGraph
          key={dataKey}
          agents={agents}
          feedbackLoops={feedbackLoops}
          delegationChains={delegationChains}
        />
      </div>

      {/* Section 2 & 3: Bottom row (~40% height) */}
      <div className="flex-[2] min-h-0 flex border-t border-border/30">
        {/* Feedback Loops (left) */}
        <div className="flex-1 min-w-0 border-r border-border/30 overflow-y-auto">
          <FeedbackLoopsSection feedbackLoops={feedbackLoops} agents={agents} />
        </div>

        {/* Decision Authority Matrix (right) */}
        <div className="flex-1 min-w-0 overflow-auto">
          <DecisionMatrixSection agents={agents} />
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Agent hierarchy graph (ReactFlow)
// =============================================================================

function AgentHierarchyGraph({
  agents,
  feedbackLoops,
  delegationChains,
}: {
  agents: AgenticAgent[];
  feedbackLoops: AgenticFeedbackLoop[];
  delegationChains: AgenticDelegationChain[];
}) {
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
  const [selectedAgent, setSelectedAgent] = useState<AgenticAgent | null>(null);

  const { nodes: initialNodes, edges: initialEdges } = useMemo(
    () => layoutAgentGraph(agents, delegationChains),
    [agents, delegationChains],
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

  // Agent lookup map
  // Use a ref to stabilize the lookup so handleNodeClick doesn't cause ReactFlow re-renders
  const agentById = useMemo(() => {
    const map = new Map<string, AgenticAgent>();
    for (const a of agents) map.set(a.id, a);
    return map;
  }, [agents]);

  const agentByIdRef = useRef(agentById);
  agentByIdRef.current = agentById;

  // Hover dimming
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

  // Clean up hover state on unmount (e.g., tab switch)
  useEffect(() => {
    return () => {
      setHoveredNodeId(null);
    };
  }, []);

  const handleNodeClick = useCallback((_: React.MouseEvent, node: Node) => {
    const agent = agentByIdRef.current.get(node.id);
    if (agent) setSelectedAgent(agent);
  }, []);

  const handleCloseDetail = useCallback(() => {
    setSelectedAgent(null);
  }, []);

  return (
    <div className="flex w-full h-full">
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
              const d = node.data as unknown as AgentNodeData | undefined;
              return roleColor(d?.agent?.role).accent;
            }}
            maskColor="rgba(0,0,0,0.6)"
          />
          <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#374151" />
        </ReactFlow>

        {/* Legend */}
        <div className="absolute bottom-12 left-2 bg-card/90 border border-border/50 rounded-lg p-2.5 space-y-1.5 z-10">
          <div className="text-[10px] font-semibold text-zinc-400 mb-1">Roles</div>
          {(["orchestrator", "executor", "analyzer", "session", "monitor"] as const).map((role) => {
            const rc = ROLE_COLORS[role];
            return (
              <div key={role} className="flex items-center gap-1.5 text-[10px]">
                <span className="w-2.5 h-2.5 rounded-full" style={{ background: rc.accent }} />
                <span className="text-zinc-400 capitalize">{role}</span>
              </div>
            );
          })}

          <div className="text-[10px] font-semibold text-zinc-400 mt-2 mb-1 pt-1 border-t border-border/30">
            Edges
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <div className="w-4 h-0 border-t-2 border-[#64748b]" />
            <span className="text-zinc-400">Delegates To</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px]">
            <div className="w-4 h-0 border-t-2 border-[#f59e0b]" />
            <span className="text-zinc-400">Delegation Chain</span>
          </div>
        </div>
      </div>

      {/* Detail sidebar */}
      {selectedAgent && (
        <AgentDetailSidebar
          agent={selectedAgent}
          agents={agents}
          feedbackLoops={feedbackLoops}
          onClose={handleCloseDetail}
        />
      )}
    </div>
  );
}
