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
import { coordCallsReady, useCoordMode, type CoordGating } from "@/contexts/CoordModeContext";
import {
  CoordConnectionRequired,
  coordDisabledCopy,
} from "@/components/shared/CoordConnectionRequired";
import {
  getFleetHealth,
  type FleetAlert,
  type FleetAuth,
  type FleetHealth,
  type FleetMachineSnapshot,
} from "./coordinatorApi";

const logger = createLogger("FleetHealthPanel");

/** Operator-facing name of this surface, shared by the notice and the
 *  disabled control's tooltip so both read from one `coordDisabledCopy`. */
const FLEET_HEALTH_SURFACE = "Fleet health";

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

/** Derived view-state for the panel — pure so it's unit-testable under
 *  the runner's node-environment vitest (no jsdom to render the JSX).
 *
 *  An auth rejection (coord 401/403) is NOT a transport error: it must
 *  read as an auth failure, not "coord unreachable", and must NOT show
 *  the stale machine grid as if current. coord is anonymous today so
 *  `auth.state` is `ok` in practice; an absent `auth` (older backend
 *  during dev) is treated as `ok` for back-compat. */
export function deriveFleetView(
  data: FleetHealth | null,
  gating?: CoordGating,
): {
  authState: FleetAuth["state"];
  isUnauthorized: boolean;
  isUnpaired: boolean;
  isAuthBlocked: boolean;
  machines: FleetMachineSnapshot[];
  alerts: FleetAlert[];
  rollup: { critical: number; warning: number; info: number };
  /** True when the runner is positively isolated. Drives all three of:
   *  render the notice instead of any fleet content, skip the 1 Hz poll
   *  (an isolated runner has no coord to poll, so polling it once a
   *  second is pure waste that also manufactures a permanent error
   *  banner), and disable the panel's own Refresh control. */
  coordDisabled: boolean;
} {
  const authState = data?.auth?.state ?? "ok";
  const isUnauthorized = authState === "unauthorized";
  const isUnpaired = authState === "unpaired";
  const isAuthBlocked = isUnauthorized || isUnpaired;
  // `gating` absent (a caller that predates the §6.4 gate, or a panel
  // rendered outside CoordModeProvider) fails OPEN — see
  // CoordModeContext's header on why a wrongly-disabled panel is worse
  // than a wrongly-enabled one.
  const coordDisabled = gating?.isolated ?? false;
  return {
    authState,
    isUnauthorized,
    isUnpaired,
    isAuthBlocked,
    // When auth-blocked, drop any stale grid/alerts so we never render
    // them as if current. Same for isolated: there is no fleet.
    machines: isAuthBlocked || coordDisabled ? [] : (data?.health?.machines ?? []),
    alerts: isAuthBlocked || coordDisabled ? [] : (data?.alerts ?? []),
    // Auth-blocked gets the SAME suppression the grid and alert list get two
    // lines up. It did not before, which was invisible while the rollup was
    // only a number: now that an unexplained rollup surfaces as an explicit
    // "N critical fleet-wide" badge (see `deriveAlertBadges`), leaving it
    // through would render a stale pre-rejection frame as current — beside a
    // notice saying coord rejected our credentials.
    rollup:
      isAuthBlocked || coordDisabled
        ? { critical: 0, warning: 0, info: 0 }
        : (data?.health?.alerts ?? { critical: 0, warning: 0, info: 0 }),
    coordDisabled,
  };
}

/**
 * Severity counts for the header, split by the POPULATION each one describes.
 *
 * THE DEFECT this closes: the header rendered `rollup.critical` /
 * `rollup.warning` straight from `health.alerts`, which is coord's FLEET-WIDE
 * alert rollup, immediately beside `machines.length`, which counts only the
 * machines registered with a pollable `/health` URL — the same set the body
 * lists. The two populations are unrelated, so the panel could (and did, on
 * 2026-08-22) read `0 machines / 61 critical / 1929 warning` in the header
 * while the body said `No machines registered with a pollable /health URL
 * yet`. Nothing in the UI said the counts were about a different population,
 * so the only available reading was that the panel contradicted itself.
 *
 * The split makes the population explicit instead of implicit:
 *
 * - `scoped` counts the active alerts whose `machine_id` is one of the
 *   machines the body lists. These share the header's `N machines` population,
 *   so they render as plain `N critical` / `N warning` badges.
 * - `fleetWide` is what the rollup counts BEYOND those — alerts against
 *   machines this list does not show, and fleet-scope alerts with no
 *   `machine_id` at all. Non-zero only when there is a genuine excess, and it
 *   renders with an explicit "fleet-wide" label.
 *
 * Nothing is hidden: with no machines listed, every alert is fleet-wide and the
 * header says so. `fleetWide` is derived by subtracting the scoped counts from
 * the rollup (clamped at zero) rather than by counting `alerts` directly,
 * because `alerts` is coord's active-alert PAGE while `rollup` is the true
 * total — trusting the page for the total would under-report.
 *
 * Pure so it is unit-testable under the runner's node-environment vitest.
 */
export function deriveAlertBadges(
  machines: FleetMachineSnapshot[],
  alerts: FleetAlert[],
  rollup: { critical: number; warning: number; info: number },
): {
  scoped: { critical: number; warning: number };
  fleetWide: { critical: number; warning: number };
} {
  const listed = new Set(machines.map((m) => m.machine_id));
  const scoped = { critical: 0, warning: 0 };
  for (const a of alerts) {
    if (a.machine_id === null || !listed.has(a.machine_id)) continue;
    if (a.severity === "critical") scoped.critical += 1;
    else if (a.severity === "warning") scoped.warning += 1;
  }
  return {
    scoped,
    fleetWide: {
      critical: Math.max(0, rollup.critical - scoped.critical),
      warning: Math.max(0, rollup.warning - scoped.warning),
    },
  };
}

export function FleetHealthPanel() {
  const coord = useCoordMode();
  const [data, setData] = useState<FleetHealth | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  // Avoid overlapping polls if coord is slow.
  const inFlight = useRef(false);

  const { isUnauthorized, isUnpaired, isAuthBlocked, machines, alerts, rollup, coordDisabled } =
    deriveFleetView(data, coord);
  // Header badges must describe the SAME population the body lists — see
  // `deriveAlertBadges` for the header/body contradiction this resolves.
  const badges = deriveAlertBadges(machines, alerts, rollup);
  // Derived from the SAME `source` as the notice body below, so the two can
  // never disagree about why the panel is off.
  const disabledTooltip = coordDisabled
    ? coordDisabledCopy(coord.source, FLEET_HEALTH_SURFACE).tooltip
    : undefined;

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
    // An isolated runner has no coord: polling a phantom base once a
    // second would burn a request per second and manufacture a permanent
    // "coord unreachable" banner for a condition that is configuration,
    // not an outage. The mode is config-derived, so this is not a
    // reachability heuristic — it never flips back on a network blip.
    //
    // `coordCallsReady` also holds the FIRST request until the mode has
    // settled. Branching on `coordDisabled` alone let every app start fire
    // one GET (and arm the interval) before `get_coord_mode` answered, so an
    // isolated runner still dialled the phantom base once per launch.
    if (!coordCallsReady(coord)) return;
    void load();
    const id = window.setInterval(() => void load(), POLL_MS);
    return () => window.clearInterval(id);
  }, [load, coord]);

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
          {badges.scoped.critical > 0 && (
            <span
              data-ui-bridge-id="productivity.fleet-health-scoped-critical"
              className="rounded px-1.5 py-0.5 text-[10px] font-semibold border border-red-500/40 bg-red-500/10 text-red-400"
            >
              {badges.scoped.critical} critical
            </span>
          )}
          {badges.scoped.warning > 0 && (
            <span
              data-ui-bridge-id="productivity.fleet-health-scoped-warning"
              className="rounded px-1.5 py-0.5 text-[10px] font-semibold border border-amber-500/30 bg-amber-500/5 text-amber-400"
            >
              {badges.scoped.warning} warning
            </span>
          )}
          {/* Alerts the listed machines do NOT account for. Explicitly
              labelled, because the population is not the one this header's
              machine count (or the grid below) describes. */}
          {(badges.fleetWide.critical > 0 || badges.fleetWide.warning > 0) && (
            <span
              data-ui-bridge-id="productivity.fleet-health-fleet-wide"
              title="Alerts across the whole fleet, including machines not listed below and fleet-scope alerts with no machine"
              className="rounded px-1.5 py-0.5 text-[10px] font-semibold border border-border bg-muted/30 text-muted-foreground"
            >
              {[
                badges.fleetWide.critical > 0 ? `${badges.fleetWide.critical} critical` : null,
                badges.fleetWide.warning > 0 ? `${badges.fleetWide.warning} warning` : null,
              ]
                .filter(Boolean)
                .join(", ")}{" "}
              fleet-wide
            </span>
          )}
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading || coordDisabled}
          title={coordDisabled ? disabledTooltip : undefined}
          data-ui-bridge-id="productivity.fleet-health-refresh"
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </header>

      {/* A genuine command rejection (coord unreachable / parse error) ≠
          runner broken — keep the last good frame behind a retriable
          banner. Suppressed when coord returned an auth rejection: that
          gets its own honest auth state below, not a "coord unreachable"
          message. */}
      {error && !isAuthBlocked && !coordDisabled && (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-2 text-xs text-red-400">
          coord unreachable — showing last known state. ({error})
        </div>
      )}

      {/* Auth-rejected: token present but coord rejected/expired it. Honest
          about the cause — this is NOT a network blip — and does not show
          the stale machine grid as if it were current. */}
      {coordDisabled ? (
        <CoordConnectionRequired
          source={coord.source}
          surface={FLEET_HEALTH_SURFACE}
          uiBridgeId="productivity.fleet-health-isolated"
        />
      ) : isUnauthorized ? (
        <div
          className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-xs text-amber-400"
          data-ui-bridge-id="productivity.fleet-health-unauthorized"
        >
          <div className="font-semibold text-foreground">Fleet view requires sign-in</div>
          <p className="mt-1 text-amber-400/90">
            This device&apos;s coord token was rejected or expired. Re-pair the device to restore
            the fleet view.
          </p>
        </div>
      ) : isUnpaired ? (
        <div
          className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-xs text-amber-400"
          data-ui-bridge-id="productivity.fleet-health-unpaired"
        >
          <div className="font-semibold text-foreground">Device not paired</div>
          <p className="mt-1 text-amber-400/90">Pair this device with coord to see fleet health.</p>
        </div>
      ) : machines.length === 0 ? (
        /* Per-machine state grid */
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
                <span className="font-mono text-xs font-semibold truncate">{m.hostname}</span>
                <span className="text-[10px] uppercase tracking-wide font-semibold">{m.state}</span>
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
