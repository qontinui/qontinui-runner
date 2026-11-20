/**
 * "Claude did not start" — the failed fresh-launch banner (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 4).
 *
 * A failed `claude` leaves its shell alive, so nothing else on the page says
 * the launch failed. `launchVerification` sets `tab.launchFailure` only when
 * a failure was OBSERVED — the handshake wait ran out over a readable
 * scrollback, or the shim beaconed a failure — and this banner names that
 * signal (`handshake not seen in N s`, `shim: <kind>`) beside the pane's
 * Analyze button and a Dismiss. A launch nothing could be read for sets no
 * field and shows nothing here: UNKNOWN, never "started".
 *
 * Same advisory column and shape as {@link ResumeFailedBanner}.
 */

import { AlertTriangle, X } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
import { AdvisorySlot } from "./AdvisoryStack";
import type { TerminalTab } from "./useTerminalManager";
import { TabTitle } from "./displayTitle";
import { AnalyzeLaunchButton } from "./AnalyzeLaunchButton";
import { describeLaunchFailure } from "./launchVerification";

/** The tabs this banner lists — exported for unit tests. */
export function failedLaunchTabs(tabs: readonly TerminalTab[]): TerminalTab[] {
  return tabs.filter((t) => {
    const f = t.launchFailure;
    if (!f || t.isAlive === false) return false;
    return f.handshakeWaitedMs !== undefined || f.beaconKind !== undefined;
  });
}

export function LaunchFailedBanner({
  tabs,
  onDismiss,
}: {
  tabs: readonly TerminalTab[];
  onDismiss: (tabId: string) => void;
}) {
  const failed = failedLaunchTabs(tabs);
  const { ref } = useUIElement({
    id: "terminal-launch-failed-banner",
    type: "generic",
    label: "Launch failed banner",
  });
  if (failed.length === 0) return null;

  return (
    <AdvisorySlot>
      <div
        ref={ref}
        data-ui-bridge-id="terminal.launch-failed-banner"
        className="w-[360px] rounded border shadow-lg p-2.5 bg-[#e0af68]/10 border-[#e0af68]/40"
      >
        <div className="flex items-start gap-2">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-0.5 text-[#e0af68]" />
          <div className="flex-1 min-w-0">
            <div className="text-[12px] font-semibold text-[#c0caf5] leading-snug">
              {failed.length === 1
                ? "Claude did not start"
                : `Claude did not start in ${failed.length} panes`}
            </div>
            <ul className="mt-1.5 space-y-1.5">
              {failed.map((t) => (
                <li
                  key={t.id}
                  data-ui-bridge-id="terminal.launch-failed-banner-item"
                  data-terminal-id={t.id}
                  className="text-[11px] leading-snug"
                >
                  <div className="flex items-center gap-2">
                    <span className="text-[#c0caf5] font-medium truncate flex-1">
                      <TabTitle tab={t} />
                    </span>
                    <AnalyzeLaunchButton tabId={t.id} variant="banner" />
                    <button
                      type="button"
                      data-ui-bridge-id="terminal.launch-failed-dismiss"
                      data-terminal-id={t.id}
                      onClick={() => onDismiss(t.id)}
                      className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#565f89]/40 text-[#a9b1d6] hover:bg-[#565f89]/15 text-[10px]"
                      title="Dismiss this notice; the pane is left as it is"
                    >
                      <X className="w-2.5 h-2.5" />
                      Dismiss
                    </button>
                  </div>
                  <div
                    data-launch-failure-signal
                    className="text-[10px] text-[#a9b1d6] mt-0.5 break-words"
                  >
                    {describeLaunchFailure(t.launchFailure ?? {})}
                  </div>
                </li>
              ))}
            </ul>
          </div>
        </div>
      </div>
    </AdvisorySlot>
  );
}
