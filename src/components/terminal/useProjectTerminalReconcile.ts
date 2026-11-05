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
 * operator-confirmed action. "Move them" REPLACES only a pane the process
 * table positively reads as a bare idle shell (its root process is a shell
 * with nothing at all running under it): it opens a replacement at the root
 * and closes the original **only after its replacement exists** and a second
 * read still finds the original bare. Every other pane — one running `claude`,
 * an npm-installed claude (`node … cli.js`, which the claude image test cannot
 * see), any other process, a remote pane, one whose table cannot be read — is
 * kept where it is and named in the chip with the reason. If a replacement
 * fails to spawn, the original is left running: losing a shell is strictly
 * worse than showing the chip again.
 */

import { useCallback, useMemo, useState } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { normalizePathForCompare } from "./pathCompare";
import { probeClaudeInPane, type PaneClaudeProbe } from "./resumeVerification";
import type { TerminalTab } from "./useTerminalManager";

/** The subset of a tab this reconciliation reads. */
export interface ReconcilableTerminal {
  id: string;
  title: string;
  workingDir?: string;
  isAlive: boolean;
  type?: TerminalTab["type"];
  tenantId?: string;
  /**
   * Remote tab identity (Phase 4). A remote tab's `workingDir` is the TARGET
   * machine's cwd, so it is never "outside" this machine's project root —
   * excluded from the reconcile rather than replaced by a local shell.
   */
  remote?: unknown;
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
    if (t.remote) return false;
    if (!t.isAlive) return false;
    const wd = t.workingDir?.trim();
    if (!wd) return false;
    return normalizePathForCompare(wd) !== normalizedRoot;
  });
}

/** Whether a pane may be replaced, and if not, why. */
export type BareShellReading = { bare: true } | { bare: false; reason: string };

/**
 * Does one process-table read show this pane is a BARE IDLE SHELL — its root
 * process a shell (never a `claude` of any install) with no descendant at all?
 * Anything short of that positive reading keeps the pane. PURE.
 *
 * `state: "absent"` alone is not that reading: it only says no process NAMED
 * `claude` is in the subtree, and an npm-installed Claude Code runs as
 * `node … cli.js`. The root's descendant count is what sees it.
 */
export function readBareIdleShell(
  tab: Pick<ReconcilableTerminal, "remote">,
  probe: PaneClaudeProbe,
): BareShellReading {
  if (tab.remote || probe.state === "remote") {
    return { bare: false, reason: "a remote pane — its processes cannot be read here" };
  }
  if (probe.state === "unknown" || !probe.root) {
    return { bare: false, reason: "its processes could not be read" };
  }
  if (probe.state === "live") return { bare: false, reason: "claude is running in it" };
  const { image, isShell, descendants } = probe.root;
  if (image === null) return { bare: false, reason: "its process could not be found" };
  if (!isShell) return { bare: false, reason: `${image} is running in it` };
  if (descendants > 0) {
    return {
      bare: false,
      reason: `${descendants} process${descendants === 1 ? " is" : "es are"} running in it`,
    };
  }
  return { bare: true };
}

/** A pane "Move them" left where it is. */
export interface KeptTerminal {
  id: string;
  title: string;
  reason: string;
}

/** The effects "Move them" needs, injected so the whole move is testable. */
export interface ProjectMoveDeps {
  /** `terminal_probe_claude`, read through {@link probeClaudeInPane}. */
  probe: (tabId: string) => Promise<PaneClaudeProbe>;
  createTerminal: (
    title: string,
    workingDir: string,
    tenantId: string | undefined,
    spawnSource: { label: string; queued: number },
  ) => Promise<string | null>;
  closeTerminal: (id: string) => void;
}

export interface ProjectMoveOutcome {
  /** Originals closed after their replacement came up at the root. */
  replaced: string[];
  /** Panes kept because they are not bare idle shells. */
  kept: KeptTerminal[];
  /** Bare shells whose replacement could not be spawned — left running. */
  failed: KeptTerminal[];
}

/**
 * "Move them": for each outside pane, in order — read it; keep it unless it is
 * a bare idle shell; open its replacement at `root`; read it AGAIN and close the
 * original only if it is still bare (a command the operator started meanwhile
 * keeps it, and the just-opened replacement is closed instead). Serial, so a
 * resource-guard refusal mid-move names the move and the terminals still ahead.
 */
export async function moveTerminalsToProject(
  outside: readonly ReconcilableTerminal[],
  root: string,
  deps: ProjectMoveDeps,
): Promise<ProjectMoveOutcome> {
  const outcome: ProjectMoveOutcome = { replaced: [], kept: [], failed: [] };
  for (const [index, tab] of outside.entries()) {
    const before = readBareIdleShell(tab, await deps.probe(tab.id));
    if (!before.bare) {
      outcome.kept.push({ id: tab.id, title: tab.title, reason: before.reason });
      continue;
    }
    // Explicit `root` as `workingDir`: the replacement must be pinned to the
    // project root at spawn time (the Rust `terminal_create` derives
    // `intent_repo` from it — see `resolveSpawnWorkingDir`), and the tab's
    // tenant is carried over so the replacement lands in the same tenant.
    const replacementId = await deps.createTerminal(tab.title, root, tab.tenantId, {
      label: "move terminals to project",
      queued: outside.length - index,
    });
    if (!replacementId) {
      // The replacement never came up — keep the original alive.
      outcome.failed.push({
        id: tab.id,
        title: tab.title,
        reason: "its replacement could not be opened",
      });
      continue;
    }
    const after = readBareIdleShell(tab, await deps.probe(tab.id));
    if (!after.bare) {
      deps.closeTerminal(replacementId);
      outcome.kept.push({
        id: tab.id,
        title: tab.title,
        reason: `${after.reason} (it started during the move)`,
      });
      continue;
    }
    deps.closeTerminal(tab.id);
    outcome.replaced.push(tab.id);
  }
  return outcome;
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
   * The panes the last `moveThem()` left where they are because they are not
   * bare idle shells, each with the reason. Empty before any move.
   */
  kept: KeptTerminal[];
  /**
   * "[Move them]" — replace each outside terminal that is a bare idle shell
   * with one opened at the project root; keep every other one. Never closes
   * anything whose replacement failed. Idempotent while in flight.
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
  const [keptAll, setKeptAll] = useState<KeptTerminal[]>([]);

  const root = projectRoot?.trim() || null;
  const normalizedRoot = root ? normalizePathForCompare(root) : null;

  const outside = useMemo(() => findTerminalsOutsideProject(tabs, root), [tabs, root]);
  // Only name a kept pane while it is still outside the root.
  const kept = useMemo(
    () => keptAll.filter((k) => outside.some((t) => t.id === k.id)),
    [keptAll, outside],
  );

  const dismissed = normalizedRoot !== null && dismissedRoot === normalizedRoot;

  const dismiss = useCallback(() => {
    setDismissedRoot(normalizedRoot);
  }, [normalizedRoot]);

  const moveThem = useCallback(async () => {
    if (!root || moving || outside.length === 0) return;
    setMoving(true);
    setError(null);
    let outcome: ProjectMoveOutcome;
    try {
      outcome = await moveTerminalsToProject(outside, root, {
        probe: (tabId) => probeClaudeInPane(tabId),
        createTerminal,
        closeTerminal,
      });
    } finally {
      setMoving(false);
    }
    setKeptAll([...outcome.kept, ...outcome.failed]);
    const failed = outcome.failed.length;
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
    kept,
    moveThem,
    dismiss,
  };
}
