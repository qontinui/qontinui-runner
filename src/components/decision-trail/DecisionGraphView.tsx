import { useMemo } from "react";
import type { DecisionPreview, Decision } from "@/types/decision-trail";
import { DECISION_CATEGORY_COLORS, parseJsonField } from "@/types/decision-trail";
import type { DecisionCategory } from "@/types/decision-trail";

interface DecisionGraphViewProps {
  decisions: DecisionPreview[];
  /** Full decision data keyed by ID, used for edge rendering when available. */
  fullDecisions?: Map<string, Decision>;
  onSelectDecision: (id: string) => void;
}

interface GraphNode {
  id: string;
  title: string;
  category: string;
  scale: string;
  x: number;
  y: number;
}

interface GraphEdge {
  from: string;
  to: string;
}

function layoutNodes(
  decisions: DecisionPreview[],
  fullDecisions?: Map<string, Decision>,
): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const nodes: GraphNode[] = [];
  const edges: GraphEdge[] = [];
  const idSet = new Set(decisions.map((d) => d.id));

  // Grid layout grouped by category
  const byCategory = new Map<string, DecisionPreview[]>();
  for (const d of decisions) {
    const list = byCategory.get(d.category) || [];
    list.push(d);
    byCategory.set(d.category, list);
  }

  let colIndex = 0;
  for (const [, catDecisions] of byCategory) {
    let rowIndex = 0;
    for (const d of catDecisions) {
      nodes.push({
        id: d.id,
        title: d.title,
        category: d.category,
        scale: d.scale,
        x: 40 + colIndex * 220,
        y: 40 + rowIndex * 80,
      });

      // Build edges from full decision data if available
      if (fullDecisions) {
        const full = fullDecisions.get(d.id);
        if (full) {
          const related = parseJsonField<string[]>(full.relatedDecisionsJson, []);
          for (const relId of related) {
            if (idSet.has(relId)) {
              edges.push({ from: d.id, to: relId });
            }
          }
        }
      }

      rowIndex++;
    }
    colIndex++;
  }

  return { nodes, edges };
}

export function DecisionGraphView({
  decisions,
  fullDecisions,
  onSelectDecision,
}: DecisionGraphViewProps) {
  const { nodes, edges } = useMemo(
    () => layoutNodes(decisions, fullDecisions),
    [decisions, fullDecisions],
  );

  if (decisions.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        No decisions to visualize yet.
      </div>
    );
  }

  const width = Math.max(
    800,
    nodes.reduce((max, n) => Math.max(max, n.x + 200), 0),
  );
  const height = Math.max(
    400,
    nodes.reduce((max, n) => Math.max(max, n.y + 60), 0),
  );

  // Build node position map for edges
  const posMap = new Map(nodes.map((n) => [n.id, { x: n.x, y: n.y }]));

  return (
    <div className="w-full h-full overflow-auto">
      <svg width={width} height={height} className="min-w-full">
        {/* Category labels */}
        {(() => {
          const cats = new Map<string, { x: number; label: string }>();
          for (const n of nodes) {
            if (!cats.has(n.category)) {
              cats.set(n.category, { x: n.x, label: n.category });
            }
          }
          return Array.from(cats.entries()).map(([cat, { x, label }]) => (
            <text
              key={`label-${cat}`}
              x={x + 90}
              y={24}
              fill={DECISION_CATEGORY_COLORS[cat as DecisionCategory] || "#6B7280"}
              fontSize={10}
              fontWeight="bold"
              textAnchor="middle"
              style={{ textTransform: "uppercase" }}
            >
              {label}
            </text>
          ));
        })()}

        {/* Edges */}
        {edges.map((e, i) => {
          const from = posMap.get(e.from);
          const to = posMap.get(e.to);
          if (!from || !to) return null;
          return (
            <line
              key={`edge-${i}`}
              x1={from.x + 90}
              y1={from.y + 20}
              x2={to.x + 90}
              y2={to.y + 20}
              stroke="var(--border)"
              strokeWidth={1}
              strokeDasharray="4 2"
            />
          );
        })}

        {/* Nodes */}
        {nodes.map((node) => {
          const color = DECISION_CATEGORY_COLORS[node.category as DecisionCategory] || "#6B7280";
          const isStrategic = node.scale === "strategic";
          return (
            <g key={node.id} onClick={() => onSelectDecision(node.id)} className="cursor-pointer">
              <rect
                x={node.x}
                y={node.y + 10}
                width={180}
                height={isStrategic ? 48 : 40}
                rx={6}
                fill="var(--card)"
                stroke={color}
                strokeWidth={isStrategic ? 2 : 1}
              />
              <circle cx={node.x + 12} cy={node.y + 30} r={4} fill={color} />
              <text
                x={node.x + 22}
                y={node.y + 34}
                fill="var(--foreground)"
                fontSize={11}
                fontFamily="inherit"
              >
                {node.title.length > 22 ? node.title.slice(0, 22) + "..." : node.title}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
