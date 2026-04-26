import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Watches for a binary swap mid-session and renders a non-blocking refresh
 * banner when the runner exe behind the live webview no longer matches the
 * build the page was loaded from.
 *
 * Flow:
 *   1. `build.rs` bakes `RUNNER_BUILD_ID = <git-sha-short>-<unix-ms>` into
 *      the binary. `vite.config.ts` independently bakes the same format
 *      into `index.html` as `<meta name="build-id" content="...">`.
 *   2. On mount, reads the meta tag and compares against `invoke('get_build_id')`
 *      (exposes the binary's embedded `env!("RUNNER_BUILD_ID")`).
 *   3. Because Vite reruns before cargo on a real rebuild, both values
 *      diverge naturally on a binary swap; on a fresh intact install they match.
 */
export function BuildRefreshBanner() {
  const [stale, setStale] = useState(false);

  useEffect(() => {
    const pageBuildId =
      document.querySelector<HTMLMetaElement>('meta[name="build-id"]')?.content ?? null;
    if (!pageBuildId) return;
    invoke<string>("get_build_id")
      .then((binaryBuildId) => {
        if (binaryBuildId !== pageBuildId) setStale(true);
      })
      .catch(() => {});
  }, []);

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
