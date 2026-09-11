import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

import {
  FLEET_DEFAULT_LIMIT,
  mergeDeviceCatalog,
  mergeStateCatalog,
  statesSeenIn,
  type FleetDeviceOption,
  type FleetServerFilter,
} from "./fleetDiscovery";

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
  /**
   * `coord.devices.hostname`. Null when the device row is absent (a LEFT join)
   * or the column is degraded.
   */
  deviceHostname: string | null;
  /**
   * The operator-facing device name. coord serves this from
   * `coord.devices.name`, wired out under this key — there is no
   * `display_name` COLUMN and never has been, only that alias. Null on the same
   * two conditions as `deviceHostname`, which the same probe gates.
   */
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
  /**
   * The LIVENESS axis — coord's `SessionState`
   * (`expected | active | pending_resolution | stale | closed`). NOT the work
   * axis: `working` / `waiting_human` / `finished` are `sessionStatus` values,
   * and an earlier revision of this comment listed them here, which is what a
   * state filter built from it would have offered — a dropdown of values coord
   * never puts in this column.
   */
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
   * True when coord served at least one field degraded. A caller must not
   * present a degraded FIELD (device names, work-axis status, bridge ids) as
   * observed while this is set; the row count itself is unaffected by column
   * degradation and may be shown.
   */
  degraded: boolean;
  /**
   * Every device seen across the reads this hook has made, accumulated.
   *
   * The picker's device filter is served from this rather than from the latest
   * response: selecting a device sends `device_id` to coord, whose next
   * response then holds only that device, and a dropdown rebuilt from it would
   * offer no way back to the others.
   */
  deviceCatalog: FleetDeviceOption[];
  /**
   * Every distinct `state` value seen across those reads. Accumulated for the
   * same reason as `deviceCatalog`: a `state`-narrowed read carries only the
   * selected value.
   */
  stateCatalog: string[];
  /**
   * The query `response` was actually served for, or null before any read
   * completed.
   *
   * This exists because the caller's filter state and the last response are
   * INDEPENDENT: the moment a filter changes, the component's own filter object
   * describes a request in flight while `response` still holds the previous
   * one. A caller that reported "coord truncated this read at {its own limit}"
   * would then be stating something false — and a failed refetch makes that
   * permanent, since the old `response` is deliberately kept and `loading`
   * returns to false. Anything said ABOUT the served rows must be said with
   * this, never with the caller's pending filters.
   */
  appliedQuery: FleetServerFilter | null;
  refresh: () => Promise<void>;
}

/**
 * Stable empty list for the no-response case.
 *
 * A fresh `[]` per render would give `sessions` a new identity every time and
 * silently defeat every `useMemo` a consumer keys on it.
 */
const NO_SESSIONS: FleetSession[] = [];

/**
 * The distinct devices a page of rows came from, labelled.
 *
 * Pure and exported so the picker's filter options are testable without a live
 * coord. First row per device wins the label — every row of one device carries
 * the same identity fields, so there is nothing to reconcile.
 */
export function devicesSeenIn(sessions: FleetSession[]): FleetDeviceOption[] {
  const byId = new Map<string, FleetDeviceOption>();
  for (const s of sessions) {
    if (byId.has(s.deviceId)) continue;
    // Whether the label is the id placeholder is decided HERE, where the two
    // identity fields are in hand — not later by sniffing the label's text.
    const named = s.deviceDisplayName?.trim() || s.deviceHostname?.trim();
    byId.set(s.deviceId, {
      deviceId: s.deviceId,
      label: deviceLabel(s),
      isCallerDevice: s.isCallerDevice,
      labelIsFallback: !named,
    });
  }
  return [...byId.values()];
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
    !r.sessionBridgeColumnPresent || !r.workAxisColumnsPresent || !r.deviceIdentityColumnsPresent
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
 * The label to show for a device: the operator-facing name, falling back to the
 * hostname, then to a shortened id so a row is never unlabelled. A blank value
 * at either level is treated as absent rather than rendered as an empty label.
 *
 * The display name leads because it is what an operator chose; the hostname is
 * what the machine calls itself.
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
  const [deviceCatalog, setDeviceCatalog] = useState<FleetDeviceOption[]>([]);
  const [stateCatalog, setStateCatalog] = useState<string[]>([]);
  const [appliedQuery, setAppliedQuery] = useState<FleetServerFilter | null>(null);

  const fetchSessions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // The Rust command returns coord's body directly (no CommandResponse
      // envelope) and rejects on transport or non-2xx, so a thrown value here
      // is the honest failure — including 401/403, which means "this runner is
      // not paired", NOT "the fleet is empty".
      const query = {
        deviceId: opts?.deviceId ?? null,
        state: opts?.state ?? null,
        includeClosed: opts?.includeClosed ?? false,
        limit: opts?.limit ?? null,
      };
      const result = await invoke<FleetSessionsResponse>("fleet_sessions_list", {
        args: query,
      });
      setResponse(result);
      setLoaded(true);
      // Recorded in the SAME tick as the response it belongs to, so no consumer
      // can pair these rows with a filter set they were not served for. The
      // limit is resolved rather than passed through: coord applies its own
      // default when the caller names none, and a null here would leave a
      // consumer guessing what "more" means.
      setAppliedQuery({
        deviceId: query.deviceId,
        state: query.state,
        includeClosed: query.includeClosed,
        limit: query.limit ?? FLEET_DEFAULT_LIMIT,
      });
      // Accumulate here — in the fetch, which is an event — rather than in an
      // effect over `response`: an effect that calls setState costs a cascading
      // render per read, and the catalogues are a property of the read SEQUENCE,
      // which is this hook's to own.
      const rows = result.sessions ?? [];
      const seen = devicesSeenIn(rows);
      if (seen.length > 0) setDeviceCatalog((prev) => mergeDeviceCatalog(prev, seen));
      const states = statesSeenIn(rows);
      if (states.length > 0) setStateCatalog((prev) => mergeStateCatalog(prev, states));
    } catch (err) {
      // Keep the previous response rather than clearing it: a failed refresh
      // must not silently empty a list the operator is reading.
      setError(`Failed to load fleet sessions: ${err}`);
    } finally {
      setLoading(false);
    }
  }, [opts?.deviceId, opts?.state, opts?.includeClosed, opts?.limit]);

  // Runs on mount and again whenever the filter set changes identity — the
  // hook advertises `opts` as an input, so a new deviceId/state must refetch.
  useEffect(() => {
    void fetchSessions();
  }, [fetchSessions]);

  const sessions = response?.sessions ?? NO_SESSIONS;

  return {
    sessions,
    response,
    loading,
    error,
    emptyReason: emptyReasonFor(loaded, error, sessions),
    degraded: isDegraded(response),
    deviceCatalog,
    stateCatalog,
    appliedQuery,
    refresh: fetchSessions,
  };
}
