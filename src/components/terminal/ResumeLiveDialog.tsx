import { AlertTriangle, X } from "lucide-react";

import { conversationLabel } from "./paneLabel";
import { describeHolder, paneHolderOf, type ResumeGatePrompt } from "./resumeGate";
import type { LiveClaudeSession } from "./liveClaudeSessions";

/**
 * The resume-live dialog (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3). The click gate (`resumeGate.runClickResume`) opens it when a
 * Resume names a conversation that a live claude already holds, or when the
 * live registry could not be read.
 *
 * - **Live, held in a runner pane:** "Go to the existing pane" (the default),
 *   "Fork as a new conversation", "Resume anyway (two writers)", Cancel.
 * - **Live, held outside every runner pane:** the holder's pid, account and
 *   working directory, then Fork / Resume anyway / Cancel (Cancel default).
 * - **Unknown (D2):** "Could not read which conversations are live". Cancel is
 *   the default; Resume anyway says it may create a second writer.
 *
 * Fork is offered only when the prompt allows it (`canFork`: a Claude session
 * opened into a new tab). Same shell as `ui/ConfirmDialog` — fixed overlay,
 * zinc panel, amber warning header — with a vertical action list, because the
 * actions carry sentences. Every control carries `data-ui-bridge-id` (what the
 * UI Bridge SDK scans) and `data-ui-id`.
 */

export const RESUME_LIVE_DIALOG_ID = "terminal.resume-live-dialog";
export const RESUME_LIVE_GO_TO_PANE_ID = "terminal.resume-live-go-to-pane";
export const RESUME_LIVE_FORK_ID = "terminal.resume-live-fork";
export const RESUME_LIVE_RESUME_ANYWAY_ID = "terminal.resume-live-resume-anyway";
export const RESUME_LIVE_CANCEL_ID = "terminal.resume-live-cancel";
export const RESUME_LIVE_CLOSE_ID = "terminal.resume-live-close";
export const RESUME_LIVE_HOLDERS_ID = "terminal.resume-live-holders";

/** The UI Bridge id of one holder row. */
export function resumeLiveHolderId(h: Pick<LiveClaudeSession, "pid">): string {
  return `terminal.resume-live-holder-${h.pid}`;
}

/** The text of the "Resume anyway" button for each dialog form. */
export const RESUME_ANYWAY_LIVE_TEXT =
  "Resume anyway (two writers) — two processes will write one transcript and one worktree";
export const RESUME_ANYWAY_UNKNOWN_TEXT =
  "Resume anyway — this may create a second writer on the transcript and worktree";

export interface ResumeLiveDialogProps {
  prompt: ResumeGatePrompt;
  /** "Go to the existing pane": switch to the holder's page, focus and flash it. */
  onGoToPane: (holder: LiveClaudeSession) => void;
  onFork: () => void;
  onResumeAnyway: () => void;
  onCancel: () => void;
}

/** Both UI annotation attributes for one control. */
function ids(id: string) {
  return { "data-ui-bridge-id": id, "data-ui-id": id };
}

const ACTION =
  "w-full text-left px-4 py-2 text-sm font-medium rounded-md transition-colors " +
  "focus:outline-none focus:ring-2 focus:ring-offset-0";

export function ResumeLiveDialog({
  prompt,
  onGoToPane,
  onFork,
  onResumeAnyway,
  onCancel,
}: ResumeLiveDialogProps) {
  const label = conversationLabel(prompt.conversationName, prompt.claudeSessionId);
  const live = prompt.kind === "live";
  const paneHolder = live ? paneHolderOf(prompt.holders) : null;
  const title = live ? "Conversation already open" : "Could not read which conversations are live";
  const message = live
    ? `"${label}" is already live — a claude process is writing it right now.`
    : `Resume "${label}"? The live-session registry could not be read, so it may already ` +
      "be open in another pane or window.";
  // The UNKNOWN form names Fork only when the dialog offers it (`canFork`):
  // the Retry and a non-Claude session have no fork button to point at.
  const explanation = live
    ? "Resuming it again would start a second claude on the same transcript and worktree."
    : prompt.canFork
      ? "Cancel and look for it first, or fork it into a new conversation, which never " +
        "shares a transcript."
      : "Cancel and look for it first.";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop: a click or Escape is Cancel. */}
      <div
        className="absolute inset-0 bg-black/50"
        onClick={onCancel}
        role="button"
        tabIndex={-1}
        aria-label="Close dialog"
      />
      <div
        {...ids(RESUME_LIVE_DIALOG_ID)}
        data-gate-kind={prompt.kind}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === "Escape") onCancel();
        }}
        className="relative bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-full max-w-lg mx-4 overflow-hidden"
      >
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 bg-amber-500/10">
          <div className="flex items-center gap-2 min-w-0">
            <AlertTriangle className="w-5 h-5 text-amber-400 shrink-0" />
            <h3 className="text-lg font-semibold text-amber-400">{title}</h3>
          </div>
          <button
            {...ids(RESUME_LIVE_CLOSE_ID)}
            onClick={onCancel}
            aria-label="Cancel"
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <div className="p-6 space-y-3">
          <p className="text-zinc-300 break-words">{message}</p>
          {live && prompt.holders.length > 0 && (
            <ul {...ids(RESUME_LIVE_HOLDERS_ID)} className="text-xs space-y-1 text-zinc-400">
              {prompt.holders.map((h) => (
                <li
                  key={`${h.pid}-${h.terminalId ?? "outside"}`}
                  {...ids(resumeLiveHolderId(h))}
                  data-holder-pane={h.terminalId ? "pane" : "outside"}
                  className="break-words"
                >
                  {describeHolder(h)}
                </li>
              ))}
            </ul>
          )}
          <p className="text-sm text-zinc-400">{explanation}</p>
        </div>

        <div className="flex flex-col gap-2 px-6 py-4 border-t border-zinc-700 bg-zinc-800/50">
          {paneHolder && (
            <button
              {...ids(RESUME_LIVE_GO_TO_PANE_ID)}
              autoFocus
              onClick={() => onGoToPane(paneHolder)}
              className={`${ACTION} bg-blue-600 hover:bg-blue-500 text-white focus:ring-blue-300`}
            >
              Go to the existing pane
            </button>
          )}
          {prompt.canFork && (
            <button
              {...ids(RESUME_LIVE_FORK_ID)}
              onClick={onFork}
              className={`${ACTION} bg-zinc-700 hover:bg-zinc-600 text-zinc-100 focus:ring-zinc-400`}
            >
              Fork as a new conversation (a copy with a new id)
            </button>
          )}
          <button
            {...ids(RESUME_LIVE_RESUME_ANYWAY_ID)}
            onClick={onResumeAnyway}
            className={`${ACTION} bg-red-600/90 hover:bg-red-500 text-white focus:ring-red-300`}
          >
            {live ? RESUME_ANYWAY_LIVE_TEXT : RESUME_ANYWAY_UNKNOWN_TEXT}
          </button>
          <button
            {...ids(RESUME_LIVE_CANCEL_ID)}
            autoFocus={!paneHolder}
            onClick={onCancel}
            className={`${ACTION} text-zinc-300 hover:text-zinc-100 hover:bg-zinc-700/50 focus:ring-zinc-500`}
          >
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
