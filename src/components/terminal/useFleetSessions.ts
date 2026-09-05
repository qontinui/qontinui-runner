import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * One session somewhere on the fleet, as coord's `GET /coord/sessions/fleet`
 * reports it (plan `2026-08-31-remote-session-tabs-in-runner-terminal`,
 * Phase 2).
 *
 * Keys are camelCase, matching the Rust route's serde output exactly. Every
 * field except the three ids is optional on the wire: coord degrades a column
 * it cannot read to `null` rather than failing the request, so an absent value
 * here is UNKNOWN, never a positive "this session has none".
 */
export interface FleetSession {
  /** The `coord.sessions` row id. */
  sessionId: string;
  /** The machine this session is running on. */
  deviceId: string;
  /**
   * True when this row is on the CALLER's own device — i.e. a LOCAL session.
   * Always false for an operator principal, which has no device; read
   * `callerDeviceId` on the response to tell that case apart.
   */
  isCallerDevice: boolean;
  /** `coord.devices.hostname`. Null when unknown or degraded. */
  deviceHostname: string | null;
  deviceDisplayName: string | null;
  /**
   * The HARNESS (Claude Code) session id — the id a runner actually holds, and
   * the key a future attach will address. Null when coord's bridge column is
   * absent; read `sessionBridgeColumnPresent` before treating that as "this
   * session has no harness id".
   */
  claudeCodeSessionId: string | null;
  sessionKind: string | null;
  intent: string | null;
  /** The LIVENESS axis (`working`, `waiting_human`, …). */
  state: string | null;
  /**
   * The WORK axis, orthogonal to `state`. Null may mean unset OR degraded —
   * read `workAxisColumnsPresent`.
   */
  sessionStatus: string | null;
  workUnitSlug: string | null;
  repo: string | null;
  branch: string | null;
  provider: string | null;
  correlationTopic: string | null;
  startedAt: string | null;
  lastHeartbeatAt: string | null;
  closedAt: string | null;
}

/**
 * coord's response envelope. The three `*Present` flags are load-bearing and
 * must not be dropped: each says whether the corresponding field was OBSERVED
 * or merely absent, and the UI is required to render that difference.
 */
export interface FleetSessionsResponse {
  tenantId: string;
  /** The device coord authenticated this runner as. Null for an operator. */
  callerDeviceId: string | null;
  sessions: FleetSession[];
  /** Rows returned. NOT a tenant total — see `truncated`. */
  count: number;
  /** True when more rows matched than were served. */
  truncated: boolean;
  sessionBridgeColumnPresent: boolean;
  workAxisColumnsPresent: boolean;
  deviceIdentityColumnsPresent: boolean;
}

export interface FleetSessionsQuery {
  deviceId?: string;
  state?: string;
  includeClosed?: boolean;
  limit?: number;
}

/**
 * Why an empty list is empty. The picker must never render "no remote sessions"
 * without knowing which of these produced the zero — that is the difference
 * between an observation and an absence of evidence.
 */
export type FleetEmptyReason =
  | "not-loaded" // no read has completed yet
  | "error" // the read failed; `error` carries the reason
  | "observed-empty"; // coord answered, with zero rows and no degradation

export interface UseFleetSessionsResult {
  sessions: FleetSession[];
  /** The full envelope of the last successful read, or null. */
  response: FleetSessionsResponse | null;
  loading: boolean;
  error: string | null;
  /**
   * Set when `sessions` is empty, saying WHY. `observed-empty` is the only
   * value that licenses the words "no sessions"; the others are UNKNOWN.
   */
  emptyReason: FleetEmptyReason | null;
  /**
   * True when coord served at least one field degraded. A caller showing
   * counts or "nobody is working" must suppress that claim while this is set.
   */
  degraded: boolean;
  refresh: () => Promise<void>;
}

/**
 * True when any capability flag on the response is false — i.e. coord could not
 * read a column and served typed nulls instead of failing.
 *
 * Exported for the picker's banner and for tests: the rule "a degraded read is
 * not an observation" is the one this whole phase turns on, so it lives in one
 * place rather than being re-derived per consumer.
 */
export function isDegraded(r: FleetSessionsResponse | null): boolean {
  if (!r) return false;
  return (
    !r.sessionBridgeColumnPresent ||
    !r.workAxisColumnsPresent ||
    !r.deviceIdentityColumnsPresent
  );
}

/**
 * Decide why an empty `sessions` array is empty.
 *
 * Pure, and separate from the hook, because it is the honesty rule the UI
 * depends on: only a completed, error-free read may be called
 * `observed-empty`.
 */
export function emptyReasonFor(
  loaded: boolean,
  error: string | null,
  sessions: FleetSession[],
): FleetEmptyReason | null {
  if (sessions.length > 0) return null;
  if (error) return "error";
  if (!loaded) return "not-loaded";
  return "observed-empty";
}

/**
 * Group sessions by device, newest activity first within each device, and the
 * caller's own device first overall.
 *
 * The local-device-first ordering is deliberate: the picker exists to reach
 * REMOTE sessions, and putting the operator's own box at the top is what makes
 * the remainder legible as "everywhere else".
 */
export interface FleetDeviceGroup {
  deviceId: string;
  /** Best available human label, falling back to the id. */
  label: string;
  isCallerDevice: boolean;
  sessions: FleetSession[];
}

export function groupByDevice(sessions: FleetSession[]): FleetDeviceGroup[] {
  const byDevice = new Map<string, FleetSession[]>();
  for (const s of sessions) {
    const list = byDevice.get(s.deviceId);
    if (list) list.push(s);
    else byDevice.set(s.deviceId, [s]);
  }

  const groups: FleetDeviceGroup[] = [];
  for (const [deviceId, rows] of byDevice) {
    const first = rows[0];
    groups.push({
      deviceId,
      label: deviceLabel(first),
      isCallerDevice: first.isCallerDevice,
      sessions: rows,
    });
  }

  // Caller's device first, then by label so the order is stable across reads
  // (device ids are opaque, so sorting by them would look arbitrary).
  groups.sort((a, b) => {
    if (a.isCallerDevice !== b.isCallerDevice) return a.isCallerDevice ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
  return groups;
}

/**
 * The label to show for a device. Falls back through display name → hostname →
 * a shortened id, so a row is never unlabelled.
 */
export function deviceLabel(s: FleetSession): string {
  const named = s.deviceDisplayName?.trim() || s.deviceHostname?.trim();
  if (named) return named;
  return `device ${s.deviceId.slice(0, 8)}`;
}

/**
 * Read-only discovery of the fleet's sessions. No attach, no keystrokes — those
 * are Phases 3-5 and are gated on the authorization-grain work this phase does
 * not touch.
 */
export function useFleetSessions(opts?: FleetSessionsQuery): UseFleetSessionsResult {
  const [response, setResponse] = useState<FleetSessionsResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const didLoad = useRef(false);

  const fetchSessions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // The Rust command returns coord's body directly (no CommandResponse
      // envelope) and rejects on transport or non-2xx, so a thrown value here
      // is the honest failure — including 401/403, which means "this runner is
      // not paired", NOT "the fleet is empty".
      const result = await invoke<FleetSessionsResponse>("fleet_sessions_list", {
        args: {
          deviceId: opts?.deviceId ?? null,
          state: opts?.state ?? null,
          includeClosed: opts?.includeClosed ?? false,
          limit: opts?.limit ?? null,
        },
      });
      setResponse(result);
      setLoaded(true);
    } catch (err) {
      // Keep the previous response rather than clearing it: a failed refresh
      // must not silently empty a list the operator is reading.
      setError(`Failed to load fleet sessions: ${err}`);
    } finally {
      setLoading(false);
    }
  }, [opts?.deviceId, opts?.state, opts?.includeClosed, opts?.limit]);

  useEffect(() => {
    if (!didLoad.current) {
      didLoad.current = true;
      void fetchSessions();
    }
  }, [fetchSessions]);

  const sessions = response?.sessions ?? [];

  return {
    sessions,
    response,
    loading,
    error,
    emptyReason: emptyReasonFor(loaded, error, sessions),
    degraded: isDegraded(response),
    refresh: fetchSessions,
  };
}
