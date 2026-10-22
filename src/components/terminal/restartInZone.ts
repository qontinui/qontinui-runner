/**
 * Restart-in-zone: replace a finished or errored pane with a fresh terminal in
 * the same place. `TransitionEffectsProvider.handleRestartInZone` is this,
 * bound to the page's contexts; it lives here so the whole sequence — spawn,
 * take the old pane's place, keep its label, retire it — runs against a real
 * `useZoneLayout` in a test, in both work modes.
 */

import type { ResourceGuardSource } from "@/lib/resourceGuard";
import type { CloseTerminalOptions } from "./useTerminalManager";

/**
 * What a restart attempt actually did.
 *
 * A restart has three exits and a void signature made all three look alike to
 * a caller: the zone was not in a restartable state, the replacement terminal
 * failed to spawn (the resource gate refuses below the free-commit floor, and
 * the operator can decline the override), or the swap completed. The middle
 * one leaves the ORIGINAL errored pane in place, and `/restart` rendered
 * success for it.
 */
export type RestartOutcome =
  | { restarted: true; tabId: string; retiredTabId: string | null }
  | { restarted: false; reason: "not-restartable" | "spawn-failed"; state?: string };

/** The page state and actions a restart reads and drives. */
export interface RestartInZoneDeps {
  zoneLayout: {
    assignments: Record<number, string>;
    /** `useZoneLayout().replaceTab`: the new tab takes the old one's place. */
    replaceTab: (oldId: string, newId: string) => void;
    setFocusedZone: (zone: number) => void;
    /** `useZoneLayout().cosmeticZoneFor`: whose label a zone shows. */
    cosmeticZoneFor: (zoneIdx: number) => number | null;
  };
  tabs: readonly { id: string; title?: string; workingDir?: string | null }[];
  sessionStates: Readonly<Record<string, string>>;
  zoneLabels: Readonly<Record<number, string>>;
  setZoneLabel: (zone: number, label: string) => void;
  createTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
    spawnSource?: ResourceGuardSource,
  ) => Promise<string | null>;
  closeTerminal: (tabId: string, options?: CloseTerminalOptions) => void;
}

/**
 * Replace the finished/errored pane in `zoneIdx` with a fresh terminal.
 *
 * The new tab takes the old one's place through `replaceTab`: its zone in
 * the grid, and in tmux mode the one viewport AND the grid zone the session
 * returns to (whose label, notes and pin are the pane's), plus its MRU
 * recency. A plain zone assignment would swap only the viewport in tmux and
 * leave the grid snapshot pointing at the tab about to be closed — the
 * replacement would come back from tmux in no zone at all.
 *
 * The label is read and re-written through `cosmeticZoneFor`: the zone itself
 * in grid mode, the shown session's grid zone in tmux (the viewport's index,
 * 0, names no label of its own there). A session with no grid zone has no
 * label to carry.
 *
 * RETIRES THE OLD PANE through `closeTerminal`, the one path that drops the
 * tab from the roster (clearing its error/completed state from the status
 * strip), closes its durable lifecycle record with reason `explicit`, kills
 * the PTY and re-syncs the tab list. Leaving it orphaned kept a "closed
 * record, live PTY" pair and a status strip stuck at "1 error".
 *
 * Ordering: the replacement is created FIRST and the old pane retired only
 * once it exists and holds the place. The spawn can legitimately fail, and
 * destroying the operator's scrollback for a replacement that never arrived
 * would lose the only evidence of what went wrong.
 */
export async function restartInZone(
  deps: RestartInZoneDeps,
  zoneIdx: number,
): Promise<RestartOutcome> {
  const { zoneLayout } = deps;
  const oldTabId = zoneLayout.assignments[zoneIdx];
  const oldTab = deps.tabs.find((t) => t.id === oldTabId);
  const state = oldTabId ? (deps.sessionStates[oldTabId] ?? "idle") : "idle";
  if (!oldTabId || (state !== "completed" && state !== "error")) {
    return { restarted: false, reason: "not-restartable", state };
  }
  const labelZone = zoneLayout.cosmeticZoneFor(zoneIdx);
  const label = labelZone === null ? undefined : deps.zoneLabels[labelZone];
  const tabId = await deps.createTerminal(
    oldTab?.title ? `${oldTab.title} (2)` : undefined,
    oldTab?.workingDir ?? undefined,
    undefined,
    { label: "restart in zone" },
  );
  if (!tabId) {
    // The replacement never existed, so the old pane was deliberately NOT
    // retired. Saying so is the whole point of the return type.
    return { restarted: false, reason: "spawn-failed" };
  }
  zoneLayout.replaceTab(oldTabId, tabId);
  zoneLayout.setFocusedZone(zoneIdx);
  if (label && labelZone !== null) deps.setZoneLabel(labelZone, label);
  // Retire the pane just replaced — after its place points at the
  // replacement, so the zone is never momentarily empty.
  // `immediate`: a restart REPLACES the pane, so the retirement is not an
  // operator close and must not hold the old PTY open for the Undo window.
  deps.closeTerminal(oldTabId, { immediate: true });
  return { restarted: true, tabId, retiredTabId: oldTabId };
}
