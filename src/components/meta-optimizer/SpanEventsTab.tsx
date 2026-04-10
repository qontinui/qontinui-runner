/**
 * SpanEventsTab — Browse span events (reward, message, object) grouped by execution.
 */

import { useState, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface SpanEvent {
  id: string;
  execution_id: string;
  trace_id: string | null;
  agent_type: string;
  event_type: string;
  step_index: number;
  // reward fields
  metric_name: string | null;
  reward_value: number | null;
  // message fields
  role: string | null;
  content: string | null;
  // object fields
  data_key: string | null;
  data_json: string | null;
  created_at: string;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const EVENT_TYPE_COLORS: Record<string, string> = {
  reward: "text-green-400 bg-green-900/30",
  message: "text-blue-400 bg-blue-900/30",
  object: "text-purple-400 bg-purple-900/30",
};

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async function fetchApi<T>(path: string): Promise<T | null> {
  try {
    const resp = await tracedFetch(`${getApiBase()}/meta-optimizer/${path}`);
    if (!resp.ok) return null;
    const json: ApiResponse<T> = await resp.json();
    return json.success ? (json.data ?? null) : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function truncate(text: string, maxLen: number): string {
  if (text.length <= maxLen) return text;
  return text.slice(0, maxLen) + "...";
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function SpanEventsTab() {
  const [events, setEvents] = useState<SpanEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const [executionFilter, setExecutionFilter] = useState("");
  const [agentTypeFilter, setAgentTypeFilter] = useState("");
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const [expandedCells, setExpandedCells] = useState<Set<string>>(new Set());

  const loadEvents = useCallback(async () => {
    setLoading(true);
    try {
      const params = new URLSearchParams();
      params.set("limit", "200");
      if (executionFilter.trim()) params.set("execution_id", executionFilter.trim());
      if (agentTypeFilter.trim()) params.set("agent_type", agentTypeFilter.trim());
      const data = await fetchApi<SpanEvent[]>(`span-events?${params.toString()}`);
      setEvents(data ?? []);
      setCollapsedGroups(new Set());
      setExpandedCells(new Set());
    } finally {
      setLoading(false);
    }
  }, [executionFilter, agentTypeFilter]);

  const toggleGroup = (execId: string) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(execId)) next.delete(execId);
      else next.add(execId);
      return next;
    });
  };

  const toggleCell = (cellKey: string) => {
    setExpandedCells((prev) => {
      const next = new Set(prev);
      if (next.has(cellKey)) next.delete(cellKey);
      else next.add(cellKey);
      return next;
    });
  };

  const shortId = (id: string) => id.slice(0, 8);

  // Group events by execution_id
  const grouped: Record<string, SpanEvent[]> = {};
  for (const ev of events) {
    if (!grouped[ev.execution_id]) grouped[ev.execution_id] = [];
    grouped[ev.execution_id].push(ev);
  }
  const executionIds = Object.keys(grouped);

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium text-zinc-200">Span Events</h2>
        <span className="text-xs text-zinc-500 ml-auto">{events.length} events</span>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-3">
        <input
          type="text"
          placeholder="execution_id filter"
          value={executionFilter}
          onChange={(e) => setExecutionFilter(e.target.value)}
          className="bg-zinc-900 border border-zinc-700 text-zinc-200 text-sm rounded px-2 py-1.5 w-64 placeholder-zinc-600 focus:outline-none focus:border-zinc-500"
        />
        <input
          type="text"
          placeholder="agent_type filter"
          value={agentTypeFilter}
          onChange={(e) => setAgentTypeFilter(e.target.value)}
          className="bg-zinc-900 border border-zinc-700 text-zinc-200 text-sm rounded px-2 py-1.5 w-48 placeholder-zinc-600 focus:outline-none focus:border-zinc-500"
        />
        <button
          onClick={loadEvents}
          className="text-sm text-zinc-200 bg-zinc-700 hover:bg-zinc-600 px-3 py-1.5 rounded transition-colors"
        >
          Load
        </button>
      </div>

      {/* Content */}
      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : events.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No span events loaded. Enter optional filters and click Load.
        </div>
      ) : (
        <div className="space-y-3">
          {executionIds.map((execId) => {
            const group = grouped[execId];
            const collapsed = collapsedGroups.has(execId);
            return (
              <div
                key={execId}
                className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
              >
                {/* Group header */}
                <div
                  className="flex items-center gap-3 px-3 py-2 cursor-pointer hover:bg-zinc-800/50"
                  onClick={() => toggleGroup(execId)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      toggleGroup(execId);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                >
                  <span className="text-xs text-zinc-500">{collapsed ? "+" : "-"}</span>
                  <span className="text-sm text-zinc-200 font-mono">{shortId(execId)}</span>
                  <span className="text-xs text-zinc-500">{group.length} events</span>
                  {group[0]?.agent_type && (
                    <span className="text-xs text-zinc-400">{group[0].agent_type}</span>
                  )}
                </div>

                {/* Event rows */}
                {!collapsed && (
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="text-zinc-500 text-xs border-t border-zinc-800">
                        <th className="text-left py-2 px-2">Step</th>
                        <th className="text-left py-2 px-2">Type</th>
                        <th className="text-left py-2 px-2">Agent</th>
                        <th className="text-left py-2 px-2">Details</th>
                        <th className="text-left py-2 px-2">Date</th>
                      </tr>
                    </thead>
                    <tbody>
                      {group.map((ev) => {
                        const cellKey = ev.id;
                        const isExpanded = expandedCells.has(cellKey);

                        return (
                          <tr
                            key={ev.id}
                            className="border-t border-zinc-800/50 hover:bg-zinc-800/30"
                          >
                            <td className="py-1.5 px-2 text-zinc-400 text-xs">{ev.step_index}</td>
                            <td className="py-1.5 px-2">
                              <span
                                className={`text-xs px-1.5 py-0.5 rounded ${EVENT_TYPE_COLORS[ev.event_type] || "text-zinc-400 bg-zinc-800/50"}`}
                              >
                                {ev.event_type}
                              </span>
                            </td>
                            <td className="py-1.5 px-2 text-zinc-400 text-xs">{ev.agent_type}</td>
                            <td className="py-1.5 px-2 text-xs max-w-md">
                              {ev.event_type === "reward" && (
                                <div className="flex items-center gap-2">
                                  <span className="text-zinc-400">
                                    {ev.metric_name ?? "metric"}
                                  </span>
                                  <span
                                    className={`font-mono ${
                                      ev.reward_value != null && ev.reward_value < 0.5
                                        ? "text-red-400"
                                        : "text-green-400"
                                    }`}
                                  >
                                    {ev.reward_value?.toFixed(3) ?? "n/a"}
                                  </span>
                                </div>
                              )}
                              {ev.event_type === "message" && (
                                <div
                                  className="cursor-pointer"
                                  onClick={() => toggleCell(cellKey)}
                                  onKeyDown={(e) => {
                                    if (e.key === "Enter" || e.key === " ") {
                                      e.preventDefault();
                                      toggleCell(cellKey);
                                    }
                                  }}
                                  role="button"
                                  tabIndex={0}
                                >
                                  <span className="text-cyan-400 mr-2">{ev.role ?? "?"}</span>
                                  <span className="text-zinc-300">
                                    {isExpanded
                                      ? (ev.content ?? "")
                                      : truncate(ev.content ?? "", 200)}
                                  </span>
                                </div>
                              )}
                              {ev.event_type === "object" && (
                                <div
                                  className="cursor-pointer"
                                  onClick={() => toggleCell(cellKey)}
                                  onKeyDown={(e) => {
                                    if (e.key === "Enter" || e.key === " ") {
                                      e.preventDefault();
                                      toggleCell(cellKey);
                                    }
                                  }}
                                  role="button"
                                  tabIndex={0}
                                >
                                  <span className="text-purple-400 mr-2">
                                    {ev.data_key ?? "key"}
                                  </span>
                                  <span className="text-zinc-300 font-mono">
                                    {isExpanded
                                      ? (ev.data_json ?? "")
                                      : truncate(ev.data_json ?? "", 200)}
                                  </span>
                                </div>
                              )}
                              {ev.event_type !== "reward" &&
                                ev.event_type !== "message" &&
                                ev.event_type !== "object" && (
                                  <span className="text-zinc-500">-</span>
                                )}
                            </td>
                            <td className="py-1.5 px-2 text-zinc-500 text-xs whitespace-nowrap">
                              {new Date(ev.created_at).toLocaleString()}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
