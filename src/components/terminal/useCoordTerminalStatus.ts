/**
 * Per-tab coord-mcp availability — the Terminal page's title-bar dot.
 *
 * Each Claude-bound terminal (`tab.claudeSessionId` present) gets its own
 * coord-mcp proxy nonce at spawn time (`NonceBinding::terminal_id` on the
 * Rust side), and that nonce's credential picture can differ per terminal —
 * two terminals acting as different tenants can disagree about whether
 * coord is reachable at all. `GET /coord-mcp/terminal-status` answers one
 * light per terminal id from purely local, no-network state (the live nonce
 * registry plus the credential store), mirroring exactly what the coord-mcp
 * proxy itself would decide for that terminal's next call. See
 * `coord_mcp::terminal_status` on the Rust side for the full rationale.
 *
 * Plain (non-Claude) shell tabs never get a nonce and are excluded from
 * polling entirely — rendering `red` for a bash tab that never asked for
 * coord would be noise, not signal.
 */

import { useState, useEffect, useRef } from "react";
import { resolvePort } from "@/lib/runner-api";
import type { TerminalTab } from "./useTerminalManager";

/** Polling interval for `/coord-mcp/terminal-status` (ms). The endpoint is
 *  local-only (no network call of its own), so this is about UI freshness,
 *  not request cost. */
export const COORD_TERMINAL_STATUS_POLL_MS = 5000;

export type CoordLight = "green" | "yellow" | "red";

/** Wire shape of one entry in `GET /coord-mcp/terminal-status`'s `statuses`
 *  map (`coord_mcp::terminal_status::TerminalCoordStatus` on the Rust
 *  side — field names are verbatim, no camelCase rename). */
export interface TerminalCoordStatus {
  light: CoordLight;
  /** `ok` / `degraded` / `unknown` / `no-credential` / `refuses` /
   *  `no-nonce` — the raw reason behind `light`, for a tooltip. */
  verdict: string;
  detail: string;
  has_nonce: boolean;
}

interface TerminalStatusResponse {
  statuses?: Record<string, TerminalCoordStatus>;
}

/**
 * Hook: poll `/coord-mcp/terminal-status` for every Claude-bound tab and
 * return `Record<tabId, TerminalCoordStatus>`. Tabs without a
 * `claudeSessionId` (plain shells) are absent from the map — callers should
 * treat absence as "no dot", not as an unavailability verdict.
 *
 * Implementation mirrors {@link useRegistryAwareness}: a `tabsRef` keeps the
 * interval callback reading the latest tabs without re-installing it on
 * every identity change, fetch errors drop that poll's entries rather than
 * throwing, and the interval is cleared on unmount.
 */
export function useCoordTerminalStatus(
  tabs: TerminalTab[],
): Record<string, TerminalCoordStatus> {
  const [statuses, setStatuses] = useState<Record<string, TerminalCoordStatus>>({});
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  useEffect(() => {
    let active = true;

    const poll = async () => {
      const ids = tabsRef.current
        .filter((t) => !!t.claudeSessionId)
        .map((t) => t.id);
      if (ids.length === 0) {
        if (active) setStatuses({});
        return;
      }
      try {
        // Resolve the port per-poll, not once at mount — on secondary
        // runners `useApiReady` populates it asynchronously, so a hook
        // mounted before the `api-ready` event would otherwise keep
        // polling the primary forever (same reasoning as
        // `useRegistryAwareness`).
        const url = `http://127.0.0.1:${resolvePort()}/coord-mcp/terminal-status?ids=${ids
          .map((id) => encodeURIComponent(id))
          .join(",")}`;
        const resp = await fetch(url);
        if (!resp.ok) return;
        const body = (await resp.json()) as TerminalStatusResponse;
        if (!active) return;
        setStatuses(body.statuses ?? {});
      } catch {
        // Swallow — a probe failure just leaves the previous lights up
        // rather than flashing every dot to an error state.
      }
    };

    void poll();
    const interval = setInterval(() => {
      void poll();
    }, COORD_TERMINAL_STATUS_POLL_MS);

    return () => {
      active = false;
      clearInterval(interval);
    };
  }, []);

  return statuses;
}
