/**
 * FleetHealthPanel — Row 9 Phase 4 (fleet health aggregation + alerting).
 *
 * Plan: `D:/qontinui-root/plans/2026-05-14-failure-modes-at-scale-design.md`
 * §3.6. coord's health watcher polls every registered machine's
 * `/health` every 30s, drives the healthy→degraded→partitioned state
 * machine, publishes the latest snapshot per machine to the
 * `fleet-health` JetStream KV bucket, and writes §3.6 threshold
 * breaches to `coord.alerts`.
 *
 * The browser can't watch a NATS KV bucket, so this panel polls coord's
 * HTTP rollup (`/coord/fleet/health` + `/coord/alerts`) via the
 * `get_fleet_health` Tauri proxy. coord republishes each poll over the
 * Redis/JS `/ws` bridge too, so a 1 Hz panel poll renders fleet state
 * well within the "<1s after KV update" target without a browser NATS
 * client.
 *
 * Lives at the bottom of `CoordinatorDashboard` alongside
 * `FileActivityPanel`, outside the productivity-stack spec-locked
 * five-panel block (the lock asserts only `exists` per panel, not
 * relative order — appending is safe; the spec narrative gets a
 * parallel update in the same commit).
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, AlertOctagon, AlertTriangle, Info, RefreshCw, Server } from "lucide-react";
import { createLogger } from "@/lib/logger";
import {
  getFleetHealth,
  type FleetAlert,
  type FleetHealth,
  type FleetMachineSnapshot,
} from "./coordinatorApi";

const logger = createLogger("FleetHealthPanel");

/** Poll cadence. coord's watcher tick is 30s but it republishes the KV
 *  snapshot every tick; a 1 Hz panel poll keeps render latency under
 *  the §3.6 "<1s after KV update" target on the live `/ws` path. */
const POLL_MS = 1000;

const STATE_STYLE: Record<FleetMachineSnapshot["state"], string> = {
  healthy: "border-emerald-500/30 bg-emerald-500/5 text-emerald-400",
  degraded: "border-amber-500/30 bg-amber-500/5 text-amber-400",
  partitioned: "border-red-500/30 bg-red-500/10 text-red-400",
  abandoned: "border-zinc-500/30 bg-zinc-500/10 text-zinc-400",
};

const SEVERITY_STYLE: Record<FleetAlert["severity"], string> = {
  critical: "border-red-500/40 bg-red-500/10 text-red-400",
  warning: "border-amber-500/30 bg-amber-500/5 text-amber-400",
  info: "border-sky-500/30 bg-sky-500/5 text-sky-300",
};

function SeverityIcon({ severity }: { severity: FleetAlert["severity"] }) {
  if (severity === "critical") return <AlertOctagon className="w-3.5 h-3.5 shrink-0" />;
  if (severity === "warning") return <AlertTriangle className="w-3.5 h-3.5 shrink-0" />;
  return <Info className="w-3.5 h-3.5 shrink-0" />;
}

function ago(ts: string | null): string {
  if (!ts) return "never";
  const ms = Date.now() - new Date(ts).getTime();
  if (Number.isNaN(ms)) return "—";
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  return `${Math.round(s / 3600)}h ago`;
}

export function FleetHealthPanel() {
  const [data, setData] = useState<FleetHealth | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  // Avoid overlapping polls if coord is slow.
  const inFlight = useRef(false);

  const load = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setLoading(true);
    try {
      const fresh = await getFleetHealth();
      setData(fresh);
      setError(null);
    } catch (e) {
      // coord unreachable ≠ runner broken — keep the last good frame
      // and surface a retriable banner.
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      logger.debug("getFleetHealth failed", { msg });
    } finally {
      inFlight.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => void load(), POLL_MS);
    return () => window.clearInterval(id);
  }, [load]);

  const machines = data?.health.machines ?? [];
  const alerts = data?.alerts ?? [];
  const rollup = data?.health.alerts ?? { critical: 0, warning: 0, info: 0 };

  return (
    <section
      role="region"
      aria-labelledby="productivity-fleet-health-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.fleet-health"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Server className="w-4 h-4 text-sky-400" />
          <h2
            id="productivity-fleet-health-heading"
            className="text-sm font-semibold text-foreground"
          >
            Fleet Health
          </h2>
          <span className="text-xs text-muted-foreground">
            {machines.length} machine{machines.length === 1 ? "" : "s"}
          </span>
          {rollup.critical > 0 && (
            <span className="rounded px-1.5 py-0.5 text-[10px] font-semibold border border-red-500/40 bg-red-500/10 text-red-400">
              {rollup.critical} critical
            </span>
          )}
          {rollup.warning > 0 && (
            <span className="rounded px-1.5 py-0.5 text-[10px] font-semibold border border-amber-500/30 bg-amber-500/5 text-amber-400">
              {rollup.warning} warning
            </span>
          )}
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading}
          data-ui-bridge-id="productivity.fleet-health-refresh"
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </header>

      {error && (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-2 text-xs text-red-400">
          coord unreachable — showing last known state. ({error})
        </div>
      )}

      {/* Per-machine state grid */}
      {machines.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No machines registered with a pollable /health URL yet.
        </div>
      ) : (
        <ul className="grid grid-cols-1 sm:grid-cols-2 gap-2">
          {machines.map((m) => (
            <li
              key={m.machine_id}
              className={`rounded-md border p-2.5 ${STATE_STYLE[m.state]}`}
              data-ui-bridge-id="productivity.fleet-health-machine"
            >
              <div className="flex items-center justify-between gap-2">
                <span className="font-mono text-xs font-semibold truncate">
                  {m.hostname}
                </span>
                <span className="text-[10px] uppercase tracking-wide font-semibold">
                  {m.state}
                </span>
              </div>
              <div className="mt-1 flex items-center gap-3 text-[11px] text-muted-foreground">
                <span className="inline-flex items-center gap-1">
                  <Activity className="w-3 h-3" />
                  {m.agents_active} agent{m.agents_active === 1 ? "" : "s"}
                </span>
                <span>probe {ago(m.last_probe_at)}</span>
                {m.consecutive_failures > 0 && (
                  <span className="text-red-400">{m.consecutive_failures} fail</span>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}

      {/* Active alerts (criticals first — coord already orders them) */}
      {alerts.length > 0 && (
        <div className="flex flex-col gap-1.5">
          <h3 className="text-xs font-semibold text-muted-foreground">
            Active alerts ({alerts.length})
          </h3>
          <ul className="flex flex-col gap-1.5">
            {alerts.map((a) => (
              <li
                key={a.id}
                className={`flex items-start gap-2 rounded-md border p-2 text-xs ${SEVERITY_STYLE[a.severity]}`}
                data-ui-bridge-id="productivity.fleet-health-alert"
              >
                <SeverityIcon severity={a.severity} />
                <div className="min-w-0">
                  <div className="font-medium break-words">{a.summary}</div>
                  <div className="mt-0.5 text-[10px] opacity-70">
                    {a.kind} · ×{a.occurrences} · since {ago(a.first_seen_at)}
                    {a.page_due_at ? ` · page ${ago(a.page_due_at)}` : ""}
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

export default FleetHealthPanel;
