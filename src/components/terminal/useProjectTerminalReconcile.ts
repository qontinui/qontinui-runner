/**
 * `useProjectTerminalReconcile` — reconcile LIVE terminals against the active
 * project's root, WITHOUT ever re-`cd`-ing a running shell.
 *
 * Projects-dashboard plan §7.2 step 4, quoted in full because it is the whole
 * design constraint:
 *
 *   > **Never re-`cd` a live shell.** A running session may hold an agent
 *   > mid-edit; re-cwd-ing it is data loss. Instead show a dismissible chip:
 *   > *"3 terminals are in a different folder"* → [Move them] (which opens
 *   > replacements and closes the old ones only on confirm) / [Leave them].
 *
 * So this hook writes NOTHING on its own. It only reports which terminals sit
 * outside the project root and exposes `moveThem()` — the explicit,
 * operator-confirmed action that opens a replacement terminal at the root for
 * each one and closes the original **only after its replacement exists**. If a
 * replacement fails to spawn, the original is left running: losing a shell is
 * strictly worse than showing the chip again.
 */

import { useCallback, useMemo, useState } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { normalizePathForCompare } from "./pathCompare";
import type { TerminalTab } from "./useTerminalManager";

/** The subset of a tab this reconciliation reads. */
export interface ReconcilableTerminal {
  id: string;
  title: string;
  workingDir?: string;
  isAlive: boolean;
  type?: TerminalTab["type"];
  tenantId?: string;
}

/**
 * Pure: the terminals whose cwd is NOT the project root.
 *
 * Deliberately conservative — three classes are never reported:
 *
 *   - **plan tabs** (`type === "plan"`) have no PTY and no cwd at all;
 *   - **exited tabs** (`isAlive === false`) are tombstones; "moving" one would
 *     spawn a shell the operator never asked for;
 *   - **tabs with no `workingDir`** — an UNKNOWN cwd is not a known mismatch.
 *     Claiming otherwise would offer to close a healthy session on a guess.
 *
 * Comparison goes through {@link normalizePathForCompare} so `D:\Proj` (a
 * spawn cwd) and `D:/proj/` (a folder-picker root) are the same directory.
 * Exported so the detection contract is unit-testable without React.
 */
export function findTerminalsOutsideProject<T extends ReconcilableTerminal>(
  tabs: readonly T[],
  projectRoot: string | null | undefined,
): T[] {
  const root = projectRoot?.trim();
  if (!root) return [];
  const normalizedRoot = normalizePathForCompare(root);
  return tabs.filter((t) => {
    if (t.type === "plan") return false;
    if (!t.isAlive) return false;
    const wd = t.workingDir?.trim();
    if (!wd) return false;
    return normalizePathForCompare(wd) !== normalizedRoot;
  });
}

export interface ProjectTerminalReconcile {
  /** The active project root the terminals were compared against. */
  projectRoot: string | null;
  /** Live terminals sitting outside that root. */
  outside: ReconcilableTerminal[];
  /** `outside.length`, the number the chip renders. */
  count: number;
  /** True while `moveThem()` is running. */
  moving: boolean;
  /** True once the operator chose "Leave them" for THIS root. */
  dismissed: boolean;
  /**
   * Whether a chip should render: there is a project root, at least one
   * terminal is outside it, and the operator has not dismissed it.
   */
  shouldPrompt: boolean;
  /** Set when one or more replacements could not be spawned. */
  error: string | null;
  /**
   * "[Move them]" — for each outside terminal, open a replacement at the
   * project root, then close the original. Never closes anything whose
   * replacement failed. Idempotent while in flight.
   */
  moveThem: () => Promise<void>;
  /** "[Leave them]" — dismiss for this root (a new root re-arms the chip). */
  dismiss: () => void;
}

/**
 * @param projectRoot the active project's root; `null`/absent disables the
 *   whole surface (`shouldPrompt` is false and `moveThem` is a no-op).
 */
export function useProjectTerminalReconcile(
  projectRoot: string | null | undefined,
): ProjectTerminalReconcile {
  const { tabs, createTerminal, closeTerminal } = useTerminalSession();
  // Dismissal is keyed by the NORMALIZED root, so switching projects (or
  // correcting a root's spelling) re-arms the chip while re-activating the
  // same project keeps it dismissed.
  const [dismissedRoot, setDismissedRoot] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const root = projectRoot?.trim() || null;
  const normalizedRoot = root ? normalizePathForCompare(root) : null;

  const outside = useMemo(() => findTerminalsOutsideProject(tabs, root), [tabs, root]);

  const dismissed = normalizedRoot !== null && dismissedRoot === normalizedRoot;

  const dismiss = useCallback(() => {
    setDismissedRoot(normalizedRoot);
  }, [normalizedRoot]);

  const moveThem = useCallback(async () => {
    if (!root || moving || outside.length === 0) return;
    setMoving(true);
    setError(null);
    let failed = 0;
    try {
      for (const tab of outside) {
        // Explicit `root` as `workingDir`: the replacement must be pinned to
        // the project root at spawn time (the Rust `terminal_create` derives
        // `intent_repo` from it — see `resolveSpawnWorkingDir`), and the tab's
        // tenant is carried over so the replacement lands in the same tenant.
        const replacementId = await createTerminal(tab.title, root, tab.tenantId);
        if (!replacementId) {
          // The replacement never came up — keep the original alive. Losing a
          // running shell is worse than leaving the chip on screen.
          failed += 1;
          continue;
        }
        closeTerminal(tab.id);
      }
    } finally {
      setMoving(false);
    }
    if (failed > 0) {
      setError(
        `${failed} terminal${failed === 1 ? "" : "s"} could not be reopened at the project folder and ${
          failed === 1 ? "was" : "were"
        } left where ${failed === 1 ? "it is" : "they are"}.`,
      );
    }
  }, [root, moving, outside, createTerminal, closeTerminal]);

  return {
    projectRoot: root,
    outside,
    count: outside.length,
    moving,
    dismissed,
    shouldPrompt: root !== null && outside.length > 0 && !dismissed,
    error,
    moveThem,
    dismiss,
  };
}
