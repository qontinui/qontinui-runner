/**
 * The "prepare for restart" panel and its header button (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 5).
 *
 * - {@link PrepareRestartButton} — "Prepare for restart" on the Terminal page,
 *   beside the session-count banner. It acknowledges itself: `Received…`, then
 *   `Preparing…` with a spinner (disabled), then the prepare's own reading
 *   ("Safe to restart" / "Waiting on N" / UNKNOWN) or the failure.
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
  type RestartPrepSnapshot,
  type RestartPrepStore,
} from "./restartPrep";

export const PREPARE_RESTART_BUTTON_ID = "terminal.prepare-restart-button";
export const RESTART_PREP_PANEL_ID = "terminal.restart-prep-panel";
export const RESTART_PREP_CANCEL_ID = "terminal.restart-prep-cancel";
export const restartPrepRowId = (sessionId: string): string =>
  `terminal.restart-prep-row-${sessionId}`;
export const restartPrepHeldId = (sessionId: string): string =>
  `terminal.restart-prep-held-${sessionId}`;

/** The store's snapshot, with the event listener held while mounted. */
export function useRestartPrep(store: RestartPrepStore = restartPrepStore): RestartPrepSnapshot {
  useEffect(() => store.retain(), [store]);
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

const busy = (a: ActionState): boolean => a.phase === "received" || a.phase === "in-flight";

/**
 * The header button's label, disabled flag and tone — pure, so the
 * acknowledgement states are pinned without a DOM.
 */
export function prepareButtonState(snap: RestartPrepSnapshot): {
  label: string;
  disabled: boolean;
  spinner: boolean;
  tone: "idle" | "ok" | "wait" | "error";
  title: string;
} {
  if (snap.prepare.phase === "received") {
    return { label: "Received…", disabled: true, spinner: true, tone: "wait", title: "Received" };
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
  if (snap.prepare.phase === "failed") {
    return {
      label: "Prepare failed — retry",
      disabled: false,
      spinner: false,
      tone: "error",
      title: snap.prepare.message ?? "the prepare failed",
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
      title: headline?.text ?? "",
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

/** Prepare (or re-open the panel of the prepare already running). */
function startPrepare(store: RestartPrepStore): void {
  store.openPanel();
  store.prepare("").catch(() => {
    // The failure is on the button and in the panel (`prepare.phase`).
  });
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
      "open the progress panel. Idempotent while a prepare is running.",
    actions: [
      {
        id: "prepare",
        label: "Prepare for restart",
        description: "Start (or re-join) the restart prepare and open its panel.",
        // `write` — takes a spawn hold (a coord drain of this device's agent lane on
        // the primary) and types a save-and-pause notice into live sessions. Cancel
        // reverses all of it but the notices.
        effect: "write",
        handler: () => startPrepare(store),
      },
    ],
    state: () => ({ label: state.label, disabled: state.disabled }),
  });

  return (
    <div className="flex flex-col">
      <div className="flex justify-end px-2 py-0.5">
        <button
          type="button"
          data-ui-bridge-id={PREPARE_RESTART_BUTTON_ID}
          data-prepare-phase={snap.prepare.phase}
          disabled={state.disabled}
          onClick={() => startPrepare(store)}
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
}: {
  row: CohortRow;
  heldState: ActionState | undefined;
  canToggle: boolean;
  store: RestartPrepStore;
}) {
  const id = row.session.claudeSessionId;
  const chip = rowStateChip(row);
  const toggling = heldState?.phase === "in-flight";
  const toggle = (): void => {
    if (!canToggle || toggling) return;
    store.setHeld(id, !row.held).catch(() => {
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
        label: row.held ? "Release" : "Hold",
        description: "Flip whether this session is held for a hand restore.",
        // `write` — flips one flag on the prepare record; toggling again reverses it.
        effect: "write",
        handler: () => toggle(),
      },
    ],
    state: () => ({ held: row.held, phase: heldState?.phase ?? "idle" }),
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
      <span className="truncate min-w-0 flex-1 text-[#c0caf5]" title={`${rowName(row)}\n${id}`}>
        {rowName(row)}
      </span>
      <span className="shrink-0 px-1 rounded bg-[#7aa2f7]/10 text-[#7aa2f7]">
        {row.session.accountLabel?.trim() || "account unknown"}
      </span>
      <span className="shrink-0 px-1 rounded bg-[#bb9af7]/10 text-[#bb9af7]" title={effortTitle}>
        {effortBadge(row)}
      </span>
      <span className="shrink-0 text-[#565f89]" title={`page ${row.session.pageId}`}>
        {row.session.pageId} · zone {row.session.zoneIndex + 1}
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
        onClick={toggle}
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

/** Where a prepare that is not in its window stands. */
function endedLine(snap: RestartPrepSnapshot): string {
  const rec = snap.record;
  if (!rec) return "No restart prepare is in progress.";
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
  const active = inWindow(rec);
  const canToggle =
    rec?.state === "preparing" || rec?.state === "ready" || rec?.state === "restoring";
  const cancelBusy = busy(snap.cancel);
  const releaseLabel = view?.same_build_hold || view?.release_failed ? "Release hold" : "Cancel";
  const cancel = (): void => {
    if (!active || cancelBusy) return;
    store.cancel().catch(() => {
      // Shown beside the button (`cancel.phase === "failed"`).
    });
  };

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
    state: () => ({
      headline: headline?.text ?? null,
      headlineKind: headline?.kind ?? null,
      state: rec?.state ?? null,
      hold: view?.hold ?? null,
      rows: rec?.cohort.rows.length ?? 0,
    }),
  });

  useUIComponent({
    id: RESTART_PREP_CANCEL_ID,
    name: releaseLabel === "Cancel" ? "Cancel the restart prepare" : "Release the restart hold",
    description:
      "Release the spawn hold and lift the account freeze; sessions that were told to pause are " +
      "told to resume. Notices already delivered stay delivered.",
    actions: [
      {
        id: "cancel",
        label: releaseLabel,
        description: "Cancel the prepare (or release a same-build restart's hold).",
        // `write` — releases this device's spawn hold and unfreezes account moves.
        effect: "write",
        handler: () => cancel(),
      },
    ],
    state: () => ({ phase: snap.cancel.phase, enabled: active && !cancelBusy }),
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
          {headline.kind === "waiting" && (
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

      {snap.prepare.phase === "failed" && (
        <div className="px-2 pb-1 text-[#f7768e]">
          Prepare failed: {snap.prepare.message ?? "unknown error"}
        </div>
      )}

      {rec && view && (
        <div className="px-2 pb-1 text-[#a9b1d6]" data-restart-prep-hold={view.hold}>
          Hold: {view.hold}
          {active && rec.hold_until && !view.hold_expired_at && ` · expires ${rec.hold_until}`}
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
        />
      ))}

      {active && (
        <div className="flex items-center gap-2 px-2 py-1 border-t border-[#2a2d3d]">
          <button
            type="button"
            data-ui-bridge-id={RESTART_PREP_CANCEL_ID}
            data-cancel-phase={snap.cancel.phase}
            disabled={cancelBusy}
            onClick={cancel}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[#f7768e] bg-[#f7768e]/10 hover:bg-[#f7768e]/20 disabled:opacity-60 disabled:cursor-wait"
          >
            {cancelBusy && <Loader2 className="w-2.5 h-2.5 animate-spin" />}
            {snap.cancel.phase === "received"
              ? "Received…"
              : snap.cancel.phase === "in-flight"
                ? "Releasing…"
                : releaseLabel}
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
