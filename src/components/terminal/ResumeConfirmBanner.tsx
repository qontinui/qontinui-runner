/**
 * "Confirm resume" affordance for restored tabs whose registry binding is not
 * proven (item 5 of `2026-06-13-boot-restore-wrong-sessions-remediation`).
 *
 * A record whose `bindOrigin` is not `"pinned"` was bound by the
 * freshest-transcript mtime guess (or predates the field) — the session id
 * may belong to a FOREIGN session (e.g. a VS Code CLI session in the same
 * project that won the mtime race). The boot restore still creates the tab so
 * the screen layout is preserved, but types NO resume command; this banner
 * lists those tabs with a one-click operator confirm (runs the same
 * type-and-verify path as `ResumeFailedBanner`'s retry) and a decline (closes
 * the registry record so the binding stops resurrecting).
 *
 * Renders nothing when no tab awaits confirmation. Visual language follows
 * {@link ResumeFailedBanner} (top-right advisory column).
 */

import { HelpCircle, Play, X } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";

export interface ResumeConfirmBannerProps {
  tabs: TerminalTab[];
  /** Operator confirmed the binding — type and verify the resume. */
  onConfirmResume: (tabId: string) => void;
  /** Operator declined — close the registry record, keep the plain shell. */
  onDeclineResume: (tabId: string) => void;
}

/** The tabs this banner surfaces — exported for unit tests. */
export function confirmPendingTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => t.resumeNeedsConfirm && t.isAlive !== false);
}

export function ResumeConfirmBanner({
  tabs,
  onConfirmResume,
  onDeclineResume,
}: ResumeConfirmBannerProps) {
  const pending = confirmPendingTabs(tabs);
  if (pending.length === 0) return null;

  // top-44: below ResumeFailedBanner (top-2) and CoordWarningBanner (top-24),
  // which can all be visible after a messy restore.
  return (
    <div
      data-ui-bridge-id="terminal.resume-confirm-banner"
      className="absolute top-44 right-2 z-30 w-[360px] rounded border shadow-lg p-2.5 bg-[#e0af68]/10 border-[#e0af68]/40"
    >
      <div className="flex items-start gap-2">
        <HelpCircle className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#e0af68]" />
        <div className="flex-1 min-w-0">
          <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
            {pending.length === 1
              ? "Unverified session binding — resume?"
              : `${pending.length} unverified session bindings — resume?`}
          </div>
          <div className="mt-0.5 text-[10px] text-[#a9b1d6] leading-snug">
            These sessions were matched by transcript timing, not pinned by id — they may belong
            to another window.
          </div>
          <ul className="mt-1.5 space-y-1">
            {pending.map((t) => (
              <li
                key={t.id}
                data-ui-bridge-id="terminal.resume-confirm-banner-item"
                data-terminal-id={t.id}
                className="flex items-center gap-2 text-[11px] leading-snug"
              >
                <span className="text-[#c0caf5] font-medium truncate flex-1">{t.title}</span>
                <button
                  type="button"
                  data-ui-bridge-id="terminal.resume-confirm-accept"
                  data-terminal-id={t.id}
                  onClick={() => onConfirmResume(t.id)}
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#9ece6a]/40 text-[#9ece6a] hover:bg-[#9ece6a]/15 text-[10px]"
                  title="Type the resume command and verify the Claude UI handshake"
                >
                  <Play className="w-2.5 h-2.5" />
                  Resume
                </button>
                <button
                  type="button"
                  data-ui-bridge-id="terminal.resume-confirm-decline"
                  data-terminal-id={t.id}
                  onClick={() => onDeclineResume(t.id)}
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#f7768e]/40 text-[#f7768e] hover:bg-[#f7768e]/15 text-[10px]"
                  title="Close the registry record — keep the pane as a plain shell"
                >
                  <X className="w-2.5 h-2.5" />
                  Don&apos;t resume
                </button>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
