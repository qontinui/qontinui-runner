import { useState, useEffect, useMemo, Fragment } from "react";
import { Radio, Search, Filter, ChevronDown, ChevronRight } from "lucide-react";
import { apiFetch } from "@/hooks/useApiHelpers";

// =============================================================================
// Types (matching Rust structs in workflow_event_bus.rs / mcp/inngest.rs)
// =============================================================================

interface EventSource {
  task_run_id?: string;
  workflow_id?: string;
  workflow_name?: string;
}

interface WorkflowEvent {
  name: string;
  data: unknown;
  timestamp: string;
  idempotency_key?: string;
  source: EventSource;
}

interface SubscriptionInfo {
  id: string;
  event_pattern: string;
  target_workflow_id?: string;
  once: boolean;
}

interface DurableQueueStatus {
  pending: number;
  running: number;
  max_concurrent: number;
  max_queue_depth: number;
  durable_persistence: boolean;
}

interface CircuitBreakerStatus {
  state: string;
  failure_count: number;
  failure_threshold: number;
  cooldown_ms: number;
}

interface ApiResponse<T> {
  success: boolean;
  data: T;
}

// =============================================================================
// Polling hook
// =============================================================================

function usePolledData<T>(path: string, intervalMs: number): T | null {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const res = await apiFetch<ApiResponse<T>>(path);
        if (!cancelled && res.success) setData(res.data);
      } catch {
        /* runner may be restarting */
      }
    };
    poll();
    const id = setInterval(poll, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [path, intervalMs]);
  return data;
}

// =============================================================================
// Helpers
// =============================================================================

function fmtTime(iso: string): string {
  try {
    const d = new Date(iso);
    return (
      d.toLocaleTimeString("en-US", {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      }) +
      " " +
      d.toLocaleDateString("en-US", { month: "short", day: "numeric" })
    );
  } catch {
    return iso;
  }
}

function fmtTimeFull(iso: string): string {
  try {
    return new Date(iso).toLocaleString("en-US", {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    });
  } catch {
    return iso;
  }
}

function truncJson(value: unknown, maxLen = 120): string {
  const s = typeof value === "string" ? value : (JSON.stringify(value) ?? "");
  return s.length > maxLen ? s.slice(0, maxLen) + "\u2026" : s;
}

function fmtJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? "null";
  } catch {
    return String(value);
  }
}

function srcLabel(source: EventSource): string {
  if (source.workflow_name) return source.workflow_name;
  if (source.workflow_id) return source.workflow_id;
  if (source.task_run_id) return `task:${source.task_run_id.slice(0, 8)}`;
  return "\u2014";
}

function dotColor(name: string): string {
  if (name.includes("completed") || name.includes("success")) return "bg-green-500";
  if (name.includes("failed") || name.includes("error")) return "bg-red-500";
  if (name.includes("started")) return "bg-blue-500";
  if (name.startsWith("step.")) return "bg-yellow-500";
  return "bg-zinc-500";
}

function textColor(name: string): string {
  if (name.includes("failed") || name.includes("error")) return "text-red-400";
  if (name.includes("completed") || name.includes("success")) return "text-green-400";
  return "text-zinc-200";
}

// =============================================================================
// Sub-components
// =============================================================================

function QueueWidget() {
  const queue = usePolledData<DurableQueueStatus>("/inngest/queue", 5000);
  if (!queue) return null;
  const stats = [
    { label: "Pending", value: queue.pending },
    { label: "Running", value: queue.running },
    { label: "Max", value: queue.max_concurrent },
  ];
  return (
    <div data-tutorial-id="queue-status-widget" className="flex items-center gap-2 flex-wrap">
      {stats.map((s) => (
        <div
          key={s.label}
          className="flex items-center gap-1.5 px-2 py-1 rounded border border-zinc-700 bg-zinc-800/60 text-xs"
        >
          <span className="text-zinc-400">{s.label}</span>
          <span className="font-semibold text-zinc-200 tabular-nums">{s.value}</span>
        </div>
      ))}
      <div className="flex items-center gap-1.5 px-2 py-1 rounded border border-zinc-700 bg-zinc-800/60 text-xs">
        <span
          className={`w-1.5 h-1.5 rounded-full ${queue.durable_persistence ? "bg-green-500" : "bg-zinc-500"}`}
        />
        <span className="text-zinc-400">{queue.durable_persistence ? "Durable" : "In-Memory"}</span>
      </div>
    </div>
  );
}

function CBWidget() {
  const cb = usePolledData<CircuitBreakerStatus>("/inngest/circuit-breaker", 5000);
  if (!cb) return null;
  const cfg: Record<string, { dot: string; label: string }> = {
    closed: { dot: "bg-green-500", label: "Closed" },
    open: { dot: "bg-red-500", label: "Open" },
    half_open: { dot: "bg-yellow-500", label: "Half-Open" },
  };
  const { dot, label } = cfg[cb.state] ?? { dot: "bg-zinc-500", label: cb.state };
  return (
    <div data-tutorial-id="circuit-breaker-widget" className="flex items-center gap-2 flex-wrap">
      <div className="flex items-center gap-1.5 px-2 py-1 rounded border border-zinc-700 bg-zinc-800/60 text-xs">
        <span className={`w-1.5 h-1.5 rounded-full ${dot}`} />
        <span className="text-zinc-400">Circuit</span>
        <span className="font-semibold text-zinc-200">{label}</span>
      </div>
      <div className="flex items-center gap-1.5 px-2 py-1 rounded border border-zinc-700 bg-zinc-800/60 text-xs">
        <span className="text-zinc-400">Failures</span>
        <span className="font-semibold text-zinc-200 tabular-nums">
          {cb.failure_count}/{cb.failure_threshold}
        </span>
      </div>
    </div>
  );
}

function DetailPanel({ evt }: { evt: WorkflowEvent }) {
  return (
    <tr>
      <td colSpan={4} className="px-4 py-3 bg-zinc-800/40 border-b border-zinc-700">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3 text-xs">
          <div className="space-y-1.5">
            <div>
              <span className="text-zinc-500">Full Timestamp:</span>{" "}
              <span className="text-zinc-300">{fmtTimeFull(evt.timestamp)}</span>
            </div>
            <div>
              <span className="text-zinc-500">Task Run ID:</span>{" "}
              <span className="text-zinc-300 font-mono">{evt.source.task_run_id ?? "\u2014"}</span>
            </div>
            <div>
              <span className="text-zinc-500">Workflow ID:</span>{" "}
              <span className="text-zinc-300 font-mono">{evt.source.workflow_id ?? "\u2014"}</span>
            </div>
            <div>
              <span className="text-zinc-500">Workflow Name:</span>{" "}
              <span className="text-zinc-300">{evt.source.workflow_name ?? "\u2014"}</span>
            </div>
            {evt.idempotency_key && (
              <div>
                <span className="text-zinc-500">Idempotency Key:</span>{" "}
                <span className="text-zinc-300 font-mono">{evt.idempotency_key}</span>
              </div>
            )}
          </div>
          <div>
            <div className="text-zinc-500 mb-1">Full Payload:</div>
            <pre className="p-2 rounded bg-zinc-900 border border-zinc-700 text-zinc-300 overflow-auto max-h-48 text-[11px] leading-relaxed whitespace-pre-wrap">
              {fmtJson(evt.data)}
            </pre>
          </div>
        </div>
      </td>
    </tr>
  );
}

// =============================================================================
// Main component
// =============================================================================

export function EventHistoryPage() {
  const [filter, setFilter] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const events = usePolledData<WorkflowEvent[]>("/inngest/events?limit=100", 5000);
  const subscriptions = usePolledData<SubscriptionInfo[]>("/inngest/subscriptions", 15000);

  const filtered = useMemo(() => {
    if (!events) return [];
    const list = [...events].reverse(); // newest first
    if (!filter) return list;
    const lower = filter.toLowerCase();
    return list.filter(
      (e) =>
        e.name.toLowerCase().includes(lower) || srcLabel(e.source).toLowerCase().includes(lower),
    );
  }, [events, filter]);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <header className="flex items-center justify-between px-4 py-2.5 border-b border-zinc-800 shrink-0">
        <div className="flex items-center gap-2">
          <Radio className="h-4 w-4 text-amber-500" />
          <h1 className="text-sm font-semibold text-zinc-100">Workflow Event History</h1>
        </div>
        <div className="flex items-center gap-3 text-xs text-zinc-400">
          {subscriptions && (
            <span className="flex items-center gap-1">
              <Filter className="w-3 h-3" />
              {subscriptions.length} subscription{subscriptions.length !== 1 ? "s" : ""}
            </span>
          )}
          {events && (
            <span>
              {events.length} event{events.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
      </header>

      {/* Status widgets + filter */}
      <div className="flex items-center gap-3 px-4 py-2 border-b border-zinc-800 shrink-0 flex-wrap">
        <QueueWidget />
        <CBWidget />
        <div className="relative ml-auto min-w-[200px]">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-zinc-500" />
          <input
            type="text"
            aria-label="Filter by event name or source"
            placeholder="Filter by event name or source..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-full pl-7 pr-2 py-1 text-xs rounded border border-zinc-700 bg-zinc-800 text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
          />
        </div>
      </div>

      {/* Content */}
      <main className="flex-1 overflow-y-auto">
        {!events && (
          <div className="flex items-center justify-center py-12 text-zinc-500 text-xs">
            Loading event history...
          </div>
        )}

        {events && filtered.length === 0 && (
          <div className="flex flex-col items-center justify-center py-12 text-zinc-500">
            <Radio className="w-10 h-10 mb-2 opacity-30" />
            <p className="text-xs">
              {filter ? "No events match the filter." : "No events recorded yet."}
            </p>
          </div>
        )}

        {filtered.length > 0 && (
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-zinc-900 border-b border-zinc-800">
              <tr className="text-left text-zinc-500">
                <th className="px-4 py-1.5 font-medium w-36">Timestamp</th>
                <th className="px-4 py-1.5 font-medium">Event Name</th>
                <th className="px-4 py-1.5 font-medium w-40">Source</th>
                <th className="px-4 py-1.5 font-medium">Data Preview</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/60">
              {filtered.map((evt, idx) => {
                const key = evt.idempotency_key ?? `${evt.timestamp}-${idx}`;
                const isExpanded = expandedId === key;
                return (
                  <Fragment key={key}>
                    <tr
                      className="hover:bg-zinc-800/40 cursor-pointer transition-colors"
                      onClick={() => setExpandedId(isExpanded ? null : key)}
                    >
                      <td className="px-4 py-1.5 text-zinc-500 whitespace-nowrap font-mono">
                        <span className="flex items-center gap-1.5">
                          {isExpanded ? (
                            <ChevronDown className="w-3 h-3" />
                          ) : (
                            <ChevronRight className="w-3 h-3" />
                          )}
                          {fmtTime(evt.timestamp)}
                        </span>
                      </td>
                      <td className="px-4 py-1.5">
                        <span className="flex items-center gap-1.5">
                          <span
                            className={`w-1.5 h-1.5 rounded-full shrink-0 ${dotColor(evt.name)}`}
                          />
                          <span className={`font-medium ${textColor(evt.name)}`}>{evt.name}</span>
                        </span>
                      </td>
                      <td className="px-4 py-1.5 text-zinc-400 whitespace-nowrap">
                        {srcLabel(evt.source)}
                      </td>
                      <td className="px-4 py-1.5 text-zinc-500 font-mono max-w-md truncate">
                        {truncJson(evt.data)}
                      </td>
                    </tr>
                    {isExpanded && <DetailPanel evt={evt} />}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        )}

        {/* Subscriptions */}
        {subscriptions && subscriptions.length > 0 && (
          <div className="mx-4 my-4">
            <h2 className="text-xs font-medium text-zinc-500 mb-1.5">Active Subscriptions</h2>
            <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-1.5">
              {subscriptions.map((sub) => (
                <div
                  key={sub.id}
                  className="flex items-center gap-1.5 px-2 py-1.5 rounded border border-zinc-700 bg-zinc-800/60 text-xs"
                >
                  <span className="w-1.5 h-1.5 rounded-full bg-green-500 shrink-0" />
                  <span className="font-mono text-zinc-300 truncate">{sub.event_pattern}</span>
                  {sub.once && <span className="ml-auto text-zinc-500">once</span>}
                </div>
              ))}
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
