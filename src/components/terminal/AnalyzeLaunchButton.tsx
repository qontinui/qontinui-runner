/**
 * The "Analyze launch" button (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 3), in the three
 * places it appears: the zone hover cluster beside Restart (`icon`), the zone
 * overflow menu (`menuitem`), and `LaunchFailedBanner` (`banner`).
 *
 * Every variant reads one per-pane status from {@link AnalyzeLaunchContext}:
 * the click disables the button at once with a spinner; a success reads
 * "Analyzer opened"; a failure re-enables it (the toast names the step).
 * Renders nothing outside a provider.
 */

import type { MouseEvent } from "react";
import { Loader2, Stethoscope } from "lucide-react";
import type { AnalyzeStatus } from "./analyzeLaunch";
import { useAnalyzeLaunchApi } from "./useAnalyzeLaunch";

export const ANALYZE_LAUNCH_BRIDGE_ID = "terminal.analyze-launch";

/** What a button shows for a status — exported for unit tests. */
export function analyzeButtonView(status: AnalyzeStatus | undefined): {
  disabled: boolean;
  spinner: boolean;
  label: string;
} {
  if (status === "running") return { disabled: true, spinner: true, label: "Analyzing launch…" };
  if (status === "opened") return { disabled: false, spinner: false, label: "Analyzer opened" };
  return { disabled: false, spinner: false, label: "Analyze launch" };
}

const TITLE =
  "Capture this pane's launch evidence and open an analyzer Claude session in a new pane, which retries the launch and records or fixes the cause";

export function AnalyzeLaunchButton({
  tabId,
  variant,
  className = "",
  onDone,
}: {
  tabId: string;
  variant: "icon" | "menuitem" | "banner";
  className?: string;
  /** Called after the click is handed off (the overflow menu closes itself). */
  onDone?: () => void;
}) {
  const api = useAnalyzeLaunchApi();
  if (!api) return null;
  const view = analyzeButtonView(api.statusOf(tabId));
  const Icon = view.spinner ? Loader2 : Stethoscope;
  const iconClass = `${variant === "banner" ? "w-2.5 h-2.5" : "w-3 h-3"} shrink-0${view.spinner ? " animate-spin" : ""}`;
  const onClick = (e: MouseEvent) => {
    e.stopPropagation();
    if (view.disabled) return;
    api.analyze(tabId);
    onDone?.();
  };
  const common = {
    type: "button" as const,
    "data-ui-bridge-id": ANALYZE_LAUNCH_BRIDGE_ID,
    "data-terminal-id": tabId,
    "data-analyze-status": api.statusOf(tabId) ?? "idle",
    onClick,
    disabled: view.disabled,
    "aria-disabled": view.disabled,
    "aria-busy": view.spinner,
    title: TITLE,
  };

  if (variant === "icon") {
    return (
      <button
        {...common}
        aria-label={view.label}
        className={`p-1 rounded transition-colors disabled:cursor-wait ${
          view.label === "Analyzer opened"
            ? "text-[#9ece6a] hover:bg-[#9ece6a]/10"
            : "text-[#e0af68] hover:text-[#ffc777] hover:bg-[#e0af68]/10"
        }${className}`}
      >
        <Icon className={iconClass} aria-hidden />
      </button>
    );
  }
  if (variant === "menuitem") {
    return (
      <button {...common} role="menuitem" className={className}>
        <Icon className={iconClass} aria-hidden />
        {view.label}
      </button>
    );
  }
  return (
    <button
      {...common}
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#e0af68]/40 text-[#e0af68] hover:bg-[#e0af68]/15 text-[10px] disabled:cursor-wait disabled:opacity-70 ${className}`}
    >
      <Icon className={iconClass} aria-hidden />
      {view.label}
    </button>
  );
}
