import { useCallback, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  ChevronsDown,
  Link2,
  Monitor,
  Plus,
  Power,
  RefreshCw,
  Search,
  Server,
  X,
} from "lucide-react";
import {
  useFleetSessions,
  deviceLabel as fleetDeviceLabel,
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
  attachWaitingMessage,
  fleetSessionAttachId,
  openedTabStillOpen,
  remoteSessionLabel,
  type RemoteTerminalInfoWire,
} from "./remoteTabs";
import { useRemoteAttachWaiting } from "./useRemoteAttachWaiting";
import {
  describeFact,
  describeSurface,
  devicesToProbe,
  FACT_TONE_CLASS,
  servesInteractivity,
} from "./remoteInteractivityFacts";
import { useInteractivityProbe } from "./useInteractivityProbe";
import {
  createButtonState,
  describeRemoteCreateFailure,
  fleetDeviceCreateErrorId,
  fleetDeviceCreateId,
  IDLE_DEVICE_CREATE,
  type DeviceCreateState,
} from "./remoteCreate";
import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { RemoteSessionEndFlow, type RemoteEndTarget } from "./RemoteSessionEndFlow";
import { CloseAllFinishedDialog, type BulkEndItem } from "./CloseAllFinishedDialog";
import { useFinishedFleetSessions } from "./useFinishedFleetSessions";
import {
  closeAllFinishedCandidates,
  closeAllFinishedLabel,
  closeAllFinishedTitle,
  endButtonState,
  endedHiddenMessage,
  fleetSessionEndId,
  fleetSessionEndResultId,
  FLEET_CLOSE_ALL_FINISHED_ID,
  FLEET_ENDED_HIDDEN_ID,
  FLEET_FINISHED_UNAVAILABLE_ID,
  type EndResultView,
} from "./remoteSessionEndView";
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
/**
 * Control id for the picker's ROOT — the element carrying every `data-fleet-*`.
 *
 * `data-page-element` does NOT put an element in
 * `GET /ui-bridge/control/snapshot`: the scanner registers interactive
 * elements plus anything carrying `data-ui-bridge-id`, and a `<div>` has
 * neither a role nor a qualifying tag (the same finding `pastSessionRowId`
 * exists for, documented at `PastSessionsView.tsx`). Without this stamp the
 * whole projected block below is reachable only by an explicit selector, and
 * a driver reading the snapshot sees none of it.
 *
 * What it COSTS, because the rest of this docstring only says what it buys.
 * Registering a container also emits that container's `text` and
 * `textContent`, both UNCAPPED — only `label` is capped, at 80 codepoints — so
 * the panel's visible text joins the snapshot twice more, on top of the
 * per-row copies each `fleetSessionRowId` already contributes. At a few
 * hundred sessions that is tens of KB per snapshot. And `inferElementType`
 * calls a role-less `div` `generic`, whose actions include `click`, so a
 * walker exercising every registered element will click a full-panel
 * container. Both are equally true of `pastSessionRowId`; neither is free.
 */
export const FLEET_PICKER_ROOT_ID = "terminal.fleet-picker-root";
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

/**
 * The "New terminal" action for one device. Disabled WITH a reason for the
 * caller's own machine (that is the ordinary local button) and for a group
 * whose device id coord could not vouch for.
 */
function RemoteCreateButton({
  deviceId,
  deviceLabel,
  isCallerDevice,
  state,
  onCreate,
  className,
}: {
  deviceId: string;
  deviceLabel: string;
  isCallerDevice: boolean;
  state: DeviceCreateState | undefined;
  onCreate: (deviceId: string, deviceLabel: string) => Promise<void>;
  className?: string;
}) {
  const btn = createButtonState({ deviceId, isCallerDevice });
  const pending = state?.pending === true;
  return (
    <button
      type="button"
      data-ui-bridge-id={fleetDeviceCreateId(deviceId)}
      onClick={() => void onCreate(deviceId, deviceLabel)}
      disabled={btn.disabled || pending}
      aria-disabled={btn.disabled || pending}
      title={
        btn.reason ??
        (pending
          ? "Creating — minting a grant, waiting for the remote runner to spawn, then attaching"
          : `Open a NEW terminal on ${deviceLabel}. That machine picks the working directory ` +
            `from its own allowed list; this one never sends a path.`)
      }
      className={
        "flex items-center gap-1 shrink-0 px-1.5 py-0.5 rounded text-[10px] " +
        "bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/30 transition-colors " +
        "disabled:opacity-40 disabled:cursor-not-allowed " +
        (className ?? "")
      }
    >
      {pending ? (
        <div className="w-2.5 h-2.5 border-2 border-[#9ece6a] border-t-transparent rounded-full animate-spin" />
      ) : (
        <Plus className="w-2.5 h-2.5" />
      )}
      {pending ? "Creating…" : "New terminal"}
    </button>
  );
}

/**
 * What a create ANSWERED with, rendered inline and kept until the next attempt.
 *
 * **The refusal is the deliverable here, not the success.** `accept_remote_create`
 * is off on every device until someone opts in, so the first use of the button
 * against any target is refused — by design, not by fault. This panel therefore
 * renders the refusing party's own explanation plus the concrete steps that
 * change it, and says separately when a terminal WAS spawned that this window
 * is not showing.
 */
function RemoteCreateOutcome({
  deviceId,
  state,
  onRetry,
}: {
  deviceId: string;
  state: DeviceCreateState | undefined;
  onRetry: () => void;
}) {
  const { tabs } = useTerminalSession();
  if (!state || state.pending) return null;
  if (state.refusal) {
    const r = state.refusal;
    return (
      <div
        data-ui-bridge-id={fleetDeviceCreateErrorId(deviceId)}
        data-remote-create-code={r.code}
        data-remote-create-stage={r.stage}
        role="alert"
        className="px-3 py-1.5 border-b border-[#2a2d3d] bg-[#f7768e]/5 text-[10px] break-words"
      >
        <div className="flex items-start gap-1.5">
          <AlertTriangle className="w-3 h-3 mt-px shrink-0 text-[#f7768e]" />
          <div className="min-w-0">
            <div className="text-[#f7768e] font-medium">{r.headline}</div>
            <div className="mt-0.5 text-[#a9b1d6]">{r.explanation}</div>
            {r.remedy.length > 0 && (
              <ul className="mt-1 space-y-0.5 text-[#c0caf5] list-disc pl-3.5">
                {r.remedy.map((step) => (
                  <li key={step}>{step}</li>
                ))}
              </ul>
            )}
            {r.strandedTerminalId && (
              <div className="mt-1 text-[#e0af68]">
                A terminal ({r.strandedTerminalId}) IS running on that machine and is not shown
                here. Retrying creates another one.
              </div>
            )}
            <div className="mt-1 flex items-center gap-2">
              <button
                type="button"
                onClick={onRetry}
                className="px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#c0caf5] hover:bg-[#3a3d4d] transition-colors"
              >
                Try again
              </button>
              {r.code && <span className="text-[#565f89]">code: {r.code}</span>}
            </div>
          </div>
        </div>
      </div>
    );
  }
  if (openedTabStillOpen(state.openedId, tabs)) {
    return (
      <div
        data-ui-bridge-id={`terminal.fleet-device-create-open.${deviceId}`}
        className="px-3 py-1 border-b border-[#2a2d3d] text-[10px] text-[#9ece6a]"
      >
        Created and attached — tab open on this page.
      </div>
    );
  }
  return null;
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

  /**
   * Sessions THIS UI saw end (`ended`) or found already gone (`not_found`).
   * Hidden from the list because coord learns a device's close late (plan
   * `2026-09-30-close-remote-sessions-from-the-local-runner`, Risks) — the
   * fleet row would otherwise re-offer a session that no longer exists. A
   * note says how many are hidden and why.
   */
  const [endedHere, setEndedHere] = useState<ReadonlySet<string>>(() => new Set());
  /** The last end result per row that did NOT end it — kept inline. */
  const [endResults, setEndResults] = useState<Record<string, EndResultView>>({});
  const [endTarget, setEndTarget] = useState<RemoteEndTarget | null>(null);
  const [bulkItems, setBulkItems] = useState<{ items: BulkEndItem[]; moreExist: boolean } | null>(
    null,
  );
  const finished = useFinishedFleetSessions();
  const refreshFinished = finished.refresh;

  const visible = useMemo(() => filterFleetSessions(sessions, text), [sessions, text]);
  const groups = useMemo(
    () => groupByDevice(visible.filter((s) => !endedHere.has(s.sessionId))),
    [visible, endedHere],
  );
  const finishedCandidates = useMemo(
    () =>
      finished.read.kind === "ok"
        ? closeAllFinishedCandidates(
            finished.read.sessions,
            endedHere,
            finished.read.callerDeviceId,
          )
        : [],
    [finished.read, endedHere],
  );
  const endedHiddenNote = endedHiddenMessage(
    sessions.filter((s) => endedHere.has(s.sessionId)).length,
  );
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
  // Pass the tenant the envelope says this read covered, so an empty page names
  // its scope instead of claiming the fleet. `fleetEmptyReadMessage` says
  // UNKNOWN rather than guessing when it is missing: coord types it
  // `tenant_id: Uuid`, but `fleet_sessions_list` hands the body back as an
  // untyped `serde_json::Value`, so nothing actually checks the field.
  //
  // Completeness is coord's POSITIVE signal — `kind === "none"`, coord said
  // this was the last page — rather than the absence of `"more-available"`,
  // which would read `unreachable` (coord served a cursor the walk can no
  // longer use) as a finished walk. That is defence in depth, not a live bug:
  // wherever this message renders the accumulation is empty, so the last page
  // was empty, so it carried no cursor (coord's `finish_page` truncates to
  // `limit >= 1` rows before minting one) and the kind is always `"none"`.
  // Reading the positive signal costs nothing and does not depend on that
  // chain holding.
  const emptyRead = fleetEmptyReadMessage(
    appliedQuery ?? server,
    text,
    response?.tenantId ?? null,
    truncation.kind === "none",
  );
  const devicesLoaded = useMemo(() => new Set(sessions.map((s) => s.deviceId)).size, [sessions]);
  const filteredOut = fleetFilteredOutMessage(
    sessions.length,
    visible.length,
    text,
    truncation.kind === "more-available",
  );
  const conflict = fleetFilterConflict(server);

  /**
   * Remote interactivity (plan
   * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
   * A3): loading the Fleet view probes each REMOTE device's not-fresh sessions
   * once, then re-reads so the facts it filed show. Off against a coord that
   * serves no facts (it has no door to record them in).
   */
  const interactivityServed = servesInteractivity(response);
  const probeDevices = useMemo(() => devicesToProbe(deviceCatalog), [deviceCatalog]);
  const refreshAfterSweep = useCallback(() => void refresh(), [refresh]);
  const { sweeping: probingDevice, errors: probeErrors } = useInteractivityProbe(
    probeDevices,
    interactivityServed,
    refreshAfterSweep,
  );

  const { pageId, setActiveId, tabs } = useTerminalSession();
  const [attachState, setAttachState] = useState<Record<string, RowAttachState>>({});
  /** The runner's own progress while it re-presents a grant the target has
   * not recorded yet — keyed by session id, empty when nothing is waiting. */
  const attachWaiting = useRemoteAttachWaiting();
  /** Per-DEVICE create state. Keyed by device id: the action belongs to the
   * group header, not to any one session row. */
  const [createState, setCreateState] = useState<Record<string, DeviceCreateState>>({});

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

  /**
   * "New terminal" on a device group — the CREATE half of parity with a headed
   * runner (Phase 5). One operator action covers the whole flow: the runner
   * mints a single-use create grant from coord, presents it through the relay,
   * the TARGET picks the working directory out of its own configuration and
   * registers the new PTY as a coord session, and this side then mints an
   * attach grant for that session and opens the tab — so the operator ends up
   * IN the terminal rather than being told one was made.
   *
   * The caller supplies no path and no repo: those are the target's to choose,
   * and a caller-chosen working directory is the hole this plan's D2 closed.
   */
  const createRemote = useCallback(
    async (deviceId: string, deviceLabel: string) => {
      const set = (patch: Partial<DeviceCreateState>) =>
        setCreateState((prev) => ({
          ...prev,
          [deviceId]: { ...(prev[deviceId] ?? IDLE_DEVICE_CREATE), ...patch },
        }));
      set({ pending: true, refusal: null, openedId: null });
      try {
        const info = await invoke<RemoteTerminalInfoWire>("terminal_create_remote", {
          deviceId,
          deviceLabel,
          title: null,
          // The target offers a SET of roots and answers with its own default.
          // Naming a key is the most a caller may do, and this action does not.
          workingDirKey: null,
          intentRepo: null,
          pageId: pageId !== "default" ? pageId : null,
        });
        set({ pending: false, openedId: info.id });
        setActiveId(info.id);
        // The new session exists on that device now; the list should show it.
        void refresh();
      } catch (err) {
        set({ pending: false, refusal: describeRemoteCreateFailure(err) });
      }
    },
    [pageId, refresh, setActiveId],
  );

  const remoteCount = visible.filter((s) => !s.isCallerDevice).length;

  /** Fold one row's end result in: hide it if it ended/was gone, else keep the line. */
  const recordEnd = useCallback((sessionId: string, view: EndResultView) => {
    if (view.hidesRow) {
      setEndedHere((prev) => new Set(prev).add(sessionId));
      setEndResults((prev) => {
        if (!(sessionId in prev)) return prev;
        const next = { ...prev };
        delete next[sessionId];
        return next;
      });
    } else {
      setEndResults((prev) => ({ ...prev, [sessionId]: view }));
    }
  }, []);

  const refreshAll = useCallback(() => {
    void refresh();
    void refreshFinished();
  }, [refresh, refreshFinished]);

  /**
   * Open the bulk confirm over a FRESH read. Paging under the filter is not a
   * snapshot — a session can become finished mid-walk — so the count on the
   * button is re-derived at the moment of confirming, and the dialog then runs
   * over exactly the list it SHOWED (a snapshot in `bulkItems`; nothing is
   * re-fetched between confirm and run). The client-side finished guard is
   * applied again inside `closeAllFinishedCandidates`.
   */
  const [bulkPreparing, setBulkPreparing] = useState(false);
  const openBulk = useCallback(async () => {
    setBulkPreparing(true);
    try {
      const read = await refreshFinished();
      if (read.kind !== "ok") return;
      const candidates = closeAllFinishedCandidates(read.sessions, endedHere, read.callerDeviceId);
      if (candidates.length === 0) return;
      setBulkItems({
        moreExist: read.capped,
        items: candidates.map((s) => ({
          sessionId: s.sessionId,
          deviceId: s.deviceId,
          deviceLabel: fleetDeviceLabel(s),
          sessionLabel: remoteSessionLabel(s),
        })),
      });
    } finally {
      setBulkPreparing(false);
    }
  }, [refreshFinished, endedHere]);

  return (
    <div
      data-page-element={FLEET_SESSION_PICKER_ELEMENT}
      // Registers the root so the `data-fleet-*` block below is CAPTURED as
      // `dataset` in the control snapshot. `data-page-element` alone leaves it
      // scrapeable by selector only — see `FLEET_PICKER_ROOT_ID`.
      data-ui-bridge-id={FLEET_PICKER_ROOT_ID}
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
      // The tenant the served rows were scoped to — the fact whose absence let
      // an empty read claim the whole fleet on 2026-09-28. It belongs in this
      // block and not beside the prose that states it, for two separate
      // reasons: `read-value` returns an element's text and never its
      // attributes, so the sentence is all that route can see; and only a
      // REGISTERED element contributes its `dataset` to the control snapshot,
      // which is what the `data-ui-bridge-id` above buys for this block and
      // for nothing else in the subtree.
      //
      // "" covers both "no successful read yet" and "coord named no tenant",
      // the same convention as `data-fleet-error-code`. They are told apart by
      // `data-fleet-truncation`: `fleetTruncation(null, ..)` is "unknown", and
      // `loaded` implies a non-null response (both set in one tick), so
      // "unknown" means no successful read and anything else means coord
      // answered without naming a tenant. NOT `data-fleet-loaded`, which reads
      // 0 for both in the only state this attribute is about — the empty
      // read — so it settles the question exactly where it never arises.
      data-fleet-tenant={response?.tenantId ?? ""}
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
          {/* Suppressed at zero rows, and that is the FIRST line of the
              2026-09-28 false report, not a tidy-up: `fleetCountSummary` emits
              "0 sessions on 0 devices" with no scope at all, directly under a
              panel titled Fleet and directly above the message. Fixing only
              the sentence below would have left the same unscoped claim on the
              line an operator reads first. With no rows it carries nothing the
              scoped message does not — all five of its inputs are necessarily
              0, so the suppressed string is always exactly that one — so it is
              not rendered rather than being given a second copy of the tenant.
              `visible.length` would be the WRONG predicate: at 47 loaded and 0
              matched it would suppress "0 of 47 loaded on 0 of 3 devices",
              which is the most informative reading of this line. Two knock-on
              effects, both benign: the count is also suppressed in the error
              and not-loaded empty states, where a different string renders;
              and a UI Bridge `read-value` on this span returns `null` there
              (`readGatedValue` takes the text path for a span, and an empty
              `textContent` becomes `undefined ?? null`), not "". The
              tenant itself is projected as an ATTRIBUTE on the root element,
              not read off this prose. */}
          {sessions.length === 0
            ? null
            : fleetCountSummary({
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
        {/* Close all finished (Phase 5c). The count is REMOTE finished
            sessions from coord's `session_status=finished` read — never this
            device's, and never a number when that read failed. */}
        <button
          type="button"
          data-ui-bridge-id={FLEET_CLOSE_ALL_FINISHED_ID}
          data-finished-read={finished.read.kind}
          data-finished-count={finished.read.kind === "ok" ? finishedCandidates.length : ""}
          onClick={() => void openBulk()}
          disabled={bulkPreparing || finished.read.kind !== "ok" || finishedCandidates.length === 0}
          aria-disabled={
            bulkPreparing || finished.read.kind !== "ok" || finishedCandidates.length === 0
          }
          title={closeAllFinishedTitle(finished.read, finishedCandidates.length)}
          className="flex items-center gap-1 shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-[#e0af68]/15 text-[#e0af68] hover:bg-[#e0af68]/30 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          <Power className="w-2.5 h-2.5" />
          {closeAllFinishedLabel(finished.read, finishedCandidates.length)}
        </button>
        <button
          data-ui-bridge-id={FLEET_PICKER_REFRESH_ID}
          onClick={refreshAll}
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

      {(finished.read.kind === "unavailable" || finished.read.kind === "error") && (
        <div
          data-ui-bridge-id={FLEET_FINISHED_UNAVAILABLE_ID}
          data-finished-read={finished.read.kind}
          className="flex items-start gap-1.5 px-3 py-1 text-[10px] text-[#e0af68] border-b border-[#2a2d3d]"
        >
          <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
          <span className="min-w-0 break-words">
            {finished.read.kind === "unavailable"
              ? "Finished filter unavailable — "
              : "Could not read finished sessions — "}
            {finished.read.message}
          </span>
        </div>
      )}
      {endedHiddenNote && (
        <div
          data-ui-bridge-id={FLEET_ENDED_HIDDEN_ID}
          className="px-3 py-1 text-[10px] text-[#565f89] border-b border-[#2a2d3d]"
        >
          {endedHiddenNote}
        </div>
      )}

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
                state has to draw it too.

                ⚠️ The `walkStalled` arm below is DEAD, and this comment used to
                claim the reachability that would make it live ("coord serves an
                empty page carrying a cursor and the walk then stalls on the
                next one"). Two independent reasons it cannot happen. coord's
                `finish_page` truncates to `limit >= 1` rows before minting a
                cursor, so an empty page carries none. And `fleetCursorStalled`
                requires `sent !== null`, so stalling needs a cursor from an
                earlier page, which needs that page to have had rows — putting
                `sessions.length` above 0 and this whole branch out of reach.
                Left in place as defence rather than deleted, but labelled, so
                the next reader does not take it for an observed state. */}
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
                {/* A device pinned by id that served no rows renders NO group,
                    so the create action above is unreachable — and that is
                    precisely the case this feature exists for: a headless
                    runner with nothing running on it yet. Offer it here. */}
                {appliedQuery?.deviceId && isLikelyDeviceId(appliedQuery.deviceId) && (
                  <div className="mt-3 text-left">
                    <RemoteCreateButton
                      deviceId={appliedQuery.deviceId}
                      deviceLabel={`device ${appliedQuery.deviceId.slice(0, 8)}`}
                      isCallerDevice={false}
                      state={createState[appliedQuery.deviceId]}
                      onCreate={createRemote}
                      className="mx-auto"
                    />
                    <RemoteCreateOutcome
                      deviceId={appliedQuery.deviceId}
                      state={createState[appliedQuery.deviceId]}
                      onRetry={() =>
                        void createRemote(
                          appliedQuery.deviceId as string,
                          `device ${(appliedQuery.deviceId as string).slice(0, 8)}`,
                        )
                      }
                    />
                  </div>
                )}
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
                  {probingDevice === g.deviceId && (
                    <span
                      data-ui-bridge-id={`terminal.fleet-device-probing.${g.deviceId}`}
                      className="text-[9px] text-[#e0af68]"
                      role="status"
                      title="Measuring whether this device's sessions are readable and writable from here — no byte is typed into any session"
                    >
                      measuring…
                    </span>
                  )}
                  {probeErrors[g.deviceId] && (
                    <span
                      data-ui-bridge-id={`terminal.fleet-device-probe-error.${g.deviceId}`}
                      className="text-[9px] text-[#f7768e] truncate"
                      title={probeErrors[g.deviceId]}
                    >
                      measurement failed
                    </span>
                  )}
                  <div className="flex-1" />
                  {/* CREATE (Phase 5). A per-DEVICE action, so it lives on the
                      group header — the per-tab affordances belong in
                      RemoteTabControls and a per-session row cannot express
                      "make a new one here". */}
                  <RemoteCreateButton
                    deviceId={g.deviceId}
                    deviceLabel={g.label}
                    isCallerDevice={g.isCallerDevice}
                    state={createState[g.deviceId]}
                    onCreate={createRemote}
                  />
                </div>
                <RemoteCreateOutcome
                  deviceId={g.deviceId}
                  state={createState[g.deviceId]}
                  onRetry={() => void createRemote(g.deviceId, g.label)}
                />

                {g.sessions.map((s) => {
                  const btn = attachButtonState(s, response?.deviceIdentityColumnsPresent);
                  const endBtn = endButtonState(s, response?.deviceIdentityColumnsPresent);
                  const endResult = endResults[s.sessionId];
                  const row = attachState[s.sessionId];
                  const pending = row?.pending === true;
                  // The attach is not a single round trip: a target that has
                  // not recorded the grant yet is waited out, same grant
                  // re-presented, for up to a whole catch-up poll tick.
                  const waitingLine = attachWaitingMessage(attachWaiting[s.sessionId]);
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
                              ? (waitingLine ??
                                "Attaching — minting a grant and waiting for the remote runner")
                              : `Open a tab onto this session on ${g.label}`)
                          }
                          className="flex items-center gap-1 shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                        >
                          {pending ? (
                            <div className="w-2.5 h-2.5 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
                          ) : (
                            <Link2 className="w-2.5 h-2.5" />
                          )}
                          {pending ? (waitingLine ? "Waiting…" : "Attaching…") : "Attach"}
                        </button>
                        {/* End on remote (Phase 5b). Not offered for this
                          machine's own sessions — those are local. */}
                        {!s.isCallerDevice && (
                          <button
                            type="button"
                            data-ui-bridge-id={fleetSessionEndId(s.sessionId)}
                            onClick={() =>
                              setEndTarget({
                                deviceId: s.deviceId,
                                deviceLabel: g.label,
                                sessionId: s.sessionId,
                                sessionLabel: remoteSessionLabel(s),
                              })
                            }
                            disabled={endBtn.disabled}
                            aria-disabled={endBtn.disabled}
                            aria-label="End on remote session"
                            title={
                              endBtn.reason ??
                              `End this session on ${g.label} (graceful /exit, confirmed first)`
                            }
                            className="flex items-center gap-1 shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-[#e0af68]/15 text-[#e0af68] hover:bg-[#e0af68]/30 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                          >
                            <Power className="w-2.5 h-2.5" />
                            End
                          </button>
                        )}
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
                      {interactivityServed &&
                        (() => {
                          // The two measured facts. An ABSENT fact (a coord
                          // before them) renders nothing — never "failed" —
                          // and `unknown` carries its reason, distinct from
                          // `failed`, which carries the refusal code.
                          const read = describeFact("read", s.readableRemotely);
                          const write = describeFact("write", s.writableRemotely);
                          const surface = describeSurface(s.interactiveSurface);
                          if (!read && !write && !surface) return null;
                          return (
                            <div
                              data-ui-bridge-id={`terminal.fleet-session-interactivity.${s.sessionId}`}
                              data-read-state={read?.state}
                              data-read-reason={read?.reason ?? undefined}
                              data-write-state={write?.state}
                              data-write-reason={write?.reason ?? undefined}
                              data-surface={s.interactiveSurface}
                              className="text-[10px] text-[#565f89] truncate"
                            >
                              {[read, write].map((f, i) =>
                                f ? (
                                  <span key={i} className={FACT_TONE_CLASS[f.tone]} title={f.title}>
                                    {i > 0 && read ? " · " : ""}
                                    {f.label}
                                  </span>
                                ) : null,
                              )}
                              {surface && (
                                <span title="Whether coord classifies this session as a remote PTY">
                                  {read || write ? " · " : ""}
                                  {surface}
                                </span>
                              )}
                            </div>
                          );
                        })()}
                      {waitingLine && (
                        <div
                          data-ui-bridge-id={`terminal.fleet-session-attach-waiting.${s.sessionId}`}
                          data-attach-wait-attempt={attachWaiting[s.sessionId]?.attempt}
                          className="mt-0.5 text-[10px] text-[#e0af68] break-words"
                          role="status"
                        >
                          {waitingLine}
                        </div>
                      )}
                      {endResult && (
                        <div
                          data-ui-bridge-id={fleetSessionEndResultId(s.sessionId)}
                          data-end-outcome={endResult.outcome}
                          className="mt-0.5 text-[10px] break-words"
                          role="status"
                        >
                          <span className={endResult.toneClass}>End: {endResult.label}</span>
                          <span className="text-[#565f89]">
                            {" "}
                            — {endResult.headline}
                            {endResult.detail ? ` (${endResult.detail})` : ""}
                          </span>
                        </div>
                      )}
                      {row?.error && (
                        <div
                          data-ui-bridge-id={`terminal.fleet-session-attach-error.${s.sessionId}`}
                          className="mt-0.5 text-[10px] text-[#f7768e] break-words"
                          role="alert"
                        >
                          Attach failed: {row.error}
                        </div>
                      )}
                      {/* Read against the live tab list, not the id alone:
                        the id is set once on success, so it outlives the tab. */}
                      {openedTabStillOpen(row?.openedId, tabs) && !row?.error && !pending && (
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

      {endTarget && (
        <RemoteSessionEndFlow
          target={endTarget}
          onClose={(_result, view) => {
            if (view) recordEnd(endTarget.sessionId, view);
            setEndTarget(null);
          }}
        />
      )}
      {bulkItems &&
        createPortal(
          <CloseAllFinishedDialog
            items={bulkItems.items}
            moreExist={bulkItems.moreExist}
            onClose={(results) => {
              for (const [id, view] of Object.entries(results)) {
                if (view) recordEnd(id, view);
              }
              setBulkItems(null);
            }}
          />,
          document.body,
        )}
    </div>
  );
}
