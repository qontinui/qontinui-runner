import { useCallback, useEffect, useState } from "react";
import { Bot } from "lucide-react";
import { resolvePort } from "@/lib/runner-api";

/**
 * One-click start/stop for the single `/merge-train-steward` session
 * (plan `2026-07-19-runner-steward-button-and-pr-shepherd-retirement.md`).
 *
 * Rendered in `SessionManagerPanel.tsx`, which IS mounted in the live
 * component tree (`TerminalPage.tsx` — `{workflowGen.showSidebar &&
 * <SessionManagerPanel .../>}`). An earlier iteration placed this control in
 * `LaunchMenu.tsx`, which compiles and has tests but is not actually rendered
 * anywhere in the app (only self-referenced by its own test file) — moved
 * here so a human operator can actually see and click it.
 *
 * The backend (`mcp/steward.rs`) spawns the PTY server-side and emits the
 * same `terminal-created` event any other server-created terminal does, so
 * once `POST /steward/start` returns, the new tab appears via the existing
 * `useTerminalManager` ingest listener — no separate assign/focus plumbing
 * needed here.
 *
 * Self-contained: owns its own fetch/poll state rather than threading new
 * props through `SessionManagerHeader` (a purely presentational component
 * driven entirely by `useSessionManager` today) — mirrors how
 * `WaitForReleasePanel` in `LaunchMenu.tsx` is a self-contained subcomponent
 * with its own effects.
 */

/** Polling interval while the panel is mounted (ms). */
export const STEWARD_POLL_INTERVAL_MS = 2000;

/** Shape of `GET /steward/status`'s `data` payload. */
export interface StewardStatus {
  running: boolean;
  session_id?: string;
  mode?: string;
  interval?: string;
  started_at?: number;
}

interface StewardApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export function buildStewardUrl(port: number, path: "status" | "start" | "stop"): string {
  return `http://127.0.0.1:${port}/steward/${path}`;
}

export function StewardControl() {
  const [status, setStatus] = useState<StewardStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const fetchStatus = useCallback(async (): Promise<void> => {
    try {
      const port = resolvePort();
      const resp = await fetch(buildStewardUrl(port, "status"));
      if (!resp.ok) return;
      const json = (await resp.json()) as StewardApiResponse<StewardStatus>;
      if (json.success && json.data) setStatus(json.data);
    } catch {
      // Network error — leave the existing status in place so the control
      // doesn't flicker when the runner is briefly busy.
    }
  }, []);

  // The state-updating call is only ever invoked from a timer callback,
  // never synchronously in the effect body (avoids
  // react-hooks/set-state-in-effect). The zero-delay `setTimeout` gives an
  // effectively-immediate first fetch.
  useEffect(() => {
    const tick = () => void fetchStatus();
    const immediate = setTimeout(tick, 0);
    const interval = setInterval(tick, STEWARD_POLL_INTERVAL_MS);
    return () => {
      clearTimeout(immediate);
      clearInterval(interval);
    };
  }, [fetchStatus]);

  const handleStart = async (): Promise<void> => {
    setBusy(true);
    try {
      const port = resolvePort();
      await fetch(buildStewardUrl(port, "start"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      });
    } catch {
      // Swallow — the next status poll reflects reality either way.
    } finally {
      setBusy(false);
      void fetchStatus();
    }
  };

  const handleStop = async (): Promise<void> => {
    setBusy(true);
    try {
      const port = resolvePort();
      await fetch(buildStewardUrl(port, "stop"), { method: "POST" });
    } catch {
      // Swallow — the next status poll reflects reality either way.
    } finally {
      setBusy(false);
      void fetchStatus();
    }
  };

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d] bg-[#13141f]">
      {status?.running ? (
        <>
          <span
            data-testid="steward-running-dot"
            className="w-2 h-2 rounded-full bg-[#9ece6a] shrink-0"
          />
          <span className="flex-1 text-[11px] text-[#c0caf5]" data-testid="steward-status-label">
            Merge-train steward: Running
            {status.mode && (
              <span className="ml-1.5 text-[10px] text-[#565f89]">({status.mode})</span>
            )}
          </span>
          <button
            type="button"
            data-testid="steward-stop-button"
            disabled={busy}
            onClick={() => void handleStop()}
            className="px-1.5 py-0.5 rounded text-[10px] font-mono bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 transition-colors disabled:opacity-50 shrink-0"
          >
            Stop
          </button>
        </>
      ) : (
        <button
          type="button"
          data-testid="steward-start-button"
          disabled={busy}
          onClick={() => void handleStart()}
          className="w-full flex items-center gap-2 text-[11px] text-[#c0caf5] hover:text-[#7aa2f7] transition-colors disabled:opacity-50"
        >
          <Bot className="w-3.5 h-3.5 text-[#565f89]" />
          <span>Start merge-train steward</span>
        </button>
      )}
    </div>
  );
}
