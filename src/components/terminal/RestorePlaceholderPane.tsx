/**
 * A prepared restart's session, waiting in its own zone for the operator's
 * Restore (plan `2026-10-21-operator-prepare-for-restart-and-progressive-restore`,
 * Phase 7, D6). There is no PTY behind it: `ZoneGrid` mounts this instead of a
 * `TerminalInstance`, the way a plan tab mounts its viewer.
 *
 * It shows what the prepare captured — the name, the account, the effort (and
 * whether the session is held for a hand restore), the working directory and
 * plan, and the state at prepare including when a usage-limited account
 * resets — and offers:
 *
 * - **Restore** — enqueue this session on the one restore queue. Restoring a
 *   held or finished session by name is allowed; that is what "hand restore"
 *   means. A usage-limited account restores under the SAME account anyway
 *   (D3: a restore never re-targets an account).
 * - **Finish / Unfinish** — `terminal_session_set_finished`, reversible. A
 *   finished row leaves the bulk buttons.
 * - **Hold / Release hold** — through `RESTART_PREP_SET_HELD_COMMAND`.
 *
 * Nothing on the pane claims more than was read: an effort or hold the status
 * read has not delivered reads "unknown" / "not read yet".
 */

import { useEffect, useState } from "react";
import { CheckCircle2, Hand, RotateCcw } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import {
  effortBadgeText,
  placeholderSessionIdOf,
  restoreButtonLabel,
  restoreLabelChangesAt,
  restoreLabelRefreshMs,
  setPlaceholderFinished,
  setPlaceholderHeld,
  stateAtPrepareText,
} from "./restorePlaceholders";
import { restorePhaseText, useRestoreCohort, useRestoreQueue } from "./useRestoreQueue";

export const restorePlaceholderIds = (sid: string) => ({
  pane: `terminal.restore-placeholder-${sid}`,
  restore: `terminal.restore-placeholder-restore-${sid}`,
  finish: `terminal.restore-placeholder-finish-${sid}`,
  held: `terminal.restore-placeholder-held-${sid}`,
});

export function RestorePlaceholderPane({ tabId }: { tabId: string }) {
  const sid = placeholderSessionIdOf(tabId) ?? "";
  const cohort = useRestoreCohort();
  const queue = useRestoreQueue();
  const row = cohort.rows.find((r) => r.claudeSessionId === sid);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ids = restorePlaceholderIds(sid);
  // Re-render when a usage-limited row's reset passes, so its Restore label
  // ("Account … resets at …") does not go stale while the pane sits open.
  // The timer depends on the fixed reset instant, so an unrelated re-render
  // does not re-arm it; `labelTick` re-arms only a capped (very long) wait.
  const [labelTick, setLabelTick] = useState(0);
  const labelChangesAt = row ? restoreLabelChangesAt(row) : null;
  useEffect(() => {
    const ms = restoreLabelRefreshMs(labelChangesAt);
    if (ms === null) return;
    const handle = setTimeout(() => setLabelTick((t) => t + 1), ms);
    return () => clearTimeout(handle);
  }, [labelChangesAt, labelTick]);
  const phase = queue.phaseOf(sid);
  const phaseText = restorePhaseText(phase);
  const inQueue =
    phase?.phase === "queued" || phase?.phase === "restoring" || phase?.phase === "waiting";

  const restore = () => {
    if (!row || row.blocker !== null) return;
    setError(null);
    queue.enqueue([row]);
  };
  const toggleFinished = async () => {
    if (!row) return;
    setBusy(true);
    setError(await setPlaceholderFinished(sid, !row.finished));
    setBusy(false);
  };
  const toggleHeld = async () => {
    if (!row || row.held === null) return;
    setBusy(true);
    const err = await setPlaceholderHeld(row.prepareId, sid, !row.held);
    setError(err ? `hold not changed: ${err}` : null);
    setBusy(false);
  };

  useUIComponent({
    id: `terminal.restore-placeholder-pane-${sid}`,
    name: `Restore placeholder: ${row?.name ?? sid}`,
    description: "A restart-cohort session waiting in its zone for a hand Restore.",
    actions: [
      {
        id: "restore",
        label: row ? restoreButtonLabel(row) : "Restore",
        description: "Queue this session for a verified backend resume under its own account.",
        // `destructive` — starts a `claude --resume` process (as
        // `since-restart.resume-selected`).
        effect: "destructive",
        handler: () => {
          restore();
        },
      },
      {
        id: "finish",
        label: row?.finished ? "Unfinish" : "Finish",
        description: "Mark the session finished on the work axis, or unmark it. Reversible.",
        // `write` — flips the finish mark; toggling back undoes it.
        effect: "write",
        handler: () => {
          void toggleFinished();
        },
      },
      {
        id: "toggle-held",
        label: row?.held ? "Release hold" : "Hold",
        description: "Hold the session for a hand restore (bulk restores skip it), or release it.",
        // `write` — flips one cohort row's flag; toggling back undoes it.
        effect: "write",
        handler: () => {
          void toggleHeld();
        },
      },
    ],
  });

  if (!row) {
    return (
      <div
        data-ui-bridge-id={ids.pane}
        className="h-full w-full flex items-center justify-center text-[12px] text-[#565f89]"
      >
        This session is no longer waiting for a restore.
      </div>
    );
  }

  return (
    <div
      data-ui-bridge-id={ids.pane}
      data-restore-phase={phase?.phase ?? "idle"}
      className="h-full w-full flex flex-col items-center justify-center gap-2 p-4 bg-[#16161e] text-[#a9b1d6]"
    >
      <div className="text-[14px] font-semibold text-[#c0caf5] text-center break-all">
        {row.name}
      </div>
      <div className="flex flex-wrap items-center justify-center gap-1.5 text-[10px]">
        <span className="px-1.5 py-0.5 rounded bg-[#7aa2f7]/10 text-[#7aa2f7]">
          {row.accountLabel ?? row.configDir ?? "account unknown"}
        </span>
        <span
          className={`px-1.5 py-0.5 rounded ${
            row.held === true ? "bg-[#e0af68]/15 text-[#e0af68]" : "bg-[#9ece6a]/10 text-[#9ece6a]"
          }`}
        >
          {effortBadgeText(row)}
        </span>
        {row.finished && (
          <span className="px-1.5 py-0.5 rounded bg-[#565f89]/20 text-[#a9b1d6]">finished</span>
        )}
      </div>
      <div className="text-[11px] text-center break-all opacity-80">
        {row.workingDir ?? "working directory unknown"}
        {row.planSlug ? ` · plan ${row.planSlug}` : ""}
      </div>
      <div className="text-[11px] opacity-80">{stateAtPrepareText(row)}</div>
      {row.blocker && (
        <div className="text-[11px] text-[#e0af68]">not restorable yet: {row.blocker}</div>
      )}
      {phaseText && <div className="text-[11px] text-[#7dcfff]">{phaseText}</div>}
      {error && <div className="text-[11px] text-[#f7768e] break-all">{error}</div>}
      <div className="flex items-center gap-2 mt-1">
        <button
          type="button"
          data-ui-bridge-id={ids.restore}
          disabled={inQueue || row.blocker !== null}
          title={row.blocker ? `not restorable yet: ${row.blocker}` : undefined}
          onClick={restore}
          className="flex items-center gap-1 px-2.5 py-1 rounded text-[11px] bg-[#7aa2f7]/20 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-50"
        >
          <RotateCcw className="w-3 h-3" />
          {phase?.phase === "failed" ? "Retry" : restoreButtonLabel(row)}
        </button>
        <button
          type="button"
          data-ui-bridge-id={ids.finish}
          disabled={busy || inQueue}
          onClick={() => void toggleFinished()}
          className="flex items-center gap-1 px-2.5 py-1 rounded text-[11px] bg-[#565f89]/20 hover:bg-[#565f89]/30 disabled:opacity-50"
        >
          <CheckCircle2 className="w-3 h-3" />
          {row.finished ? "Unfinish" : "Finish"}
        </button>
        <button
          type="button"
          data-ui-bridge-id={ids.held}
          disabled={busy || row.held === null}
          onClick={() => void toggleHeld()}
          className="flex items-center gap-1 px-2.5 py-1 rounded text-[11px] bg-[#e0af68]/10 text-[#e0af68] hover:bg-[#e0af68]/20 disabled:opacity-50"
        >
          <Hand className="w-3 h-3" />
          {row.held ? "Release hold" : "Hold"}
        </button>
      </div>
    </div>
  );
}
