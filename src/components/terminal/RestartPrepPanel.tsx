/**
 * The "prepare for restart" panel and its header button (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 5).
 *
 * - {@link PrepareRestartButton} — "Prepare for restart" on the Terminal page,
 *   beside the session-count banner. It acknowledges itself: the click turns
 *   it into a disabled `Preparing…` spinner in the same frame, then it reads
 *   the prepare's own state ("Safe to restart" / "Waiting on N" / UNKNOWN) or
 *   the failure.
 * - {@link RestartPrepPanel} — the hold, one row per cohort session (name,
 *   account, effort and where it was read, page, state chip, held toggle), a
 *   headline that is exactly one of {@link READY_HEADLINE} / "Waiting on N
 *   sessions" / "UNKNOWN: <input>", and Cancel.
 *
 * Both read {@link restartPrepStore}, which listens to the backend's
 * `restart-prep-changed` event instead of polling. Every chip is the backend's
 * observation; nothing here derives "paused" or "ready" for itself.
 */

import { useEffect, useSyncExternalStore } from "react";
import { Loader2, Power, X } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";

import {
  blockerLine,
  effortBadge,
  inWindow,
  restartPrepHeadline,
  restartPrepStore,
  rowName,
  rowStateChip,
  type ActionState,
  type CohortRow,
  type PrepState,
  type RestartPrepSnapshot,
  type RestartPrepStore,
} from "./restartPrep";

export const PREPARE_RESTART_BUTTON_ID = "terminal.prepare-restart-button";
export const RESTART_PREP_PANEL_ID = "terminal.restart-prep-panel";
export const RESTART_PREP_CANCEL_ID = "terminal.restart-prep-cancel";
export const RESTART_PREP_EXTEND_ID = "terminal.restart-prep-extend";
export const RESTART_PREP_DISMISS_ID = "terminal.restart-prep-dismiss";
export const restartPrepRowId = (sessionId: string): string =>
  `terminal.restart-prep-row-${sessionId}`;
export const restartPrepHeldId = (sessionId: string): string =>
  `terminal.restart-prep-held-${sessionId}`;

/** The store's snapshot, with the event listener held while mounted. */
export function useRestartPrep(store: RestartPrepStore = restartPrepStore): RestartPrepSnapshot {
  useEffect(() => store.retain(), [store]);
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

const busy = (a: ActionState): boolean => a.phase === "in-flight";

/**
 * The header button's label, disabled flag and tone — pure, so the
 * acknowledgement states are pinned without a DOM.
 *
 * While a prepare is in its window the button reads that prepare's headline
 * and a click only OPENS the panel: the prepare is already running, and
 * re-sending it (a write that may retry the coord hold and starts a notice
 * pass) is `/prepare-restart`'s job, not a glance at the progress. "Disabled
 * while preparing" is therefore the request in flight, not the whole window.
 */
export function prepareButtonState(snap: RestartPrepSnapshot): {
  label: string;
  disabled: boolean;
  spinner: boolean;
  tone: "idle" | "ok" | "wait" | "error";
  title: string;
} {
  if (!snap.loaded && snap.readError === null) {
    return {
      label: "Restart prep: reading…",
      disabled: false,
      spinner: true,
      tone: "wait",
      title: "Reading whether a prepare is already running — click to open the panel",
    };
  }
  if (snap.prepare.phase === "in-flight") {
    return {
      label: "Preparing…",
      disabled: true,
      spinner: true,
      tone: "wait",
      title: "Holding spawns and recording every session",
    };
  }
  if (inWindow(snap.record)) {
    const headline = restartPrepHeadline(snap);
    const label =
      headline?.kind === "ready"
        ? "Safe to restart"
        : headline?.kind === "waiting"
          ? `Restart prep: ${headline.text.replace(/ sessions?$/, "")}`
          : "Restart prep: UNKNOWN";
    return {
      label,
      disabled: false,
      spinner: false,
      tone: headline?.kind === "ready" ? "ok" : headline?.kind === "waiting" ? "wait" : "error",
      title: `${headline?.text ?? ""} — click to open the progress panel`,
    };
  }
  if (snap.record?.state === "restoring") {
    return {
      label: "Restart prep: restoring",
      disabled: false,
      spinner: false,
      tone: "wait",
      title:
        "The last prepare's sessions are waiting to be restored — click to open the panel. A new " +
        "prepare waits until they are restored or dismissed.",
    };
  }
  if (snap.prepare.phase === "failed") {
    return {
      label: "Prepare failed — retry",
      disabled: false,
      spinner: false,
      tone: "error",
      title: snap.prepare.message ?? "the prepare failed",
    };
  }
  if (snap.readError !== null) {
    // An unread record is not "no prepare": say so, and still let the
    // operator prepare (the backend answers a running prepare with itself).
    return {
      label: "Restart prep: UNKNOWN",
      disabled: false,
      spinner: false,
      tone: "error",
      title: `The restart-prep record could not be read (${snap.readError}) — click to prepare`,
    };
  }
  return {
    label: "Prepare for restart",
    disabled: false,
    spinner: false,
    tone: "idle",
    title:
      "Hold new spawns until the runner is rebuilt, tell live sessions to save and pause, keep " +
      "limit-hit sessions on their account, and record every session for the restore",
  };
}

const TONE_CLASS = {
  idle: "text-[#a9b1d6] hover:text-[#c0caf5] hover:bg-[#2a2d3d]",
  ok: "text-[#9ece6a] bg-[#9ece6a]/10 hover:bg-[#9ece6a]/20",
  wait: "text-[#e0af68] bg-[#e0af68]/10 hover:bg-[#e0af68]/20",
  error: "text-[#f7768e] bg-[#f7768e]/10 hover:bg-[#f7768e]/20",
} as const;

/**
 * The button's click: open the panel, and prepare unless a prepare is
 * already in its window. Resolves to the reply (or `null` when it only
 * opened the panel); rejects with the backend's refusal.
 */
function clickPrepare(store: RestartPrepStore): Promise<unknown> {
  store.openPanel();
  const snap = store.getSnapshot();
  // Not yet read, running, or a rebuilt runner's restore in progress: the
  // click only shows it. Re-sending a prepare is `/prepare-restart`'s job.
  if (!snap.loaded || inWindow(snap.record) || snap.record?.state === "restoring") {
    return Promise.resolve(null);
  }
  return store.prepare("");
}

/** The header button, and the panel below it while open. */
export function PrepareRestartButton({ store = restartPrepStore }: { store?: RestartPrepStore }) {
  const snap = useRestartPrep(store);
  const state = prepareButtonState(snap);

  useUIComponent({
    id: PREPARE_RESTART_BUTTON_ID,
    name: "Prepare for restart",
    description:
      "Prepare the runner for a restart: hold new spawns until it is rebuilt, tell live sessions " +
      "to save and pause, keep limit-hit sessions on their account, record every session, and " +
      "open the progress panel. While a prepare is running it only opens the panel.",
    actions: [
      {
        id: "open-panel",
        label: "Open the restart-prep panel",
        description: "Show the restart prepare's progress (or the last one's outcome).",
        // `read` — shows the panel; nothing about the prepare changes.
        effect: "read",
        handler: () => store.openPanel(),
      },
      {
        id: "prepare",
        label: "Prepare for restart",
        description:
          "Start the restart prepare and open its panel (only open it, while one is running).",
        // `destructive` (CONTRACT.md "Promotion to destructive", Dim 1 + Dim 2):
        // it drains this device's coord agent lane — machine/fleet state — and
        // types a save-and-pause notice into every live session, which Cancel
        // cannot take back. The plan's Phase 5 said `write`; the rubric decides.
        effect: "destructive",
        handler: () => clickPrepare(store),
      },
    ],
    // Read from the store at CALL time: `useUIComponent` keeps the getter it
    // registered first, so a closure over this render would serve the first
    // render's label for good.
    state: () => {
      const now = prepareButtonState(store.getSnapshot());
      return { label: now.label, disabled: now.disabled };
    },
  });

  return (
    <div className="flex flex-col">
      <div className="flex justify-end px-2 py-0.5">
        <button
          type="button"
          data-ui-bridge-id={PREPARE_RESTART_BUTTON_ID}
          data-prepare-phase={snap.prepare.phase}
          disabled={state.disabled}
          onClick={() => {
            clickPrepare(store).catch(() => {
              // Shown on the button and in the panel (`prepare.phase`).
            });
          }}
          title={state.title}
          className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors disabled:opacity-70 disabled:cursor-wait ${TONE_CLASS[state.tone]}`}
        >
          {state.spinner ? (
            <Loader2 className="w-2.5 h-2.5 animate-spin" />
          ) : (
            <Power className="w-2.5 h-2.5" />
          )}
          {state.label}
        </button>
      </div>
      {snap.panelOpen && <RestartPrepPanel snapshot={snap} store={store} />}
    </div>
  );
}

const CHIP_CLASS = {
  ok: "bg-[#9ece6a]/10 text-[#9ece6a]",
  wait: "bg-[#e0af68]/10 text-[#e0af68]",
  unknown: "bg-[#f7768e]/10 text-[#f7768e]",
} as const;

/** One cohort session. */
function RestartPrepRow({
  row,
  heldState,
  canToggle,
  store,
  recordState,
  current,
}: {
  recordState: PrepState;
  /** The record's verdict (and row states) were read by this process. */
  current: boolean;
  row: CohortRow;
  heldState: ActionState | undefined;
  canToggle: boolean;
  store: RestartPrepStore;
}) {
  const id = row.session.claudeSessionId;
  const chip = rowStateChip(row, recordState, current);
  const toggling = heldState?.phase === "in-flight";
  /** Rejects (to a UI Bridge caller) when the toggle cannot act — never a silent no-op. */
  const toggle = (): Promise<unknown> => {
    if (!canToggle) {
      return Promise.reject(new Error("this prepare is over — a held flag no longer applies"));
    }
    if (toggling) return Promise.reject(new Error("the held flag is already changing"));
    return store.setHeld(id, !row.held);
  };
  const onToggleClick = (): void => {
    toggle().catch(() => {
      // Shown on the toggle (`held[id].phase === "failed"`).
    });
  };

  useUIComponent({
    id: restartPrepHeldId(id),
    name: `Held for hand restore — ${rowName(row)}`,
    description:
      "Hold this session for a hand restore (bulk restores skip it), or release it to the bulk restores.",
    actions: [
      {
        id: "toggle",
        label: "Toggle hold",
        description: "Flip whether this session is held for a hand restore.",
        // `write` — flips one flag on the prepare record; toggling again reverses it.
        effect: "write",
        handler: () => toggle(),
      },
    ],
    // At call time, from the store (see the button's `state`).
    state: () => {
      const now = store.getSnapshot();
      const current = now.record?.cohort.rows.find((r) => r.session.claudeSessionId === id);
      return { held: current?.held ?? null, phase: now.held[id]?.phase ?? "idle" };
    },
  });

  const effortTitle =
    `effort ${row.effort.level ?? "unknown"} (${row.effort.source}); baseline ` +
    `${row.baseline.baseline ?? "unknown"} (${row.baseline.default_source})`;

  return (
    <div
      data-ui-bridge-id={restartPrepRowId(id)}
      data-pause-state={row.pause_state ?? "unread"}
      className="flex items-center gap-1.5 px-2 py-1 text-[10px] border-t border-[#2a2d3d]"
    >
      <span
        className="truncate min-w-[6rem] flex-1 text-[#c0caf5]"
        title={`${rowName(row)}\n${id}`}
      >
        {rowName(row)}
      </span>
      <span className="shrink-0 px-1 rounded bg-[#7aa2f7]/10 text-[#7aa2f7]">
        {row.session.accountLabel?.trim() || "account unknown"}
      </span>
      <span className="shrink-0 px-1 rounded bg-[#bb9af7]/10 text-[#bb9af7]" title={effortTitle}>
        {effortBadge(row)}
      </span>
      {/* The page id is a UUID: it truncates, never the name beside it. */}
      <span
        className="min-w-0 max-w-[7rem] truncate text-[#565f89]"
        title={`page ${row.session.pageId}, zone ${row.session.zoneIndex + 1}`}
      >
        zone {row.session.zoneIndex + 1} · page {row.session.pageId}
      </span>
      <span
        className={`shrink-0 px-1 rounded ${CHIP_CLASS[chip.tone]}`}
        title={row.pause_reason ?? undefined}
      >
        {chip.text}
      </span>
      <button
        type="button"
        data-ui-bridge-id={restartPrepHeldId(id)}
        data-held={row.held ? "true" : "false"}
        data-held-phase={heldState?.phase ?? "idle"}
        disabled={!canToggle || toggling}
        onClick={onToggleClick}
        title={
          heldState?.phase === "failed"
            ? `Could not change the hold: ${heldState.message ?? ""}`
            : row.held
              ? "Held for a hand restore — bulk restores skip it. Click to release."
              : "Restored by the bulk restores. Click to hold it for a hand restore."
        }
        className={`shrink-0 flex items-center gap-1 px-1 rounded disabled:opacity-60 ${
          heldState?.phase === "failed"
            ? "text-[#f7768e] bg-[#f7768e]/10"
            : row.held
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6]"
        }`}
      >
        {toggling && <Loader2 className="w-2.5 h-2.5 animate-spin" />}
        {row.held ? "held for hand restore" : "not held"}
      </button>
    </div>
  );
}

/** An RFC 3339 instant in the operator's locale; the raw value when unparseable. */
function formatWhen(rfc3339: string): string {
  const t = Date.parse(rfc3339);
  return Number.isNaN(t) ? rfc3339 : new Date(t).toLocaleString();
}

/** Where a prepare that is not in its window stands. */
function endedLine(snap: RestartPrepSnapshot): string {
  const rec = snap.record;
  // `record: None` is "no READABLE record" (`RestartPrepStatus::record`), not
  // proof that no prepare ran — an unparseable record reads the same.
  if (!rec) return "No readable prepare record: none is running, or its record could not be read.";
  switch (rec.state) {
    case "cancelled":
      return `Prepare cancelled — spawns are no longer held (${snap.view?.hold ?? "release unread"}).`;
    case "restoring":
      return "The runner restarted — its sessions are waiting to be restored.";
    case "done":
      return "The last prepare is done — every session was restored or dismissed.";
    default:
      return `Prepare ${rec.state}.`;
  }
}

/** What the panel's UI Bridge `state` reports, for a snapshot read at call time. */
export function panelStateOf(snap: RestartPrepSnapshot): Record<string, unknown> {
  const headline = restartPrepHeadline(snap);
  return {
    headline: headline?.text ?? null,
    headlineKind: headline?.kind ?? null,
    state: snap.record?.state ?? null,
    hold: snap.view?.hold ?? null,
    rows: snap.record?.cohort.rows.length ?? 0,
    liveUpdates: snap.eventsError === null,
  };
}

/** The panel. Presentational over a snapshot; actions go to `store`. */
export function RestartPrepPanel({
  snapshot: snap,
  store = restartPrepStore,
}: {
  snapshot: RestartPrepSnapshot;
  store?: RestartPrepStore;
}) {
  const rec = snap.record;
  const view = snap.view;
  const headline = restartPrepHeadline(snap);
  // Opening the panel reads the record once — the events keep it current
  // from there, and a panel reopened without them is re-read.
  useEffect(() => {
    void store.refresh();
  }, [store]);
  const active = inWindow(rec);
  const canToggle =
    rec?.state === "preparing" || rec?.state === "ready" || rec?.state === "restoring";
  const cancelBusy = busy(snap.cancel);
  const releaseLabel = view?.same_build_hold || view?.release_failed ? "Release hold" : "Cancel";
  /** Rejects (to a UI Bridge caller) when there is nothing to cancel. */
  const cancel = (): Promise<unknown> =>
    active
      ? store.cancel()
      : Promise.reject(new Error("no prepare is in progress — nothing to cancel"));
  const extend = (): Promise<unknown> =>
    active
      ? store.extend()
      : Promise.reject(new Error("no prepare is in progress — nothing to extend"));
  const swallow = (p: Promise<unknown>): void => {
    p.catch(() => {
      // Shown beside the button (`cancel` / `extend` phase `failed`).
    });
  };
  const extendBusy = busy(snap.extend);
  const restoring = rec?.state === "restoring";
  const dismissBusy = busy(snap.dismiss);
  const dismiss = (): Promise<unknown> =>
    restoring
      ? store.dismiss()
      : Promise.reject(new Error("no restore is waiting — nothing to dismiss"));

  useUIComponent({
    id: RESTART_PREP_PANEL_ID,
    name: "Restart prep",
    description:
      "The restart prepare's progress: the spawn hold, every recorded session's state, and whether " +
      "it is safe to restart.",
    actions: [
      {
        id: "refresh",
        label: "Refresh",
        description: "Re-read the prepare record.",
        // `read` — a query.
        effect: "read",
        handler: () => store.refresh(),
      },
      {
        id: "close",
        label: "Close",
        description: "Close the panel. The prepare keeps running.",
        // `read` — hides the panel; nothing about the prepare changes.
        effect: "read",
        handler: () => store.closePanel(),
      },
    ],
    // At call time, from the store (see the button's `state`).
    state: () => panelStateOf(store.getSnapshot()),
  });

  useUIComponent({
    id: RESTART_PREP_CANCEL_ID,
    name: "Cancel the restart prepare / release its hold",
    description:
      "Release the spawn hold and lift the account freeze; sessions that were told to pause are " +
      "told to resume. Notices already delivered stay delivered.",
    actions: [
      {
        id: "cancel",
        // A fixed label: `useUIComponent` re-registers actions only when its
        // component key changes, so a label that switched between "Cancel"
        // and "Release hold" would go stale on the bridge.
        label: "Cancel / release hold",
        description: "Cancel the prepare (or release a same-build restart's hold).",
        // `destructive` (CONTRACT.md Dim 2 + Dim 1): it releases this device's coord
        // drain — machine/fleet state — and types "resume your work" into every
        // session that was told to pause. The plan's Phase 5 said `write`.
        effect: "destructive",
        handler: () => cancel(),
      },
    ],
    state: () => {
      const now = store.getSnapshot();
      return { phase: now.cancel.phase, enabled: inWindow(now.record) && !busy(now.cancel) };
    },
  });

  useUIComponent({
    id: RESTART_PREP_EXTEND_ID,
    name: "Extend the restart hold",
    description:
      "Push the spawn hold's expiry 6 hours later (an expired hold is renewed from now), up to " +
      "coord's drain ceiling. Re-sent to coord as the same hold.",
    actions: [
      {
        id: "extend",
        label: "Extend 6 h",
        description: "Extend the spawn hold by 6 hours.",
        // `destructive` (CONTRACT.md Dim 2): it lengthens this device's coord drain —
        // machine/fleet state that keeps agent work off this machine for longer.
        effect: "destructive",
        handler: () => extend(),
      },
    ],
    state: () => {
      const now = store.getSnapshot();
      return { phase: now.extend.phase, enabled: inWindow(now.record) && !busy(now.extend) };
    },
  });

  useUIComponent({
    id: RESTART_PREP_DISMISS_ID,
    name: "Dismiss the waiting restore",
    description:
      "End a rebuilt runner's restore of the prepared sessions: the ones still waiting stop being " +
      "placeholders and go to Past Sessions (resumable by hand there), and a new prepare may start.",
    actions: [
      {
        id: "dismiss",
        label: "Dismiss",
        description: "Dismiss the sessions still waiting to be restored.",
        // `destructive` (CONTRACT.md Dim 1): the waiting placeholders lose the
        // shield that keeps them from the orphan close, and dismissing cannot be
        // undone — the sessions come back only by hand from Past Sessions.
        effect: "destructive",
        handler: () => dismiss(),
      },
    ],
    state: () => {
      const now = store.getSnapshot();
      return {
        phase: now.dismiss.phase,
        enabled: now.record?.state === "restoring" && !busy(now.dismiss),
      };
    },
  });

  const headlineClass =
    headline?.kind === "ready"
      ? "text-[#9ece6a]"
      : headline?.kind === "waiting"
        ? "text-[#e0af68]"
        : "text-[#f7768e]";

  return (
    <div
      data-ui-bridge-id={RESTART_PREP_PANEL_ID}
      data-headline-kind={headline?.kind ?? "none"}
      data-prep-state={rec?.state ?? "none"}
      className="mx-2 mb-1 rounded border border-[#2a2d3d] bg-[#1a1b26] text-[10px]"
    >
      <div className="flex items-center gap-2 px-2 py-1">
        <Power className="w-3 h-3 text-[#565f89]" />
        <span className="font-medium text-[#a9b1d6]">Prepare for restart</span>
        <div className="flex-1" />
        <button
          type="button"
          onClick={() => store.closePanel()}
          title="Close the panel — the prepare keeps running"
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
        >
          <X className="w-3 h-3" />
        </button>
      </div>

      {headline ? (
        <div className="px-2 pb-1" data-restart-prep-headline={headline.kind}>
          <div className={`font-medium ${headlineClass}`}>{headline.text}</div>
          {headline.kind !== "ready" && headline.blockers.length > 0 && (
            <ul className="mt-0.5 space-y-0.5 text-[#a9b1d6]">
              {headline.blockers.map((b, i) => (
                <li key={`${b.session ?? "prepare"}-${i}`}>{blockerLine(b)}</li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <div className="px-2 pb-1 text-[#a9b1d6]">{endedLine(snap)}</div>
      )}

      {snap.eventsError !== null && (
        <div className="px-2 pb-1 text-[#e0af68]">
          Live updates are unavailable ({snap.eventsError}) — this panel shows the last read; close
          and reopen it to read again.
        </div>
      )}

      {snap.prepare.phase === "failed" && !active && (
        <div className="px-2 pb-1 text-[#f7768e]">
          Prepare failed: {snap.prepare.message ?? "unknown error"}
        </div>
      )}

      {rec && view && (
        <div className="px-2 pb-1 text-[#a9b1d6]" data-restart-prep-hold={view.hold}>
          Hold: {view.hold}
          {active && rec.hold_until && !view.hold_expired_at && (
            <span title={rec.hold_until}> · expires {formatWhen(rec.hold_until)}</span>
          )}
          {active && (
            <button
              type="button"
              data-ui-bridge-id={RESTART_PREP_EXTEND_ID}
              data-extend-phase={snap.extend.phase}
              disabled={extendBusy}
              onClick={() => swallow(extend())}
              title="Push the hold's expiry 6 hours later"
              className="ml-2 inline-flex items-center gap-1 px-1 rounded text-[#7aa2f7] bg-[#7aa2f7]/10 hover:bg-[#7aa2f7]/20 disabled:opacity-60 disabled:cursor-wait"
            >
              {extendBusy && <Loader2 className="w-2.5 h-2.5 animate-spin" />}
              {extendBusy ? "Extending…" : "Extend 6 h"}
            </button>
          )}
          {snap.extend.phase === "done" && (
            <div className="text-[#565f89]">Extended — {snap.extend.message}</div>
          )}
          {snap.extend.phase === "failed" && (
            <div className="text-[#f7768e]">Extend failed: {snap.extend.message}</div>
          )}
          {view.same_build_hold && (
            <div className="text-[#e0af68]">
              Restarted on the same build. Spawns still held — Release hold to resume them.
            </div>
          )}
          {view.release_failed && (
            <div className="text-[#f7768e]">
              The hold was not released ({view.release_failed}) — the prepare is still in force.
            </div>
          )}
        </div>
      )}

      {rec && rec.cohort.rows.length === 0 && (
        <div className="px-2 pb-1 text-[#565f89]">
          No sessions were open when this was prepared.
        </div>
      )}
      {rec?.cohort.rows.map((row) => (
        <RestartPrepRow
          key={row.session.claudeSessionId}
          row={row}
          heldState={snap.held[row.session.claudeSessionId]}
          canToggle={canToggle}
          store={store}
          recordState={rec.state}
          current={view?.verdict_current === true}
        />
      ))}

      {restoring && (
        <div className="flex items-center gap-2 px-2 py-1 border-t border-[#2a2d3d]">
          <button
            type="button"
            data-ui-bridge-id={RESTART_PREP_DISMISS_ID}
            data-dismiss-phase={snap.dismiss.phase}
            disabled={dismissBusy}
            onClick={() => swallow(dismiss())}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[#f7768e] bg-[#f7768e]/10 hover:bg-[#f7768e]/20 disabled:opacity-60 disabled:cursor-wait"
          >
            {dismissBusy && <Loader2 className="w-2.5 h-2.5 animate-spin" />}
            {dismissBusy ? "Dismissing…" : "Dismiss the waiting restore"}
          </button>
          <span className="text-[#565f89]">
            Sessions not yet restored go to Past Sessions, resumable by hand.
          </span>
          {snap.dismiss.phase === "failed" && (
            <span className="text-[#f7768e]">{snap.dismiss.message}</span>
          )}
        </div>
      )}

      {active && (
        <div className="flex items-center gap-2 px-2 py-1 border-t border-[#2a2d3d]">
          <button
            type="button"
            data-ui-bridge-id={RESTART_PREP_CANCEL_ID}
            data-cancel-phase={snap.cancel.phase}
            disabled={cancelBusy}
            onClick={() => swallow(cancel())}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[#f7768e] bg-[#f7768e]/10 hover:bg-[#f7768e]/20 disabled:opacity-60 disabled:cursor-wait"
          >
            {cancelBusy && <Loader2 className="w-2.5 h-2.5 animate-spin" />}
            {cancelBusy ? "Releasing…" : releaseLabel}
          </button>
          <span className="text-[#565f89]">
            Releases the spawn hold and the account freeze; notices already sent stay sent.
          </span>
          {snap.cancel.phase === "failed" && (
            <span className="text-[#f7768e]">{snap.cancel.message}</span>
          )}
        </div>
      )}
    </div>
  );
}
