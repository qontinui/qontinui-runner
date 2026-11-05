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
 * operator-confirmed action. "Move them" REPLACES a pane only when two
 * independent readings agree it is a shell sitting at its prompt:
 *
 *   1. the PROCESS TABLE (`terminal_probe_claude`): the pane's root process is
 *      a shell and nothing at all runs under it. Necessary, not sufficient —
 *      PowerShell runs cmdlets and scripts in-process (`Get-Content -Wait`, a
 *      `.ps1` loop, `Enter-PSSession`), so a busy pwsh can have no descendant;
 *   2. the SHELL'S OWN PROMPT STATE (OSC 633): the last mark this window
 *      parsed from the pane — from its live stream, or from the ring replay a
 *      remount re-derives it from — is a prompt (633;A), and that prompt
 *      carried `633;P;CommandMarks=1`: its shell marks every typed command line
 *      with a command start (633;C). bash < 4.4, PowerShell without PSReadLine
 *      and a PowerShell nested prompt draw 633;A without that promise, so their
 *      prompt mark says nothing. A pane with no such reading is kept: one whose
 *      shell emits no marks (`bash ./job.sh`, `cmd`), and one whose output this
 *      window knows it did not parse since its last prompt (dropped while
 *      hidden, overrun out of the ring, spliced out of a resync, painted over
 *      by a reconnect). The shell side announces the capability only where
 *      its C path is proven present at that prompt; anywhere it cannot prove
 *      it, it stays silent and the pane is kept.
 *
 * What neither witness sees: work a shell runs in-process and in the
 * background without a command line — a PowerShell `Start-ThreadJob` (or
 * `Register-ObjectEvent` action) keeps running at an idle prompt, with no
 * descendant and no 633;C. Replacing such a pane ends that work.
 *
 * Both are read before the replacement is opened and again just before the
 * original is closed, and the original closes **only after its replacement
 * exists** and the second pair of readings still agrees. Every other pane —
 * `claude`, an npm-installed claude (`node … cli.js`, which the claude image
 * test cannot see), any other process, a command running in the shell, a
 * remote pane, one whose table or prompt state cannot be read — is kept where
 * it is and named in the chip with the reason. If a replacement fails to
 * spawn, the original is left running: losing a shell is strictly worse than
 * showing the chip again.
 */

import { useCallback, useMemo, useState } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { normalizePathForCompare } from "./pathCompare";
import { probeClaudeInPane, type PaneClaudeProbe } from "./resumeVerification";
import type { ShellPromptMark } from "./shellIntegrationEvents";
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

/**
 * Whether a pane may be replaced, and if not, why. `busy` is true only when a
 * reading POSITIVELY saw something running (claude, another program, a
 * descendant, a command the shell started) — never for a pane that simply
 * could not be read.
 */
export type BareShellReading = { bare: true } | { bare: false; reason: string; busy: boolean };

/** What a pane's shell last reported, as `useShellIntegration` parsed it. */
export interface ShellPromptReading {
  /** Its last prompt-state mark; `undefined` when none is current. */
  mark: ShellPromptMark | undefined;
  /** It announced that it marks every command start. */
  announcesCommandStarts: boolean;
}

/**
 * Is this pane a bare idle shell? Both readings must say so (see the module
 * docs): the process table — its root a shell (never a `claude` of any install)
 * with no descendant at all — AND the shell's own prompt state: a parsed prompt
 * mark from a shell that marks every command start. Anything short of that
 * keeps the pane. PURE.
 *
 * `state: "absent"` alone is not the process half: it only says no process
 * NAMED `claude` is in the subtree, and an npm-installed Claude Code runs as
 * `node … cli.js`. The root's descendant count is what sees it.
 */
export function readBareIdleShell(
  tab: Pick<ReconcilableTerminal, "remote">,
  probe: PaneClaudeProbe,
  shell: ShellPromptReading,
): BareShellReading {
  if (tab.remote || probe.state === "remote") {
    return {
      bare: false,
      reason: "a remote pane — its processes cannot be read here",
      busy: false,
    };
  }
  if (probe.state === "unknown" || !probe.root) {
    return { bare: false, reason: "its processes could not be read", busy: false };
  }
  if (probe.state === "live") return { bare: false, reason: "claude is running in it", busy: true };
  const { image, isShell, descendants } = probe.root;
  if (image === null) {
    return { bare: false, reason: "its process could not be found", busy: false };
  }
  if (!isShell) return { bare: false, reason: `${image} is running in it`, busy: true };
  if (descendants > 0) {
    return {
      bare: false,
      reason: `${descendants} process${descendants === 1 ? " is" : "es are"} running in it`,
      busy: true,
    };
  }
  // The prompt half is a POSITIVE check: only a parsed prompt mark from a shell
  // that marks command starts reads idle; anything else keeps the pane.
  if (shell.mark === undefined) {
    // Only a shell the runner integrates can be helped by a fresh prompt; for
    // any other, pressing Enter changes nothing, so say what it is instead.
    const stem = shellStem(image);
    return {
      bare: false,
      reason: INTEGRATED_SHELLS.has(stem)
        ? "no prompt seen since this pane was shown — press Enter in it, then Check again"
        : `its shell (${stem}) does not report prompt state`,
      busy: false,
    };
  }
  if (!shell.announcesCommandStarts) {
    return { bare: false, reason: "its shell does not report command starts", busy: false };
  }
  if (shell.mark !== "prompt_start") {
    return { bare: false, reason: "its shell last reported a command start", busy: true };
  }
  return { bare: true };
}

/**
 * The shells the runner's shell integration covers (bash, zsh, PowerShell):
 * the only ones that ever report a prompt mark. Other shells the runner counts
 * as shells (`cmd`, `fish`, `sh`, `dash`, `ksh`, `nu`) never do.
 */
const INTEGRATED_SHELLS: ReadonlySet<string> = new Set(["bash", "zsh", "pwsh", "powershell"]);

/** An image name's lowercase stem: `C:\\…\\pwsh.exe` → `pwsh`. PURE. */
function shellStem(image: string): string {
  const base = image.split(/[\\/]/).pop() ?? image;
  return base.replace(/\.exe$/i, "").toLowerCase();
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
  /** The pane's last OSC 633 prompt-state mark (`useShellIntegration().lastPromptMark`). */
  shellMark: (tabId: string) => ShellPromptMark | undefined;
  /** Its shell announced command-start marks (`useShellIntegration().announcesCommandStarts`). */
  announcesCommandStarts: (tabId: string) => boolean;
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
 * "Move them": for each outside pane, in order — read it (process table AND
 * prompt state); keep it unless both say bare idle shell; open its replacement
 * at `root`; read both AGAIN and close the original only if they still agree
 * (otherwise the original is kept and the just-opened replacement is closed
 * instead). Serial, so a resource-guard refusal mid-move names the move and the
 * terminals still ahead.
 */
function shellReading(deps: ProjectMoveDeps, tabId: string): ShellPromptReading {
  return {
    mark: deps.shellMark(tabId),
    announcesCommandStarts: deps.announcesCommandStarts(tabId),
  };
}

export async function moveTerminalsToProject(
  outside: readonly ReconcilableTerminal[],
  root: string,
  deps: ProjectMoveDeps,
): Promise<ProjectMoveOutcome> {
  const outcome: ProjectMoveOutcome = { replaced: [], kept: [], failed: [] };
  for (const [index, tab] of outside.entries()) {
    const before = readBareIdleShell(tab, await deps.probe(tab.id), shellReading(deps, tab.id));
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
    const after = readBareIdleShell(tab, await deps.probe(tab.id), shellReading(deps, tab.id));
    if (!after.bare) {
      deps.closeTerminal(replacementId);
      outcome.kept.push({
        id: tab.id,
        title: tab.title,
        // Only a POSITIVE second sighting started "during the move"; a second
        // read that simply failed says nothing about when.
        reason: after.busy ? `${after.reason} (it started during the move)` : after.reason,
      });
      continue;
    }
    deps.closeTerminal(tab.id);
    outcome.replaced.push(tab.id);
  }
  return outcome;
}

/** The last "Move them" outcome, keyed by the NORMALIZED root it ran for. */
export interface LastProjectMove {
  root: string;
  kept: KeptTerminal[];
  failed: KeptTerminal[];
}

/** What the chip shows about the last move, for the root it is showing NOW. */
export interface ProjectMoveSummary {
  /** Every pane the last move left where it is, still outside: kept, then failed. */
  kept: KeptTerminal[];
  /** Panes whose replacement could not be opened, as a sentence; `null` when none. */
  error: string | null;
  /** Every outside pane was KEPT (not failed) — another press can only re-check. */
  allKept: boolean;
}

/**
 * Summarise `lastMove` for the chip showing `normalizedRoot`. PURE.
 *
 * A move made for another root says nothing about this one (a project switch
 * shows no stale reasons, error or "Check again"); only panes still outside
 * the root are named; and a pane whose replacement failed to open is not
 * "kept" — another press may move it — so it never makes `allKept` true.
 */
export function summarizeLastMove(
  lastMove: LastProjectMove | null,
  normalizedRoot: string | null,
  outside: readonly Pick<ReconcilableTerminal, "id">[],
): ProjectMoveSummary {
  const mine = lastMove !== null && lastMove.root === normalizedRoot ? lastMove : null;
  const stillOutside = (k: KeptTerminal) => outside.some((t) => t.id === k.id);
  const keptOnly = (mine?.kept ?? []).filter(stillOutside);
  const failed = (mine?.failed ?? []).filter(stillOutside);
  const n = failed.length;
  return {
    kept: [...keptOnly, ...failed],
    error:
      n > 0
        ? `${n} terminal${n === 1 ? "" : "s"} could not be reopened at the project folder and ${
            n === 1 ? "was" : "were"
          } left where ${n === 1 ? "it is" : "they are"}.`
        : null,
    allKept: outside.length > 0 && keptOnly.length === outside.length,
  };
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
   * The panes the last `moveThem()` for THIS root left where they are, each
   * with the reason — not bare idle shells, or a replacement that could not be
   * opened. Empty before any move, and after a switch to another root.
   */
  kept: KeptTerminal[];
  /**
   * True when the last `moveThem()` FOR THIS ROOT kept every pane still outside
   * it because it is not a bare idle shell (a pane whose replacement failed to
   * open does not count), so pressing the button again can only re-check them
   * — the chip says "Check again" rather than promising a move.
   */
  allKept: boolean;
  /**
   * "[Move them]" — replace each outside terminal that both readings show is a
   * bare idle shell with one opened at the project root; keep every other one.
   * Never closes anything whose replacement failed. Idempotent while in flight.
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
  const { tabs, createTerminal, closeTerminal, shellIntegration } = useTerminalSession();
  const { lastPromptMark, announcesCommandStarts } = shellIntegration;
  // Dismissal is keyed by the NORMALIZED root, so switching projects (or
  // correcting a root's spelling) re-arms the chip while re-activating the
  // same project keeps it dismissed.
  const [dismissedRoot, setDismissedRoot] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  // The last move's outcome, keyed by the NORMALIZED root it ran for (as the
  // dismissal is): a project switch shows no reasons, error or "Check again"
  // from a root the operator never moved.
  const [lastMove, setLastMove] = useState<LastProjectMove | null>(null);

  const root = projectRoot?.trim() || null;
  const normalizedRoot = root ? normalizePathForCompare(root) : null;

  const outside = useMemo(() => findTerminalsOutsideProject(tabs, root), [tabs, root]);
  const summary = useMemo(
    () => summarizeLastMove(lastMove, normalizedRoot, outside),
    [lastMove, normalizedRoot, outside],
  );

  const dismissed = normalizedRoot !== null && dismissedRoot === normalizedRoot;

  const dismiss = useCallback(() => {
    setDismissedRoot(normalizedRoot);
  }, [normalizedRoot]);

  const moveThem = useCallback(async () => {
    if (!root || !normalizedRoot || moving || outside.length === 0) return;
    setMoving(true);
    let outcome: ProjectMoveOutcome;
    try {
      outcome = await moveTerminalsToProject(outside, root, {
        probe: (tabId) => probeClaudeInPane(tabId),
        shellMark: lastPromptMark,
        announcesCommandStarts,
        createTerminal,
        closeTerminal,
      });
    } finally {
      setMoving(false);
    }
    setLastMove({ root: normalizedRoot, kept: outcome.kept, failed: outcome.failed });
  }, [
    root,
    normalizedRoot,
    moving,
    outside,
    createTerminal,
    closeTerminal,
    lastPromptMark,
    announcesCommandStarts,
  ]);

  return {
    projectRoot: root,
    outside,
    count: outside.length,
    moving,
    dismissed,
    shouldPrompt: root !== null && outside.length > 0 && !dismissed,
    error: summary.error,
    kept: summary.kept,
    allKept: summary.allKept,
    moveThem,
    dismiss,
  };
}
