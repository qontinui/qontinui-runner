import React from "react";
import { AlertCircle, AlertTriangle, Info, X } from "lucide-react";
import { getSeverityColors } from "@/design-system";
import {
  dismissLabel,
  residualLabel,
  type ErrorEventPayload,
} from "./errorBannerState";

function severityClasses(severity: string): string {
  const colors = getSeverityColors(severity);
  return `${colors.bg} ${colors.text} ${colors.border}`;
}

function SeverityIcon({ severity, className }: { severity: string; className: string }) {
  switch (severity) {
    case "warning":
      return <AlertTriangle className={className} />;
    case "error":
    case "critical":
      return <AlertCircle className={className} />;
    default:
      return <Info className={className} />;
  }
}

/**
 * The app-wide `error` event, rendered IN FLOW as a full-width row under the
 * status bar. It takes its own height and pushes the page down; it never floats
 * over a working pane (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls` F1 — the
 * old `fixed top-20 right-4 z-40` card sat on top of the rightmost terminal zone
 * and its hover toolbar).
 */
export function ErrorBanner({
  error,
  onDismiss,
}: {
  error: ErrorEventPayload;
  onDismiss: () => void;
}) {
  const label = dismissLabel(error);
  return (
    <div
      role="alert"
      data-ui-bridge-id="app-error-banner"
      data-content-role="status"
      data-content-label="app error"
      className={`flex items-start gap-3 px-4 py-2 border-b ${severityClasses(error.severity)}`}
    >
      <div className="shrink-0 mt-0.5">
        <SeverityIcon severity={error.severity} className="w-5 h-5" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline gap-3 flex-wrap">
          <h3 className="font-semibold">{error.title}</h3>
          <p className="text-sm">{error.message}</p>
        </div>
        {error.details && (
          <details className="mt-1">
            <summary className="text-xs cursor-pointer hover:underline">Technical Details</summary>
            <pre className="mt-1 text-xs bg-black/10 p-2 rounded overflow-x-auto max-h-48">
              {error.details}
            </pre>
          </details>
        )}
        {error.suggested_action && (
          <p className="mt-1 text-sm font-medium">{error.suggested_action}</p>
        )}
        <p className="mt-1 text-xs opacity-60">Error Code: {error.error_code}</p>
      </div>
      <button
        type="button"
        onClick={onDismiss}
        aria-label={label}
        title={label}
        data-ui-bridge-id="app-error-banner-dismiss"
        className="shrink-0 ml-2 p-1 hover:bg-black/10 rounded"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}

/**
 * What a dismissed fault collapses to: a compact chip in the status bar that
 * names the code and when the fault was last observed, and re-opens the banner
 * on click. It goes away only when an `error-cleared` event for the code is
 * observed — dismissing never makes a live fault vanish (F2).
 */
export function ErrorResidual({
  error,
  lastSeenAt,
  now,
  onReopen,
}: {
  error: ErrorEventPayload;
  lastSeenAt: number;
  now: number;
  onReopen: () => void;
}) {
  const text = residualLabel(error, lastSeenAt, now);
  return (
    <button
      type="button"
      onClick={onReopen}
      aria-label={`Show ${error.title} (${text})`}
      title={`${error.title} — click to show`}
      data-ui-bridge-id="app-error-residual"
      data-content-role="status"
      data-content-label="dismissed app error"
      className={`flex items-center gap-1.5 px-2 py-0.5 rounded-full border text-xs font-medium ${severityClasses(error.severity)}`}
    >
      <SeverityIcon severity={error.severity} className="w-3.5 h-3.5" />
      <span className="truncate max-w-[260px]">{text}</span>
    </button>
  );
}
