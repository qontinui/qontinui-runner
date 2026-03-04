import {
  RefreshCw,
  Trash2,
  CheckCircle2,
  AlertTriangle,
  XCircle,
} from "lucide-react";
import { useState, useEffect, useCallback } from "react";
import type { TrackedIntegration, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";

export function HealthMonitorPanel() {
  const [integrations, setIntegrations] = useState<TrackedIntegration[]>([]);
  const [loading, setLoading] = useState(true);

  const fetchStatus = useCallback(async () => {
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/status`);
      const data: ApiResponse<TrackedIntegration[]> = await resp.json();
      if (data.success && data.data) {
        setIntegrations(data.data);
      }
    } catch {
      // Silently ignore
    } finally {
      setLoading(false);
    }
  }, []);

  const runHealthCheck = useCallback(async () => {
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/health-check`, {
        method: "POST",
      });
      const data: ApiResponse<TrackedIntegration[]> = await resp.json();
      if (data.success && data.data) {
        setIntegrations(data.data);
      }
    } catch {
      // Fallback to status
      fetchStatus();
    }
  }, [fetchStatus]);

  useEffect(() => {
    // Initial load: just fetch status (fast)
    fetchStatus();
    // Then run active health checks periodically
    const interval = setInterval(runHealthCheck, 15000);
    return () => clearInterval(interval);
  }, [fetchStatus, runHealthCheck]);

  const deleteIntegration = useCallback(
    async (id: string) => {
      try {
        await fetch(`${getApiBase()}/ui-bridge/integration/${encodeURIComponent(id)}`, {
          method: "DELETE",
        });
        fetchStatus();
      } catch (err) {
        console.error("Failed to delete:", err);
      }
    },
    [fetchStatus]
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-muted-foreground text-sm">
        <RefreshCw className="w-4 h-4 animate-spin mr-2" />
        Loading integrations...
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium">Health Monitor</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Track the health and status of all UI Bridge integrations
          </p>
        </div>
        <button
          onClick={runHealthCheck}
          className="text-muted-foreground hover:text-foreground transition-colors"
          title="Run health checks"
        >
          <RefreshCw className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Summary cards */}
      {integrations.length > 0 && (
        <div className="grid grid-cols-3 gap-3">
          <SummaryCard
            label="Active"
            count={integrations.filter((i) => i.status === "active").length}
            color="green"
          />
          <SummaryCard
            label="Disconnected"
            count={
              integrations.filter((i) => i.status === "disconnected").length
            }
            color="red"
          />
          <SummaryCard
            label="Outdated"
            count={integrations.filter((i) => i.status === "outdated").length}
            color="yellow"
          />
        </div>
      )}

      {/* Table */}
      {integrations.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground text-sm">
          No integrations tracked yet. Use Discovery, Runtime Injection, or
          Source Integration to add one.
        </div>
      ) : (
        <div className="border border-border rounded-lg overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="bg-card/80 border-b border-border">
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Project
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Type
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Framework
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  SDK
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Status
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Elements
                </th>
                <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                  Last Check
                </th>
                <th className="text-right px-3 py-2 text-xs font-medium text-muted-foreground">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {integrations.map((integration) => (
                <tr
                  key={integration.id}
                  className="border-b border-border/50 last:border-0"
                >
                  <td className="px-3 py-2">
                    <div className="max-w-[200px] truncate text-foreground">
                      {integration.label ||
                        integration.project_path ||
                        integration.target_url ||
                        integration.id}
                    </div>
                  </td>
                  <td className="px-3 py-2">
                    <TypeBadge type={integration.integration_type} />
                  </td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {integration.framework || "-"}
                  </td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {integration.sdk_version || "-"}
                  </td>
                  <td className="px-3 py-2">
                    <HealthBadge status={integration.status} />
                  </td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {integration.element_count ?? "-"}
                  </td>
                  <td className="px-3 py-2 text-muted-foreground text-xs">
                    {integration.last_health_check
                      ? formatTimestamp(integration.last_health_check)
                      : "-"}
                  </td>
                  <td className="px-3 py-2">
                    <div className="flex items-center justify-end">
                      <button
                        onClick={() => deleteIntegration(integration.id)}
                        className="p-1 rounded text-muted-foreground hover:text-red-400 hover:bg-red-500/10 transition-colors"
                        title="Remove integration"
                      >
                        <Trash2 className="w-3.5 h-3.5" />
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function SummaryCard({
  label,
  count,
  color,
}: {
  label: string;
  count: number;
  color: "green" | "red" | "yellow";
}) {
  const colorClasses = {
    green: "text-green-400 bg-green-500/5 border-green-500/20",
    red: "text-red-400 bg-red-500/5 border-red-500/20",
    yellow: "text-yellow-400 bg-yellow-500/5 border-yellow-500/20",
  };
  return (
    <div
      className={`px-3 py-2 rounded-lg border text-center ${colorClasses[color]}`}
    >
      <div className="text-lg font-bold">{count}</div>
      <div className="text-[10px] font-medium opacity-80">{label}</div>
    </div>
  );
}

function HealthBadge({ status }: { status: string }) {
  const config: Record<
    string,
    { icon: typeof CheckCircle2; bg: string; text: string }
  > = {
    active: {
      icon: CheckCircle2,
      bg: "bg-green-500/10",
      text: "text-green-400",
    },
    disconnected: {
      icon: XCircle,
      bg: "bg-red-500/10",
      text: "text-red-400",
    },
    outdated: {
      icon: AlertTriangle,
      bg: "bg-yellow-500/10",
      text: "text-yellow-400",
    },
  };
  const c = config[status] || config.active;
  const Icon = c.icon;
  return (
    <span
      className={`inline-flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded-full font-medium ${c.bg} ${c.text}`}
    >
      <Icon className="w-3 h-3" />
      {status}
    </span>
  );
}

function TypeBadge({ type }: { type: string }) {
  const config: Record<string, { bg: string; text: string }> = {
    runtime: { bg: "bg-cyan-500/10", text: "text-cyan-400" },
    source: { bg: "bg-purple-500/10", text: "text-purple-400" },
    both: { bg: "bg-blue-500/10", text: "text-blue-400" },
  };
  const c = config[type] || config.runtime;
  return (
    <span
      className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${c.bg} ${c.text}`}
    >
      {type}
    </span>
  );
}

function formatTimestamp(ts: number): string {
  // Handle both seconds and milliseconds
  const ms = ts > 1e12 ? ts : ts * 1000;
  const d = new Date(ms);
  const now = Date.now();
  const diff = now - ms;

  if (diff < 60000) return "just now";
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
  if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
  return d.toLocaleDateString();
}
