/**
 * "Restore all on this page (n)" — the per-page bulk restore of a prepared
 * restart's cohort (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7).
 *
 * The runner's top-level page tab is what the operator calls "a tab", so each
 * page restores its own placeholders in one click. `n` counts the rows the
 * click would actually restore — unheld and unfinished; held ones stay
 * placeholders and the click says how many it skipped. Renders nothing on a
 * page with no placeholders.
 */

import { RotateCcw } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { bulkEligible, type RestorePlaceholder } from "./restorePlaceholders";
import { useBulkRestore, useRestoreCohort } from "./useRestoreQueue";
import { loadKnownPageIds } from "./useTerminalPages";

/** PURE: this page's placeholders and how many a bulk click restores. */
export function pageRestoreModel(
  rows: readonly RestorePlaceholder[],
  pageId: string,
): { rows: RestorePlaceholder[]; eligible: number } | null {
  const mine = rows.filter((r) => r.pageId === pageId);
  if (mine.length === 0) return null;
  return { rows: mine, eligible: mine.filter(bulkEligible).length };
}

export function restorePageId(pageId: string): string {
  return `terminal.restore-page-${pageId}`;
}

export function RestorePageBar({
  pageId,
  notify,
}: {
  pageId: string;
  notify: (message: string) => void;
}) {
  const cohort = useRestoreCohort();
  const bulk = useBulkRestore(notify);
  const model = pageRestoreModel(cohort.rows, pageId);
  const restoreAll = () => {
    if (model) bulk(model.rows, loadKnownPageIds());
  };

  useUIComponent({
    id: `terminal.restore-page-bar-${pageId}`,
    name: "Restore all on this page",
    description:
      "Restore this page's restart-cohort placeholders one at a time, skipping held and finished ones.",
    actions: [
      {
        id: "restore-all",
        label: "Restore all on this page",
        description:
          "Queue every unheld, unfinished placeholder on this page for a verified backend resume.",
        // `destructive` — each restore starts a `claude --resume` process under
        // the session's own account, as `since-restart.resume-selected` does.
        effect: "destructive",
        handler: () => {
          restoreAll();
        },
      },
    ],
  });

  if (!model) return null;
  return (
    <div className="flex items-center gap-2 px-3 py-1 border-b border-[#2a2d3d] bg-[#1a1b26] text-[11px] text-[#a9b1d6]">
      <span className="flex-1 min-w-0 truncate">
        {model.rows.length} session{model.rows.length === 1 ? "" : "s"} from the restart wait here
      </span>
      <button
        type="button"
        data-ui-bridge-id={restorePageId(pageId)}
        disabled={model.eligible === 0}
        onClick={restoreAll}
        className="flex items-center gap-1 px-2 py-0.5 rounded bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25 disabled:opacity-50"
      >
        <RotateCcw className="w-3 h-3" />
        Restore all on this page ({model.eligible})
      </button>
    </div>
  );
}
