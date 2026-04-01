/**
 * NavigationFlowGraph
 *
 * Shows how pages connect via navigation: nodes = pages (from specs),
 * edges = navigation links (from spec navigation groups + pageNavigate actions).
 * Layout: LR flow showing user journeys.
 */

import { useMemo, useState, useEffect, useCallback, useRef } from "react";
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
import { Navigation, Loader2 } from "lucide-react";
import { copyMermaidToClipboard } from "@/lib/development-intelligence/mermaid-export";

interface PageSpec {
  metadata: {
    pageId: string;
    pageUrl: string;
    component: string;
  };
  groups: Array<{
    category: string;
    name: string;
    assertions: Array<{
      category: string;
      target?: {
        criteria?: {
          action?: string;
          pageNavigate?: string;
          href?: string;
        };
      };
      expected?: string;
    }>;
  }>;
}

interface NavLink {
  from: string;
  to: string;
  label?: string;
}

const NODE_W = 160;
const NODE_H = 50;

// =============================================================================
// Page Node
// =============================================================================

function PageNode({ data }: { data: { label: string; route: string; assertionCount: number } }) {
  return (
    <div
      className="rounded-lg border border-border/50 bg-card px-3 py-2 text-xs shadow-sm"
      style={{ minWidth: NODE_W }}
    >
      <Handle type="target" position={Position.Left} className="!bg-border" />
      <div className="font-semibold truncate">{data.label}</div>
      <div className="text-muted-foreground text-[10px]">
        {data.route} · {data.assertionCount} assertions
      </div>
      <Handle type="source" position={Position.Right} className="!bg-border" />
    </div>
  );
}

const nodeTypes = { pageNode: PageNode };

// =============================================================================
// Extract navigation links from spec data
// =============================================================================

function extractNavLinks(specs: PageSpec[]): NavLink[] {
  const links: NavLink[] = [];
  const pageUrls = new Map<string, string>();

  for (const spec of specs) {
    pageUrls.set(spec.metadata.pageUrl, spec.metadata.pageId);
    pageUrls.set(spec.metadata.pageId, spec.metadata.pageId);
  }

  for (const spec of specs) {
    for (const group of spec.groups) {
      if (group.category !== "navigation") continue;

      for (const assertion of group.assertions) {
        const target = assertion.target?.criteria?.pageNavigate || assertion.target?.criteria?.href;

        if (target) {
          const targetPageId = pageUrls.get(target) ?? target.replace(/^\//, "");
          if (targetPageId !== spec.metadata.pageId) {
            links.push({
              from: spec.metadata.pageId,
              to: targetPageId,
              label: group.name,
            });
          }
        }

        // Also scan expected text for route references
        if (assertion.expected) {
          for (const [url, pageId] of pageUrls.entries()) {
            if (
              url.startsWith("/") &&
              assertion.expected.includes(url) &&
              pageId !== spec.metadata.pageId
            ) {
              links.push({
                from: spec.metadata.pageId,
                to: pageId,
              });
            }
          }
        }
      }
    }
  }

  // Deduplicate
  const seen = new Set<string>();
  return links.filter((l) => {
    const key = `${l.from}→${l.to}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

// =============================================================================
// Build graph
// =============================================================================

function buildNavGraph(specs: PageSpec[], links: NavLink[]): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "LR", ranksep: 100, nodesep: 50 });

  // Only include pages that have at least one link
  const linkedPages = new Set<string>();
  for (const link of links) {
    linkedPages.add(link.from);
    linkedPages.add(link.to);
  }

  // If no links found, show all pages
  const pagesToShow =
    linkedPages.size > 0 ? linkedPages : new Set(specs.map((s) => s.metadata.pageId));

  const nodes: Node[] = [];
  const specByPageId = new Map(specs.map((s) => [s.metadata.pageId, s]));

  for (const pageId of pagesToShow) {
    g.setNode(pageId, { width: NODE_W, height: NODE_H });
    const spec = specByPageId.get(pageId);
    const assertionCount = spec
      ? spec.groups.reduce((sum, gr) => sum + gr.assertions.length, 0)
      : 0;

    nodes.push({
      id: pageId,
      type: "pageNode",
      position: { x: 0, y: 0 },
      data: {
        label: pageId,
        route: spec?.metadata.pageUrl ?? `/${pageId}`,
        assertionCount,
      },
    });
  }

  const edges: Edge[] = links
    .filter((l) => pagesToShow.has(l.from) && pagesToShow.has(l.to))
    .map((l, i) => {
      g.setEdge(l.from, l.to);
      return {
        id: `nav-${i}`,
        source: l.from,
        target: l.to,
        label: l.label,
        labelStyle: { fontSize: 9, fill: "#94a3b8" },
        labelBgStyle: { fill: "#1a1b26", fillOpacity: 0.9 },
        style: { stroke: "#3b82f6", strokeWidth: 1.5 },
        markerEnd: {
          type: MarkerType.ArrowClosed,
          width: 12,
          height: 12,
          color: "#3b82f6",
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

  return { nodes, edges };
}

// =============================================================================
// Main Component
// =============================================================================

export function NavigationFlowGraph() {
  const [specs, setSpecs] = useState<PageSpec[]>([]);
  const [loading, setLoading] = useState(true);

  const loadSpecs = useCallback(async (signal: AbortSignal) => {
    try {
      const resp = await fetch("http://localhost:9876/specs", { signal });
      const json = await resp.json();
      if (!signal.aborted && json.data) {
        setSpecs(json.data);
      }
    } catch {
      // Specs endpoint may not be available
    } finally {
      if (!signal.aborted) setLoading(false);
    }
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    loadSpecs(controller.signal);
    return () => {
      controller.abort();
    };
  }, [loadSpecs]);

  const navLinks = useMemo(() => extractNavLinks(specs), [specs]);
  const { nodes: initialNodes, edges: initialEdges } = useMemo(
    () => buildNavGraph(specs, navLinks),
    [specs, navLinks],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  const prevInitialRef = useRef({ nodes: initialNodes, edges: initialEdges });
  if (prevInitialRef.current.nodes !== initialNodes) {
    prevInitialRef.current.nodes = initialNodes;
    setNodes(initialNodes);
  }
  if (prevInitialRef.current.edges !== initialEdges) {
    prevInitialRef.current.edges = initialEdges;
    setEdges(initialEdges);
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (nodes.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Navigation className="w-8 h-8" />
        <span>No navigation flows detected in page specs</span>
      </div>
    );
  }

  return (
    <div className="h-full relative">
      <ReactFlow
        nodes={nodes}
        edges={edges}
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
        <MiniMap maskColor="rgba(0,0,0,0.1)" />
      </ReactFlow>

      {/* Export button */}
      <button
        onClick={() =>
          copyMermaidToClipboard(nodes, edges, {
            direction: "LR",
            title: "Page Navigation Flow",
          })
        }
        className="absolute top-2 right-2 z-10 flex items-center gap-1 px-2 py-1 text-[10px] bg-card/90 border border-border/50 rounded-md hover:bg-muted/80 transition-colors text-muted-foreground"
        title="Copy as Mermaid markdown"
      >
        <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
          <path d="M0 6.75C0 5.784.784 5 1.75 5h1.5a.75.75 0 010 1.5h-1.5a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-1.5a.75.75 0 011.5 0v1.5A1.75 1.75 0 019.25 16h-7.5A1.75 1.75 0 010 14.25v-7.5z" />
          <path d="M5 1.75C5 .784 5.784 0 6.75 0h7.5C15.216 0 16 .784 16 1.75v7.5A1.75 1.75 0 0114.25 11h-7.5A1.75 1.75 0 015 9.25v-7.5zm1.75-.25a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-7.5a.25.25 0 00-.25-.25h-7.5z" />
        </svg>
        Mermaid
      </button>
    </div>
  );
}
