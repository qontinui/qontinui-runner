import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { InteractiveSurface, InteractivityFact } from "./remoteInteractivityFacts";
import {
  FLEET_DEFAULT_LIMIT,
  fleetScopeKey,
  fleetUnionTruncation,
  mergeDeviceCatalog,
  mergeStateCatalog,
  statesSeenIn,
  type FleetDeviceOption,
  type FleetErrorCode,
  type FleetReadTenants,
  type FleetServerFilter,
  type FleetTruncation,
} from "./fleetDiscovery";
import {
  FleetTenantWalker,
  initialFleetWalkSnapshot,
  servedTenantOf,
  type FleetPageFetcher,
  type FleetWalkSnapshot,
} from "./fleetWalker";

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
  /**
   * Bytes this session produced reached a SOURCE device's pane — measured at
   * the source (plan
   * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
   * A2). Always an object on a coord that serves it (`unknown/unprobed` when
   * nothing was observed); OPTIONAL here because an older coord omits it, and
   * an absent fact is UNKNOWN — rendered as nothing, never as "failed".
   */
  readableRemotely?: InteractivityFact;
  /** Bytes a source sent were written into this session's PTY — measured at
   * the TARGET. Same object, same absence rule. */
  writableRemotely?: InteractivityFact;
  /** Whether this session is a remote PTY surface at all. Absent on an older
   * coord. */
  interactiveSurface?: InteractiveSurface;
  /**
   * The tenant this row was SERVED under — stamped by the walk
   * (`stampServedTenant` in `fleetWalker.ts`), never on coord's wire. In a
   * merged multi-tenant view rows from different tenants sit side by side, and
   * attach / create must mint under THIS tenant: coord resolves a target within
   * the presented principal's tenant only. `null` when neither the envelope nor
   * the request named one (the runner's authority order then decides again).
   */
  servedTenantId?: string | null;
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
  /**
   * `false` ⇒ coord could not read its interactivity observations on this
   * call, and every fact is `unknown/events_unreadable`. ABSENT ⇒ a coord that
   * predates the facts altogether (see `servesInteractivity`).
   */
  interactivityEventsPresent?: boolean;
  /** The freshness window coord applied, in seconds (a compiled constant). */
  freshForSecs?: number;
}

export interface FleetSessionsQuery {
  deviceId?: string;
  state?: string;
  includeClosed?: boolean;
  /** Page size for each page of each walk, NOT a reachability control. */
  limit?: number;
  /**
   * The tenants whose fleets to read — ONE independent cursor walk each, merged
   * (plan
   * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
   * Phase 4). Each entry is sent as the command's `tenant` arg; `null` sends
   * none and the runner's own authority order picks (the machine pin, else the
   * default credential slot). It is the CREDENTIAL a read presents, not a query
   * parameter — coord scopes rows to the principal's tenant only, and
   * fingerprints it into the cursor, which is why a union is N walks rather than
   * one wider one. Absent ⇒ `[null]`, the single pre-selector read.
   */
  tenants?: readonly (string | null)[];
}

/**
 * Why an empty list is empty. The picker must never render "no remote sessions"
 * without knowing which of these produced the zero — that is the difference
 * between an observation and an absence of evidence.
 */
export type FleetEmptyReason =
  | "not-loaded" // no read has completed yet
  | "error" // the read failed (every tenant's, in a merged view); `error` carries the reason
  | "partial" // merged: some tenants answered with zero rows, others FAILED — unknown for those
  | "observed-empty"; // coord answered, with zero rows and no degradation

/**
 * One tenant whose walk carries an error — a failed read, or a walk that cannot
 * advance (`walkStalled`). Listed per tenant so one tenant's 401 never blanks,
 * or hides behind, another's rows.
 */
export interface FleetTenantFailure {
  /** The tenant the walk ASKED for (`null` = the runner's own default). */
  requestedTenant: string | null;
  error: string;
  errorCode: FleetErrorCode | null;
  /** The walk answered and only its next page is out of reach. */
  walkStalled: boolean;
}

export interface UseFleetSessionsResult {
  /** True when more than one tenant is walked — the union rules apply. */
  merged: boolean;
  /**
   * Every row the walks have served for the CURRENT scope, accumulated across
   * pages, deduplicated by `sessionId`, and stamped with `servedTenantId`. A
   * restart replaces a walk's rows wholesale — carrying rows across a scope
   * change would build a list no filter set describes.
   */
  sessions: FleetSession[];
  /** Each walk's own state, in the order `tenants` asked for them. */
  walks: FleetWalkSnapshot[];
  /**
   * The envelope of each walk's last successful page, keyed by the tenant its
   * rows were served under — what a row's own degraded flags are read from.
   */
  envelopes: ReadonlyMap<string | null, FleetSessionsResponse>;
  /** A page-ONE read is in flight on some walk — the list may be blank or stale. */
  loading: boolean;
  /** A SUBSEQUENT page is in flight — the rows on screen stay valid. */
  loadingMore: boolean;
  /**
   * The failure to show IN PLACE OF the list. Single walk: its error. Merged:
   * set only when EVERY tenant's read failed — a partial failure is in
   * `failures`, beside the rows the other tenants served, never in place of
   * them.
   */
  error: string | null;
  /**
   * coord's stable machine code for that failure, or null when it carried none
   * (a transport error), there was none, or the view is merged (each failure's
   * code is on its own `failures` entry). `cursor_scope_mismatch` never reaches
   * here: it is a restart, not an error.
   */
  errorCode: FleetErrorCode | null;
  /**
   * True when `error` describes a walk that cannot ADVANCE rather than a read
   * that FAILED — coord answered, the rows are current, and only the next page
   * is out of reach. The two must not share a banner. Always false when merged.
   */
  walkStalled: boolean;
  /** Every walk carrying an error, per tenant. The merged view's error strip. */
  failures: FleetTenantFailure[];
  /**
   * Set when `sessions` is empty, saying WHY. `observed-empty` is the only
   * value that licenses the words "no sessions" unqualified; `partial` licenses
   * them only scoped to the tenants that answered and naming those that did
   * not; the others are UNKNOWN.
   */
  emptyReason: FleetEmptyReason | null;
  /**
   * True when coord served at least one field degraded on any walk. A caller
   * must not present a degraded FIELD as observed while this is set.
   */
  degraded: boolean;
  /** Completeness over the union (`fleetUnionTruncation`). */
  truncation: FleetTruncation;
  /** Which tenants answered and which failed — the empty message's scope. */
  readTenants: FleetReadTenants;
  /** Distinct tenants the served rows were scoped to, in walk order. */
  servedTenants: string[];
  /**
   * Every device seen across the reads this hook has made, accumulated across
   * every tenant of the current tenant set.
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
   * The query the served rows were actually served for, or null before any
   * read completed.
   *
   * This exists because the caller's filter state and the last response are
   * INDEPENDENT: the moment a filter changes, the component's own filter object
   * describes a request in flight while the rows still belong to the previous
   * one. A caller that reported "coord truncated this read at {its own limit}"
   * would then be stating something false — and a failed refetch makes that
   * permanent, since the old rows are deliberately kept and `loading` returns to
   * false. Anything said ABOUT the served rows must be said with this, never
   * with the caller's pending filters. Every walk shares one filter set.
   */
  appliedQuery: FleetServerFilter | null;
  /** Pages accepted since the last restart, summed over walks. */
  pagesLoaded: number;
  /**
   * True when some walk holds a cursor coord handed back — more rows are
   * genuinely REACHABLE, not merely unserved. False is only ever "complete as
   * of those reads".
   */
  hasMore: boolean;
  /** Restart every walk from coord's first page, with no cursor. */
  refresh: () => Promise<void>;
  /** Restart ONE tenant's walk — the per-tenant retry. */
  refreshTenant: (requestedTenant: string | null) => Promise<void>;
  /** Fetch the next page of every walk that has a cursor. A no-op when none does. */
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
 * Group sessions by device, most recently ACTIVE first within each device, and
 * the caller's own device first overall.
 *
 * Activity order is imposed HERE rather than inherited from coord, because
 * coord no longer serves it. Since qontinui-coord#2085 the fleet walk is
 * `started_at DESC`: a heartbeat rewrites `last_heartbeat_at` (every 15 s by
 * default, the runner's `DEFAULT_HEARTBEAT_SECS`), and a cursor walking a key
 * that moves silently drops rows. So rows ARRIVE in start order, and without
 * this sort a loaded session started three days ago and heartbeating now would
 * sit below one started an hour ago that went quiet. See
 * {@link activityInstant} for the key.
 *
 * It orders what is LOADED, and nothing else. A row the walk has not reached
 * cannot be placed. Before #2085 a long-running, currently-active session
 * tended to arrive on page 1; now it may sit on a page not yet loaded.
 * Reordering cannot fix that — only walking further can.
 *
 * The local-device-first ordering is deliberate: the picker exists to reach
 * REMOTE sessions, and putting the operator's own box at the top is what makes
 * the remainder legible as "everywhere else".
 */
export interface FleetDeviceGroup {
  /**
   * The group's identity: the device id, or — when grouping by tenant too —
   * `<deviceId>|<tenant>`. Stable across reads; use it as the React key.
   */
  key: string;
  deviceId: string;
  /**
   * The tenant this group's rows were served under (their `servedTenantId`).
   * When grouping by tenant every row shares it; otherwise it is the first
   * row's, which in a single-tenant read is every row's.
   */
  tenantId: string | null;
  /** Best available human label, falling back to the id. */
  label: string;
  isCallerDevice: boolean;
  sessions: FleetSession[];
}

/**
 * One of coord's instants in a form every JS engine must parse.
 *
 * coord serializes `DateTime<Utc>` as RFC 3339 with MICROSECOND precision
 * (`2026-09-12T10:00:00.123456Z`). More than three fractional digits is
 * outside ECMAScript's date-time string format, so whether `Date.parse`
 * accepts it is up to the engine. V8 does; WebKit is not guaranteed to, and a
 * NaN there would silently sink every row to the bottom of its group.
 * Trimming to milliseconds yields the standard form, which only drops the
 * sub-millisecond digits.
 */
export function normalizeCoordInstant(v: string): string {
  return v.replace(/(\.\d{3})\d+/, "$1");
}

/**
 * The instant a row is ordered by within its device: its last heartbeat if that
 * parses, else its start. A row with NEITHER usable sorts last — a row coord
 * could not date is not evidence of recent activity.
 */
export function activityInstant(s: FleetSession): number {
  for (const v of [s.lastHeartbeatAt, s.startedAt]) {
    const t = v ? Date.parse(normalizeCoordInstant(v)) : Number.NaN;
    if (!Number.isNaN(t)) return t;
  }
  return Number.NEGATIVE_INFINITY;
}

/**
 * Newest activity first, with the session id as a total tiebreak so equal
 * instants do not reorder between renders. Two undated rows give `-Inf - -Inf`,
 * which is `NaN` and falsy, so they fall through to the id as well. The id
 * comparison is a plain code-unit one, so the order does not depend on locale.
 */
function byActivityDesc(a: FleetSession, b: FleetSession): number {
  const byInstant = activityInstant(b) - activityInstant(a);
  if (byInstant) return byInstant;
  if (a.sessionId === b.sessionId) return 0;
  return a.sessionId < b.sessionId ? -1 : 1;
}

/**
 * `byTenant` splits a device's rows by the tenant they were served under — the
 * MERGED view's grouping (Phase 4). A device bound to two tenants then shows as
 * two groups, so a group's "New terminal" names one tenant to mint under and
 * its count is one tenant's, never a sum the header does not explain.
 */
export function groupByDevice(
  sessions: FleetSession[],
  opts?: { byTenant?: boolean },
): FleetDeviceGroup[] {
  const byTenant = opts?.byTenant === true;
  const byKey = new Map<string, FleetSession[]>();
  for (const s of sessions) {
    const key = byTenant ? `${s.deviceId}|${s.servedTenantId ?? ""}` : s.deviceId;
    const list = byKey.get(key);
    if (list) list.push(s);
    else byKey.set(key, [s]);
  }

  const groups: FleetDeviceGroup[] = [];
  for (const [key, rows] of byKey) {
    const first = rows[0];
    groups.push({
      key,
      deviceId: first.deviceId,
      tenantId: first.servedTenantId ?? null,
      label: deviceLabel(first),
      isCallerDevice: first.isCallerDevice,
      sessions: [...rows].sort(byActivityDesc),
    });
  }

  // Caller's device first, then by label so the order is stable across reads
  // (device ids are opaque, so sorting by them would look arbitrary), then by
  // tenant so one device's tenant groups sit together in a fixed order.
  groups.sort((a, b) => {
    if (a.isCallerDevice !== b.isCallerDevice) return a.isCallerDevice ? -1 : 1;
    return a.label.localeCompare(b.label) || (a.tenantId ?? "").localeCompare(b.tenantId ?? "");
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

/** Rows of several walks as one list, first occurrence of a `sessionId` wins.
 * A session lives in exactly one tenant, so a repeat is a guard, not a merge. */
function unionSessions(walks: readonly FleetWalkSnapshot[]): FleetSession[] {
  if (walks.length === 1) return walks[0].walk.sessions;
  const seen = new Set<string>();
  const out: FleetSession[] = [];
  for (const w of walks) {
    for (const s of w.walk.sessions) {
      if (seen.has(s.sessionId)) continue;
      seen.add(s.sessionId);
      out.push(s);
    }
  }
  return out;
}

/** A walk that FAILED (not merely stalled) and so answered nothing current. */
function walkFailed(w: FleetWalkSnapshot): boolean {
  return w.error !== null && !w.walkStalled;
}

/**
 * Fold N walk snapshots into what the picker renders — every count, flag and
 * completeness claim re-derived over the UNION (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 4).
 *
 * Pure and exported because the honesty rules are the point: one tenant's
 * failure must not blank the others (`error` stays null until EVERY walk
 * failed), and must not let an empty union read as an empty fleet
 * (`emptyReason` is `partial`, never `observed-empty`, while any tenant is
 * unanswered). A single walk folds to exactly what that walk alone says, so a
 * single-tenant view is unchanged.
 */
export function mergeFleetWalks(
  walks: readonly FleetWalkSnapshot[],
): Omit<
  UseFleetSessionsResult,
  "deviceCatalog" | "stateCatalog" | "refresh" | "refreshTenant" | "loadMore"
> {
  const merged = walks.length > 1;
  const sessions = unionSessions(walks);
  const single = walks.length === 1 ? walks[0] : null;

  const envelopes = new Map<string | null, FleetSessionsResponse>();
  const servedTenants: string[] = [];
  for (const w of walks) {
    if (!w.response) continue;
    const served = servedTenantOf(w.response, w.requestedTenant);
    envelopes.set(served, w.response);
    if (served !== null && !servedTenants.includes(served)) servedTenants.push(served);
  }

  const failures: FleetTenantFailure[] = walks
    .filter((w) => w.error !== null)
    .map((w) => ({
      requestedTenant: w.requestedTenant,
      error: w.error as string,
      errorCode: w.errorCode,
      walkStalled: w.walkStalled,
    }));

  const allFailed = walks.length > 0 && walks.every(walkFailed);
  let error: string | null;
  let errorCode: FleetErrorCode | null;
  let walkStalled: boolean;
  if (single) {
    ({ error, errorCode, walkStalled } = single);
  } else {
    error = allFailed
      ? `No tenant's fleet read succeeded — each of the ${walks.length} tenants' errors is listed above.`
      : null;
    errorCode = null;
    walkStalled = false;
  }

  let emptyReason: FleetEmptyReason | null;
  if (single) {
    emptyReason = emptyReasonFor(single.loaded, single.error, sessions);
  } else if (sessions.length > 0) {
    emptyReason = null;
  } else if (allFailed) {
    emptyReason = "error";
  } else if (walks.some((w) => !w.loaded && !walkFailed(w))) {
    emptyReason = "not-loaded";
  } else if (walks.some(walkFailed)) {
    emptyReason = "partial";
  } else {
    emptyReason = "observed-empty";
  }

  const readTenants: FleetReadTenants = {
    answered: walks
      .filter((w) => w.response !== null && !walkFailed(w))
      .map((w) => servedTenantOf(w.response, w.requestedTenant)),
    failed: walks.filter(walkFailed).map((w) => w.requestedTenant),
  };

  return {
    merged,
    sessions,
    walks: [...walks],
    envelopes,
    loading: walks.some((w) => w.loading),
    loadingMore: walks.some((w) => w.loadingMore),
    error,
    errorCode,
    walkStalled,
    failures,
    emptyReason,
    degraded: walks.some((w) => isDegraded(w.response)),
    // In a MERGED read, a walk that failed or is re-reading has no CURRENT
    // envelope: the one it holds belongs to a previous read (seeded, or the
    // read the refresh failed to replace). Classified as unanswered (`unknown`)
    // so the union cannot read `none` — complete — over a tenant that has not
    // positively said "last page" for THIS read. A single walk keeps its own
    // envelope, so a single-tenant view reads exactly as it always did.
    truncation: fleetUnionTruncation(
      walks.map((w) => ({
        response: merged && (walkFailed(w) || w.loading) ? null : w.response,
        loaded: w.walk.sessions.length,
        canAdvance: w.walk.nextCursor !== null,
      })),
    ),
    readTenants,
    servedTenants,
    appliedQuery: walks.find((w) => w.appliedQuery !== null)?.appliedQuery ?? null,
    pagesLoaded: walks.reduce((n, w) => n + w.walk.pages, 0),
    hasMore: walks.some((w) => w.walk.nextCursor !== null),
  };
}

/** The production page fetch: the `fleet_sessions_list` command. */
const invokeFleetPage: FleetPageFetcher = (req) =>
  invoke<FleetSessionsResponse>("fleet_sessions_list", {
    args: {
      deviceId: req.deviceId,
      state: req.state,
      includeClosed: req.includeClosed,
      limit: req.limit,
      cursor: req.cursor,
      tenant: req.tenant,
    },
  });

/** The default tenant set: one read, no tenant sent. */
const DEFAULT_TENANTS: readonly (string | null)[] = [null];

/** The walk snapshots of ONE scope, tagged with that scope's key. */
interface ScopedSnapshots {
  key: string;
  list: FleetWalkSnapshot[];
}

/**
 * Read-only discovery of the fleet's sessions: one {@link FleetTenantWalker}
 * per requested tenant, folded by {@link mergeFleetWalks}.
 *
 * ## The walks
 *
 * Every walk RESTARTS on mount, on `refresh`, and whenever the SCOPE changes —
 * `deviceId` / `state` / `includeClosed`, the three coord validates a cursor
 * against, and the tenant SET, since coord fingerprints the tenant in from the
 * principal. `limit` is deliberately not one of them: coord's cursor survives a
 * changed page size, so resizing a page must not throw away pages already
 * loaded; each walker reads the size through `limitRef` at call time.
 *
 * A scope change builds NEW walkers and disposes the old ones, so a response
 * still in flight for the previous scope is discarded rather than merged. Each
 * new walker is SEEDED with its tenant's previous rows: a filter change keeps
 * the list on screen while it re-reads (the picker says so), and a tenant no
 * longer requested drops out at once.
 */
export function useFleetSessions(opts?: FleetSessionsQuery): UseFleetSessionsResult {
  const deviceId = opts?.deviceId ?? null;
  const state = opts?.state ?? null;
  const includeClosed = opts?.includeClosed ?? false;
  const limit = opts?.limit ?? FLEET_DEFAULT_LIMIT;
  const tenants = opts?.tenants ?? DEFAULT_TENANTS;
  /** The tenant set as one comparable string — callers pass fresh arrays. */
  const tenantsKey = JSON.stringify(tenants);
  /**
   * The walks' scope, as one comparable string, named EXPLICITLY in the
   * restart effect's dependencies: every per-tenant cursor scope, in order.
   */
  const scopeKey = JSON.stringify(
    tenants.map((tenantId) => fleetScopeKey({ deviceId, state, includeClosed, tenantId })),
  );

  const [snapshots, setSnapshots] = useState<ScopedSnapshots>(() => ({
    key: scopeKey,
    list: tenants.map(initialFleetWalkSnapshot),
  }));
  const [deviceCatalog, setDeviceCatalog] = useState<FleetDeviceOption[]>([]);
  const [stateCatalog, setStateCatalog] = useState<string[]>([]);

  /** The live walkers, for `refresh` / `loadMore` (event handlers, not render). */
  const walkersRef = useRef<FleetTenantWalker[]>([]);
  /** The snapshots as of the last commit — what a new scope's walkers seed from. */
  const snapshotsRef = useRef(snapshots);
  /**
   * The tenant set the catalogues were accumulated under. They deliberately
   * survive a filter change — but not a TENANT change: offering another
   * tenant's devices as filter options would name devices no read can return.
   */
  const catalogTenantsRef = useRef(tenantsKey);

  /**
   * The page size as of the CALL. Synced in an effect declared ABOVE the
   * restart effect: React runs effects in declaration order, so a commit that
   * changes the page size and the scope together syncs this first and the
   * restart reads the new size.
   */
  const limitRef = useRef(limit);
  useEffect(() => {
    limitRef.current = limit;
  }, [limit]);
  useEffect(() => {
    snapshotsRef.current = snapshots;
  }, [snapshots]);
  /**
   * The tenant set as of the last commit, read by the restart effect below. The
   * effect is keyed on `tenantsKey` (callers pass a fresh array each render, so
   * the array's identity is no trigger); this ref hands it the array itself.
   * Declared above the restart effect for the same ordering reason as `limitRef`.
   */
  const tenantsRef = useRef(tenants);
  useEffect(() => {
    tenantsRef.current = tenants;
  }, [tenants]);

  /** One walker's change, folded into the snapshot list of ITS scope. The
   * first change of a new scope replaces the previous scope's list. */
  const publish = useCallback(
    (
      key: string,
      scopeTenants: readonly (string | null)[],
      index: number,
      snap: FleetWalkSnapshot,
    ) => {
      setSnapshots((prev) => {
        const base =
          prev.key === key
            ? prev.list
            : scopeTenants.map(
                (t) =>
                  prev.list.find((s) => s.requestedTenant === t) ?? initialFleetWalkSnapshot(t),
              );
        const list = base.slice();
        list[index] = snap;
        return { key, list };
      });
    },
    [],
  );

  /**
   * Accumulate the catalogues here — on a page, which is an event — rather than
   * in an effect over the snapshots: the catalogues are a property of the read
   * SEQUENCE, which this hook owns.
   */
  const acceptRows = useCallback((key: string, rows: FleetSession[]) => {
    const fresh = catalogTenantsRef.current !== key;
    catalogTenantsRef.current = key;
    const seen = devicesSeenIn(rows);
    if (fresh || seen.length > 0) {
      setDeviceCatalog((prev) => mergeDeviceCatalog(fresh ? [] : prev, seen));
    }
    const states = statesSeenIn(rows);
    if (fresh || states.length > 0) {
      setStateCatalog((prev) => mergeStateCatalog(fresh ? [] : prev, states));
    }
  }, []);

  // Runs on mount and whenever the SCOPE changes. `scopeKey` covers every
  // input below; they are named as well so the trigger needs no lint
  // suppression, and they move together by construction.
  useEffect(() => {
    const scopeTenants = [...tenantsRef.current];
    const previous = snapshotsRef.current.list;
    const walkers = scopeTenants.map(
      (tenantId, index) =>
        new FleetTenantWalker({
          scope: { deviceId, state, includeClosed, tenantId },
          seed: previous.find((s) => s.requestedTenant === tenantId),
          fetchPage: invokeFleetPage,
          pageSize: () => limitRef.current,
          onChange: (snap) => publish(scopeKey, scopeTenants, index, snap),
          onRows: (rows) => acceptRows(tenantsKey, rows),
        }),
    );
    walkersRef.current = walkers;
    // A TENANT-set change empties both filter catalogues NOW, not when the
    // first page arrives: until then they would offer the previous tenants'
    // devices and states as options no read of the new set can return — and
    // a first page that fails would leave them standing indefinitely.
    if (catalogTenantsRef.current !== tenantsKey) {
      catalogTenantsRef.current = tenantsKey;
      setDeviceCatalog([]);
      setStateCatalog([]);
    }
    for (const w of walkers) void w.restart();
    return () => {
      for (const w of walkers) w.dispose();
    };
  }, [scopeKey, tenantsKey, deviceId, state, includeClosed, publish, acceptRows]);

  const refresh = useCallback(async () => {
    await Promise.all(walkersRef.current.map((w) => w.restart()));
  }, []);
  const refreshTenant = useCallback(async (requestedTenant: string | null) => {
    await Promise.all(
      walkersRef.current
        .filter((w) => w.snapshot.requestedTenant === requestedTenant)
        .map((w) => w.restart()),
    );
  }, []);
  const loadMore = useCallback(async () => {
    await Promise.all(walkersRef.current.filter((w) => w.canAdvance).map((w) => w.loadMore()));
  }, []);

  // Until the new scope's first walker reports, the list is still the previous
  // scope's — exactly the rows the previous read served, which is what the
  // picker's "re-reading" banner describes once `loading` is set.
  const folded = useMemo(() => mergeFleetWalks(snapshots.list), [snapshots]);

  return {
    ...folded,
    deviceCatalog,
    stateCatalog,
    refresh,
    refreshTenant,
    loadMore,
  };
}
