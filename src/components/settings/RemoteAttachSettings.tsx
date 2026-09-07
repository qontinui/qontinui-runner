import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Monitor } from "lucide-react";
import type { LogFunction } from "./types";

/**
 * `accept_remote_attach` — whether OTHER machines in the tenant may open a tab
 * onto a session running here (plan
 * `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c). The
 * preference is the OFF switch; every attach is additionally gated by a
 * coord-minted per-session grant that this runner verifies before any
 * keystroke reaches a PTY. Saved locally and mirrored to coord's device row,
 * which is what coord's grant mint consults.
 */
export type AcceptRemoteAttach = "same_user" | "tenant" | "off";

export const ACCEPT_REMOTE_ATTACH_DEFAULT: AcceptRemoteAttach = "same_user";

export const ACCEPT_REMOTE_ATTACH_OPTIONS: ReadonlyArray<{
  value: AcceptRemoteAttach;
  label: string;
  /** One line, no hedging: what this value lets happen. */
  explain: string;
}> = [
  {
    value: "same_user",
    label: "Same user",
    explain: "Only devices signed in as the same user as this one may attach — the default.",
  },
  {
    value: "tenant",
    label: "Whole tenant",
    explain: "Any device in this tenant may attach, still one coord-minted grant per session.",
  },
  {
    value: "off",
    label: "Off",
    explain: "No remote attach at all; coord refuses to mint a grant for sessions on this device.",
  },
];

interface CommandResponse<T> {
  success: boolean;
  message: string | null;
  data: T | null;
}

interface PreferencePayload {
  accept_remote_attach: AcceptRemoteAttach;
  mirrored?: boolean;
  mirror_error?: string | null;
}

export function RemoteAttachSettings({ onLog }: { onLog: LogFunction }) {
  const [value, setValue] = useState<AcceptRemoteAttach | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<CommandResponse<PreferencePayload>>("remote_attach_preference_get")
      .then((r) => {
        if (cancelled) return;
        if (r.success && r.data) setValue(r.data.accept_remote_attach);
        else setLoadError(r.message ?? "the runner returned no preference");
      })
      .catch((err) => {
        if (!cancelled) setLoadError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const save = useCallback(
    async (next: AcceptRemoteAttach) => {
      const prev = value;
      setSaving(true);
      setStatus(null);
      setValue(next);
      try {
        const r = await invoke<CommandResponse<PreferencePayload>>("remote_attach_preference_set", {
          acceptRemoteAttach: next,
        });
        if (!r.success) throw new Error(r.message ?? "save refused");
        const mirrored = r.data?.mirrored !== false;
        setStatus(
          mirrored
            ? "Saved and mirrored to coord."
            : `Saved locally; coord mirror failed (${r.data?.mirror_error ?? "unknown"}) — retried on the next relay connect. Until then coord may mint against the previous value.`,
        );
        onLog(
          mirrored ? "success" : "warning",
          `Remote attach: ${next}${mirrored ? "" : " (mirror pending)"}`,
        );
      } catch (err) {
        setValue(prev);
        setStatus(`Not saved: ${String(err)}`);
        onLog("error", `Remote attach preference save failed: ${err}`);
      } finally {
        setSaving(false);
      }
    },
    [onLog, value],
  );

  return (
    <div className="space-y-3 rounded-lg bg-card/50 p-4" data-ui-bridge-id="settings.remote-attach">
      <div className="font-medium text-sm flex items-center gap-2">
        <Monitor className="w-4 h-4 text-primary" />
        Remote attach
      </div>
      <p className="text-xs text-muted-foreground">
        Whether other machines in this tenant may open a tab onto a terminal session running here.
        Every attach also needs a per-session grant minted by coord and checked by this runner
        before any keystroke lands; this setting is the off switch, not the safeguard.
      </p>
      {loadError && (
        <p className="text-xs text-red-400" data-ui-bridge-id="settings.remote-attach-load-error">
          Current value unknown — {loadError}. Picking a value below still saves it.
        </p>
      )}
      <div className="space-y-1.5" role="radiogroup" aria-label="Accept remote attach">
        {ACCEPT_REMOTE_ATTACH_OPTIONS.map((opt) => (
          <label
            key={opt.value}
            className="flex items-start gap-2 text-xs cursor-pointer"
            data-ui-bridge-id={`settings.remote-attach-${opt.value}`}
          >
            <input
              type="radio"
              name="accept_remote_attach"
              value={opt.value}
              checked={value === opt.value}
              disabled={saving}
              onChange={() => void save(opt.value)}
              className="mt-0.5"
            />
            <span>
              <span className="font-medium">
                {opt.label}
                {opt.value === ACCEPT_REMOTE_ATTACH_DEFAULT ? " (default)" : ""}
              </span>
              <span className="text-muted-foreground"> — {opt.explain}</span>
            </span>
          </label>
        ))}
      </div>
      {(saving || status) && (
        <p
          className="text-xs text-muted-foreground"
          data-ui-bridge-id="settings.remote-attach-status"
        >
          {saving ? "Saving…" : status}
        </p>
      )}
    </div>
  );
}
