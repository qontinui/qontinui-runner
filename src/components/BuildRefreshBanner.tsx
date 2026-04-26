import { useState } from "react";
import { useBuildIdWatcher } from "@qontinui/ui-bridge/react";
import { getApiBase } from "../lib/runner-api";

/**
 * Watches for a binary swap mid-session and renders a non-blocking refresh
 * banner when the runner exe behind the live webview no longer matches the
 * build the page was loaded from.
 *
 * Flow:
 *   1. `build.rs` bakes `RUNNER_BUILD_ID = <git-sha-short>-<unix-ms>` into
 *      the binary; `vite.config.ts` independently bakes the same format
 *      into `index.html` as `<meta name="build-id">`.
 *   2. The runner's `/health` handler exposes the compile-time value at
 *      `buildId` (top-level mirror of `data.buildId`).
 *   3. `useBuildIdWatcher` reads the meta tag once on mount and polls
 *      `/health` for `{ buildId }`. On divergence (mid-session binary
 *      swap) it fires `onBuildIdChange` once.
 *   4. The banner offers a one-click reload that forces WebView2 to
 *      refetch the embedded index.html plus assets.
 *
 * Vite reruns before cargo on a real rebuild, so the meta tag and the
 * binary's RUNNER_BUILD_ID always pair up on a fresh, intact install —
 * `onBuildIdChange` never fires unless someone swapped the exe behind
 * the running webview.
 */
export function BuildRefreshBanner() {
  const [stale, setStale] = useState(false);
  useBuildIdWatcher({
    pollUrl: `${getApiBase()}/health`,
    pollIntervalMs: 30_000,
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
