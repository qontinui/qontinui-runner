/**
 * QRoutingTab — Q-learning architecture router visualization.
 *
 * Shows:
 * - Q-table heatmap (states x architectures, colored by Q-value)
 * - Policy view (recommended architecture per state + confidence)
 * - Visit count overlay
 * - Convergence stats
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

// ─── Types matching Rust serialization ────────────────────────────────────────

interface QTableRow {
  state_key: string;
  action: string;
  q_value: number;
  visit_count: number;
}

interface PolicyEntry {
  state_key: string;
  recommended_architecture: string;
  q_value: number;
  confidence: number;
  total_visits: number;
}

interface QRoutingStats {
  total_state_action_pairs: number;
  states_with_data: number;
  routable_states: number;
  total_possible_states: number;
  total_visits: number;
  avg_q_value: number;
}

interface OverrideEntry {
  state_key: string;
  forced_action: string;
}

// ─── Constants ────────────────────────────────────────────────────────────────

const ARCHITECTURES = ["traditional", "agentic_verification", "multi_agent_pipeline"];
const ARCH_LABELS: Record<string, string> = {
  traditional: "Traditional",
  agentic_verification: "Agentic Verify",
  multi_agent_pipeline: "Multi-Agent",
};

const DOMAIN_LABELS: Record<string, string> = {
  frontend: "Frontend",
  backend: "Backend",
  database: "Database",
  testing: "Testing",
  infra: "Infra",
};

const COMPLEXITY_LABELS: Record<string, string> = {
  trivial: "Trivial",
  simple: "Simple",
  moderate: "Moderate",
  complex: "Complex",
  highly_complex: "H.Complex",
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Parse a state_key like "backend:moderate:no_ui" into parts. */
function parseStateKey(key: string): { domain: string; complexity: string; hasUi: boolean } {
  const [domain, complexity, ui] = key.split(":");
  return { domain, complexity, hasUi: ui === "ui" };
}

/** Format a state_key for display. */
function formatState(key: string): string {
  const { domain, complexity, hasUi } = parseStateKey(key);
  const d = DOMAIN_LABELS[domain] ?? domain;
  const c = COMPLEXITY_LABELS[complexity] ?? complexity;
  return `${d} / ${c}${hasUi ? " + UI" : ""}`;
}

/** Color a Q-value (0-1 range) as a background. Higher = greener. */
function qValueColor(q: number): string {
  if (q <= 0) return "#27272a"; // zinc-800
  const intensity = Math.min(q, 1);
  const g = Math.round(60 + intensity * 160);
  const r = Math.round(40 + (1 - intensity) * 40);
  return `rgb(${r}, ${g}, 60)`;
}

/** Color a visit count. Higher = more saturated blue. */
function visitColor(count: number): string {
  if (count === 0) return "#27272a";
  const intensity = Math.min(count / 30, 1); // 30 visits = full intensity
  const b = Math.round(80 + intensity * 150);
  return `rgb(30, 50, ${b})`;
}

/** Confidence as a percentage string with color class. */
function confidenceClass(confidence: number): string {
  if (confidence >= 0.9) return "text-green-400";
  if (confidence >= 0.7) return "text-blue-400";
  if (confidence >= 0.5) return "text-yellow-400";
  return "text-zinc-500";
}

// ─── Component ────────────────────────────────────────────────────────────────

type ViewMode = "heatmap" | "policy" | "visits";

export function QRoutingTab() {
  const [table, setTable] = useState<QTableRow[]>([]);
  const [policy, setPolicy] = useState<PolicyEntry[]>([]);
  const [stats, setStats] = useState<QRoutingStats | null>(null);
  const [overrides, setOverrides] = useState<OverrideEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [viewMode, setViewMode] = useState<ViewMode>("heatmap");
  const [hoveredCell, setHoveredCell] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const [t, p, s, o] = await Promise.all([
        invoke<QTableRow[]>("get_q_routing_table"),
        invoke<PolicyEntry[]>("get_q_routing_policy"),
        invoke<QRoutingStats>("get_q_routing_stats"),
        invoke<OverrideEntry[]>("get_q_routing_overrides"),
      ]);
      setTable(t);
      setPolicy(p);
      setStats(s);
      setOverrides(o);
    } catch {
      // silent fail — table may not exist yet
    } finally {
      setLoading(false);
    }
  }, []);

  const handleSetOverride = useCallback(
    async (stateKey: string, action: string) => {
      try {
        await invoke("set_q_routing_override", { stateKey, forcedAction: action });
        fetchData();
      } catch {
        // silent fail
      }
    },
    [fetchData],
  );

  const handleRemoveOverride = useCallback(
    async (stateKey: string) => {
      try {
        await invoke("remove_q_routing_override", { stateKey });
        fetchData();
      } catch {
        // silent fail
      }
    },
    [fetchData],
  );

  const handleResetTable = useCallback(async () => {
    if (!confirm("Reset all Q-routing data? Overrides will be preserved.")) return;
    try {
      await invoke("reset_q_routing_table");
      fetchData();
    } catch {
      // silent fail
    }
  }, [fetchData]);

  useEffect(() => {
    fetchData();
    const id = setInterval(fetchData, 10_000);
    return () => clearInterval(id);
  }, [fetchData]);

  if (loading) {
    return <div className="p-6 text-zinc-500 text-sm">Loading Q-routing data...</div>;
  }

  if (table.length === 0) {
    return (
      <div className="p-6">
        <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-8 text-center">
          <div className="text-zinc-400 text-sm mb-2">No Q-routing data yet</div>
          <div className="text-zinc-600 text-xs max-w-md mx-auto">
            Q-values are populated automatically as workflows complete. Run campaigns with different
            architectures to seed the Q-table, then switch to{" "}
            <code className="text-blue-400">q_learning</code> mutation strategy.
          </div>
        </div>
      </div>
    );
  }

  // Build lookup: state_key -> architecture -> QTableRow
  const lookup = new Map<string, Map<string, QTableRow>>();
  for (const row of table) {
    if (!lookup.has(row.state_key)) lookup.set(row.state_key, new Map());
    lookup.get(row.state_key)!.set(row.action, row);
  }

  // Build policy lookup: state_key -> PolicyEntry
  const policyLookup = new Map<string, PolicyEntry>();
  for (const entry of policy) {
    policyLookup.set(entry.state_key, entry);
  }

  // Build overrides lookup: state_key -> forced_action
  const overrideLookup = new Map<string, string>();
  for (const entry of overrides) {
    overrideLookup.set(entry.state_key, entry.forced_action);
  }

  // Sorted unique state keys
  const stateKeys = Array.from(lookup.keys()).sort();

  return (
    <div className="p-4 space-y-4">
      {/* Stats bar */}
      {stats && <StatsBar stats={stats} />}

      {/* View mode toggle + actions */}
      <div className="flex items-center gap-1">
        {(["heatmap", "policy", "visits"] as ViewMode[]).map((mode) => (
          <button
            key={mode}
            onClick={() => setViewMode(mode)}
            className={`px-3 py-1 text-xs rounded transition-colors ${
              viewMode === mode
                ? "bg-blue-600 text-white"
                : "bg-zinc-800 text-zinc-400 hover:text-zinc-200"
            }`}
          >
            {mode === "heatmap" ? "Q-Values" : mode === "policy" ? "Policy" : "Visit Counts"}
          </button>
        ))}
        <div className="flex-1" />
        <button
          onClick={handleResetTable}
          className="px-3 py-1 text-xs rounded bg-zinc-800 text-red-400 hover:bg-red-900/30 hover:text-red-300 transition-colors"
        >
          Reset Q-Table
        </button>
      </div>

      {/* Main content */}
      {viewMode === "heatmap" && (
        <HeatmapView
          stateKeys={stateKeys}
          lookup={lookup}
          policyLookup={policyLookup}
          overrideLookup={overrideLookup}
          hoveredCell={hoveredCell}
          setHoveredCell={setHoveredCell}
        />
      )}
      {viewMode === "policy" && (
        <PolicyView
          policy={policy}
          overrideLookup={overrideLookup}
          onSetOverride={handleSetOverride}
          onRemoveOverride={handleRemoveOverride}
        />
      )}
      {viewMode === "visits" && <VisitsView stateKeys={stateKeys} lookup={lookup} />}
    </div>
  );
}

// ─── Stats Bar ────────────────────────────────────────────────────────────────

function StatsBar({ stats }: { stats: QRoutingStats }) {
  return (
    <div className="grid grid-cols-2 sm:grid-cols-4 lg:grid-cols-6 gap-3">
      <StatCard
        label="States with Data"
        value={stats.states_with_data}
        sub={`/ ${stats.total_possible_states}`}
      />
      <StatCard
        label="Routable States"
        value={stats.routable_states}
        sub={`/ ${stats.states_with_data}`}
      />
      <StatCard label="State-Action Pairs" value={stats.total_state_action_pairs} />
      <StatCard label="Total Visits" value={stats.total_visits} />
      <StatCard label="Avg Q-Value" value={stats.avg_q_value.toFixed(3)} />
      <StatCard
        label="Coverage"
        value={`${((stats.states_with_data / stats.total_possible_states) * 100).toFixed(0)}%`}
      />
    </div>
  );
}

function StatCard({ label, value, sub }: { label: string; value: string | number; sub?: string }) {
  return (
    <div className="bg-zinc-900 border border-zinc-800 rounded-lg px-3 py-2">
      <div className="text-[10px] text-zinc-500 uppercase tracking-wider">{label}</div>
      <div className="text-lg font-mono text-zinc-200">
        {value}
        {sub && <span className="text-xs text-zinc-600 ml-1">{sub}</span>}
      </div>
    </div>
  );
}

// ─── Heatmap View ─────────────────────────────────────────────────────────────

function HeatmapView({
  stateKeys,
  lookup,
  policyLookup,
  overrideLookup,
  hoveredCell,
  setHoveredCell,
}: {
  stateKeys: string[];
  lookup: Map<string, Map<string, QTableRow>>;
  policyLookup: Map<string, PolicyEntry>;
  overrideLookup: Map<string, string>;
  hoveredCell: string | null;
  setHoveredCell: (v: string | null) => void;
}) {
  return (
    <div className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-zinc-800">
            <th className="text-left px-3 py-2 text-zinc-500 text-xs font-normal w-48">State</th>
            {ARCHITECTURES.map((arch) => (
              <th
                key={arch}
                className="text-center px-3 py-2 text-zinc-500 text-xs font-normal w-36"
              >
                {ARCH_LABELS[arch]}
              </th>
            ))}
            <th className="text-center px-3 py-2 text-zinc-500 text-xs font-normal w-24">Best</th>
          </tr>
        </thead>
        <tbody>
          {stateKeys.map((stateKey) => {
            const archMap = lookup.get(stateKey)!;
            const overrideArch = overrideLookup.get(stateKey);
            const policyEntry = policyLookup.get(stateKey);
            const bestArch = policyEntry?.recommended_architecture;

            return (
              <tr key={stateKey} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
                <td className="px-3 py-2 text-zinc-300 text-xs font-mono">
                  {formatState(stateKey)}
                </td>
                {ARCHITECTURES.map((arch) => {
                  const entry = archMap.get(arch);
                  const cellKey = `${stateKey}:${arch}`;
                  const isHovered = hoveredCell === cellKey;
                  const isBest = arch === bestArch;

                  return (
                    <td
                      key={arch}
                      className="px-1 py-1 text-center"
                      onMouseEnter={() => setHoveredCell(cellKey)}
                      onMouseLeave={() => setHoveredCell(null)}
                    >
                      <div
                        className={`rounded px-2 py-1.5 transition-all ${
                          isBest ? "ring-1 ring-blue-500/50" : ""
                        } ${isHovered ? "ring-1 ring-zinc-500" : ""}`}
                        style={{ backgroundColor: entry ? qValueColor(entry.q_value) : "#18181b" }}
                      >
                        {entry ? (
                          <div>
                            <div className="text-zinc-100 font-mono text-xs">
                              {entry.q_value.toFixed(3)}
                            </div>
                            <div className="text-zinc-400 text-[10px]">n={entry.visit_count}</div>
                          </div>
                        ) : (
                          <div className="text-zinc-700 text-xs">--</div>
                        )}
                      </div>
                    </td>
                  );
                })}
                <td className="px-3 py-2 text-center">
                  {overrideArch ? (
                    <span className="text-xs text-orange-400 font-medium" title="Manual override">
                      {ARCH_LABELS[overrideArch] ?? overrideArch} (locked)
                    </span>
                  ) : policyEntry ? (
                    <span className="text-xs text-blue-400 font-medium">
                      {ARCH_LABELS[policyEntry.recommended_architecture] ??
                        policyEntry.recommended_architecture}
                    </span>
                  ) : (
                    <span className="text-zinc-700 text-xs">--</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ─── Policy View ──────────────────────────────────────────────────────────────

function PolicyView({
  policy,
  overrideLookup,
  onSetOverride,
  onRemoveOverride,
}: {
  policy: PolicyEntry[];
  overrideLookup: Map<string, string>;
  onSetOverride: (stateKey: string, action: string) => void;
  onRemoveOverride: (stateKey: string) => void;
}) {
  const sorted = [...policy].sort((a, b) => b.total_visits - a.total_visits);

  return (
    <div className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-zinc-800">
            <th className="text-left px-3 py-2 text-zinc-500 text-xs font-normal">State</th>
            <th className="text-left px-3 py-2 text-zinc-500 text-xs font-normal">Recommended</th>
            <th className="text-right px-3 py-2 text-zinc-500 text-xs font-normal">Q-Value</th>
            <th className="text-right px-3 py-2 text-zinc-500 text-xs font-normal">Confidence</th>
            <th className="text-right px-3 py-2 text-zinc-500 text-xs font-normal">Visits</th>
            <th className="text-center px-3 py-2 text-zinc-500 text-xs font-normal">Status</th>
            <th className="text-center px-3 py-2 text-zinc-500 text-xs font-normal w-40">
              Override
            </th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((entry) => {
            const isRoutable = entry.total_visits >= 3;
            const currentOverride = overrideLookup.get(entry.state_key);

            return (
              <tr
                key={entry.state_key}
                className="border-b border-zinc-800/50 hover:bg-zinc-800/30"
              >
                <td className="px-3 py-2 text-zinc-300 text-xs font-mono">
                  {formatState(entry.state_key)}
                </td>
                <td className="px-3 py-2">
                  <span className="inline-flex items-center gap-1.5 text-xs">
                    <span
                      className="w-2 h-2 rounded-full"
                      style={{ backgroundColor: archColor(entry.recommended_architecture) }}
                    />
                    {ARCH_LABELS[entry.recommended_architecture] ?? entry.recommended_architecture}
                  </span>
                </td>
                <td className="px-3 py-2 text-right font-mono text-xs text-zinc-200">
                  {entry.q_value.toFixed(3)}
                </td>
                <td
                  className={`px-3 py-2 text-right font-mono text-xs ${confidenceClass(entry.confidence)}`}
                >
                  {(entry.confidence * 100).toFixed(0)}%
                </td>
                <td className="px-3 py-2 text-right font-mono text-xs text-zinc-400">
                  {entry.total_visits}
                </td>
                <td className="px-3 py-2 text-center">
                  {isRoutable ? (
                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-900/40 text-green-400 border border-green-800/50">
                      Active
                    </span>
                  ) : (
                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-zinc-800 text-zinc-500 border border-zinc-700">
                      Cold
                    </span>
                  )}
                </td>
                <td className="px-2 py-1 text-center">
                  {currentOverride ? (
                    <div className="inline-flex items-center gap-1">
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-orange-900/40 text-orange-400 border border-orange-800/50">
                        {ARCH_LABELS[currentOverride] ?? currentOverride}
                      </span>
                      <button
                        onClick={() => onRemoveOverride(entry.state_key)}
                        className="text-zinc-500 hover:text-red-400 text-xs px-1"
                        title="Remove override"
                      >
                        x
                      </button>
                    </div>
                  ) : (
                    <select
                      className="text-[10px] bg-zinc-800 border border-zinc-700 rounded px-1 py-0.5 text-zinc-400 cursor-pointer"
                      value=""
                      onChange={(e) => {
                        if (e.target.value) onSetOverride(entry.state_key, e.target.value);
                      }}
                    >
                      <option value="">Lock to...</option>
                      {ARCHITECTURES.map((arch) => (
                        <option key={arch} value={arch}>
                          {ARCH_LABELS[arch]}
                        </option>
                      ))}
                    </select>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {sorted.length === 0 && (
        <div className="p-6 text-center text-zinc-600 text-sm">No policy data</div>
      )}
    </div>
  );
}

/** Color for each architecture. */
function archColor(arch: string): string {
  switch (arch) {
    case "traditional":
      return "#3b82f6"; // blue
    case "agentic_verification":
      return "#22c55e"; // green
    case "multi_agent_pipeline":
      return "#a855f7"; // purple
    default:
      return "#71717a";
  }
}

// ─── Visits View ──────────────────────────────────────────────────────────────

function VisitsView({
  stateKeys,
  lookup,
}: {
  stateKeys: string[];
  lookup: Map<string, Map<string, QTableRow>>;
}) {
  return (
    <div className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-zinc-800">
            <th className="text-left px-3 py-2 text-zinc-500 text-xs font-normal w-48">State</th>
            {ARCHITECTURES.map((arch) => (
              <th
                key={arch}
                className="text-center px-3 py-2 text-zinc-500 text-xs font-normal w-36"
              >
                {ARCH_LABELS[arch]}
              </th>
            ))}
            <th className="text-right px-3 py-2 text-zinc-500 text-xs font-normal w-20">Total</th>
          </tr>
        </thead>
        <tbody>
          {stateKeys.map((stateKey) => {
            const archMap = lookup.get(stateKey)!;
            let total = 0;
            for (const entry of archMap.values()) total += entry.visit_count;

            return (
              <tr key={stateKey} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
                <td className="px-3 py-2 text-zinc-300 text-xs font-mono">
                  {formatState(stateKey)}
                </td>
                {ARCHITECTURES.map((arch) => {
                  const entry = archMap.get(arch);
                  const count = entry?.visit_count ?? 0;

                  return (
                    <td key={arch} className="px-1 py-1 text-center">
                      <div
                        className="rounded px-2 py-1.5"
                        style={{ backgroundColor: visitColor(count) }}
                      >
                        <div className="text-zinc-100 font-mono text-xs">
                          {count > 0 ? count : "--"}
                        </div>
                      </div>
                    </td>
                  );
                })}
                <td className="px-3 py-2 text-right font-mono text-xs text-zinc-400">{total}</td>
              </tr>
            );
          })}
        </tbody>
      </table>

      {/* Legend */}
      <div className="px-3 py-2 border-t border-zinc-800 flex items-center gap-4 text-[10px] text-zinc-500">
        <span>Visit density:</span>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded" style={{ backgroundColor: visitColor(0) }} />
          <span>0</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded" style={{ backgroundColor: visitColor(5) }} />
          <span>5</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded" style={{ backgroundColor: visitColor(15) }} />
          <span>15</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded" style={{ backgroundColor: visitColor(30) }} />
          <span>30+</span>
        </div>
      </div>
    </div>
  );
}
