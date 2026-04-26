import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useBuildIdWatcher } from "@qontinui/ui-bridge/react";

/**
 * Watches for a binary-swap-mid-session and renders a non-blocking refresh
 * banner when the runner exe behind the live webview no longer matches the
 * build the page was loaded from.
 *
 * Flow:
 *   1. `build.rs` bakes `RUNNER_BUILD_ID = <git-sha-short>-<unix-ms>` into
 *      the binary. `vite.config.ts` independently bakes the same format
 *      into `index.html` as `<meta name="build-id" content="...">`.
 *   2. `useBuildIdWatcher` reads the meta tag once on mount and compares it
 *      against the value returned by `invoke('get_build_id')` (which the
 *      Tauri command exposes from the embedded `env!("RUNNER_BUILD_ID")`).
 *   3. Because Vite reruns before cargo on a real rebuild, both values
 *      diverge naturally on a binary swap; on a fresh, intact install the
 *      values pair up and `onBuildIdChange` never fires.
 *   4. The banner offers a one-click reload that forces WebView2 to refetch
 *      the embedded index.html plus assets.
 *
 * `pollIntervalMs: 0` — a one-shot check on mount is enough. Mid-session
 * binary swap is the only divergence cause; nothing else changes the
 * compiled `RUNNER_BUILD_ID` while the webview stays open.
 *
 * Inline styles mirror the supervisor's `BuildRefreshBanner` so the two
 * surfaces feel the same to operators glancing across browser tabs.
 */
export function BuildRefreshBanner() {
  const [stale, setStale] = useState(false);
  useBuildIdWatcher({
    getCurrentBuildId: () => invoke<string>("get_build_id"),
    pollIntervalMs: 0,
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
