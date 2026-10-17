import { useCallback, useState } from "react";

import { ConfirmDialog } from "../ui/ConfirmDialog";
import { remoteSessionEnd } from "./remoteSessionEnd";
import {
  CLOSE_ALL_FINISHED_CONCURRENCY,
  describeEndResult,
  endInvokeFailure,
  fleetCloseAllFinishedResultId,
  FLEET_CLOSE_ALL_FINISHED_CANCEL_ID,
  FLEET_CLOSE_ALL_FINISHED_CONFIRM_ID,
  FLEET_CLOSE_ALL_FINISHED_DIALOG_ID,
  FLEET_CLOSE_ALL_FINISHED_RESULTS_ID,
  runBounded,
  type EndResultView,
} from "./remoteSessionEndView";
import type { RemoteSessionEndResult } from "./remoteTabs";

/**
 * "Close all finished" (plan
 * `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 5c): confirm
 * a listed set, end each GRACEFULLY with bounded concurrency, and show one
 * result per session. No force in bulk — a refused or wedged session is left
 * running and said so.
 */

export interface BulkEndItem {
  sessionId: string;
  deviceId: string;
  deviceLabel: string;
  sessionLabel: string;
}

/** A settled row, or null while it is still in flight / not started. */
export type BulkEndResults = Record<string, EndResultView | undefined>;

/**
 * The per-session result list. Pure markup over `results`, exported so the
 * rendering rules are testable with `renderToStaticMarkup`.
 */
export function BulkEndResultList({
  items,
  results,
  showStatus = true,
}: {
  items: readonly BulkEndItem[];
  results: BulkEndResults;
  /** False before the run: the list is what WILL be ended, with no status yet. */
  showStatus?: boolean;
}) {
  return (
    <ul
      data-ui-bridge-id={FLEET_CLOSE_ALL_FINISHED_RESULTS_ID}
      className="max-h-64 overflow-y-auto text-xs space-y-1"
    >
      {items.map((it) => {
        const v = results[it.sessionId];
        return (
          <li
            key={it.sessionId}
            data-ui-bridge-id={fleetCloseAllFinishedResultId(it.sessionId)}
            data-end-outcome={v?.outcome ?? "pending"}
            className="break-words"
          >
            <span className="text-zinc-300">{it.deviceLabel}</span>
            <span className="text-zinc-500"> · {it.sessionLabel}</span>
            {showStatus && " — "}
            {!showStatus ? null : v ? (
              <>
                <span className={v.toneClass}>{v.label}</span>
                {v.detail && <span className="text-zinc-500"> ({v.detail})</span>}
              </>
            ) : (
              <span className="text-zinc-500">…</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** One-line tally of settled results, by rendered outcome. */
export function bulkEndSummary(items: readonly BulkEndItem[], results: BulkEndResults): string {
  const counts: Record<string, number> = {};
  let settled = 0;
  for (const it of items) {
    const v = results[it.sessionId];
    if (!v) continue;
    settled += 1;
    counts[v.label] = (counts[v.label] ?? 0) + 1;
  }
  const parts = Object.entries(counts).map(([k, n]) => `${n} ${k}`);
  return `${settled} of ${items.length} answered${parts.length ? ` — ${parts.join(", ")}` : ""}.`;
}

type Phase = "confirm" | "running" | "done";

export function CloseAllFinishedDialog({
  items,
  moreExist,
  onClose,
}: {
  items: readonly BulkEndItem[];
  /** True when the finished read hit its bound — more exist than listed. */
  moreExist: boolean;
  /** Called on close with every settled result. */
  onClose: (results: BulkEndResults) => void;
}) {
  const [phase, setPhase] = useState<Phase>("confirm");
  const [results, setResults] = useState<BulkEndResults>({});

  const run = useCallback(async () => {
    setPhase("running");
    await runBounded(
      items,
      CLOSE_ALL_FINISHED_CONCURRENCY,
      async (it): Promise<RemoteSessionEndResult> => {
        try {
          return await remoteSessionEnd(it.deviceId, it.sessionId, false);
        } catch (err) {
          return endInvokeFailure(err, it.deviceId, it.sessionId);
        }
      },
      (it, r) => setResults((prev) => ({ ...prev, [it.sessionId]: describeEndResult(r, false) })),
    );
    setPhase("done");
  }, [items]);

  const n = items.length;
  const message =
    phase === "confirm"
      ? `End ${n} finished session${n === 1 ? "" : "s"} on other devices?`
      : phase === "running"
        ? `Ending — ${bulkEndSummary(items, results)}`
        : `Done — ${bulkEndSummary(items, results)}`;
  const description =
    phase === "confirm"
      ? "Each is asked to exit gracefully (/exit). A busy or wedged session is refused and left " +
        "running — there is no force in bulk." +
        (moreExist
          ? " More finished sessions exist than this read covered; run it again after."
          : "")
      : phase === "done"
        ? "Sessions that ended or were already gone are hidden from the list here. coord learns " +
          "a device's close late, so the fleet list may still show them until it catches up."
        : undefined;

  return (
    <ConfirmDialog
      open
      title="Close all finished sessions"
      message={message}
      description={description}
      variant="warning"
      confirmText={`End ${n}`}
      cancelText={phase === "confirm" ? "Cancel" : "Close"}
      isLoading={phase === "running"}
      hideConfirm={phase === "done"}
      dialogId={FLEET_CLOSE_ALL_FINISHED_DIALOG_ID}
      confirmId={FLEET_CLOSE_ALL_FINISHED_CONFIRM_ID}
      cancelId={FLEET_CLOSE_ALL_FINISHED_CANCEL_ID}
      onClose={() => onClose(results)}
      onConfirm={() => void run()}
    >
      <BulkEndResultList items={items} results={results} showStatus={phase !== "confirm"} />
    </ConfirmDialog>
  );
}
