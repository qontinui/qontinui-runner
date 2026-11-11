/**
 * The global restart-cohort strip (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7):
 * "Restart cohort from <time>: N waiting · M held · Restore all · Dismiss".
 * A sibling of `RunnerDrainBanner`, so it shows on every page.
 *
 * - **Restore all** feeds every page's unheld, unfinished placeholders to the
 *   one restore queue, page by page in strip order, and says how many it
 *   skipped.
 * - **Dismiss** ends the restore without restoring the rest: the record moves
 *   to `done` (irreversibly), every waiting session stays in Past Sessions
 *   (resumable by hand only), the restore-pending markers are cleared and the
 *   placeholders go — the same door, and the same end, as the panel's Dismiss.
 *
 * Renders nothing while no cohort is restoring; a restoring cohort with no
 * row left to show keeps a strip, so Dismiss stays reachable. It also owns the cohort's
 * periodic re-read (`useRestoreCohortRefresh`), since it is mounted once.
 */

import { useState } from "react";
import { RotateCcw, X } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { bulkEligible, cohortStripText } from "./restorePlaceholders";
import { useRestoreCohortRefresh } from "./useRestorePlaceholderBinding";
import {
  bulkNoteText,
  dismissRestoreCohort,
  previewBulk,
  useBulkRestore,
  useRestoreCohort,
} from "./useRestoreQueue";
import { loadKnownPageIds } from "./useTerminalPages";
import { drainBannerModel } from "./RunnerDrainBanner";
import { useCoordDrainState } from "@/hooks/useCoordDrainState";

export function RestoreCohortStrip({ notify }: { notify: (message: string) => void }) {
  const cohort = useRestoreCohort();
  // Mounted once, app-wide: keeps the cohort current while it restores.
  useRestoreCohortRefresh();
  const bulk = useBulkRestore(notify);
  const eligible = cohort.rows.filter(bulkEligible).length;
  const [dismissing, setDismissing] = useState(false);
  // The drain banner sits in the same top-centre slot: stack below it.
  const drainShown = drainBannerModel(useCoordDrainState()) !== null;
  const text = cohortStripText(cohort.rows, cohort.startedAt, cohort.prepareId);
  const restoreAll = () => {
    bulk(cohort.rows, loadKnownPageIds());
  };
  const dismiss = async () => {
    if (cohort.prepareId === null) return;
    setDismissing(true);
    const err = await dismissRestoreCohort(cohort.prepareId);
    setDismissing(false);
    if (err) notify(`The restart cohort was not dismissed: ${err}`);
  };

  useUIComponent({
    id: "terminal.restore-cohort-strip-actions",
    name: "Restart cohort",
    description: "The sessions a prepared restart left waiting for a hand restore.",
    actions: [
      {
        id: "restore-all",
        label: "Restore all",
        description:
          "Queue every unheld, unfinished placeholder on every page for a verified resume.",
        // `destructive` — each restore starts a `claude --resume` process.
        effect: "destructive",
        handler: () => {
          restoreAll();
        },
      },
      {
        id: "dismiss",
        label: "Dismiss",
        description:
          "End the restore: the waiting sessions stop being placeholders and lose the shield " +
          "that keeps them from the orphan close. It cannot be undone — they come back only " +
          "by hand from Past Sessions.",
        // `destructive` (CONTRACT.md Dim 1), as the panel's Dismiss of the same
        // door: `restart_prep_dismiss` moves the record `restoring` → `done`,
        // which has no reverse, and the waiting rows lose their shield.
        effect: "destructive",
        handler: () => {
          void dismiss();
        },
      },
    ],
  });

  if (text === null) return null;
  const note = bulkNoteText(previewBulk(cohort.rows));
  return (
    <div
      data-ui-bridge-id="terminal.restore-cohort-strip"
      className={`absolute ${drainShown ? "top-24" : "top-2"} left-1/2 -translate-x-1/2 z-30 w-[520px] max-w-[92%] shadow-lg rounded border border-[#7aa2f7]/40 bg-[#1a1b26] px-3 py-1.5 flex items-center gap-2 text-[12px] text-[#c0caf5]`}
    >
      <span className="flex-1 min-w-0 truncate" title={text}>
        {text}
      </span>
      <button
        type="button"
        data-ui-bridge-id="terminal.restore-cohort-all"
        disabled={eligible === 0}
        onClick={restoreAll}
        className="flex items-center gap-1 px-2 py-0.5 rounded bg-[#7aa2f7]/20 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-50"
      >
        <RotateCcw className="w-3 h-3" />
        Restore all ({eligible})
      </button>
      {note && (
        <span
          data-ui-bridge-id="terminal.restore-cohort-note"
          role="status"
          className="text-[10px] text-[#e0af68] truncate max-w-[14rem]"
          title={note}
        >
          {note}
        </span>
      )}
      <button
        type="button"
        data-ui-bridge-id="terminal.restore-cohort-dismiss"
        disabled={dismissing}
        onClick={() => void dismiss()}
        className="flex items-center gap-1 px-2 py-0.5 rounded bg-[#565f89]/20 hover:bg-[#565f89]/30 disabled:opacity-50"
      >
        <X className="w-3 h-3" />
        Dismiss
      </button>
    </div>
  );
}
