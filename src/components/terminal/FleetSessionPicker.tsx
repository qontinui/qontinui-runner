import { useMemo } from "react";
import { AlertTriangle, Monitor, RefreshCw, Server } from "lucide-react";
import {
  useFleetSessions,
  groupByDevice,
  type FleetSession,
  type FleetSessionsResponse,
} from "./useFleetSessions";

/**
 * Read-only picker listing which Claude Code sessions exist on which fleet
 * machine (plan `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 2).
 *
 * Discovery ONLY. There is no attach affordance and no keystroke path here —
 * those are Phases 3-5, gated on the authorization-grain work (security
 * prerequisite item 5) this phase deliberately does not touch. Shipping the
 * read on its own is the point: it is independently useful, and it lands before
 * anything can be typed into.
 */

export const FLEET_SESSION_PICKER_ELEMENT = "fleet-session-picker";
export const FLEET_DEVICE_GROUP_ELEMENT = "fleet-device-group";
export const FLEET_SESSION_ROW_ELEMENT = "fleet-session-row";
export const FLEET_PICKER_REFRESH_ID = "terminal.fleet-picker-refresh";
export const FLEET_PICKER_RETRY_ID = "terminal.fleet-picker-retry";

export function fleetSessionRowId(sessionId: string): string {
  return `terminal.fleet-session.${sessionId}`;
}

export function fleetDeviceGroupId(deviceId: string): string {
  return `terminal.fleet-device.${deviceId}`;
}

/**
 * What to show a session as. `state` is the liveness axis and `sessionStatus`
 * the orthogonal work axis; a session can be `working` on one and `finished` on
 * the other, so both are surfaced rather than collapsed into one word.
 *
 * A null on either is UNKNOWN — rendered as an em dash, never as a default like
 * "idle", which would be a claim coord did not make.
 */
export function sessionStateLabel(s: FleetSession): string {
  const liveness = s.state?.trim() || "—";
  const work = s.sessionStatus?.trim();
  return work ? `${liveness} · ${work}` : liveness;
}

/**
 * The one-line description of a session. Prefers the work-unit slug (what it is
 * FOR) over the free-text intent (what someone typed), and falls back to the
 * repo/branch it sits on.
 */
export function sessionDescription(s: FleetSession): string {
  const slug = s.workUnitSlug?.trim();
  if (slug) return slug;
  const intent = s.intent?.trim();
  if (intent) return intent;
  const repo = s.repo?.trim();
  const branch = s.branch?.trim();
  if (repo && branch) return `${repo} @ ${branch}`;
  if (repo) return repo;
  return "(no declared work)";
}

/**
 * The banner text when coord served a degraded read, or null when it did not.
 *
 * Exported and pure so the honesty rule is testable: the picker must SAY that a
 * field was unreadable rather than rendering its absence as a fact.
 */
export function degradedNotice(r: FleetSessionsResponse | null): string | null {
  if (!r) return null;
  const missing: string[] = [];
  if (!r.sessionBridgeColumnPresent) missing.push("harness session ids");
  if (!r.workAxisColumnsPresent) missing.push("work status");
  if (!r.deviceIdentityColumnsPresent) missing.push("device names");
  if (missing.length === 0) return null;
  return `coord could not read ${missing.join(", ")} — those fields are unknown, not empty.`;
}

export function FleetSessionPicker() {
  const { sessions, response, loading, error, emptyReason, refresh } = useFleetSessions();
  const groups = useMemo(() => groupByDevice(sessions), [sessions]);
  const notice = degradedNotice(response);

  const remoteCount = sessions.filter((s) => !s.isCallerDevice).length;

  return (
    <div
      data-page-element={FLEET_SESSION_PICKER_ELEMENT}
      className="flex-1 flex flex-col min-h-0"
    >
      {/* Sub-header: counts + refresh */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d]">
        <Server className="w-3 h-3 text-[#565f89]" />
        <span className="text-[10px] text-[#565f89] font-medium">
          {sessions.length} session{sessions.length !== 1 ? "s" : ""} on {groups.length} device
          {groups.length !== 1 ? "s" : ""}
          {remoteCount > 0 ? ` · ${remoteCount} remote` : ""}
          {response?.truncated ? " · more not shown" : ""}
        </span>
        <div className="flex-1" />
        <button
          data-ui-bridge-id={FLEET_PICKER_REFRESH_ID}
          onClick={() => void refresh()}
          disabled={loading}
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
          title="Refresh fleet sessions"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* A degraded read is announced, never rendered as fact. */}
      {notice && (
        <div className="px-3 py-1.5 text-[10px] text-[#e0af68] border-b border-[#2a2d3d] flex items-start gap-1.5">
          <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
          <span>{notice}</span>
        </div>
      )}

      <div className="flex-1 overflow-y-auto scrollbar-dark">
        {loading && sessions.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading fleet sessions...
          </div>
        ) : error ? (
          <div className="px-3 py-8 text-center text-[#f7768e] text-xs">
            <AlertTriangle className="w-4 h-4 mx-auto mb-2" />
            {error}
            <div className="mt-1 text-[#565f89]">
              This is a failed read, not an empty fleet.
            </div>
            <button
              data-ui-bridge-id={FLEET_PICKER_RETRY_ID}
              onClick={() => void refresh()}
              className="mt-2 px-2 py-1 rounded bg-[#2a2d3d] text-[#c0caf5] hover:bg-[#3a3d4d] transition-colors"
            >
              Retry
            </button>
          </div>
        ) : sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">
            {emptyReason === "observed-empty"
              ? "No open sessions anywhere on the fleet."
              : "Fleet sessions are unknown — no successful read yet."}
          </div>
        ) : (
          groups.map((g) => (
            <div
              key={g.deviceId}
              data-page-element={FLEET_DEVICE_GROUP_ELEMENT}
              data-ui-bridge-id={fleetDeviceGroupId(g.deviceId)}
            >
              <div className="flex items-center gap-1.5 px-3 py-1 bg-[#1a1b26] border-b border-[#2a2d3d] sticky top-0">
                <Monitor className="w-3 h-3 text-[#565f89]" />
                <span className="text-[10px] font-medium text-[#c0caf5]">{g.label}</span>
                {g.isCallerDevice && (
                  <span className="text-[9px] px-1 rounded bg-[#2a2d3d] text-[#565f89]">
                    this machine
                  </span>
                )}
                <span className="text-[10px] text-[#565f89]">
                  {g.sessions.length} session{g.sessions.length !== 1 ? "s" : ""}
                </span>
              </div>

              {g.sessions.map((s) => (
                <div
                  key={s.sessionId}
                  data-page-element={FLEET_SESSION_ROW_ELEMENT}
                  data-ui-bridge-id={fleetSessionRowId(s.sessionId)}
                  className="px-3 py-1.5 border-b border-[#2a2d3d] hover:bg-[#1f2130] transition-colors"
                >
                  <div className="flex items-baseline gap-2">
                    <span className="text-[11px] text-[#c0caf5] truncate">
                      {sessionDescription(s)}
                    </span>
                    <div className="flex-1" />
                    <span className="text-[10px] text-[#565f89] shrink-0">
                      {sessionStateLabel(s)}
                    </span>
                  </div>
                  {(s.provider || s.correlationTopic) && (
                    <div className="text-[10px] text-[#565f89] truncate">
                      {[s.provider, s.correlationTopic].filter(Boolean).join(" · ")}
                    </div>
                  )}
                </div>
              ))}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
