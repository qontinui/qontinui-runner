import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { DoorClosed } from "lucide-react";
import type { LogFunction } from "./types";

/**
 * `sessions.finished_session_close` — whether the runner closes a FINISHED
 * terminal session's window by itself (plan
 * `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`,
 * D3). The wind-down executor's `finished_close` arm graceful-`/exit`s a
 * session that was marked finished, has sat idle past the grace period, and
 * whose every worktree is clean and pushed — no drain needed. Because it acts
 * on the user's behalf, its off switch lives here and not only in the
 * environment. The transcript survives the close.
 *
 * `QONTINUI_FINISHED_SESSION_CLOSE=0` in the runner's environment is a machine
 * kill switch that wins over this value; the panel shows when it is engaged.
 */
export type FinishedSessionClose = "on" | "shadow" | "off";

export const FINISHED_SESSION_CLOSE_DEFAULT: FinishedSessionClose = "on";

export const FINISHED_SESSION_CLOSE_OPTIONS: ReadonlyArray<{
  value: FinishedSessionClose;
  label: string;
  /** One line, no hedging: what this value lets happen. */
  explain: string;
}> = [
  {
    value: "on",
    label: "On",
    explain:
      "A finished session idle past the grace period, with every worktree clean and pushed, is closed — the default.",
  },
  {
    value: "shadow",
    label: "Shadow",
    explain: "Every check runs and the runner logs which sessions it would close, but closes none.",
  },
  {
    value: "off",
    label: "Off",
    explain: "Finished sessions stay open until you close them, or until a drain winds them down.",
  },
];

interface CommandResponse<T> {
  success: boolean;
  message: string | null;
  data: T | null;
}

interface FinishedSessionClosePayload {
  finished_session_close: FinishedSessionClose;
  effective: FinishedSessionClose;
  kill_switch_engaged: boolean;
  kill_switch_env: string;
}

export function FinishedSessionCloseSettings({ onLog }: { onLog: LogFunction }) {
  const [payload, setPayload] = useState<FinishedSessionClosePayload | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<CommandResponse<FinishedSessionClosePayload>>("finished_session_close_get")
      .then((r) => {
        if (cancelled) return;
        if (r.success && r.data) setPayload(r.data);
        else setLoadError(r.message ?? "the runner returned no value");
      })
      .catch((err) => {
        if (!cancelled) setLoadError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const save = useCallback(
    async (next: FinishedSessionClose) => {
      const prev = payload;
      setSaving(true);
      setStatus(null);
      if (prev) setPayload({ ...prev, finished_session_close: next });
      try {
        const r = await invoke<CommandResponse<FinishedSessionClosePayload>>(
          "finished_session_close_set",
          { finishedSessionClose: next },
        );
        if (!r.success || !r.data) throw new Error(r.message ?? "save refused");
        setPayload(r.data);
        setStatus(r.message ?? "Saved.");
        onLog("success", `Close finished sessions: ${next}`);
      } catch (err) {
        setPayload(prev);
        setStatus(`Not saved: ${String(err)}`);
        onLog("error", `Close-finished-sessions save failed: ${err}`);
      } finally {
        setSaving(false);
      }
    },
    [onLog, payload],
  );

  const value = payload?.finished_session_close ?? null;

  return (
    <div
      className="space-y-3 rounded-lg bg-card/50 p-4"
      data-ui-bridge-id="settings.finished-session-close"
    >
      <h4 className="font-medium text-sm flex items-center gap-2">
        <DoorClosed className="w-4 h-4" />
        Sessions
      </h4>
      <div className="text-sm font-medium">Close finished sessions after grace</div>
      <p className="text-xs text-muted-foreground">
        When a session is marked finished, sits idle past the grace period, and every worktree it
        touched is committed and pushed, the runner exits it with <code>/exit</code> and closes its
        window. A session with uncommitted, untracked or unpushed work — or one whose state cannot
        be read — is never closed. The transcript is kept.
      </p>
      {loadError && (
        <p
          className="text-xs text-red-400"
          data-ui-bridge-id="settings.finished-session-close-load-error"
        >
          Current value unknown — {loadError}. Picking a value below still saves it.
        </p>
      )}
      {payload?.kill_switch_engaged && (
        <p
          className="text-xs text-amber-400"
          data-ui-bridge-id="settings.finished-session-close-kill-switch"
        >
          {payload.kill_switch_env}=0 is set in this runner&apos;s environment, so the runner is not
          closing finished sessions whatever is selected here.
        </p>
      )}
      <div className="space-y-1.5" role="radiogroup" aria-label="Close finished sessions">
        {FINISHED_SESSION_CLOSE_OPTIONS.map((opt) => (
          <label
            key={opt.value}
            className="flex items-start gap-2 text-xs cursor-pointer"
            data-ui-bridge-id={`settings.finished-session-close-${opt.value}`}
          >
            <input
              type="radio"
              name="finished_session_close"
              value={opt.value}
              checked={value === opt.value}
              disabled={saving}
              onChange={() => void save(opt.value)}
              className="mt-0.5"
            />
            <span>
              <span className="font-medium">
                {opt.label}
                {opt.value === FINISHED_SESSION_CLOSE_DEFAULT ? " (default)" : ""}
              </span>
              <span className="text-muted-foreground"> — {opt.explain}</span>
            </span>
          </label>
        ))}
      </div>
      {(saving || status) && (
        <p
          className="text-xs text-muted-foreground"
          data-ui-bridge-id="settings.finished-session-close-status"
        >
          {saving ? "Saving…" : status}
        </p>
      )}
    </div>
  );
}
