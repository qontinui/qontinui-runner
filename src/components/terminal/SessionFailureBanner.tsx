/**
 * The one per-tab failure surface for AI sessions (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 7). It generalises the former `ResumeFailedBanner`: a resume whose
 * handshake never appeared is now one `SessionFailure` kind among the rest
 * (quota or budget exhausted, rate limited, overloaded, auth required, the
 * CLI exiting on its own, …), each classified by the runner and rendered here
 * from its payload.
 *
 * - Title and details come straight from the payload. The runner words a
 *   `hint`-confidence failure (a scraped phrase, a timeout) as "may have …"
 *   until something confirms it, and the row is marked unconfirmed.
 * - Actions come from the payload too. `resume` on a live tab re-runs the
 *   verified resume (the old "Retry resume"); an action this page has no
 *   one-click handler for is named, not faked, so the operator knows what
 *   would help.
 * - A row can be dismissed (the operator's acknowledgement). Otherwise a
 *   failure clears only when the runner sees its recovery's evidence — a
 *   verified handshake, a usage probe finding the account fine, a working
 *   replacement session — never on a timer.
 *
 * The terminal-only "fresh conversation" restore note is not a failure and is
 * rendered by `RestoreTerminalOnlyNote`, beside this banner in the same
 * top-right advisory column.
 */

import { AlertTriangle, CircleHelp, RotateCcw, X } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
import { AdvisorySlot } from "./AdvisoryStack";
import type { TerminalTab } from "./useTerminalManager";
import { TabTitle } from "./displayTitle";
import {
  ACTION_LABELS,
  failureBannerEntries,
  isHint,
  type FailureAction,
  type FailureBannerEntry,
  type FailuresByTerminal,
} from "./sessionFailures";

export interface SessionFailureBannerProps {
  tabs: TerminalTab[];
  failures: FailuresByTerminal;
  /** Re-run the type-and-verify resume for one tab. */
  onRetryResume: (tabId: string) => void;
  /** Acknowledge one failure. */
  onDismiss: (tabId: string, failureId: string) => void;
}

/**
 * Whether `action` on this row has a one-click handler here — exported for
 * unit tests. Only `resume` does, and only for a live tab that holds a
 * session id and is not already mid-resume: typing into a dead pane or
 * racing a running retry would be a guess.
 */
export function actionIsClickable(action: FailureAction, entry: FailureBannerEntry): boolean {
  const { tab } = entry;
  return (
    action === "resume" &&
    tab.isAlive !== false &&
    Boolean(tab.claudeSessionId) &&
    tab.isReconnecting !== true
  );
}

/** The named-but-not-clickable actions of a row — exported for unit tests. */
export function suggestedActions(entry: FailureBannerEntry): FailureAction[] {
  return entry.failure.actions.filter((a) => a !== "none" && !actionIsClickable(a, entry));
}

export function SessionFailureBanner({
  tabs,
  failures,
  onRetryResume,
  onDismiss,
}: SessionFailureBannerProps) {
  const rows = failureBannerEntries(tabs, failures);
  const { ref } = useUIElement({
    id: "terminal-session-failure-banner",
    type: "generic",
    label: "Session failure banner",
  });
  if (rows.length === 0) return null;

  return (
    <AdvisorySlot>
      <div
        ref={ref}
        data-ui-bridge-id="terminal.session-failure-banner"
        className="w-[360px] rounded border shadow-lg p-2.5 bg-[#f7768e]/10 border-[#f7768e]/40"
      >
        <ul className="space-y-2">
          {rows.map((row) => {
            const { tab, failure } = row;
            const hint = isHint(failure);
            const suggested = suggestedActions(row);
            return (
              <li
                key={`${tab.id}:${failure.id}`}
                data-ui-bridge-id="terminal.session-failure-item"
                data-terminal-id={tab.id}
                data-failure-kind={failure.kind}
                data-failure-confidence={failure.evidence.confidence}
                className="flex items-start gap-2"
              >
                {hint ? (
                  <CircleHelp className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#e0af68]" />
                ) : (
                  <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#f7768e]" />
                )}
                <div className="flex-1 min-w-0">
                  <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
                    {failure.title}
                    {hint && (
                      <span className="ml-1.5 text-[10px] font-normal text-[#e0af68]">
                        (unconfirmed)
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] text-[#c0caf5] font-medium truncate">
                    <TabTitle tab={tab} />
                  </div>
                  {failure.details && (
                    <div className="text-[10px] text-[#a9b1d6] leading-snug mt-0.5">
                      {failure.details}
                    </div>
                  )}
                  {failure.reason && (
                    <div
                      className="text-[10px] text-[#565f89] leading-snug mt-0.5 truncate"
                      title={failure.reason}
                    >
                      {failure.reason}
                    </div>
                  )}
                  {suggested.length > 0 && (
                    <div className="text-[10px] text-[#a9b1d6] leading-snug mt-0.5">
                      What may help: {suggested.map((a) => ACTION_LABELS[a]).join(" · ")}
                    </div>
                  )}
                  {failure.actions.includes("resume") && actionIsClickable("resume", row) && (
                    <button
                      type="button"
                      data-ui-bridge-id="terminal.session-failure-resume"
                      data-terminal-id={tab.id}
                      onClick={() => onRetryResume(tab.id)}
                      className="mt-1 flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#f7768e]/40 text-[#f7768e] hover:bg-[#f7768e]/15 text-[10px]"
                      title="Retype the resume command and re-verify the CLI's UI handshake"
                    >
                      <RotateCcw className="w-2.5 h-2.5" />
                      {ACTION_LABELS.resume}
                    </button>
                  )}
                </div>
                <button
                  type="button"
                  data-ui-bridge-id="terminal.session-failure-dismiss"
                  data-terminal-id={tab.id}
                  onClick={() => onDismiss(tab.id, failure.id)}
                  className="shrink-0 p-0.5 rounded text-[#a9b1d6] hover:bg-[#f7768e]/15"
                  title="Dismiss this failure"
                  aria-label="Dismiss this failure"
                >
                  <X className="w-3 h-3" />
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </AdvisorySlot>
  );
}
