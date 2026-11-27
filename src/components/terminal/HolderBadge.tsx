/**
 * "not upgrade-safe" marker for a tab whose pane would not survive a runner
 * upgrade (plan
 * `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
 * D7): an in-process pane (holder off, or a holder that could not be started)
 * or a holder in the runner's own cgroup. Reads `terminal_holder_states` on
 * mount and every {@link POLL_MS}; renders nothing for an upgrade-safe pane,
 * a remote mirror, or a state it could not read.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ShieldAlert } from "lucide-react";

import type { CommandResponse } from "./types";
import { NOT_UPGRADE_SAFE_LABEL, notUpgradeSafeTooltip, type HolderState } from "./holderState";

/** A fallback pane may be rehomed into a holder; re-read on this cadence. */
const POLL_MS = 30_000;
/** Until the pane is in the registry (a tab can mount first), re-read sooner. */
export const FIRST_READ_RETRY_MS = 2_000;
/** Fast re-reads give up after this many; the {@link POLL_MS} poll goes on. */
export const FIRST_READ_RETRIES = 15;

export function HolderBadge({ terminalId }: { terminalId: string }) {
  // Keyed by the pane it was read for, so a new `terminalId` renders nothing
  // from its first paint rather than the last pane's state for one frame.
  const [read, setRead] = useState<{ id: string; state: HolderState | null } | undefined>(
    undefined,
  );
  const state = read?.id === terminalId ? read.state : undefined;
  useEffect(() => {
    let everRead = false;
    let cancelled = false;
    let retries = 0;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const readStates = async () => {
      try {
        const r = await invoke<CommandResponse>("terminal_holder_states");
        if (cancelled || !r.success || !r.data || typeof r.data !== "object") return;
        const states = r.data as Record<string, HolderState | null>;
        // A read that does not list this pane (not registered yet, or
        // already gone) says nothing about it: keep the last state, and
        // while none has been read, re-read soon -- one chain at a time.
        if (!(terminalId in states)) {
          if (!everRead && retry === undefined && retries < FIRST_READ_RETRIES) {
            retries += 1;
            retry = setTimeout(() => {
              retry = undefined;
              void readStates();
            }, FIRST_READ_RETRY_MS);
          }
          return;
        }
        everRead = true;
        setRead({ id: terminalId, state: states[terminalId] });
      } catch {
        // Unreadable is not "safe": keep whatever was last read, and show
        // nothing only when nothing ever was.
      }
    };
    void readStates();
    const timer = setInterval(() => void readStates(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      if (retry !== undefined) clearTimeout(retry);
    };
  }, [terminalId]);

  const tooltip = notUpgradeSafeTooltip(state);
  if (!tooltip) return null;
  return (
    <span
      className="flex items-center gap-0.5 min-w-0 overflow-hidden text-[8px] text-[#e0af68] bg-[#e0af68]/15 px-1 py-0 rounded"
      title={tooltip}
      aria-label={`Not upgrade-safe: ${tooltip}`}
      data-holder-state={state?.notUpgradeSafe?.reason ?? "unknown"}
    >
      <ShieldAlert className="w-2.5 h-2.5 shrink-0" />
      <span className="truncate">{NOT_UPGRADE_SAFE_LABEL}</span>
    </span>
  );
}
