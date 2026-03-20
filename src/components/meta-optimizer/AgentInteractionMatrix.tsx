/**
 * AgentInteractionMatrix — SVG heatmap showing correlations between agent pairs.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AgentInteraction {
  source_agent: string;
  target_agent: string;
  correlation: number;
  sample_size: number;
}

const AGENTS = ["spec_analyst", "locator", "implementer", "verifier"];
const AGENT_LABELS: Record<string, string> = {
  spec_analyst: "Spec",
  locator: "Loc",
  implementer: "Impl",
  verifier: "Ver",
};

const CELL_SIZE = 60;
const LABEL_W = 50;
const LABEL_H = 30;
const SVG_W = LABEL_W + AGENTS.length * CELL_SIZE;
const SVG_H = LABEL_H + AGENTS.length * CELL_SIZE;

function correlationColor(v: number): string {
  if (v > 0.05) {
    const intensity = Math.min(Math.abs(v), 1);
    const g = Math.round(80 + intensity * 150);
    return `rgb(30, ${g}, 50)`;
  } else if (v < -0.05) {
    const intensity = Math.min(Math.abs(v), 1);
    const r = Math.round(80 + intensity * 150);
    return `rgb(${r}, 30, 30)`;
  }
  return "#27272a";
}

export function AgentInteractionMatrix() {
  const [interactions, setInteractions] = useState<AgentInteraction[]>([]);
  const [loading, setLoading] = useState(true);
  const [hovered, setHovered] = useState<{ row: number; col: number } | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const data = await invoke<AgentInteraction[]>("get_agent_interaction_matrix", {
        days: 30,
      });
      setInteractions(data);
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  if (loading) {
    return null;
  }

  if (interactions.length === 0) {
    return null;
  }

  // Build correlation lookup
  const lookup: Record<string, AgentInteraction> = {};
  for (const item of interactions) {
    lookup[`${item.source_agent}:${item.target_agent}`] = item;
  }

  return (
    <div>
      <div className="text-sm text-zinc-400 mb-2">Agent Interaction Matrix</div>
      <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-3 inline-block">
        <svg width={SVG_W} height={SVG_H} viewBox={`0 0 ${SVG_W} ${SVG_H}`}>
          {/* Column headers */}
          {AGENTS.map((agent, ci) => (
            <text
              key={`col-${ci}`}
              x={LABEL_W + ci * CELL_SIZE + CELL_SIZE / 2}
              y={LABEL_H - 8}
              textAnchor="middle"
              fill="#a1a1aa"
              fontSize={10}
            >
              {AGENT_LABELS[agent] ?? agent}
            </text>
          ))}

          {/* Row headers + cells */}
          {AGENTS.map((sourceAgent, ri) => (
            <g key={`row-${ri}`}>
              <text
                x={LABEL_W - 5}
                y={LABEL_H + ri * CELL_SIZE + CELL_SIZE / 2 + 4}
                textAnchor="end"
                fill="#a1a1aa"
                fontSize={10}
              >
                {AGENT_LABELS[sourceAgent] ?? sourceAgent}
              </text>

              {AGENTS.map((targetAgent, ci) => {
                const key = `${sourceAgent}:${targetAgent}`;
                const item = lookup[key];
                const isHovered = hovered?.row === ri && hovered?.col === ci;
                const isSelf = ri === ci;

                return (
                  <g key={`cell-${ri}-${ci}`}>
                    <rect
                      x={LABEL_W + ci * CELL_SIZE + 1}
                      y={LABEL_H + ri * CELL_SIZE + 1}
                      width={CELL_SIZE - 2}
                      height={CELL_SIZE - 2}
                      rx={3}
                      fill={
                        isSelf ? "#18181b" : item ? correlationColor(item.correlation) : "#27272a"
                      }
                      stroke={isHovered ? "#71717a" : "none"}
                      strokeWidth={isHovered ? 1.5 : 0}
                      onMouseEnter={() => setHovered({ row: ri, col: ci })}
                      onMouseLeave={() => setHovered(null)}
                      className="cursor-pointer"
                    />
                    {!isSelf && item && (
                      <text
                        x={LABEL_W + ci * CELL_SIZE + CELL_SIZE / 2}
                        y={LABEL_H + ri * CELL_SIZE + CELL_SIZE / 2 + 4}
                        textAnchor="middle"
                        fill="#d4d4d8"
                        fontSize={11}
                        fontWeight="500"
                        pointerEvents="none"
                      >
                        {item.correlation.toFixed(2)}
                      </text>
                    )}
                    {isSelf && (
                      <text
                        x={LABEL_W + ci * CELL_SIZE + CELL_SIZE / 2}
                        y={LABEL_H + ri * CELL_SIZE + CELL_SIZE / 2 + 4}
                        textAnchor="middle"
                        fill="#52525b"
                        fontSize={9}
                        pointerEvents="none"
                      >
                        --
                      </text>
                    )}

                    {/* Tooltip */}
                    {isHovered && item && !isSelf && (
                      <g>
                        <rect
                          x={LABEL_W + ci * CELL_SIZE + CELL_SIZE + 4}
                          y={LABEL_H + ri * CELL_SIZE}
                          width={130}
                          height={32}
                          rx={4}
                          fill="#18181b"
                          stroke="#3f3f46"
                        />
                        <text
                          x={LABEL_W + ci * CELL_SIZE + CELL_SIZE + 10}
                          y={LABEL_H + ri * CELL_SIZE + 13}
                          fill="#d4d4d8"
                          fontSize={9}
                        >
                          r = {item.correlation.toFixed(3)}
                        </text>
                        <text
                          x={LABEL_W + ci * CELL_SIZE + CELL_SIZE + 10}
                          y={LABEL_H + ri * CELL_SIZE + 25}
                          fill="#a1a1aa"
                          fontSize={9}
                        >
                          n = {item.sample_size}
                        </text>
                      </g>
                    )}
                  </g>
                );
              })}
            </g>
          ))}
        </svg>
      </div>
    </div>
  );
}
