import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";
import type { RemoteInteractivitySnapshot } from "./remoteTabs";
import { useNow1Hz } from "./useNow1Hz";

/**
 * The remote pane's own interactivity observations for `terminalId`, re-read
 * every second (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * A1). Returns `[snapshot, nowMs]` so the caller renders ages against the same
 * tick that fetched them.
 *
 * Mount it only for a LIVE remote tab (it re-renders its host every second).
 *
 * `null` means UNKNOWN — not read yet, or the runner says the pane is gone
 * (`success: false`) — and the caller renders nothing rather than a guess. A
 * transient invoke failure keeps the last snapshot: its ages keep advancing,
 * so a stalled read shows up as growing ages rather than as a blank. A tick
 * that lands while the previous read is still in flight is skipped, so reads
 * never overlap or resolve out of order.
 */
export function useRemoteInteractivity(
  terminalId: string,
): [RemoteInteractivitySnapshot | null, number] {
  const nowMs = useNow1Hz();
  // Keyed by the terminal it was read for, so a stale read never renders
  // under a different tab id.
  const [read, setRead] = useState<{
    terminalId: string;
    snapshot: RemoteInteractivitySnapshot | null;
  } | null>(null);
  const inFlight = useRef(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    if (inFlight.current) return;
    inFlight.current = true;
    invoke<CommandResponse>("terminal_remote_interactivity", { terminalId })
      .then((r) => {
        if (!mounted.current) return;
        setRead({
          terminalId,
          snapshot: r.success ? (r.data as RemoteInteractivitySnapshot) : null,
        });
      })
      .catch(() => {
        // Transient: keep the last snapshot (see the doc comment).
      })
      .finally(() => {
        inFlight.current = false;
      });
  }, [terminalId, nowMs]);

  return [read && read.terminalId === terminalId ? read.snapshot : null, nowMs];
}
