import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getApiBase } from "@/lib/runner-api";
import { tracedFetch } from "@/lib/traced-fetch";

// =============================================================================
// Types
// =============================================================================

interface PolicyEntry {
  context_key: string;
  recommended_arm: string;
  q_value: number;
  total_visits: number;
}

interface ModelRoutingState {
  q_table_size: number;
  policy: PolicyEntry[];
  available_models: string[];
}

interface RoutingTableEntry {
  context_key: string;
  model_id: string;
  q_value: number;
  visit_count: number;
  sum_of_squares: number;
}

interface DriftSignal {
  id: string;
  detector_type: string;
  metric_name: string;
  context_key: string;
  drift_level: string;
  pre_drift_mean: number;
  post_drift_mean: number;
}

interface DriftMonitorState {
  detector_count: number;
  active_detectors: string[];
}

interface StepScorecard {
  step_type: string;
  avg_normalized_credit: number;
  total_count: number;
  avg_raw_credit: number;
}

interface Strategy {
  id: string;
  name: string;
  description: string;
  status: string;
  applicability: string;
  components: string;
  stats: string;
  provenance: string;
}

interface ExperienceSummary {
  id: string;
  task_run_id: string;
  domain: string;
  complexity_tier: string;
  outcome: string;
  key_decisions: string;
  failure_points: string;
  effective_patterns: string;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// =============================================================================
// API helpers
// =============================================================================

async function fetchOL<T>(endpoint: string): Promise<T | null> {
  try {
    const resp = await tracedFetch(
      `${getApiBase()}/online-learning/${endpoint}`
    );
    if (!resp.ok) return null;
    const json: ApiResponse<T> = await resp.json();
    return json.success ? (json.data ?? null) : null;
  } catch {
    return null;
  }
}

// =============================================================================
// Sub-tab type
// =============================================================================

type SubTab =
  | "overview"
  | "model-routing"
  | "drift"
  | "credits"
  | "strategies"
  | "experiences";

// =============================================================================
// Main component
// =============================================================================

export function OnlineLearningDashboard() {
  const [activeTab, setActiveTab] = useState<SubTab>("overview");
  const [loading, setLoading] = useState(true);

  // Data
  const [routingState, setRoutingState] = useState<ModelRoutingState | null>(
    null
  );
  const [routingTable, setRoutingTable] = useState<RoutingTableEntry[]>([]);
  const [driftSignals, setDriftSignals] = useState<DriftSignal[]>([]);
  const [driftMonitor, setDriftMonitor] = useState<DriftMonitorState | null>(
    null
  );
  const [scorecard, setScorecard] = useState<StepScorecard[]>([]);
  const [strategies, setStrategies] = useState<Strategy[]>([]);
  const [experiences, setExperiences] = useState<ExperienceSummary[]>([]);

  const loadData = useCallback(async () => {
    setLoading(true);
    const [rs, rt, ds, dm, sc, st, ex] = await Promise.all([
      fetchOL<ModelRoutingState>("model-routing"),
      fetchOL<RoutingTableEntry[]>("model-routing/table"),
      fetchOL<DriftSignal[]>("drift-signals"),
      fetchOL<DriftMonitorState>("drift-monitor"),
      fetchOL<StepScorecard[]>("step-scorecard?limit=20"),
      fetchOL<Strategy[]>("strategies"),
      fetchOL<ExperienceSummary[]>("experiences?limit=20"),
    ]);
    setRoutingState(rs);
    setRoutingTable(rt ?? []);
    setDriftSignals(ds ?? []);
    setDriftMonitor(dm);
    setScorecard(sc ?? []);
    setStrategies(st ?? []);
    setExperiences(ex ?? []);
    setLoading(false);
  }, []);

  useEffect(() => {
    loadData();
    let unlisten: UnlistenFn | null = null;
    listen("drift-detected", () => loadData()).then((fn) => {
      unlisten = fn;
    });
    const interval = setInterval(loadData, 60_000);
    return () => {
      if (unlisten) unlisten();
      clearInterval(interval);
    };
  }, [loadData]);

  // ───────────────────────────────────────────────────────────────────────
  // Tab bar
  // ───────────────────────────────────────────────────────────────────────
  const tabs: { id: SubTab; label: string }[] = [
    { id: "overview", label: "Overview" },
    { id: "model-routing", label: "Model Routing" },
    { id: "drift", label: "Drift Detection" },
    { id: "credits", label: "Step Credits" },
    { id: "strategies", label: "Strategies" },
    { id: "experiences", label: "Experiences" },
  ];

  return (
    <div className="h-full flex flex-col overflow-hidden bg-[#0a0a0f]">
      {/* Header */}
      <div className="flex items-center justify-between px-5 py-3 border-b border-white/10">
        <div className="flex items-center gap-3">
          <span className="text-lg font-semibold text-white/90">
            Online Learning
          </span>
          {loading && (
            <span className="text-xs text-white/40 animate-pulse">
              Loading...
            </span>
          )}
        </div>
        <button
          onClick={loadData}
          className="text-xs px-3 py-1.5 rounded bg-white/5 hover:bg-white/10 text-white/60 hover:text-white/80 transition-colors"
        >
          Refresh
        </button>
      </div>

      {/* Sub-tab bar */}
      <div className="flex gap-1 px-5 py-2 border-b border-white/10">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`px-3 py-1.5 text-xs rounded transition-colors ${
              activeTab === tab.id
                ? "bg-blue-500/20 text-blue-400"
                : "text-white/50 hover:text-white/70 hover:bg-white/5"
            }`}
          >
            {tab.label}
            {tab.id === "drift" && driftSignals.length > 0 && (
              <span className="ml-1.5 px-1.5 py-0.5 rounded-full bg-amber-500/20 text-amber-400 text-[10px]">
                {driftSignals.length}
              </span>
            )}
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-5">
        {activeTab === "overview" && (
          <OverviewTab
            routingState={routingState}
            driftSignals={driftSignals}
            driftMonitor={driftMonitor}
            scorecard={scorecard}
            strategies={strategies}
          />
        )}
        {activeTab === "model-routing" && (
          <ModelRoutingTab
            routingState={routingState}
            routingTable={routingTable}
          />
        )}
        {activeTab === "drift" && (
          <DriftTab
            driftSignals={driftSignals}
            driftMonitor={driftMonitor}
          />
        )}
        {activeTab === "credits" && <CreditsTab scorecard={scorecard} />}
        {activeTab === "strategies" && (
          <StrategiesTab strategies={strategies} />
        )}
        {activeTab === "experiences" && (
          <ExperiencesTab experiences={experiences} />
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Overview Tab
// =============================================================================

function OverviewTab({
  routingState,
  driftSignals,
  driftMonitor,
  scorecard,
  strategies,
}: {
  routingState: ModelRoutingState | null;
  driftSignals: DriftSignal[];
  driftMonitor: DriftMonitorState | null;
  scorecard: StepScorecard[];
  strategies: Strategy[];
}) {
  return (
    <div className="space-y-4">
      {/* Stat cards */}
      <div className="grid grid-cols-4 gap-3">
        <StatCard
          label="Routing Entries"
          value={routingState?.q_table_size ?? 0}
          sub={`${routingState?.available_models.length ?? 0} models`}
        />
        <StatCard
          label="Drift Signals"
          value={driftSignals.length}
          sub={`${driftMonitor?.detector_count ?? 0} detectors`}
          alert={driftSignals.length > 0}
        />
        <StatCard
          label="Step Types Scored"
          value={scorecard.length}
          sub="credit attribution"
        />
        <StatCard
          label="Strategies"
          value={strategies.length}
          sub={`${strategies.filter((s) => s.status === "active").length} active`}
        />
      </div>

      {/* Drift alerts */}
      {driftSignals.length > 0 && (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 p-4">
          <h3 className="text-sm font-medium text-amber-400 mb-2">
            Active Drift Signals
          </h3>
          <div className="space-y-1">
            {driftSignals.slice(0, 5).map((s, i) => (
              <div
                key={i}
                className="text-xs text-white/60 flex items-center gap-2"
              >
                <span
                  className={`w-2 h-2 rounded-full ${
                    s.drift_level === "drift"
                      ? "bg-red-400"
                      : "bg-amber-400"
                  }`}
                />
                <span className="text-white/80">{s.metric_name}</span>
                <span className="text-white/40">{s.context_key}</span>
                <span className="text-white/50">
                  {s.pre_drift_mean.toFixed(3)} &rarr;{" "}
                  {s.post_drift_mean.toFixed(3)}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Top policy entries */}
      {routingState && routingState.policy.length > 0 && (
        <div className="rounded-lg border border-white/10 bg-white/[0.02] p-4">
          <h3 className="text-sm font-medium text-white/80 mb-3">
            Model Routing Policy (Top Contexts)
          </h3>
          <div className="grid grid-cols-2 gap-2">
            {routingState.policy.slice(0, 8).map((p, i) => (
              <div
                key={i}
                className="flex items-center justify-between bg-white/[0.03] rounded px-3 py-2"
              >
                <span className="text-xs text-white/60 truncate max-w-[60%]">
                  {p.context_key}
                </span>
                <div className="flex items-center gap-2">
                  <span className="text-xs font-mono text-blue-400">
                    {p.recommended_arm.split("-").slice(1, 2).join("")}
                  </span>
                  <span className="text-[10px] text-white/30">
                    q={p.q_value.toFixed(3)}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Step credit scorecard preview */}
      {scorecard.length > 0 && (
        <div className="rounded-lg border border-white/10 bg-white/[0.02] p-4">
          <h3 className="text-sm font-medium text-white/80 mb-3">
            Step Type Credit Scorecard
          </h3>
          <div className="space-y-1.5">
            {scorecard.slice(0, 6).map((s, i) => (
              <div key={i} className="flex items-center gap-3">
                <span className="text-xs text-white/60 w-32 truncate">
                  {s.step_type}
                </span>
                <div className="flex-1 h-2 bg-white/5 rounded-full overflow-hidden">
                  <div
                    className="h-full bg-blue-500/60 rounded-full"
                    style={{
                      width: `${Math.min(100, s.avg_normalized_credit * 500)}%`,
                    }}
                  />
                </div>
                <span className="text-[10px] text-white/40 w-16 text-right">
                  {s.avg_normalized_credit.toFixed(4)} ({s.total_count})
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Model Routing Tab
// =============================================================================

function ModelRoutingTab({
  routingState,
  routingTable,
}: {
  routingState: ModelRoutingState | null;
  routingTable: RoutingTableEntry[];
}) {
  return (
    <div className="space-y-4">
      {/* Models */}
      {routingState && (
        <div className="flex gap-2 mb-3">
          {routingState.available_models.map((m) => (
            <span
              key={m}
              className="px-2 py-1 text-xs rounded bg-white/5 text-white/60"
            >
              {m}
            </span>
          ))}
        </div>
      )}

      {/* Q-table */}
      {routingTable.length > 0 ? (
        <div className="rounded-lg border border-white/10 overflow-hidden">
          <table className="w-full text-xs">
            <thead>
              <tr className="bg-white/5 text-white/50">
                <th className="text-left px-3 py-2">Context</th>
                <th className="text-left px-3 py-2">Model</th>
                <th className="text-right px-3 py-2">Q-Value</th>
                <th className="text-right px-3 py-2">Visits</th>
              </tr>
            </thead>
            <tbody>
              {routingTable
                .sort((a, b) => b.q_value - a.q_value)
                .map((row, i) => (
                  <tr
                    key={i}
                    className="border-t border-white/5 hover:bg-white/[0.02]"
                  >
                    <td className="px-3 py-1.5 text-white/60 font-mono">
                      {row.context_key}
                    </td>
                    <td className="px-3 py-1.5 text-blue-400">
                      {row.model_id.replace("claude-", "").split("-20")[0]}
                    </td>
                    <td className="px-3 py-1.5 text-right text-white/70">
                      {row.q_value.toFixed(4)}
                    </td>
                    <td className="px-3 py-1.5 text-right text-white/40">
                      {row.visit_count}
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </div>
      ) : (
        <EmptyState message="No model routing data yet. The bandit learns from workflow outcomes." />
      )}
    </div>
  );
}

// =============================================================================
// Drift Detection Tab
// =============================================================================

function DriftTab({
  driftSignals,
  driftMonitor,
}: {
  driftSignals: DriftSignal[];
  driftMonitor: DriftMonitorState | null;
}) {
  return (
    <div className="space-y-4">
      {/* Monitor status */}
      {driftMonitor && (
        <div className="flex items-center gap-4 text-xs text-white/50">
          <span>
            {driftMonitor.detector_count} active detector
            {driftMonitor.detector_count !== 1 ? "s" : ""}
          </span>
          <span className="text-white/20">|</span>
          <span>ADWIN + DDM + Page-Hinkley</span>
        </div>
      )}

      {/* Signals */}
      {driftSignals.length > 0 ? (
        <div className="rounded-lg border border-white/10 overflow-hidden">
          <table className="w-full text-xs">
            <thead>
              <tr className="bg-white/5 text-white/50">
                <th className="text-left px-3 py-2">Level</th>
                <th className="text-left px-3 py-2">Metric</th>
                <th className="text-left px-3 py-2">Context</th>
                <th className="text-left px-3 py-2">Detector</th>
                <th className="text-right px-3 py-2">Pre-Drift</th>
                <th className="text-right px-3 py-2">Post-Drift</th>
              </tr>
            </thead>
            <tbody>
              {driftSignals.map((s, i) => (
                <tr
                  key={i}
                  className="border-t border-white/5 hover:bg-white/[0.02]"
                >
                  <td className="px-3 py-1.5">
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] ${
                        s.drift_level === "drift"
                          ? "bg-red-500/20 text-red-400"
                          : "bg-amber-500/20 text-amber-400"
                      }`}
                    >
                      {s.drift_level}
                    </span>
                  </td>
                  <td className="px-3 py-1.5 text-white/70">
                    {s.metric_name}
                  </td>
                  <td className="px-3 py-1.5 text-white/50 font-mono">
                    {s.context_key}
                  </td>
                  <td className="px-3 py-1.5 text-white/40">
                    {s.detector_type}
                  </td>
                  <td className="px-3 py-1.5 text-right text-white/60">
                    {s.pre_drift_mean.toFixed(4)}
                  </td>
                  <td className="px-3 py-1.5 text-right text-white/60">
                    {s.post_drift_mean.toFixed(4)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <EmptyState message="No drift signals. The system is monitoring composite score, success rate, cost, and duration per context." />
      )}
    </div>
  );
}

// =============================================================================
// Step Credits Tab
// =============================================================================

function CreditsTab({ scorecard }: { scorecard: StepScorecard[] }) {
  return (
    <div className="space-y-4">
      {scorecard.length > 0 ? (
        <div className="rounded-lg border border-white/10 overflow-hidden">
          <table className="w-full text-xs">
            <thead>
              <tr className="bg-white/5 text-white/50">
                <th className="text-left px-3 py-2">Step Type</th>
                <th className="text-right px-3 py-2">Avg Credit</th>
                <th className="text-right px-3 py-2">Raw Credit</th>
                <th className="text-right px-3 py-2">Count</th>
                <th className="text-left px-3 py-2">Bar</th>
              </tr>
            </thead>
            <tbody>
              {scorecard.map((s, i) => (
                <tr
                  key={i}
                  className="border-t border-white/5 hover:bg-white/[0.02]"
                >
                  <td className="px-3 py-1.5 text-white/70">{s.step_type}</td>
                  <td className="px-3 py-1.5 text-right text-blue-400 font-mono">
                    {s.avg_normalized_credit.toFixed(4)}
                  </td>
                  <td className="px-3 py-1.5 text-right text-white/50 font-mono">
                    {s.avg_raw_credit.toFixed(4)}
                  </td>
                  <td className="px-3 py-1.5 text-right text-white/40">
                    {s.total_count}
                  </td>
                  <td className="px-3 py-1.5 w-32">
                    <div className="h-2 bg-white/5 rounded-full overflow-hidden">
                      <div
                        className="h-full bg-emerald-500/60 rounded-full"
                        style={{
                          width: `${Math.min(100, s.avg_normalized_credit * 500)}%`,
                        }}
                      />
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <EmptyState message="No step credits yet. Credits are computed from pipeline traces after each workflow run." />
      )}
    </div>
  );
}

// =============================================================================
// Strategies Tab
// =============================================================================

function StrategiesTab({ strategies }: { strategies: Strategy[] }) {
  return (
    <div className="space-y-3">
      {strategies.length > 0 ? (
        strategies.map((s) => {
          const stats = tryParseStats(s.stats);
          return (
            <div
              key={s.id}
              className="rounded-lg border border-white/10 bg-white/[0.02] p-4"
            >
              <div className="flex items-center justify-between mb-2">
                <span className="text-sm font-medium text-white/80">
                  {s.name}
                </span>
                <span
                  className={`px-2 py-0.5 rounded text-[10px] ${
                    s.status === "active"
                      ? "bg-emerald-500/20 text-emerald-400"
                      : s.status === "candidate"
                        ? "bg-blue-500/20 text-blue-400"
                        : "bg-white/10 text-white/40"
                  }`}
                >
                  {s.status}
                </span>
              </div>
              <p className="text-xs text-white/50 mb-2">{s.description}</p>
              {stats && (
                <div className="flex gap-4 text-[10px] text-white/40">
                  <span>
                    Selected: {stats.times_selected ?? 0}x
                  </span>
                  <span>
                    Success:{" "}
                    {stats.times_selected
                      ? (
                          ((stats.times_succeeded ?? 0) /
                            stats.times_selected) *
                          100
                        ).toFixed(0)
                      : 0}
                    %
                  </span>
                  <span>
                    Avg Score: {(stats.avg_composite_score ?? 0).toFixed(3)}
                  </span>
                  <span>
                    Avg Cost: ${(stats.avg_cost_usd ?? 0).toFixed(2)}
                  </span>
                </div>
              )}
            </div>
          );
        })
      ) : (
        <EmptyState message="No strategies yet. Strategies are extracted from P90+ performing runs and validated through canary testing." />
      )}
    </div>
  );
}

// =============================================================================
// Experiences Tab
// =============================================================================

function ExperiencesTab({
  experiences,
}: {
  experiences: ExperienceSummary[];
}) {
  return (
    <div className="space-y-3">
      {experiences.length > 0 ? (
        experiences.map((e) => {
          const decisions = tryParseArray(e.key_decisions);
          const failures = tryParseArray(e.failure_points);
          const patterns = tryParseArray(e.effective_patterns);
          return (
            <div
              key={e.id}
              className="rounded-lg border border-white/10 bg-white/[0.02] p-4"
            >
              <div className="flex items-center gap-3 mb-2">
                <span
                  className={`px-1.5 py-0.5 rounded text-[10px] ${
                    e.outcome === "success" || e.outcome === "complete" || e.outcome === "completed"
                      ? "bg-emerald-500/20 text-emerald-400"
                      : "bg-red-500/20 text-red-400"
                  }`}
                >
                  {e.outcome}
                </span>
                <span className="text-xs text-white/60">{e.domain}</span>
                <span className="text-xs text-white/40">
                  {e.complexity_tier}
                </span>
                <span className="text-[10px] text-white/30 font-mono ml-auto">
                  {e.task_run_id.slice(0, 12)}...
                </span>
              </div>
              <div className="flex gap-4 text-[10px] text-white/40">
                <span>{decisions?.length ?? 0} decisions</span>
                <span>{failures?.length ?? 0} failures</span>
                <span>{patterns?.length ?? 0} patterns</span>
              </div>
            </div>
          );
        })
      ) : (
        <EmptyState message="No experience summaries yet. Experiences are extracted after each workflow run." />
      )}
    </div>
  );
}

// =============================================================================
// Shared components
// =============================================================================

function StatCard({
  label,
  value,
  sub,
  alert,
}: {
  label: string;
  value: number;
  sub: string;
  alert?: boolean;
}) {
  return (
    <div
      className={`rounded-lg border p-4 ${
        alert
          ? "border-amber-500/30 bg-amber-500/5"
          : "border-white/10 bg-white/[0.02]"
      }`}
    >
      <div className="text-2xl font-semibold text-white/90">{value}</div>
      <div className="text-xs text-white/60 mt-1">{label}</div>
      <div className="text-[10px] text-white/30 mt-0.5">{sub}</div>
    </div>
  );
}

function EmptyState({ message }: { message: string }) {
  return (
    <div className="flex items-center justify-center h-48 text-sm text-white/30">
      {message}
    </div>
  );
}

interface ParsedStats {
  times_selected?: number;
  times_succeeded?: number;
  avg_composite_score?: number;
  avg_cost_usd?: number;
}

function tryParseStats(json: string): ParsedStats | null {
  try {
    return JSON.parse(json) as ParsedStats;
  } catch {
    return null;
  }
}

function tryParseArray(json: string): unknown[] {
  try {
    const parsed = JSON.parse(json);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export default OnlineLearningDashboard;
