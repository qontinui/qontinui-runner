import type { FleetSession, FleetSessionsResponse } from "./useFleetSessions";

/**
 * Discovery logic for the Fleet session picker — the cursor walk, filtering and
 * the honesty rules that go with them (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phases 2 and 5a).
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
 * ## The contract this file codes against (Phase 5a)
 *
 * `GET /coord/sessions/fleet` used to have no offset and no cursor, so the only
 * thing a client could do with `truncated` was ask for a BIGGER page, up to a
 * hard ceiling — a ladder, not pagination. That route now does keyset
 * pagination and `truncated` is GONE. The response carries:
 *
 * - `nextCursor` — always present as a KEY, `null` on the last page. So
 *   `"nextCursor" in body` never separates a last page from an older server,
 *   and only the VALUE decides. This is the field the picker classifies on.
 * - `limit` — the EFFECTIVE, post-clamp page size. It is a statement about the
 *   REQUEST; `nextCursor` is a statement about the DATA. They can never
 *   contradict each other, and nothing here derives one from the other.
 *
 * The cursor is **opaque by contract**: it is sent back verbatim and compared
 * for equality, never constructed, parsed, inspected or reinterpreted. The walk
 * re-sends it as `?cursor=` with the IDENTICAL `device_id` / `state` /
 * `include_closed` — changing any of those mid-walk is a `400
 * cursor_scope_mismatch` and the walk must restart with no cursor. `limit` is
 * deliberately NOT part of that scope fingerprint (resizing a page changes the
 * slice, not the sequence), which is why [`fleetScopeKey`] omits it.
 *
 * ## Two filtering planes, and the difference is still load-bearing
 *
 * `device_id` and `state` are applied by coord in SQL, so they change WHICH
 * rows the walk enumerates. Text search is **client-side over the rows already
 * fetched**. With a cursor that set GROWS — every "load more" widens what the
 * box can see — so it is no longer a hard ceiling, but at any moment it is
 * still a subset, and the UI must say which plane a control is on. A text box
 * that silently searches only the first 100 of 400 sessions is a more confident
 * lie than the note it replaced.
 *
 * Everything here is pure so those rules are testable without a live coord;
 * `fleetDiscovery.test.ts` is the guard.
 */

/**
 * coord's hard per-read ceiling — `MAX_LIMIT` in
 * `qontinui-coord/crates/coord/src/session_fleet.rs`. A larger `limit` is
 * clamped server-side and the clamped value comes back on the response as
 * `limit`, so nothing here has to predict it: read it off the response.
 *
 * It is no longer a ceiling on REACHABILITY — it bounds one page, and the walk
 * has as many pages as coord has rows.
 */
export const FLEET_MAX_LIMIT = 500;

/**
 * coord's `DEFAULT_LIMIT` — what a call naming no `limit` gets. The picker
 * requests it EXPLICITLY rather than sending nothing, so the page size it is
 * showing is a number the UI knows before the first response, instead of a
 * server default it would have to guess at.
 */
export const FLEET_DEFAULT_LIMIT = 100;

/**
 * The walk's scope fingerprint — everything coord validates a cursor against.
 *
 * `limit` is deliberately absent: coord's cursor survives a changed page size,
 * so including it here would restart a walk that did not need restarting and
 * silently re-fetch every page already loaded.
 */
export interface FleetScope {
  deviceId: string | null;
  state: string | null;
  includeClosed: boolean;
}

/**
 * A stable key for a scope. Equal keys mean a cursor minted under one is valid
 * under the other; unequal keys mean the walk must restart with no cursor.
 */
export function fleetScopeKey(scope: FleetScope): string {
  return JSON.stringify([scope.deviceId, scope.state, scope.includeClosed]);
}

/** True when two scopes would accept each other's cursors. */
export function fleetScopesEqual(a: FleetScope, b: FleetScope): boolean {
  return fleetScopeKey(a) === fleetScopeKey(b);
}

/**
 * coord's cursor, normalised to "usable or absent".
 *
 * The value is OPAQUE: this reads its type and emptiness and nothing else. An
 * empty string is treated as absent because coord's own walk protocol says an
 * empty `cursor` is page one rather than an error — so sending one back would
 * silently restart the walk while the UI claimed to be advancing it.
 */
export function normalizeFleetCursor(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

/**
 * True when coord handed back the very cursor it was given.
 *
 * A keyset cursor encodes the last row of the page just served, so under the
 * real contract this can never happen — and that is exactly what makes it
 * useful: it is the signature of a cursor that was DROPPED somewhere between
 * this module and coord (an intermediary that does not forward the parameter,
 * say), which would otherwise show up as an infinite "load more" re-serving
 * page one for ever. Equality only; the cursor is never inspected.
 */
export function fleetCursorStalled(sent: string | null, received: string | null): boolean {
  return sent !== null && received !== null && sent === received;
}

/** What to say when the walk cannot advance because the cursor came back unchanged. */
export const FLEET_CURSOR_STALLED_MESSAGE =
  "coord returned the same page cursor it was given, so the walk cannot advance — " +
  "this list may be incomplete. The pages already loaded are shown.";

/**
 * The accumulated state of one walk.
 *
 * `sessions` spans every page accepted since the last restart, so it is the
 * number a completeness claim must be made with — the last RESPONSE only ever
 * holds its own page.
 */
export interface FleetWalk {
  sessions: FleetSession[];
  /** The cursor the next page must carry, or null when there is no next page. */
  nextCursor: string | null;
  /** Pages accepted since the last restart. */
  pages: number;
}

/** Stable identity so a consumer's `useMemo` is not defeated on every render. */
const NO_WALK_SESSIONS: FleetSession[] = [];

export const EMPTY_FLEET_WALK: FleetWalk = {
  sessions: NO_WALK_SESSIONS,
  nextCursor: null,
  pages: 0,
};

/** Which kind of read produced a page. */
export type FleetWalkMode = "restart" | "more";

/**
 * Accumulate rows across pages, keyed by `sessionId`.
 *
 * A keyset walk does not repeat a row, so the dedup is a GUARD rather than the
 * mechanism — but it is the guard that keeps a re-served page (a retry, a
 * cursor that did not advance, a row whose sort key moved under the walk) from
 * showing the same session twice. A repeat REPLACES in place: the later page is
 * the fresher read of that row, and moving it would reorder a list the operator
 * is looking at.
 *
 * Returns `prev` unchanged for an empty page so identity is preserved.
 */
export function accumulateFleetSessions(
  prev: FleetSession[],
  page: FleetSession[],
): FleetSession[] {
  if (page.length === 0) return prev;
  const at = new Map<string, number>();
  prev.forEach((s, i) => at.set(s.sessionId, i));
  const out = prev.slice();
  for (const s of page) {
    const i = at.get(s.sessionId);
    if (i === undefined) {
      at.set(s.sessionId, out.length);
      out.push(s);
    } else {
      out[i] = s;
    }
  }
  return out;
}

/**
 * Fold one response into the walk.
 *
 * `restart` REPLACES the accumulation — it is page one of a new sequence, and
 * carrying rows over from the previous scope would mix two different queries'
 * results into one list that no filter set describes.
 */
export function fleetWalkAccept(
  prev: FleetWalk,
  response: FleetSessionsResponse,
  mode: FleetWalkMode,
): FleetWalk {
  const page = response.sessions ?? [];
  const nextCursor = normalizeFleetCursor(response.nextCursor);
  if (mode === "restart") {
    return { sessions: accumulateFleetSessions(NO_WALK_SESSIONS, page), nextCursor, pages: 1 };
  }
  return {
    sessions: accumulateFleetSessions(prev.sessions, page),
    nextCursor,
    pages: prev.pages + 1,
  };
}

/**
 * Keep the rows, drop the cursor.
 *
 * For a cursor coord has refused as unusable: the pages already loaded are real
 * and stay on screen, but a control that can only fail again must not be
 * offered. The caller says WHY separately — dropping the cursor silently would
 * turn an incomplete list into one that claims to be complete.
 */
export function fleetWalkDropCursor(prev: FleetWalk): FleetWalk {
  if (prev.nextCursor === null) return prev;
  return { ...prev, nextCursor: null };
}

/**
 * What the picker must say about completeness, and what it can offer.
 *
 * - `none` — coord served the last page: every matching row for the current
 *   server-side filters has been loaded, AS OF THAT READ. Sessions started
 *   since need a fresh walk, which is why the refresh control says so.
 * - `more-available` — coord handed back a cursor, so more rows are genuinely
 *   REACHABLE. This is a "load more", not an apology.
 * - `unknown` — no successful read has completed, so completeness is not
 *   established. Explicitly NOT `none`: an absent answer is not a complete one.
 *
 * There is no longer an `at-ceiling` arm. It existed because the route took no
 * offset and no cursor, so past `MAX_LIMIT` rows in one bucket were unreachable
 * by ANY combination of parameters — and saying so was the honest thing to do.
 * With keyset pagination that claim is simply FALSE, and a false warning is a
 * worse failure of `ux-priorities` gate 4 than the silence it replaced.
 */
export type FleetTruncation =
  | { kind: "none" }
  | { kind: "unknown" }
  | { kind: "more-available"; shown: number; pageSize: number; message: string };

/**
 * What the refresh control has to admit: a complete list is complete as of the
 * read that completed it, and coord's `nextCursor: null` says nothing about
 * sessions that start afterwards.
 */
export const FLEET_COMPLETE_AS_OF_NOW =
  "Start the list again from coord's first page. A complete list is complete as of its " +
  "last read — sessions started since then need a fresh one.";

/**
 * Classify the walk's completeness from the LAST page's envelope.
 *
 * `loaded` is the ACCUMULATED row count, not the last page's: a walk three
 * pages deep has served 300 rows while `response.sessions` holds the last 100,
 * and a banner reading "100 loaded so far" over a 300-row list is the same
 * class of false on-screen claim this phase exists to remove.
 *
 * The page size is read off the RESPONSE rather than taken as a parameter. That
 * is deliberate and is stronger than the sibling fix it replaces: `limit` on
 * the response is coord's effective, post-clamp size for the very page being
 * classified, so it is structurally impossible to pair this classification with
 * a page size the rows were not served under. A caller-supplied limit can be
 * the pending one — and was.
 */
export function fleetTruncation(
  response: FleetSessionsResponse | null,
  loaded: number,
): FleetTruncation {
  if (!response) return { kind: "unknown" };
  // `nextCursor` is the ONLY completeness signal on the wire. `count`, the row
  // count and `limit` are all statements about the request or the page, and a
  // full page is not a truncated one.
  if (normalizeFleetCursor(response.nextCursor) === null) return { kind: "none" };

  const pageSize =
    typeof response.limit === "number" && response.limit > 0 ? response.limit : FLEET_DEFAULT_LIMIT;
  return {
    kind: "more-available",
    shown: loaded,
    pageSize,
    message:
      `${loaded} loaded so far — coord has more matching sessions, and they are reachable. ` +
      `Load the next ${pageSize}, or narrow by device or state. The text box filters only ` +
      `what is loaded.`,
  };
}

/**
 * The stable machine codes coord's fleet route puts in a `400` body.
 *
 * The body is `{"error": "<code>", "detail": "<static prose>"}`; the CODE is
 * the contract and the prose is not. `limit_not_positive` replaced an older
 * free-text `{"error":"limit must be positive"}` — a deliberate break, and the
 * reason nothing here matches on prose.
 *
 * `unknown_state` (qontinui-coord#2085) is a non-blank `?state=` that is not a
 * `coord.sessions.state`. Before it, coord answered that with `200` and an
 * empty page. Every state this picker offers comes from
 * `FLEET_STATE_VOCABULARY` or from rows coord served, so the code means the
 * runner's vocabulary and coord's have diverged.
 */
export type FleetErrorCode =
  | "cursor_scope_mismatch"
  | "cursor_malformed"
  | "cursor_version_unsupported"
  | "limit_not_positive"
  | "unknown_state";

const FLEET_ERROR_CODES: readonly string[] = [
  "cursor_scope_mismatch",
  "cursor_malformed",
  "cursor_version_unsupported",
  "limit_not_positive",
  "unknown_state",
];

/**
 * Pull coord's machine code out of a rejected read, or null when there is none.
 *
 * The Tauri wrapper rejects with a STRING that embeds coord's body verbatim
 * (`... returned 400 — body: {"error":"…","detail":"…"}`), so the code has to
 * be recovered from it. Matching requires the `"error"` KEY rather than a bare
 * occurrence of the token: a code name appearing inside `detail`'s prose, or in a
 * url, must not be read as the verdict.
 *
 * A transport failure carries no body and yields null — which is UNKNOWN, and
 * the caller renders it as the raw failure rather than inventing a code.
 */
export function fleetErrorCode(raw: unknown): FleetErrorCode | null {
  const text = typeof raw === "string" ? raw : String(raw);
  const m = /"error"\s*:\s*"([a-z_]+)"/.exec(text);
  const code = m?.[1];
  if (code && FLEET_ERROR_CODES.includes(code)) return code as FleetErrorCode;
  return null;
}

/**
 * True when a code means "restart the walk", not "tell the operator something
 * went wrong".
 *
 * `cursor_scope_mismatch` is what coord answers when a page in flight carries a
 * cursor minted under a scope the caller has since changed. Changing a filter
 * mid-walk is ordinary use, so surfacing it as an error would be blaming the
 * operator for the UI's own race.
 */
export function fleetErrorIsRestart(code: FleetErrorCode | null): boolean {
  return code === "cursor_scope_mismatch";
}

/**
 * True when a code means the CURSOR is unusable — the rows already loaded stay,
 * but no further page can be offered from it.
 */
export function fleetErrorInvalidatesCursor(code: FleetErrorCode | null): boolean {
  return code === "cursor_malformed" || code === "cursor_version_unsupported";
}

/** What to show for a failed read. Honest about which of the two it is. */
export function fleetErrorMessage(code: FleetErrorCode | null, raw: unknown): string {
  switch (code) {
    case "cursor_scope_mismatch":
      // Handled by restarting rather than shown; kept so every code has an
      // answer and a future caller that does show it says something true.
      return "The filters changed while a page was loading — starting the list again.";
    case "cursor_malformed":
      return (
        "coord rejected this list's page cursor as malformed. The pages already loaded are " +
        "shown and there may be more — refresh to start the list again."
      );
    case "cursor_version_unsupported":
      return (
        "coord does not support this page cursor's version — this runner and coord disagree " +
        "about the cursor format. The pages already loaded are shown and there may be more; " +
        "refresh to start the list again."
      );
    case "limit_not_positive":
      return "coord refused the read: the page size must be a positive number.";
    case "unknown_state":
      return (
        "coord does not recognise the selected state filter — this runner's list of session " +
        "states and coord's disagree. Clear the state filter to list every state."
      );
    default:
      return `Failed to load fleet sessions: ${raw}`;
  }
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
 *
 * Fields are joined on a NEWLINE, not a space. `fleetSearchTerms` splits the
 * query on whitespace, so no term it produces can contain one — which makes a
 * match straddling two fields structurally impossible rather than merely
 * unlikely. A space join let `"web feat"` match `repo="qontinui-web"` +
 * `branch="feat/picker"` as one run, i.e. match on the ORDER of the fields in
 * this array, which nothing guarantees and no caller could predict.
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
    .join("\n")
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

/**
 * Apply a text query to what has been LOADED. Client-side — see the module doc.
 * The loaded set grows with every page of the walk, so this reaches further
 * over time; it is never the whole fleet at any one moment.
 */
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
  /**
   * True when `label` is the id-derived placeholder rather than a name coord
   * served — i.e. the identity columns were degraded on the read this option
   * came from.
   *
   * Carried as a FLAG because the merge below has to know, and the alternative
   * was sniffing the label for the `device ` prefix: that misreads an
   * operator-chosen name like "device farm 2" in both directions, and a change
   * to the placeholder's format would break the merge with every test still
   * green.
   */
  labelIsFallback: boolean;
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
 * A better label wins over a worse one: the label falls back to a truncated id
 * when coord served the identity columns degraded, and a later non-degraded
 * read must be allowed to replace that placeholder. `labelIsFallback` is what
 * decides, never the label's own text.
 *
 * Returns `prev` UNCHANGED when the merge adds nothing, so a refresh that sees
 * the same devices does not re-render the dropdown for no reason.
 */
export function mergeDeviceCatalog(
  prev: FleetDeviceOption[],
  seen: FleetDeviceOption[],
): FleetDeviceOption[] {
  const byId = new Map(prev.map((d) => [d.deviceId, d]));
  let changed = false;
  for (const d of seen) {
    const existing = byId.get(d.deviceId);
    if (!existing) {
      byId.set(d.deviceId, d);
      changed = true;
      continue;
    }
    const takeIncomingLabel = existing.labelIsFallback && !d.labelIsFallback;
    const merged: FleetDeviceOption = {
      deviceId: d.deviceId,
      label: takeIncomingLabel ? d.label : existing.label,
      isCallerDevice: existing.isCallerDevice || d.isCallerDevice,
      labelIsFallback: takeIncomingLabel ? d.labelIsFallback : existing.labelIsFallback,
    };
    if (
      merged.label !== existing.label ||
      merged.isCallerDevice !== existing.isCallerDevice ||
      merged.labelIsFallback !== existing.labelIsFallback
    ) {
      byId.set(d.deviceId, merged);
      changed = true;
    }
  }
  if (!changed) return prev;
  return [...byId.values()].sort((a, b) => {
    if (a.isCallerDevice !== b.isCallerDevice) return a.isCallerDevice ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
}

/**
 * A value that coord's fleet route will accept as `device_id`.
 *
 * The route deserializes it into a `Uuid`, so a non-uuid is a 400 from axum's
 * `Query` extractor before any handler code runs. The picker's paste-an-id
 * escape hatch checks this and SAYS so rather than firing a request that can
 * only fail — an error the operator would otherwise have to decode from a
 * status code.
 */
export function isLikelyDeviceId(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value.trim());
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

/** The distinct non-blank `state` values a page of rows carried. */
export function statesSeenIn(sessions: FleetSession[]): string[] {
  const seen = new Set<string>();
  for (const s of sessions) {
    const v = s.state?.trim();
    if (v) seen.add(v);
  }
  return [...seen];
}

/**
 * Accumulate the states seen across reads, for the same reason the device
 * catalogue accumulates: filtering to `state=active` makes coord's next
 * response carry only that state, and a dropdown rebuilt from it would drop
 * every other value it had just offered.
 *
 * Returns `prev` unchanged when nothing is new.
 */
export function mergeStateCatalog(prev: string[], seen: string[]): string[] {
  const set = new Set(prev);
  let changed = false;
  for (const v of seen) {
    if (!set.has(v)) {
      set.add(v);
      changed = true;
    }
  }
  return changed ? [...set].sort() : prev;
}

/**
 * The states to offer: the known vocabulary, plus everything coord has actually
 * served, plus whatever is SELECTED right now.
 *
 * The vocabulary union matters because the vocabulary is enforced in Rust
 * rather than by a DB constraint precisely so it can evolve without a migration
 * — a runner pinned to a stale list would silently hide a state coord had
 * started emitting, which is the same class of quiet omission this phase is
 * fixing.
 *
 * The SELECTED union closes a sharper hole: a controlled `<select>` whose value
 * matches no `<option>` renders as the first option — "Any state" — while the
 * request in flight still carries the filter. The control would then be showing
 * one query and coord answering another, which is the exact confusion this
 * phase exists to remove.
 */
export function fleetStateOptions(seen: string[], selected: string | null): string[] {
  const extra = new Set<string>();
  for (const v of [...seen, ...(selected ? [selected] : [])]) {
    const t = v.trim();
    if (t && !FLEET_STATE_VOCABULARY.includes(t)) extra.add(t);
  }
  return [...FLEET_STATE_VOCABULARY, ...[...extra].sort()];
}

/**
 * The server-side half of the picker's filter state — what coord is asked.
 *
 * `limit` is the page size, NOT a reachability control: it bounds one page of
 * the walk and nothing else. It is kept in the filter because it is part of the
 * request and the UI reports what the rows were served under; it is absent from
 * [`FleetScope`] because coord's cursor survives a change to it.
 */
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

/** The scope half of a filter — what a cursor is validated against. */
export function fleetScopeOf(server: FleetServerFilter): FleetScope {
  return { deviceId: server.deviceId, state: server.state, includeClosed: server.includeClosed };
}

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
 * Deliberately narrower than `hasActiveFleetFilter`: a changed page size and
 * `includeClosed` cannot subtract rows, so blaming an empty result on "these
 * filters" when the only non-default is a page size would be a false
 * explanation of an honest zero.
 */
export function hasNarrowingFleetFilter(server: FleetServerFilter, text: string): boolean {
  return server.deviceId !== null || server.state !== null || fleetSearchTerms(text).length > 0;
}

/**
 * True when a filter coord ACTUALLY SAW is narrowing the read.
 *
 * Distinct from `hasNarrowingFleetFilter`, which also counts the text box: the
 * text box is client-side and never reaches coord, so it cannot explain a read
 * that came back with zero rows. Blaming it for one would conceal a genuine
 * observed-empty fleet behind an invented cause.
 */
export function hasNarrowingServerFilter(server: FleetServerFilter): boolean {
  return server.deviceId !== null || server.state !== null;
}

/**
 * What to say when coord's read itself returned no rows.
 *
 * Three different facts, never merged: coord was asked with narrowing filters
 * and answered zero; coord was asked WITHOUT them and answered zero (a real
 * empty fleet, whatever the text box holds); or the text box is set and is
 * being wrongly suspected, which is worth saying out loud since it is on screen
 * and looks like a cause.
 */
export function fleetEmptyReadMessage(
  server: FleetServerFilter,
  text: string,
): { message: string; offerClear: boolean } {
  if (hasNarrowingServerFilter(server)) {
    return {
      message:
        "No session matches these filters. This is what coord returned for them — not necessarily an empty fleet.",
      offerClear: true,
    };
  }
  if (fleetSearchTerms(text).length > 0) {
    return {
      message:
        "No open sessions anywhere on the fleet. Your text filter is not sent to coord, so it is not what emptied this list.",
      offerClear: true,
    };
  }
  return { message: "No open sessions anywhere on the fleet.", offerClear: false };
}

/**
 * The one-line count summary, which must never present a filtered subset as a
 * total.
 *
 * `matched` is what is on screen; `loaded` is what the walk has served so far.
 * When they differ the line says so, because "12 sessions" over a 400-session
 * tenant filtered to 12 is exactly the confident-total shape the truncation
 * note was already getting wrong.
 */
export function fleetCountSummary(args: {
  matched: number;
  loaded: number;
  /** Devices spanned by the rows ON SCREEN. */
  devices: number;
  /** Devices spanned by everything the walk has served. */
  devicesLoaded: number;
  remote: number;
}): string {
  const { matched, loaded, devices, devicesLoaded, remote } = args;
  const head =
    matched === loaded
      ? `${loaded} session${loaded === 1 ? "" : "s"}`
      : `${matched} of ${loaded} loaded`;
  // The device half has to be as honest as the session half: "2 of 47 loaded on
  // 1 device" reads as a claim about the fleet when the 47 may span five.
  const devicePart =
    devices === devicesLoaded
      ? `${devices} device${devices === 1 ? "" : "s"}`
      : `${devices} of ${devicesLoaded} devices`;
  const parts = [`${head} on ${devicePart}`];
  if (remote > 0) parts.push(`${remote} remote`);
  return parts.join(" · ");
}

/**
 * Why a filtered list is empty, when the underlying walk was NOT empty.
 *
 * Returns `null` when nothing has been loaded — that case belongs to
 * `emptyReasonFor`, which distinguishes a failed read from an observed-empty
 * fleet. Splitting the two keeps each answer true: "your filter matched
 * nothing" and "coord returned nothing" are different facts and must not share
 * a message.
 *
 * `moreAvailable` is what makes the message true under a cursor walk: with
 * pages left, "no session matches" is a claim about the rows fetched so far and
 * NOT about the fleet, and the way forward is to load more.
 */
export function fleetFilteredOutMessage(
  loaded: number,
  matched: number,
  text: string,
  moreAvailable = false,
): string | null {
  if (loaded === 0 || matched > 0) return null;
  const q = text.trim();
  if (!q) return null;
  const head = `No loaded session matches “${q}”. ${loaded} session${loaded === 1 ? " is" : "s are"} loaded; this box filters only those.`;
  return moreAvailable ? `${head} coord has more — load another page to widen what it sees.` : head;
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
