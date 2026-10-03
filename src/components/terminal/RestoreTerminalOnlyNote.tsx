/**
 * The terminal-only ("fresh conversation") restore note (session-restore
 * redesign Phase 5, "honest capability tiers").
 *
 * The terminal + cwd were restored but the CONVERSATION was NOT. Three causes
 * land here, so the copy states the OUTCOME and not a cause: the provider's
 * `restoreTier()` is `"terminal-only"` (it can re-open the terminal but
 * cannot `--resume` a chat by id), the session has no transcript on disk (it
 * started but never accumulated messages, so there is no conversation to
 * resume — see `classifyRestoreAction`'s transcript gate), or the id was only
 * recovered by a transcript/process-anchored BACKSTOP guess (not pinned by the
 * runner) — too weak to act on, so it is treated as no match found rather
 * than offered up for a best-effort confirm. Naming any one cause would be
 * wrong for the others, and the note aggregates terminals so it cannot
 * attribute per-tab. Informational + dismissible — the user must SEE the
 * conversation is fresh and is never misled into thinking it came back. There
 * is nothing to retry.
 *
 * It is not a failure, so it is not a `SessionFailure`: it was split out of
 * the former `ResumeFailedBanner` unchanged when that banner became
 * `SessionFailureBanner` (plan `2026-09-20-ai-session-handling-is-claude-shaped`,
 * Phase 7), and it sits beside that banner in the same top-right advisory
 * column. A tab whose resume FAILED is left to the failure banner — it is
 * actionable — so one tab appears in one place.
 */

import { MessageSquareDashed, X } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
import { AdvisorySlot } from "./AdvisoryStack";
import type { TerminalTab } from "./useTerminalManager";
import { TabTitle } from "./displayTitle";

export interface RestoreTerminalOnlyNoteProps {
  tabs: TerminalTab[];
  /**
   * Dismiss the note for one tab — clears `restoreTerminalOnly`. Optional:
   * when omitted the note is shown without a dismiss button (still honest,
   * just sticky).
   */
  onDismiss?: (tabId: string) => void;
}

/**
 * The terminal-only tabs this note surfaces — exported for unit tests. A tab
 * whose resume failed is excluded: the failure banner owns it.
 */
export function terminalOnlyRestoreTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => t.restoreTerminalOnly && !t.resumeFailed && t.isAlive !== false);
}

export function RestoreTerminalOnlyNote({ tabs, onDismiss }: RestoreTerminalOnlyNoteProps) {
  const terminalOnly = terminalOnlyRestoreTabs(tabs);
  const { ref } = useUIElement({
    id: "terminal-restore-terminal-only-note",
    type: "generic",
    label: "Terminal-only restore note",
  });
  if (terminalOnly.length === 0) return null;

  return (
    <AdvisorySlot>
      <div
        ref={ref}
        data-ui-bridge-id="terminal.restore-terminal-only-note"
        className="w-[360px] rounded border shadow-lg p-2.5 bg-[#f7768e]/10 border-[#f7768e]/40"
      >
        <div className="flex items-start gap-2">
          <MessageSquareDashed className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#7aa2f7]" />
          <div className="flex-1 min-w-0">
            <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
              {terminalOnly.length === 1
                ? "Terminal restored — fresh conversation"
                : `${terminalOnly.length} terminals restored — fresh conversations`}
            </div>
            <div className="text-[10px] text-[#a9b1d6] leading-snug mt-0.5">
              The terminal and working directory were restored, but the previous conversation
              could not be resumed by id — so it starts fresh. Nothing was lost from the terminal;
              only the chat history did not carry over.
            </div>
            <ul className="mt-1.5 space-y-1">
              {terminalOnly.map((t) => (
                <li
                  key={t.id}
                  data-ui-bridge-id="terminal.restore-terminal-only-item"
                  data-terminal-id={t.id}
                  className="flex items-center gap-2 text-[11px] leading-snug"
                >
                  <span className="text-[#c0caf5] font-medium truncate flex-1">
                    <TabTitle tab={t} />
                  </span>
                  {onDismiss && (
                    <button
                      type="button"
                      data-ui-bridge-id="terminal.restore-terminal-only-dismiss"
                      data-terminal-id={t.id}
                      onClick={() => onDismiss(t.id)}
                      className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#7aa2f7]/40 text-[#7aa2f7] hover:bg-[#7aa2f7]/15 text-[10px]"
                      title="Dismiss this fresh-conversation note"
                    >
                      <X className="w-2.5 h-2.5" />
                      Got it
                    </button>
                  )}
                </li>
              ))}
            </ul>
          </div>
        </div>
      </div>
    </AdvisorySlot>
  );
}
