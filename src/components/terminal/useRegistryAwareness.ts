/**
 * Per-tab "registry awareness" hook.
 *
 * For each terminal tab with a non-empty `workingDir`, polls the
 * runner's existing `/file-registry/probe-conflicts` endpoint every
 * 2000ms and counts how many `live_holdings` are:
 *   - held by OTHER sessions (`holder_name !== tab.title`), and
 *   - located under this tab's `workingDir` (path-prefix match with
 *     boundary check — see {@link computeOverlapCount}).
 *
 * Returns `Record<tabId, count>`; tabs without a `workingDir` are
 * absent from the map (callers should treat that as zero / no badge).
 *
 * Phase 2 of `pty-launched-ai-tabs-warning-plan.md`. Drives the small
 * yellow pill badge near the lock-state indicator in
 * {@link TerminalTabBar}.
 */

import { useState, useEffect, useRef } from "react";
import { resolvePort } from "@/lib/runner-api";
import type { TerminalTab } from "./useTerminalManager";
import type {
  ConflictReport,
  FileRegistryInfoEntry,
} from "./useSessionManager";

/** Polling interval for the probe-conflicts endpoint (ms). */
export const REGISTRY_AWARENESS_POLL_MS = 2000;

/**
 * Normalize a path the same way the runner's backend does
 * (`executor/file_registry.rs::normalize_path`): `\` → `/`, then
 * lowercase on Windows. Without this, the prefix-match in
 * {@link computeOverlapCount} will silently fail on Windows because
 * `tab.workingDir` carries the spawn-cwd verbatim (`D:\qontinui-root`)
 * while `live_holdings[].file_path` carries the backend-normalized
 * form (`d:/qontinui-root/...`).
 *
 * Windows detection: `navigator.platform` starts with `"Win"` in the
 * Tauri webview. `typeof navigator === "undefined"` covers SSR/node
 * test environments (falls back to no lowercasing — matches Unix).
 */
function normalizePath(p: string): string {
  const slashed = p.replace(/\\/g, "/");
  const isWindows =
    typeof navigator !== "undefined" && navigator.platform.startsWith("Win");
  return isWindows ? slashed.toLowerCase() : slashed;
}

/**
 * Count files in `liveHoldings` that are:
 *   1. held by a session other than `tab` (`holder_name !== tab.title`),
 *   2. located under `tab.workingDir` (prefix match with path boundary).
 *
 * Both `tab.workingDir` and `h.file_path` are normalized via
 * {@link normalizePath} before comparison so case/slash differences
 * between Tauri's spawn-cwd format and the backend-normalized
 * file-registry keys don't cause silent misses on Windows.
 *
 * Boundary check: a plain `startsWith` would treat
 * `"/repository/foo.rs".startsWith("/repo")` as true, conflating
 * sibling-directory paths. We require either exact equality OR the
 * separator `"/"` immediately after the prefix.
 *
 * Returns 0 when `tab.workingDir` is undefined or empty (otherwise
 * every path would match the empty prefix).
 */
export function computeOverlapCount(
  liveHoldings: readonly FileRegistryInfoEntry[],
  tab: { id: string; title: string; workingDir?: string },
): number {
  const cwd = tab.workingDir;
  if (!cwd) return 0;
  const cwdN = normalizePath(cwd);
  let count = 0;
  for (const h of liveHoldings) {
    if (h.holder_name === tab.title) continue;
    const p = normalizePath(h.file_path);
    if (p === cwdN || p.startsWith(cwdN + "/")) {
      count += 1;
    }
  }
  return count;
}

/**
 * Hook: poll `/file-registry/probe-conflicts` per tab on a 2s
 * interval and return per-tab overlap counts.
 *
 * Implementation mirrors {@link useFileLockTracking}:
 *   - `tabsRef` keeps the interval callback reading the latest tabs
 *     without re-installing the interval on every identity change.
 *   - Fetch errors are swallowed (count drops to 0 for that tab).
 *   - The interval is cleared on unmount.
 */
export function useRegistryAwareness(
  tabs: TerminalTab[],
): Record<string, number> {
  const [counts, setCounts] = useState<Record<string, number>>({});
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  useEffect(() => {
    let active = true;

    const probeTab = async (
      tab: TerminalTab,
    ): Promise<[string, number]> => {
      if (!tab.workingDir) return [tab.id, 0];
      try {
        // Resolve the port per-poll (not once at mount): on secondary
        // runners `useApiReady` populates the port asynchronously via
        // the `api-ready` Tauri event, so a hook that mounted before
        // the event would otherwise keep polling the primary forever.
        // Re-reading here catches up by the second tick.
        const url = `http://127.0.0.1:${resolvePort()}/file-registry/probe-conflicts`;
        const resp = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ prompt: null, cwd: tab.workingDir }),
        });
        if (!resp.ok) return [tab.id, 0];
        const report = (await resp.json()) as ConflictReport;
        const n = computeOverlapCount(report.live_holdings ?? [], tab);
        return [tab.id, n];
      } catch {
        // Swallow — probe failures are not user-facing.
        return [tab.id, 0];
      }
    };

    const poll = async () => {
      const current = tabsRef.current;
      const eligible = current.filter((t) => !!t.workingDir);
      if (eligible.length === 0) {
        if (active) setCounts({});
        return;
      }
      const results = await Promise.all(eligible.map(probeTab));
      if (!active) return;
      const next: Record<string, number> = {};
      for (const [id, n] of results) {
        next[id] = n;
      }
      setCounts(next);
    };

    void poll();
    const interval = setInterval(() => {
      void poll();
    }, REGISTRY_AWARENESS_POLL_MS);

    return () => {
      active = false;
      clearInterval(interval);
    };
  }, []);

  return counts;
}
