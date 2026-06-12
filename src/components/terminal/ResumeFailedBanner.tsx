/**
 * "Resume failed — retry" affordance for restored tabs whose typed
 * `claude --resume` never produced the Claude UI handshake (Phase 3 of
 * `2026-06-12-runner-session-registry-and-restore-hardening`, issue #548).
 *
 * The boot-restore drain verifies each resume and retries once; a tab that
 * still shows no handshake is parked with `resumeFailed: true` instead of
 * being silently presented as resumed. This banner lists those tabs with an
 * operator-clickable retry (which re-runs the same type-and-verify path).
 * The durable registry record keeps its restore-pending marker the whole
 * time, so the backend liveness poll cannot flip it `poll-dead` while the
 * operator decides.
 *
 * Renders nothing when no tab is in the failed state. Visual language
 * follows {@link SessionRecoveryBanner} / {@link CoordWarningBanner}
 * (top-right advisory column).
 */

import { AlertTriangle, RotateCcw } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";

export interface ResumeFailedBannerProps {
  tabs: TerminalTab[];
  /** Re-run the type-and-verify resume for one failed tab. */
  onRetryResume: (tabId: string) => void;
}

/** The tabs this banner surfaces — exported for unit tests. */
export function failedResumeTabs(tabs: TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => t.resumeFailed && t.isAlive !== false);
}

export function ResumeFailedBanner({ tabs, onRetryResume }: ResumeFailedBannerProps) {
  const failed = failedResumeTabs(tabs);
  if (failed.length === 0) return null;

  return (
    <div
      data-ui-bridge-id="terminal.resume-failed-banner"
      className="absolute top-2 right-2 z-30 w-[360px] rounded border shadow-lg p-2.5 bg-[#f7768e]/10 border-[#f7768e]/40"
    >
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
    </div>
  );
}
