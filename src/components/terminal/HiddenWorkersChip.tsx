/**
 * "N hidden worker(s)" chip — the way back from closing a Conductor worker
 * cell.
 *
 * Closing a worker's cell is a VIEW operation: `useTerminalManager.closeTerminal`
 * drops the tab and records the id in `dismissedWorkerIdsRef` so the live
 * adoption probe does not re-add it on the worker's next `ai-output` line.
 * That dismissal used to be permanent for the page's lifetime, so an operator
 * who closed a cell to declutter could not get it back without restarting the
 * app — while the worker kept running invisibly, which is the opposite of the
 * supervision the cell exists to provide.
 *
 * The alternative fix — expiring the dismissal on the worker's next state
 * transition — was rejected: a busy worker transitions every turn, so "close"
 * would last seconds and be exactly the hide-then-reappear behaviour the
 * dismissal set was added to prevent. An explicit affordance keeps close
 * meaning close AND makes it reversible.
 *
 * Renders NOTHING when nothing is hidden, matching `UnzonedChip`'s self-hide
 * posture (and reusing its pill idiom) so it never adds chrome for nothing.
 */

import { Eye } from "lucide-react";
import type { HiddenWorker } from "./useTerminalManager";

/**
 * Pure label/visibility helper — exported so the unit test can lock the
 * contract without rendering (the runner's vitest config is
 * `environment: "node"`, no jsdom).
 *
 * Returns `null` when nothing is hidden (the chip must not render), otherwise
 * the short label plus a `title` that names the workers — and, honestly, any
 * that a previous "show" could NOT bring back.
 */
export function hiddenWorkersChipLabel(
  hidden: readonly HiddenWorker[],
): { text: string; title: string } | null {
  if (hidden.length === 0) return null;
  const names = hidden.map((w) => w.title).join(", ");
  const missed = hidden.filter((w) => w.restoreMissedAtMs !== undefined);
  const missedNote =
    missed.length > 0
      ? ` ${missed.length} of them could not be re-opened — no longer listed as an open session${
          missed.length === 1 ? "" : "s"
        }; it may have finished.`
      : "";
  return {
    text: `${hidden.length} hidden worker${hidden.length === 1 ? "" : "s"}`,
    title: `${hidden.length} worker cell${
      hidden.length === 1 ? " was" : "s were"
    } closed on this page and ${
      hidden.length === 1 ? "is" : "are"
    } still running: ${names}. Click to show ${hidden.length === 1 ? "it" : "them"} again.${missedNote}`,
  };
}

interface HiddenWorkersChipProps {
  hidden: readonly HiddenWorker[];
  /** Restore every hidden worker view (`useTerminalManager.restoreHiddenWorkers`). */
  onRestoreAll: () => void;
}

export function HiddenWorkersChip({ hidden, onRestoreAll }: HiddenWorkersChipProps) {
  const label = hiddenWorkersChipLabel(hidden);
  if (!label) return null;

  return (
    <div
      data-page-element="hidden-workers-chip"
      className="flex items-center px-2 py-1 bg-[#13141f]/40 backdrop-blur-sm border-b border-[#2a2d3d]/20 shrink-0"
    >
      <button
        type="button"
        // Author-controlled, text-independent id: the auto-derived one would
        // be minted from the first-seen text ("N hidden workers") and freeze
        // there while the count moves. `data-ui-bridge-id` wins in the SDK.
        data-ui-bridge-id="terminal.hidden-workers-restore"
        onClick={onRestoreAll}
        title={label.title}
        className="inline-flex items-center gap-1 rounded border border-[#7aa2f7]/30 bg-[#7aa2f7]/10 px-1.5 py-px text-[10px] text-[#7aa2f7] hover:bg-[#7aa2f7]/20"
      >
        <Eye className="h-3 w-3" />
        {label.text}
      </button>
    </div>
  );
}

export default HiddenWorkersChip;
