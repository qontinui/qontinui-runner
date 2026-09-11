import type { FleetSession, FleetSessionsResponse } from "./useFleetSessions";

/**
 * Discovery logic for the Fleet session picker — paging, filtering and the
 * honesty rules that go with them (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 2).
 *
 * ## Why this file exists
 *
 * The picker asked coord for its DEFAULT page and rendered `truncated` as a
 * passive five-word note ("· more not shown"). On a real tenant that read
 * returned exactly 100 rows — the default page size — so the sessions past the
 * hundredth were not merely unlisted, they were unreachable: no page control,
 * no filter, no search. The list said it was incomplete and then offered
 * nothing to do about it, which is the failure mode `ux-priorities` calls
 * being honest about uncertainty without being actionable about it.
 *
 * ## Two filtering planes, and the difference is load-bearing
 *
 * coord's `GET /coord/sessions/fleet` takes `device_id`, `state`,
 * `include_closed` and `limit` — nothing else. So exactly two things can reach
 * a row that a truncated page did not serve: raising `limit`, and narrowing
 * with `device_id` / `state`, both of which coord applies in SQL. Text search
 * is **client-side over the rows already loaded** and therefore CANNOT reach
 * past truncation. The UI must say which plane a given control is on, because
 * a text box that silently searches only the first 100 of 400 sessions is a
 * more confident lie than the note it replaced.
 *
 * Everything here is pure so those rules are testable without a live coord;
 * `fleetDiscovery.test.ts` is the guard.
 */

/**
 * coord's hard per-read ceiling — `MAX_LIMIT` in
 * `qontinui-coord/crates/coord/src/session_fleet.rs`. A larger `limit` is
 * clamped server-side, so asking for more is not a way past it; narrowing is.
 */
export const FLEET_MAX_LIMIT = 500;

/**
 * coord's `DEFAULT_LIMIT` — what a call naming no `limit` gets. The picker
 * requests it EXPLICITLY rather than sending nothing, so the page size it is
 * showing is a number the UI knows and can report, instead of a server default
 * it would have to guess at when deciding what "more" means.
 */
export const FLEET_DEFAULT_LIMIT = 100;

/** The ladder the "Load more" control walks, ending at coord's ceiling. */
export const FLEET_LIMIT_LADDER: readonly number[] = [FLEET_DEFAULT_LIMIT, 250, FLEET_MAX_LIMIT];

/**
 * The next page size to ask coord for, or `null` when the current one is
 * already at (or past) coord's ceiling and no larger read exists.
 *
 * Returning `null` rather than a bigger number is the point: at the ceiling the
 * honest answer is "no larger read is possible, narrow instead", and a control
 * that kept offering a bigger page would be promising something coord clamps.
 */
export function nextFleetLimit(current: number): number | null {
  for (const step of FLEET_LIMIT_LADDER) {
    if (step > current) return step;
  }
  return current < FLEET_MAX_LIMIT ? FLEET_MAX_LIMIT : null;
}

/**
 * What the picker must say about completeness, and what it can offer.
 *
 * - `none` — coord served every matching row; the list is complete for the
 *   current server-side filters.
 * - `more-available` — coord had more rows than it served AND a larger read
 *   exists. `nextLimit` is the page size the control should ask for.
 * - `at-ceiling` — coord had more rows than it served and `limit` is already at
 *   `FLEET_MAX_LIMIT`. No larger read exists; only a narrower one does.
 * - `unknown` — no successful read has completed, so completeness is not
 *   established. Explicitly NOT `none`: an absent answer is not a complete one.
 */
export type FleetTruncation =
  | { kind: "none" }
  | { kind: "unknown" }
  | { kind: "more-available"; shown: number; limit: number; nextLimit: number; message: string }
  | { kind: "at-ceiling"; shown: number; limit: number; message: string };

/**
 * Classify the last read's completeness.
 *
 * `limit` is the page size that read was made with — the picker always names
 * one, so this is never inferred from the row count (a tenant with exactly 100
 * live sessions and a tenant truncated at 100 look identical by count alone;
 * only coord's `truncated` flag separates them, which is why it is the input).
 */
export function fleetTruncation(
  response: FleetSessionsResponse | null,
  limit: number,
): FleetTruncation {
  if (!response) return { kind: "unknown" };
  if (!response.truncated) return { kind: "none" };

  const shown = response.sessions.length;
  const next = nextFleetLimit(limit);
  if (next === null) {
    return {
      kind: "at-ceiling",
      shown,
      limit,
      message:
        `Showing ${shown} sessions — coord's per-read ceiling of ${FLEET_MAX_LIMIT}. ` +
        `More sessions matched than can be served in one read. Narrow by device or state ` +
        `to reach them; the text box only filters rows already loaded.`,
    };
  }
  return {
    kind: "more-available",
    shown,
    limit,
    nextLimit: next,
    message:
      `Showing the first ${shown} of more — coord truncated this read at ${limit}. ` +
      `Load ${next}, or narrow by device or state.`,
  };
}

/** Split a search box's contents into the terms a row must match. */
export function fleetSearchTerms(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
}

/**
 * Every field of a row a text search may match, lowercased and joined.
 *
 * Ids are included deliberately: a session is often referred to by its coord id
 * or its harness id (in a log line, a gate, a peer's message), and pasting one
 * into the box is the fastest way to find the row it belongs to.
 *
 * A `null` field contributes nothing — coord's nulls are UNKNOWN, and matching
 * the literal string "null" would invent a value for them.
 */
export function fleetSessionHaystack(s: FleetSession): string {
  return [
    s.sessionId,
    s.deviceId,
    s.deviceHostname,
    s.deviceDisplayName,
    s.claudeCodeSessionId,
    s.sessionKind,
    s.intent,
    s.state,
    s.sessionStatus,
    s.workUnitSlug,
    s.repo,
    s.branch,
    s.provider,
    s.correlationTopic,
  ]
    .filter((v): v is string => typeof v === "string" && v.length > 0)
    .join(" ")
    .toLowerCase();
}

/**
 * A row matches when EVERY term is a substring of its haystack — AND, not OR,
 * so adding a word always narrows. `qontinui-web main` finding the one session
 * on that repo and branch is the case this shape exists for; an OR would widen
 * to every session on either, which is the opposite of what typing more means.
 */
export function fleetSessionMatchesTerms(s: FleetSession, terms: string[]): boolean {
  if (terms.length === 0) return true;
  const hay = fleetSessionHaystack(s);
  return terms.every((t) => hay.includes(t));
}

/** Apply a text query to the loaded page. Client-side — see the module doc. */
export function filterFleetSessions(sessions: FleetSession[], query: string): FleetSession[] {
  const terms = fleetSearchTerms(query);
  if (terms.length === 0) return sessions;
  return sessions.filter((s) => fleetSessionMatchesTerms(s, terms));
}

/** One selectable device in the picker's device filter. */
export interface FleetDeviceOption {
  deviceId: string;
  label: string;
  isCallerDevice: boolean;
}

/**
 * Accumulate the devices seen across reads.
 *
 * Merging rather than recomputing is required, not tidiness: selecting a device
 * sends `device_id` to coord, so the very next response contains only that
 * device — recomputing the options from it would leave a dropdown holding one
 * entry and no way back to the others. The catalogue therefore only ever grows
 * within a mounted picker.
 *
 * A better label wins over a worse one: `deviceLabel` falls back to a truncated
 * id when coord served the identity columns degraded, and a later
 * non-degraded read must be allowed to replace that placeholder.
 */
export function mergeDeviceCatalog(
  prev: FleetDeviceOption[],
  seen: FleetDeviceOption[],
): FleetDeviceOption[] {
  const byId = new Map(prev.map((d) => [d.deviceId, d]));
  for (const d of seen) {
    const existing = byId.get(d.deviceId);
    if (!existing) {
      byId.set(d.deviceId, d);
      continue;
    }
    const existingIsFallback = existing.label.startsWith("device ");
    const incomingIsFallback = d.label.startsWith("device ");
    byId.set(d.deviceId, {
      deviceId: d.deviceId,
      label: existingIsFallback && !incomingIsFallback ? d.label : existing.label,
      isCallerDevice: existing.isCallerDevice || d.isCallerDevice,
    });
  }
  return [...byId.values()].sort((a, b) => {
    if (a.isCallerDevice !== b.isCallerDevice) return a.isCallerDevice ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
}

/**
 * coord's `coord.sessions.state` vocabulary — the LIVENESS axis
 * (`SessionState` in `qontinui-coord/crates/coord/src/sessions.rs`).
 *
 * Not to be confused with `session_status`, the orthogonal WORK axis
 * (`working | blocked | stalled | waiting_human | finished`). coord's fleet
 * route filters on `state` only, so only these values are offered as a
 * server-side filter; the work axis is reachable through the text box, which is
 * client-side and says so.
 */
export const FLEET_STATE_VOCABULARY: readonly string[] = [
  "expected",
  "active",
  "pending_resolution",
  "stale",
  "closed",
];

/**
 * The states to offer: the known vocabulary, plus anything coord actually
 * served that is not in it.
 *
 * The union matters because the vocabulary is enforced in Rust rather than by a
 * DB constraint precisely so it can evolve without a migration — a runner
 * pinned to a stale list would silently hide a state coord had started
 * emitting, which is the same class of quiet omission this phase is fixing.
 */
export function fleetStateOptions(sessions: FleetSession[]): string[] {
  const extra = new Set<string>();
  for (const s of sessions) {
    const v = s.state?.trim();
    if (v && !FLEET_STATE_VOCABULARY.includes(v)) extra.add(v);
  }
  return [...FLEET_STATE_VOCABULARY, ...[...extra].sort()];
}

/** The server-side half of the picker's filter state — what coord is asked. */
export interface FleetServerFilter {
  deviceId: string | null;
  state: string | null;
  includeClosed: boolean;
  limit: number;
}

export const DEFAULT_FLEET_SERVER_FILTER: FleetServerFilter = {
  deviceId: null,
  state: null,
  includeClosed: false,
  limit: FLEET_DEFAULT_LIMIT,
};

/**
 * True when anything narrows or widens the read away from the picker's default.
 * Drives whether a "Clear filters" control is offered at all — an always-present
 * reset on an unfiltered list is clutter, and a missing one on a filtered list
 * is a trap (`ux-priorities`: no-surprise reversibility).
 */
export function hasActiveFleetFilter(server: FleetServerFilter, text: string): boolean {
  return (
    server.deviceId !== null ||
    server.state !== null ||
    server.includeClosed ||
    server.limit !== FLEET_DEFAULT_LIMIT ||
    fleetSearchTerms(text).length > 0
  );
}

/**
 * True when a filter is NARROWING the read — the only kind that can explain an
 * empty list.
 *
 * Deliberately narrower than `hasActiveFleetFilter`: a raised `limit` and
 * `includeClosed` both WIDEN the read, so blaming an empty result on "these
 * filters" when the only non-default is a larger page would be a false
 * explanation of an honest zero.
 */
export function hasNarrowingFleetFilter(server: FleetServerFilter, text: string): boolean {
  return server.deviceId !== null || server.state !== null || fleetSearchTerms(text).length > 0;
}

/**
 * The one-line count summary, which must never present a filtered subset as a
 * total.
 *
 * `matched` is what is on screen; `loaded` is what coord served. When they
 * differ the line says so, because "12 sessions" over a 400-session tenant
 * filtered to 12 is exactly the confident-total shape the truncation note was
 * already getting wrong.
 */
export function fleetCountSummary(args: {
  matched: number;
  loaded: number;
  devices: number;
  remote: number;
}): string {
  const { matched, loaded, devices, remote } = args;
  const head =
    matched === loaded
      ? `${loaded} session${loaded === 1 ? "" : "s"}`
      : `${matched} of ${loaded} loaded`;
  const parts = [`${head} on ${devices} device${devices === 1 ? "" : "s"}`];
  if (remote > 0) parts.push(`${remote} remote`);
  return parts.join(" · ");
}

/**
 * Why a filtered list is empty, when the underlying read was NOT empty.
 *
 * Returns `null` when the loaded page was itself empty — that case belongs to
 * `emptyReasonFor`, which distinguishes a failed read from an observed-empty
 * fleet. Splitting the two keeps each answer true: "your filter matched
 * nothing" and "coord returned nothing" are different facts and must not share
 * a message.
 */
export function fleetFilteredOutMessage(
  loaded: number,
  matched: number,
  text: string,
): string | null {
  if (loaded === 0 || matched > 0) return null;
  const q = text.trim();
  if (!q) return null;
  return `No loaded session matches “${q}”. ${loaded} session${loaded === 1 ? " is" : "s are"} loaded; this box filters only those.`;
}

/**
 * A filter pair that can only starve the list, or null when there is none.
 *
 * `state=closed` with closed sessions excluded is the one such pair coord's
 * route can produce: `include_closed=false` appends `AND s.closed_at IS NULL`
 * to the SQL, so the two clauses very nearly exclude each other and the picker
 * shows an empty list that looks like a fact about the fleet.
 *
 * Reported rather than auto-corrected: silently flipping a control the operator
 * did not touch is the surprise `ux-priorities` no-surprise-reversibility rules
 * out, and the toggle that fixes it is one click away in the same row.
 */
export function fleetFilterConflict(server: FleetServerFilter): string | null {
  if (server.state === "closed" && !server.includeClosed) {
    return "state=closed while closed sessions are excluded — coord filters those out, so this pair returns (almost) nothing. Turn on “closed” to see them.";
  }
  return null;
}
