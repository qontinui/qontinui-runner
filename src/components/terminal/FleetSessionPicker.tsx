import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  ChevronsDown,
  Link2,
  Monitor,
  RefreshCw,
  Search,
  Server,
  X,
} from "lucide-react";
import {
  useFleetSessions,
  groupByDevice,
  type FleetSession,
  type FleetSessionsResponse,
} from "./useFleetSessions";
import {
  DEFAULT_FLEET_SERVER_FILTER,
  FLEET_COMPLETE_AS_OF_NOW,
  fleetCountSummary,
  fleetEmptyReadMessage,
  fleetFilterConflict,
  fleetFilteredOutMessage,
  fleetSessionActivity,
  fleetStateOptions,
  fleetTruncation,
  filterFleetSessions,
  hasActiveFleetFilter,
  isLikelyDeviceId,
  type FleetServerFilter,
} from "./fleetDiscovery";
import {
  attachButtonState,
  attachErrorMessage,
  fleetSessionAttachId,
  remoteSessionLabel,
  type RemoteTerminalInfoWire,
} from "./remoteTabs";
import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { formatRelativeTime } from "../../lib/formatting";

/**
 * Picker listing which Claude Code sessions exist on which fleet machine
 * (plan `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 2), with an
 * **Attach** action per remote row (Phase 3c) and DISCOVERY controls — search,
 * device/state filters and a page control — added by plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner` Phase 2.
 *
 * Attach calls `terminal_attach_remote {deviceId, sessionId}`; the runner mints
 * a coord grant, presents it through the relay, and opens an ordinary
 * `TerminalSession` around a `RemotePaneIo` — the tab then arrives through the
 * same `terminal-created` path a local `terminal_create` uses (no new terminal
 * backend). Every failure is typed by the runner and shown INLINE in the row,
 * kept until the next attempt: never a toast that vanishes, never silence.
 *
 * ## Completeness is a control, not a footnote
 *
 * coord serves a bounded page and hands back a `nextCursor` while more rows
 * match. This picker previously rendered incompleteness as the words "· more
 * not shown" and stopped there — on a tenant whose first read came back with
 * exactly 100 rows (coord's default page) the sessions past the hundredth were
 * simply unreachable. Phase 2 made that reachable with a page LADDER (ask for a
 * bigger page, up to a hard ceiling), because the route had no offset and no
 * cursor. Phase 5a replaced the ladder with the real thing: the route does
 * keyset pagination, `truncated` is gone, and the banner below is a genuine
 * "load more" over a walk that can reach every matching row. The old
 * at-the-ceiling copy — "some sessions cannot be paged further at all" — is now
 * FALSE and has been deleted rather than softened. The walk, the filter logic
 * and those messages live in `fleetDiscovery.ts` / `useFleetSessions.ts` and
 * are unit-tested there.
 */

export const FLEET_SESSION_PICKER_ELEMENT = "fleet-session-picker";
export const FLEET_DEVICE_GROUP_ELEMENT = "fleet-device-group";
export const FLEET_SESSION_ROW_ELEMENT = "fleet-session-row";
export const FLEET_PICKER_REFRESH_ID = "terminal.fleet-picker-refresh";
export const FLEET_PICKER_RETRY_ID = "terminal.fleet-picker-retry";
export const FLEET_PICKER_STALE_ID = "terminal.fleet-picker-stale";
export const FLEET_PICKER_SEARCH_ID = "terminal.fleet-picker-search";
export const FLEET_PICKER_DEVICE_FILTER_ID = "terminal.fleet-picker-device-filter";
export const FLEET_PICKER_STATE_FILTER_ID = "terminal.fleet-picker-state-filter";
export const FLEET_PICKER_INCLUDE_CLOSED_ID = "terminal.fleet-picker-include-closed";
export const FLEET_PICKER_CLEAR_FILTERS_ID = "terminal.fleet-picker-clear-filters";
/** The same reset, offered again from the two empty states. Distinct ids: all
 * three can be mounted at once, and a duplicated `data-ui-bridge-id` makes the
 * control ambiguous to a driver. */
export const FLEET_PICKER_CLEAR_FILTERS_EMPTY_ID = "terminal.fleet-picker-clear-filters-empty";
export const FLEET_PICKER_CLEAR_FILTERS_NOMATCH_ID = "terminal.fleet-picker-clear-filters-no-match";
export const FLEET_PICKER_DEVICE_ID_ENTRY_ID = "terminal.fleet-picker-device-id-entry";
export const FLEET_PICKER_DEVICE_ID_MODE_ID = "terminal.fleet-picker-device-id-mode";
export const FLEET_PICKER_DEVICE_ID_INVALID_ID = "terminal.fleet-picker-device-id-invalid";
export const FLEET_PICKER_REREADING_ID = "terminal.fleet-picker-rereading";
export const FLEET_PICKER_CONFLICT_ID = "terminal.fleet-picker-filter-conflict";
export const FLEET_PICKER_TRUNCATION_ID = "terminal.fleet-picker-truncation";
export const FLEET_PICKER_LOAD_MORE_ID = "terminal.fleet-picker-load-more";
/** The incomplete-and-unreachable strip. A DISTINCT id from the truncation one:
 * the two make opposite claims about whether the next page can be fetched, and
 * a driver that could not tell them apart would read "there is more" as "there
 * is a control for it". */
export const FLEET_PICKER_UNREACHABLE_ID = "terminal.fleet-picker-unreachable";

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
  if (!r.deviceIdentityColumnsPresent) missing.push("device hostnames");
  if (missing.length === 0) return null;
  return `coord could not read ${missing.join(", ")} — those fields are unknown, not empty.`;
}

/** Per-row attach state: pending, or the last typed failure (kept inline). */
interface RowAttachState {
  pending: boolean;
  error: string | null;
  /** Local terminal id of the tab the last successful attach opened. */
  openedId: string | null;
}

const SELECT_CLASS =
  "min-w-0 flex-1 text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 " +
  "text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50";

export function FleetSessionPicker() {
  /**
   * What coord is asked for. Every field here is a query parameter the fleet
   * route already accepts (`device_id`, `state`, `include_closed`, `limit`) —
   * no new server surface, and the two narrowing filters are applied by coord
   * in SQL, so they reach rows a truncated page never served.
   */
  const [server, setServer] = useState<FleetServerFilter>(DEFAULT_FLEET_SERVER_FILTER);
  /** Client-side text filter over the loaded page. Says so in the UI. */
  const [text, setText] = useState("");

  /**
   * Paste-a-device-id mode. The dropdown can only offer devices some loaded
   * page contained, and coord has no device-listing route — so on a truncated
   * tenant the one control that reaches past truncation cannot name the device
   * that truncation hid. This is the way out: coord takes a raw uuid.
   */
  const [deviceIdEntry, setDeviceIdEntry] = useState(false);

  /**
   * Re-render on a slow timer so the per-row relative times keep moving.
   *
   * `formatRelativeTime` is computed during render, and nothing else here
   * re-renders while the panel sits open — the hook only re-renders on a fetch.
   * A row that read "heartbeat 2m ago" would still read "heartbeat 2m ago" an
   * hour later, which is a stale liveness claim in the one place this list
   * exists to answer honestly. The same display elsewhere in the app
   * (`WebIntegrationSettings`) polls for exactly this reason.
   *
   * 30s, because the finest unit rendered is the minute: a shorter tick buys no
   * visible accuracy and re-renders a list that can run to hundreds of rows,
   * and a longer one lets a minute boundary sit visibly wrong. The tick is the
   * ONLY thing it advances — no read is issued, so this never hides a stale
   * fetch behind a moving label.
   */
  const [, setClockTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setClockTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, []);

  const {
    sessions,
    response,
    loading,
    loadingMore,
    error,
    errorCode,
    walkStalled,
    emptyReason,
    deviceCatalog,
    stateCatalog,
    appliedQuery,
    pagesLoaded,
    hasMore,
    refresh,
    loadMore,
  } = useFleetSessions({
    deviceId: server.deviceId ?? undefined,
    state: server.state ?? undefined,
    includeClosed: server.includeClosed,
    limit: server.limit,
  });

  const visible = useMemo(() => filterFleetSessions(sessions, text), [sessions, text]);
  const groups = useMemo(() => groupByDevice(visible), [visible]);
  const stateOptions = useMemo(
    () => fleetStateOptions(stateCatalog, server.state),
    [stateCatalog, server.state],
  );
  // Both arguments come from the hook and move in the same tick as each other:
  // `response` is the last page's envelope (which carries coord's own effective
  // page size, so the classifier cannot be handed a limit the rows were not
  // served under) and `sessions` is the accumulation that page landed in. The
  // pending `server` filter is never an input here — between a filter change
  // and its response, and permanently if that response never arrives, the two
  // describe different queries.
  // `hasMore` is the WALK's answer, and it is not the same fact as the last
  // response's `nextCursor`: a cursor coord refused, or handed back unchanged,
  // is dropped from the walk while that envelope still carries one. Without it
  // the classifier returns `more-available` in a state where `loadMore` returns
  // immediately — a "Load more" button that does nothing at all when clicked.
  const truncation = fleetTruncation(response, sessions.length, hasMore);
  const notice = degradedNotice(response);
  const filtersActive = hasActiveFleetFilter(server, text);
  const emptyRead = fleetEmptyReadMessage(appliedQuery ?? server, text);
  const devicesLoaded = useMemo(() => new Set(sessions.map((s) => s.deviceId)).size, [sessions]);
  const filteredOut = fleetFilteredOutMessage(
    sessions.length,
    visible.length,
    text,
    truncation.kind === "more-available",
  );
  const conflict = fleetFilterConflict(server);

  const { pageId, setActiveId } = useTerminalSession();
  const [attachState, setAttachState] = useState<Record<string, RowAttachState>>({});

  const clearFilters = useCallback(() => {
    setServer(DEFAULT_FLEET_SERVER_FILTER);
    setText("");
    setDeviceIdEntry(false);
  }, []);

  const attach = useCallback(
    async (s: FleetSession, deviceLabel: string) => {
      const set = (patch: Partial<RowAttachState>) =>
        setAttachState((prev) => {
          const base: RowAttachState = prev[s.sessionId] ?? {
            pending: false,
            error: null,
            openedId: null,
          };
          return { ...prev, [s.sessionId]: { ...base, ...patch } };
        });
      set({ pending: true, error: null });
      try {
        const info = await invoke<RemoteTerminalInfoWire>("terminal_attach_remote", {
          deviceId: s.deviceId,
          sessionId: s.sessionId,
          deviceLabel,
          sessionLabel: remoteSessionLabel(s),
          workingDir: null,
          // Same routing as `createTerminal`: the tab lands on THIS page via
          // the `terminal-created` listener that claims its `pageId`.
          pageId: pageId !== "default" ? pageId : null,
        });
        set({ pending: false, openedId: info.id });
        setActiveId(info.id);
      } catch (err) {
        set({ pending: false, error: attachErrorMessage(err) });
      }
    },
    [pageId, setActiveId],
  );

  const remoteCount = visible.filter((s) => !s.isCallerDevice).length;

  return (
    <div
      data-page-element={FLEET_SESSION_PICKER_ELEMENT}
      // The discovery state, projected for a UI Bridge driver in one read
      // rather than scraped off control labels.
      //
      // Split into APPLIED and PENDING because they diverge: everything under
      // `data-fleet-*` describes the query the loaded rows were served for, so
      // a driver reading the whole set in one pass gets a filter set and a row
      // count that belong together. The controls' own state — which may be a
      // request still in flight, or one that failed — is under
      // `data-fleet-pending-*`.
      data-fleet-limit={appliedQuery?.limit ?? ""}
      data-fleet-loaded={sessions.length}
      data-fleet-matched={visible.length}
      data-fleet-pages={pagesLoaded}
      data-fleet-truncation={truncation.kind}
      // coord's stable machine code for the last failed read. Projected because
      // the banner beside it carries PROSE, which is explicitly not the
      // contract — a driver that had to match on the sentence would break on
      // any rewording of it.
      data-fleet-error-code={errorCode ?? ""}
      data-fleet-device-filter={appliedQuery?.deviceId ?? ""}
      data-fleet-state-filter={appliedQuery?.state ?? ""}
      data-fleet-include-closed={appliedQuery ? String(appliedQuery.includeClosed) : ""}
      data-fleet-pending-limit={server.limit}
      data-fleet-pending-device-filter={server.deviceId ?? ""}
      data-fleet-pending-state-filter={server.state ?? ""}
      data-fleet-pending-include-closed={server.includeClosed ? "true" : "false"}
      className="flex-1 flex flex-col min-h-0"
    >
      {/* Sub-header: counts + refresh */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d]">
        <Server className="w-3 h-3 text-[#565f89] shrink-0" />
        <span className="text-[10px] text-[#565f89] font-medium truncate">
          {fleetCountSummary({
            matched: visible.length,
            loaded: sessions.length,
            devices: groups.length,
            devicesLoaded,
            remote: remoteCount,
          })}
        </span>
        <div className="flex-1" />
        {filtersActive && (
          <button
            data-ui-bridge-id={FLEET_PICKER_CLEAR_FILTERS_ID}
            aria-label="Clear fleet filters"
            onClick={clearFilters}
            className="flex items-center gap-0.5 px-1 py-0.5 rounded text-[9px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
            title="Clear every filter and go back to coord's default page"
          >
            <X className="w-2.5 h-2.5" />
            Clear
          </button>
        )}
        <button
          data-ui-bridge-id={FLEET_PICKER_REFRESH_ID}
          onClick={() => void refresh()}
          disabled={loading || loadingMore}
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
          // coord's `nextCursor: null` is "last page AS OF NOW" and nothing
          // more, so the control that re-reads has to say what a finished walk
          // does and does not claim.
          title={FLEET_COMPLETE_AS_OF_NOW}
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Search over the loaded page. Client-side, and the title says so. */}
      <div className="px-3 pt-1.5">
        <div className="relative">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3 h-3 text-[#565f89]" />
          <input
            data-ui-bridge-id={FLEET_PICKER_SEARCH_ID}
            aria-label="Filter loaded fleet sessions"
            type="text"
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="Filter loaded sessions…"
            title="Matches work unit, intent, repo, branch, device, state, provider and ids. Filters only the sessions loaded so far — loading another page widens what it can see, and the device/state filters are applied by coord in SQL."
            className="w-full pl-7 pr-2 py-1 text-[11px] bg-[#1a1b26] border border-[#2a2d3d] rounded text-[#a9b1d6] placeholder-[#414868] focus:outline-none focus:border-[#7aa2f7]/50"
          />
        </div>
      </div>

      {/* Server-side narrowing: these two reach past a truncated page. */}
      <div className="flex items-center gap-1.5 px-3 py-1.5 border-b border-[#2a2d3d]">
        {deviceIdEntry ? (
          <input
            data-ui-bridge-id={FLEET_PICKER_DEVICE_ID_ENTRY_ID}
            aria-label="Filter fleet sessions by device id"
            type="text"
            value={server.deviceId ?? ""}
            onChange={(e) => {
              const v = e.target.value.trim();
              setServer((s) => ({ ...s, deviceId: v === "" ? null : v }));
            }}
            placeholder="device uuid"
            title="A device whose sessions all fall past the pages loaded so far never reaches the dropdown — coord serves no device list. Paste its uuid here instead."
            className={SELECT_CLASS + " placeholder-[#414868]"}
          />
        ) : (
          <select
            data-ui-bridge-id={FLEET_PICKER_DEVICE_FILTER_ID}
            aria-label="Filter fleet sessions by device"
            value={server.deviceId ?? ""}
            onChange={(e) =>
              setServer((s) => ({ ...s, deviceId: e.target.value === "" ? null : e.target.value }))
            }
            title="Asks coord for one device only — applied in SQL, so it narrows the whole walk rather than the rows on screen. Lists only devices seen in the pages loaded so far; use “id” for one that is not here."
            className={SELECT_CLASS}
          >
            <option value="">All devices</option>
            {deviceCatalog.map((d) => (
              <option key={d.deviceId} value={d.deviceId}>
                {d.label}
                {d.isCallerDevice ? " (this machine)" : ""}
              </option>
            ))}
          </select>
        )}
        <button
          data-ui-bridge-id={FLEET_PICKER_DEVICE_ID_MODE_ID}
          aria-label="Enter a device id directly"
          aria-pressed={deviceIdEntry}
          onClick={() => setDeviceIdEntry((v) => !v)}
          title="Type a device uuid instead of picking from the list — the list holds only devices seen in the pages loaded so far"
          className={`shrink-0 px-1 py-0.5 rounded text-[10px] transition-colors ${
            deviceIdEntry
              ? "bg-[#7aa2f7]/15 text-[#7aa2f7]"
              : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          }`}
        >
          id
        </button>
        <select
          data-ui-bridge-id={FLEET_PICKER_STATE_FILTER_ID}
          aria-label="Filter fleet sessions by state"
          value={server.state ?? ""}
          onChange={(e) =>
            setServer((s) => ({ ...s, state: e.target.value === "" ? null : e.target.value }))
          }
          title="Asks coord for one session state only — applied in SQL, so it narrows the whole walk rather than the rows on screen"
          className={SELECT_CLASS}
        >
          <option value="">Any state</option>
          {stateOptions.map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
        <button
          data-ui-bridge-id={FLEET_PICKER_INCLUDE_CLOSED_ID}
          aria-label="Include closed sessions"
          aria-pressed={server.includeClosed}
          onClick={() => setServer((s) => ({ ...s, includeClosed: !s.includeClosed }))}
          title="Closed sessions are excluded by default — a closed session cannot be attached to, so this widens discovery, not attach"
          className={`shrink-0 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
            server.includeClosed
              ? "bg-[#7aa2f7]/15 text-[#7aa2f7]"
              : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          }`}
        >
          closed
        </button>
      </div>

      {/*
        Incompleteness as a CONTROL, and under a cursor walk it is no longer an
        apology: coord handed back a cursor, so the rows it names are REACHABLE.
        One click fetches the next page and appends it — the rows on screen stay
        put, which is why this is not styled or worded as an error.
      */}
      {truncation.kind === "more-available" && (
        <div
          data-ui-bridge-id={FLEET_PICKER_TRUNCATION_ID}
          data-truncation-kind={truncation.kind}
          className="flex items-start gap-1.5 px-3 py-1.5 text-[10px] text-[#7aa2f7] bg-[#7aa2f7]/10 border-b border-[#2a2d3d]"
        >
          <ChevronsDown className="w-3 h-3 mt-px shrink-0" />
          <span className="min-w-0">{truncation.message}</span>
          <button
            data-ui-bridge-id={FLEET_PICKER_LOAD_MORE_ID}
            aria-label={`Load the next ${truncation.pageSize} fleet sessions`}
            onClick={() => void loadMore()}
            disabled={loading || loadingMore}
            className="ml-auto shrink-0 flex items-center gap-0.5 px-1.5 py-0.5 rounded bg-[#7aa2f7]/20 text-[#7aa2f7] hover:bg-[#7aa2f7]/35 transition-colors disabled:opacity-50"
            title={`Fetch coord's next page of ${truncation.pageSize} and append it to this list`}
          >
            {loadingMore ? (
              <div className="w-2.5 h-2.5 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
            ) : (
              <ChevronsDown className="w-2.5 h-2.5" />
            )}
            {loadingMore ? "Loading…" : `Load ${truncation.pageSize} more`}
          </button>
        </div>
      )}

      {/*
        coord had more and the walk can no longer reach it — its cursor was
        refused or came back unchanged. Deliberately NOT a "load more": offering
        a page control here would offer a click the hook answers by returning
        immediately. The way forward is the refresh above, which starts a fresh
        walk with no cursor, and the banner says so instead of implying a
        control that is not there.

        Suppressed while `walkStalled`, where the error banner below carries the
        SAME fact in the same words ("the walk cannot advance") plus a Retry.
        Two incompleteness warnings stacked, one of them styled as an error, is
        the confident-on-screen-claim shape this whole phase removes. The other
        drop path — a cursor coord REFUSED — says something different (why the
        cursor is gone, not how complete the list is), so there both belong.
      */}
      {truncation.kind === "unreachable" && !walkStalled && (
        <div
          data-ui-bridge-id={FLEET_PICKER_UNREACHABLE_ID}
          data-truncation-kind={truncation.kind}
          className="flex items-start gap-1.5 px-3 py-1.5 text-[10px] text-[#e0af68] bg-[#e0af68]/10 border-b border-[#2a2d3d]"
        >
          <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
          <span className="min-w-0">{truncation.message}</span>
        </div>
      )}

      {/*
        coord deserializes `device_id` into a Uuid, so a non-uuid is a 400 from
        the extractor. Say so here rather than letting the read fail.
      */}
      {server.deviceId !== null && !isLikelyDeviceId(server.deviceId) && (
        <div
          data-ui-bridge-id={FLEET_PICKER_DEVICE_ID_INVALID_ID}
          className="flex items-start gap-1.5 px-3 py-1.5 text-[10px] text-[#f7768e] border-b border-[#2a2d3d]"
        >
          <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
          <span>
            “{server.deviceId}” is not a device uuid — coord rejects the read rather than returning
            no sessions.
          </span>
        </div>
      )}

      {/* A pair of filters that can only starve the list says so up front. */}
      {conflict && (
        <div
          data-ui-bridge-id={FLEET_PICKER_CONFLICT_ID}
          className="flex items-start gap-1.5 px-3 py-1.5 text-[10px] text-[#e0af68] border-b border-[#2a2d3d]"
        >
          <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
          <span>{conflict}</span>
        </div>
      )}

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
        ) : error && sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#f7768e] text-xs">
            <AlertTriangle className="w-4 h-4 mx-auto mb-2" />
            {error}
            {/* A stalled WALK is not a failed READ — coord answered, and the
                page it answered with was empty. "This is a failed read" over
                that is a false claim in the other direction, which is the whole
                distinction the inline banner below already draws. The empty
                state has to draw it too: this branch is reached when coord
                serves an empty page carrying a cursor and the walk then stalls
                on the next one. */}
            <div className="mt-1 text-[#565f89]">
              {walkStalled
                ? "coord answered — this page was empty and the walk cannot advance past it."
                : "This is a failed read, not an empty fleet."}
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
            {emptyReason !== "observed-empty" ? (
              "Fleet sessions are unknown — no successful read yet."
            ) : (
              <>
                {emptyRead.message}
                {emptyRead.offerClear && (
                  <button
                    data-ui-bridge-id={FLEET_PICKER_CLEAR_FILTERS_EMPTY_ID}
                    onClick={clearFilters}
                    className="block mx-auto mt-2 px-2 py-1 rounded bg-[#2a2d3d] text-[#c0caf5] hover:bg-[#3a3d4d] transition-colors"
                  >
                    Clear filters
                  </button>
                )}
              </>
            )}
          </div>
        ) : (
          <>
            {error && (
              <div
                data-ui-bridge-id={FLEET_PICKER_STALE_ID}
                className="flex items-center gap-1.5 px-3 py-1 text-[10px] text-[#f7768e] bg-[#f7768e]/10 border-b border-[#2a2d3d]"
              >
                <AlertTriangle className="w-3 h-3 shrink-0" />
                <span className="truncate" title={error}>
                  {/* A stalled WALK is not a failed READ: coord answered, the
                      rows below are current, and only the next page is out of
                      reach. Prefixing that with "last refresh failed" would be
                      a false claim in the other direction. */}
                  {walkStalled
                    ? error
                    : `Last refresh failed — showing the previous read. ${error}`}
                </span>
                <button
                  data-ui-bridge-id={FLEET_PICKER_RETRY_ID}
                  onClick={() => void refresh()}
                  className="ml-auto px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#c0caf5] hover:bg-[#3a3d4d] transition-colors"
                >
                  Retry
                </button>
              </div>
            )}
            {/*
              A RESTART keeps the previous rows on screen (deliberately — a
              filter change must not blank a list mid-read), so while one is in
              flight the rows below belong to the PREVIOUS filter. Say so:
              otherwise the selects claim one query and the list shows another.

              A `loadingMore` page is NOT this case and must not borrow this
              banner: the rows below are the same walk's earlier pages and stay
              valid, so saying they are stale would be a false claim in the
              other direction. The "Load more" button carries its own spinner.
            */}
            {loading && (
              <div
                data-ui-bridge-id={FLEET_PICKER_REREADING_ID}
                className="flex items-center gap-1.5 px-3 py-1 text-[10px] text-[#565f89] border-b border-[#2a2d3d]"
              >
                <div className="w-2.5 h-2.5 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin shrink-0" />
                <span>Re-reading coord — the rows below are from the previous read.</span>
              </div>
            )}
            {/*
              A text filter that hides every loaded row must say that is what
              happened — and must not be confusable with an empty fleet.
            */}
            {filteredOut && (
              <div className="px-3 py-6 text-center text-[#565f89] text-xs">
                {filteredOut}
                <button
                  data-ui-bridge-id={FLEET_PICKER_CLEAR_FILTERS_NOMATCH_ID}
                  onClick={clearFilters}
                  className="block mx-auto mt-2 px-2 py-1 rounded bg-[#2a2d3d] text-[#c0caf5] hover:bg-[#3a3d4d] transition-colors"
                >
                  Clear filters
                </button>
              </div>
            )}
            {groups.map((g) => (
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

                {g.sessions.map((s) => {
                  const btn = attachButtonState(s, response?.deviceIdentityColumnsPresent);
                  const row = attachState[s.sessionId];
                  const pending = row?.pending === true;
                  return (
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
                        {/* Attach (Phase 3c). Disabled WITH a reason for the
                          caller's own device, a closed session, or a row whose
                          device id coord could not vouch for. */}
                        <button
                          type="button"
                          data-ui-bridge-id={fleetSessionAttachId(s.sessionId)}
                          onClick={() => void attach(s, g.label)}
                          disabled={btn.disabled || pending}
                          aria-disabled={btn.disabled || pending}
                          title={
                            btn.reason ??
                            (pending
                              ? "Attaching — minting a grant and waiting for the remote runner"
                              : `Open a tab onto this session on ${g.label}`)
                          }
                          className="flex items-center gap-1 shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                        >
                          {pending ? (
                            <div className="w-2.5 h-2.5 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
                          ) : (
                            <Link2 className="w-2.5 h-2.5" />
                          )}
                          {pending ? "Attaching…" : "Attach"}
                        </button>
                      </div>
                      {(() => {
                        // The row's most recent OBSERVED instant, beside the
                        // two free-text fields. `state` is a stored column a
                        // watcher advances, so it can read `active` over a
                        // session that last beat days ago; the heartbeat is
                        // what an operator scanning a walked list of hundreds
                        // actually needs. Null — coord served no parseable
                        // timestamp — renders nothing rather than a placeholder
                        // that would look like an answer.
                        const activity = fleetSessionActivity(s);
                        const free = [s.provider, s.correlationTopic].filter(
                          (v): v is string => typeof v === "string" && v.length > 0,
                        );
                        if (free.length === 0 && !activity) return null;
                        return (
                          <div className="text-[10px] text-[#565f89] truncate">
                            {/* Each half carries its OWN title. One tooltip over
                                the whole line would claim to explain the free
                                text it says nothing about — and this line is
                                `truncate`d, so the tooltip is often the only way
                                to read either half. */}
                            {free.length > 0 && (
                              <span title={free.join(" · ")}>{free.join(" · ")}</span>
                            )}
                            {free.length > 0 && activity && " · "}
                            {activity && (
                              <span
                                // The exact instant, machine-readable, for the
                                // same reason `data-fleet-error-code` exists: a
                                // driver must not have to scrape a truncated,
                                // locale-formatted span for a timestamp.
                                data-session-activity={activity.iso}
                                data-session-activity-kind={activity.verb}
                                title={`${activity.verb} at ${activity.iso}`}
                              >
                                {activity.verb} {formatRelativeTime(activity.iso)}
                              </span>
                            )}
                          </div>
                        );
                      })()}
                      {row?.error && (
                        <div
                          data-ui-bridge-id={`terminal.fleet-session-attach-error.${s.sessionId}`}
                          className="mt-0.5 text-[10px] text-[#f7768e] break-words"
                          role="alert"
                        >
                          Attach failed: {row.error}
                        </div>
                      )}
                      {row?.openedId && !row.error && !pending && (
                        <div
                          data-ui-bridge-id={`terminal.fleet-session-attach-open.${s.sessionId}`}
                          className="mt-0.5 text-[10px] text-[#9ece6a]"
                        >
                          Attached — tab open on this page.
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
