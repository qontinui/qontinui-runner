import { useState } from "react";
import { useBuildIdWatcher } from "@qontinui/ui-bridge/react";
import { useApiBase } from "../lib/runner-api";

/**
 * Watches for a binary swap mid-session and renders a non-blocking refresh
 * banner when the runner exe behind the live webview no longer matches the
 * build the page was loaded from.
 *
 * Flow:
 *   1. `vite.config.ts` writes the build-id to `dist/build-id.txt` and
 *      bakes the same value into `index.html` as `<meta name="build-id">`.
 *   2. `build.rs` reads `dist/build-id.txt` and emits the same value as
 *      the `RUNNER_BUILD_ID` env baked into the binary; the `/health`
 *      handler exposes it at top-level `buildId`.
 *   3. `useBuildIdWatcher` reads the meta tag once on mount and polls
 *      `/health` for `{ buildId }`. On divergence (mid-session binary
 *      swap) it fires `onBuildIdChange` once.
 *   4. The banner offers a one-click reload that forces WebView2 to
 *      refetch the embedded index.html plus assets.
 *
 * Because Vite and cargo now read from the same on-disk source of truth,
 * a clean rebuild produces matching values — `onBuildIdChange` only fires
 * if someone swapped the exe behind the running webview.
 *
 * `useApiBase()` (rather than `getApiBase()`) is mandatory here: the
 * runner's actual port is resolved asynchronously via the `get_api_port`
 * Tauri command at startup; before that resolves, `getApiBase()` returns
 * the placeholder default `http://localhost:9876` (the *primary* runner's
 * port). On a temp/named runner that ran on a different port, the banner
 * would silently poll the primary's `/health` forever without re-subscribing
 * when the port arrived — which is exactly what manual testing surfaced.
 * `useApiBase` re-renders this component when `setApiPort` runs, the
 * `pollUrl` prop updates, and the watcher re-subscribes to the right URL.
 */
export function BuildRefreshBanner() {
  const apiBase = useApiBase();
  const [stale, setStale] = useState(false);
  useBuildIdWatcher({
    pollUrl: `${apiBase}/health`,
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
