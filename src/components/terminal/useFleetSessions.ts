import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

import {
  EMPTY_FLEET_WALK,
  FLEET_CURSOR_STALLED_MESSAGE,
  FLEET_DEFAULT_LIMIT,
  fleetCursorStalled,
  fleetErrorCode,
  fleetErrorInvalidatesCursor,
  fleetErrorIsRestart,
  fleetErrorMessage,
  fleetScopeKey,
  fleetWalkAccept,
  fleetWalkDropCursor,
  mergeDeviceCatalog,
  mergeStateCatalog,
  normalizeFleetCursor,
  statesSeenIn,
  type FleetDeviceOption,
  type FleetErrorCode,
  type FleetServerFilter,
  type FleetWalk,
  type FleetWalkMode,
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
 *
 * `truncated` is GONE (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 5a): the
 * route does keyset pagination now and completeness is `nextCursor`. Declaring
 * a field the wire no longer carries is what produced the defect this phase
 * repaired — `!response.truncated` reads `!undefined` as "complete" and the
 * picker silently claims a full list — so the field is not kept around as
 * optional, it is deleted.
 */
export interface FleetSessionsResponse {
  tenantId: string;
  /** The device coord authenticated this runner as. Null for an operator. */
  callerDeviceId: string | null;
  sessions: FleetSession[];
  /** Rows returned on THIS page. NOT a tenant total, and not a completeness
   * signal — `nextCursor` is the only one of those. */
  count: number;
  /**
   * The EFFECTIVE, post-clamp page size coord served this page under (it
   * clamps to `MAX_LIMIT`). A statement about the REQUEST — `nextCursor` is
   * the statement about the DATA, and the two can never contradict.
   */
  limit: number;
  /**
   * The cursor for the next page, or `null` on the last page.
   *
   * Always present as a KEY, so `"nextCursor" in body` must never be read as a
   * server-version probe: only the value decides. OPAQUE by contract — re-send
   * it verbatim, never construct, parse, inspect or reinterpret it.
   *
   * `null` means "last page AS OF NOW". Sessions that start after a walk
   * completes need a fresh walk.
   */
  nextCursor: string | null;
  sessionBridgeColumnPresent: boolean;
  workAxisColumnsPresent: boolean;
  deviceIdentityColumnsPresent: boolean;
}

export interface FleetSessionsQuery {
  deviceId?: string;
  state?: string;
  includeClosed?: boolean;
  /** Page size for each page of the walk, NOT a reachability control. */
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
  /**
   * Every row the walk has served for the CURRENT scope, accumulated across
   * pages and deduplicated by `sessionId`. A restart replaces this wholesale —
   * carrying rows across a scope change would build a list no filter set
   * describes.
   */
  sessions: FleetSession[];
  /** The full envelope of the last successful page, or null. */
  response: FleetSessionsResponse | null;
  /** A page-ONE read is in flight — the list may be blank or stale. */
  loading: boolean;
  /** A SUBSEQUENT page is in flight — the rows on screen stay valid. */
  loadingMore: boolean;
  error: string | null;
  /**
   * coord's stable machine code for the last failed read, or null when the
   * failure carried none (a transport error) or there was no failure.
   * `cursor_scope_mismatch` never reaches here: it is a restart, not an error.
   */
  errorCode: FleetErrorCode | null;
  /**
   * True when `error` describes a walk that cannot ADVANCE rather than a read
   * that FAILED — coord answered, the rows are current, and only the next page
   * is out of reach. The two must not share a banner: "last refresh failed" over
   * a successful read is a false claim in the other direction.
   */
  walkStalled: boolean;
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
  /** Pages accepted since the last restart. 0 before any read completes. */
  pagesLoaded: number;
  /**
   * True when coord handed back a cursor on the last page — i.e. more rows are
   * genuinely REACHABLE, not merely unserved. False is only ever "complete as
   * of that read".
   */
  hasMore: boolean;
  /** Restart the walk from coord's first page, with no cursor. */
  refresh: () => Promise<void>;
  /** Fetch the next page with the cursor. A no-op when there is none. */
  loadMore: () => Promise<void>;
}

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
 * Read-only discovery of the fleet's sessions, walked page by page with coord's
 * keyset cursor. No attach, no keystrokes — those are Phases 3-5 and are gated
 * on the authorization-grain work this phase does not touch.
 *
 * ## The walk
 *
 * A read either RESTARTS the walk (no cursor, page one, accumulation replaced)
 * or ADVANCES it (`nextCursor` re-sent verbatim, page appended). A restart is
 * what happens on mount, on `refresh`, and whenever the SCOPE changes —
 * `deviceId` / `state` / `includeClosed`, the three coord validates a cursor
 * against. `limit` is deliberately not one of them: coord's cursor survives a
 * changed page size, so resizing a page must not throw away pages already
 * loaded.
 *
 * Two things keep the accumulation honest. A monotonic generation stamps every
 * request, and a response whose generation has been superseded is DISCARDED
 * rather than merged — otherwise a page from the previous scope lands in the
 * new scope's list. And `cursor_scope_mismatch`, which coord answers when a
 * page in flight carries a cursor from a scope the caller has since changed, is
 * handled by restarting rather than by showing an error: changing a filter
 * mid-walk is ordinary use, not a fault the operator should read about.
 */
export function useFleetSessions(opts?: FleetSessionsQuery): UseFleetSessionsResult {
  const [walk, setWalk] = useState<FleetWalk>(EMPTY_FLEET_WALK);
  const [response, setResponse] = useState<FleetSessionsResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [errorCode, setErrorCode] = useState<FleetErrorCode | null>(null);
  const [walkStalled, setWalkStalled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [deviceCatalog, setDeviceCatalog] = useState<FleetDeviceOption[]>([]);
  const [stateCatalog, setStateCatalog] = useState<string[]>([]);
  const [appliedQuery, setAppliedQuery] = useState<FleetServerFilter | null>(null);
  /** Bumped to re-run the walk without a scope change — the scope-mismatch restart. */
  const [restartToken, setRestartToken] = useState(0);

  /**
   * The cursor the next page must carry.
   *
   * Held in a ref as well as in `walk` because `loadMore` has to read the value
   * as of the CLICK, not as of the render that created the callback — a stale
   * closure here would re-request a page already accumulated.
   */
  const cursorRef = useRef<string | null>(null);
  /**
   * Monotonic request id. Only the newest request may write state: a response
   * that lost the race belongs to a superseded scope, and merging it would mix
   * two queries' rows into one list.
   */
  const generationRef = useRef(0);

  const deviceId = opts?.deviceId ?? null;
  const state = opts?.state ?? null;
  const includeClosed = opts?.includeClosed ?? false;
  const limit = opts?.limit ?? FLEET_DEFAULT_LIMIT;
  /**
   * The walk's scope, as one comparable string, named EXPLICITLY in the restart
   * effect's dependencies.
   *
   * What actually fixed the page-resize restart is one line below this: `limit`
   * came out of `fetchPage`'s dependency list. While it was in, `fetchPage` got
   * a new identity on every resize, the restart effect keyed on that identity,
   * and every accumulated page was discarded and re-fetched — the opposite of
   * this module's contract, since coord's cursor survives a changed page size.
   *
   * This key does not do that work and is not load-bearing for it. It is here
   * because the restart trigger should SAY what it is: keying only on a
   * callback's identity makes the trigger an implicit consequence of that
   * callback's dependency list, which is exactly how `limit` got in. `fetchPage`
   * is still named beside it — the two move together, so the effect needs no
   * lint suppression and no claim that one replaces the other.
   */
  const scopeKey = fleetScopeKey({ deviceId, state, includeClosed });

  /**
   * The page size as of the CALL, not as of the render that built the callback.
   *
   * `limit` is read through a ref for the same reason `cursorRef` exists: it
   * must not enter `fetchPage`'s dependency list, because everything in that
   * list restarts the walk through the effect below.
   */
  const limitRef = useRef(limit);
  // Synced in an effect rather than during render: a render-phase ref write is
  // what `react-hooks/refs` flags, and nothing reads this before an effect or a
  // click handler runs — `useRef(limit)` already seeds the mount read.
  //
  // This effect MUST stay declared above the restart effect below. React runs
  // effects in declaration order, so a commit that changes the page size and the
  // scope together syncs the ref here first and the restart then reads the new
  // size. Reordering the two would leave that one commit fetching page one at
  // the previous size, silently and only in that case.
  useEffect(() => {
    limitRef.current = limit;
  }, [limit]);

  const fetchPage = useCallback(
    async (mode: FleetWalkMode) => {
      const limit = limitRef.current;
      const cursor = mode === "more" ? cursorRef.current : null;
      // Nothing to walk. Not an error and not a read: coord said this was the
      // last page, and asking again with no cursor would silently restart.
      if (mode === "more" && cursor === null) return;
      const generation = generationRef.current + 1;
      generationRef.current = generation;
      if (mode === "restart") cursorRef.current = null;
      setLoading(mode === "restart");
      setLoadingMore(mode === "more");
      setError(null);
      setErrorCode(null);
      setWalkStalled(false);
      try {
        // The Rust command returns coord's body directly (no CommandResponse
        // envelope) and rejects on transport or non-2xx, so a thrown value here
        // is the honest failure — including 401/403, which means "this runner is
        // not paired", NOT "the fleet is empty".
        const result = await invoke<FleetSessionsResponse>("fleet_sessions_list", {
          args: { deviceId, state, includeClosed, limit, cursor },
        });
        if (generationRef.current !== generation) return;

        const next = normalizeFleetCursor(result.nextCursor);
        // A keyset cursor encodes the page just served, so coord handing back
        // the cursor it was GIVEN means the parameter never reached it. Stop
        // rather than re-serve page one for ever, and say so.
        const stalled = fleetCursorStalled(cursor, next);
        cursorRef.current = stalled ? null : next;

        setWalk((prev) => {
          const accepted = fleetWalkAccept(prev, result, mode);
          return stalled ? fleetWalkDropCursor(accepted) : accepted;
        });
        setResponse(result);
        setLoaded(true);
        // Recorded in the SAME tick as the response it belongs to, so no
        // consumer can pair these rows with a filter set they were not served
        // for. The limit is coord's OWN effective, post-clamp value where it
        // served one — the requested number is only the fallback, and a null
        // would leave a consumer guessing what a page is.
        setAppliedQuery({
          deviceId,
          state,
          includeClosed,
          limit: typeof result.limit === "number" && result.limit > 0 ? result.limit : limit,
        });
        if (stalled) {
          // coord ANSWERED — the rows below are current and the read did not
          // fail. Only the next page is out of reach, and the banner has to say
          // that rather than borrow the failed-read wording.
          setError(FLEET_CURSOR_STALLED_MESSAGE);
          setWalkStalled(true);
        }
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
        if (generationRef.current !== generation) return;
        const code = fleetErrorCode(err);
        if (fleetErrorIsRestart(code)) {
          // The scope moved under a page already in flight. Ordinary use — walk
          // again from page one and show the operator nothing.
          cursorRef.current = null;
          setRestartToken((t) => t + 1);
          return;
        }
        if (fleetErrorInvalidatesCursor(code)) {
          // The cursor is unusable. Keep the pages already accumulated, but stop
          // offering a control that can only fail again — the message below is
          // what stops that from reading as a complete list.
          cursorRef.current = null;
          setWalk(fleetWalkDropCursor);
        }
        // Keep the previous rows rather than clearing them: a failed page must
        // not silently empty a list the operator is reading.
        setErrorCode(code);
        setError(fleetErrorMessage(code, err));
      } finally {
        if (generationRef.current === generation) {
          setLoading(false);
          setLoadingMore(false);
        }
      }
    },
    [deviceId, state, includeClosed],
  );

  // Runs on mount, whenever the SCOPE changes (a new device/state/include-closed
  // must restart the walk — a cursor is only valid within its scope), and when a
  // scope-mismatch restart bumps the token.
  //
  // A page RESIZE is deliberately absent from that list: it changes the slice,
  // not the sequence, and coord's cursor survives it, so resizing re-uses the
  // walk rather than throwing away the pages already loaded. The next page
  // fetched picks the new size up through `limitRef`.
  //
  // `scopeKey` and `fetchPage` move together by construction — both derive from
  // exactly `deviceId` / `state` / `includeClosed` — so naming both is honest
  // rather than redundant-and-suppressed, and it keeps the trigger readable
  // without an eslint directive standing in for the explanation.
  useEffect(() => {
    void fetchPage("restart");
  }, [fetchPage, scopeKey, restartToken]);

  const refresh = useCallback(() => fetchPage("restart"), [fetchPage]);
  const loadMore = useCallback(() => fetchPage("more"), [fetchPage]);

  return {
    sessions: walk.sessions,
    response,
    loading,
    loadingMore,
    error,
    errorCode,
    walkStalled,
    emptyReason: emptyReasonFor(loaded, error, walk.sessions),
    degraded: isDegraded(response),
    deviceCatalog,
    stateCatalog,
    appliedQuery,
    pagesLoaded: walk.pages,
    hasMore: walk.nextCursor !== null,
    refresh,
    loadMore,
  };
}
