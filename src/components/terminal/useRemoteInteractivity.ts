import { useEffect, useState } from "react";
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
 * `null` means UNKNOWN — not polled yet, the pane is gone, or the command
 * failed — and the caller renders nothing rather than a guess. Pass
 * `terminalId: null` to disable polling (a local or dead tab).
 */
export function useRemoteInteractivity(
  terminalId: string | null,
): [RemoteInteractivitySnapshot | null, number] {
  const nowMs = useNow1Hz();
  const [snapshot, setSnapshot] = useState<RemoteInteractivitySnapshot | null>(null);

  useEffect(() => {
    if (!terminalId) {
      setSnapshot(null);
      return;
    }
    let cancelled = false;
    invoke<CommandResponse>("terminal_remote_interactivity", { terminalId })
      .then((r) => {
        if (cancelled) return;
        setSnapshot(r.success ? (r.data as RemoteInteractivitySnapshot) : null);
      })
      .catch(() => {
        if (!cancelled) setSnapshot(null);
      });
    return () => {
      cancelled = true;
    };
  }, [terminalId, nowMs]);

  return [snapshot, nowMs];
}
