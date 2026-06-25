/**
 * Honest restore-status banner — the single per-tab surface that tells the
 * user the TRUTH about what a boot-restore did or could not do (session-restore
 * redesign Phase 5 "honest capability tiers", building on issue #548 Phase 3).
 * It distinguishes the four restore outcomes that are NOT a silent, fully
 * restored conversation:
 *
 *   1. RESUME FAILED (`resumeFailed`) — a `--resume` WAS typed (the runner
 *      knows the id + the provider resumes the full conversation) but the
 *      provider UI handshake never appeared after one retry. Operator-clickable
 *      "Retry resume" re-runs the same type-and-verify path. The durable record
 *      keeps its restore-pending marker so the liveness poll can't flip it
 *      `poll-dead` while the operator decides.
 *   2. RECONCILED — confirm a best-effort match (`resumeQuarantined`) — the id
 *      was recovered by a transcript/process-anchored BACKSTOP, not pinned by
 *      the runner, so it MIGHT name another tool's session. Nothing was resumed
 *      automatically; the user confirms the best-effort match with one click
 *      (which runs the normal verified resume). Presented as a match-to-confirm,
 *      never as a guaranteed restore.
 *   3. TERMINAL-ONLY / fresh conversation (`restoreTerminalOnly`) — the terminal
 *      + cwd were restored but the CONVERSATION was NOT (the provider's
 *      `restoreTier()` is `"terminal-only"`, so it can re-open the terminal but
 *      cannot `--resume` the chat by id). Informational + dismissible — the user
 *      must SEE the conversation is fresh and is never misled into thinking it
 *      came back. There is nothing to retry.
 *
 * An auto-resumed (silent, conversation restored) tab and a plain shell surface
 * in NONE of the lists — exactly the no-clutter common case.
 *
 * Renders nothing when no tab is in any of these states. Visual language
 * follows {@link SessionRecoveryBanner} / {@link CoordWarningBanner} (top-right
 * advisory column).
 */

import { AlertTriangle, HelpCircle, MessageSquareDashed, Play, RotateCcw, X } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";

export interface ResumeFailedBannerProps {
  tabs: TerminalTab[];
  /** Re-run (or confirm-and-run) the type-and-verify resume for one tab. */
  onRetryResume: (tabId: string) => void;
  /**
   * Dismiss the informational "fresh conversation" (terminal-only) note for one
   * tab — clears `restoreTerminalOnly`. There is nothing to retry for these, so
   * the only affordance is acknowledging the note. Optional: when omitted the
   * note is shown without a dismiss button (still honest, just sticky).
   */
  onDismissTerminalOnly?: (tabId: string) => void;
}

/** The failed-resume tabs this banner surfaces — exported for unit tests. */
export function failedResumeTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => t.resumeFailed && t.isAlive !== false);
}

/**
 * The quarantined (reconciled-binding, awaiting best-effort confirm) tabs this
 * banner surfaces — exported for unit tests. A tab that later FAILS its
 * confirmed resume moves to the failed list, never both.
 */
export function quarantinedResumeTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => t.resumeQuarantined && !t.resumeFailed && t.isAlive !== false);
}

/**
 * The terminal-only ("fresh conversation") tabs this banner surfaces — exported
 * for unit tests. A tab whose resume failed or is awaiting confirm takes
 * precedence over the informational note (it's actionable), so those are
 * excluded here to keep one tab in one section.
 */
export function terminalOnlyRestoreTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter(
    (t) => t.restoreTerminalOnly && !t.resumeFailed && !t.resumeQuarantined && t.isAlive !== false,
  );
}

export function ResumeFailedBanner({
  tabs,
  onRetryResume,
  onDismissTerminalOnly,
}: ResumeFailedBannerProps) {
  const failed = failedResumeTabs(tabs);
  const quarantined = quarantinedResumeTabs(tabs);
  const terminalOnly = terminalOnlyRestoreTabs(tabs);
  if (failed.length === 0 && quarantined.length === 0 && terminalOnly.length === 0) return null;

  return (
    <div
      data-ui-bridge-id="terminal.resume-failed-banner"
      className="absolute top-2 right-2 z-30 w-[360px] rounded border shadow-lg p-2.5 bg-[#f7768e]/10 border-[#f7768e]/40"
    >
      {failed.length > 0 && (
        <div className="flex items-start gap-2">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#f7768e]" />
          <div className="flex-1 min-w-0">
            <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
              {failed.length === 1
                ? "Session resume failed"
                : `${failed.length} session resumes failed`}
            </div>
            <ul className="mt-1.5 space-y-1">
              {failed.map((t) => (
                <li
                  key={t.id}
                  data-ui-bridge-id="terminal.resume-failed-banner-item"
                  data-terminal-id={t.id}
                  className="flex items-center gap-2 text-[11px] leading-snug"
                >
                  <span className="text-[#c0caf5] font-medium truncate flex-1">{t.title}</span>
                  <button
                    type="button"
                    data-ui-bridge-id="terminal.resume-failed-retry"
                    data-terminal-id={t.id}
                    onClick={() => onRetryResume(t.id)}
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#f7768e]/40 text-[#f7768e] hover:bg-[#f7768e]/15 text-[10px]"
                    title="Retype the resume command and re-verify the Claude UI handshake"
                  >
                    <RotateCcw className="w-2.5 h-2.5" />
                    Retry resume
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}

      {quarantined.length > 0 && (
        <div className={`flex items-start gap-2 ${failed.length > 0 ? "mt-2" : ""}`}>
          <HelpCircle className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#e0af68]" />
          <div className="flex-1 min-w-0">
            <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
              {quarantined.length === 1
                ? "Best-effort match — confirm to restore this session?"
                : `${quarantined.length} best-effort matches — confirm to restore?`}
            </div>
            <div className="text-[10px] text-[#a9b1d6] leading-snug mt-0.5">
              The runner did not pin these session ids — it matched them from transcript timing as a
              backstop, so a match MIGHT belong to another tool's session. Nothing was resumed
              automatically; confirm to restore the matched conversation.
            </div>
            <ul className="mt-1.5 space-y-1">
              {quarantined.map((t) => (
                <li
                  key={t.id}
                  data-ui-bridge-id="terminal.resume-quarantined-banner-item"
                  data-terminal-id={t.id}
                  className="flex items-center gap-2 text-[11px] leading-snug"
                >
                  <span className="text-[#c0caf5] font-medium truncate flex-1">{t.title}</span>
                  <button
                    type="button"
                    data-ui-bridge-id="terminal.resume-quarantined-confirm"
                    data-terminal-id={t.id}
                    onClick={() => onRetryResume(t.id)}
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#e0af68]/40 text-[#e0af68] hover:bg-[#e0af68]/15 text-[10px]"
                    title="Restore the matched session and verify the provider UI handshake"
                  >
                    <Play className="w-2.5 h-2.5" />
                    Restore match
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}

      {terminalOnly.length > 0 && (
        <div
          className={`flex items-start gap-2 ${failed.length > 0 || quarantined.length > 0 ? "mt-2" : ""}`}
        >
          <MessageSquareDashed className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#7aa2f7]" />
          <div className="flex-1 min-w-0">
            <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
              {terminalOnly.length === 1
                ? "Terminal restored — fresh conversation"
                : `${terminalOnly.length} terminals restored — fresh conversations`}
            </div>
            <div className="text-[10px] text-[#a9b1d6] leading-snug mt-0.5">
              The terminal and working directory were restored, but this provider can&apos;t resume
              the previous conversation by id — so it starts fresh. Nothing was lost from the
              terminal; only the chat history did not carry over.
            </div>
            <ul className="mt-1.5 space-y-1">
              {terminalOnly.map((t) => (
                <li
                  key={t.id}
                  data-ui-bridge-id="terminal.restore-terminal-only-item"
                  data-terminal-id={t.id}
                  className="flex items-center gap-2 text-[11px] leading-snug"
                >
                  <span className="text-[#c0caf5] font-medium truncate flex-1">{t.title}</span>
                  {onDismissTerminalOnly && (
                    <button
                      type="button"
                      data-ui-bridge-id="terminal.restore-terminal-only-dismiss"
                      data-terminal-id={t.id}
                      onClick={() => onDismissTerminalOnly(t.id)}
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
      )}
    </div>
  );
}
