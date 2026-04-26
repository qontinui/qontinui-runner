import { useState } from "react";
import { useBuildIdWatcher } from "@qontinui/ui-bridge/react";

// TODO: shared `useBuildIdWatcher` no longer accepts `getCurrentBuildId` /
// pollIntervalMs:0 (custom-getter mode was removed by 0251a9e in the
// ui-bridge package). Re-wire either by polling the runner's /health
// endpoint with a server-side {buildId} adapter or by re-adding the
// invoke source to the shared hook. Until then this banner is inert.
export function BuildRefreshBanner() {
  const [stale, setStale] = useState(false);
  useBuildIdWatcher({
    onBuildIdChange: () => setStale(true),
  });

  if (!stale) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        position: "fixed",
        bottom: 16,
        right: 16,
        zIndex: 9999,
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "10px 14px",
        background: "var(--bg-tertiary, #242837)",
        color: "var(--text-primary, #e4e4e7)",
        border: "1px solid var(--accent, #6366f1)",
        borderRadius: 8,
        boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
        fontSize: "0.875rem",
      }}
    >
      <span>New runner build detected — refresh to update</span>
      <button
        type="button"
        onClick={() => window.location.reload()}
        style={{
          background: "var(--accent, #6366f1)",
          color: "#fff",
          border: "none",
          borderRadius: 4,
          padding: "4px 10px",
          fontSize: "0.8125rem",
          fontWeight: 600,
          cursor: "pointer",
        }}
      >
        Refresh
      </button>
    </div>
  );
}
