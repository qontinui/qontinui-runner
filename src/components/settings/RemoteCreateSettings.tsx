import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { TerminalSquare } from "lucide-react";
import { describeThrown } from "@/lib/utils";
import type { LogFunction } from "./types";
import {
  ACCEPT_REMOTE_CREATE_DEFAULT,
  ACCEPT_REMOTE_CREATE_OPTIONS,
  describeCoordMirror,
  isAcceptRemoteCreate,
  type AcceptRemoteCreate,
  type CoordMirror,
} from "./remoteCreateSettingsHelpers";

/**
 * `accept_remote_create` — whether OTHER machines in the tenant may start a
 * new terminal session on this one. Saved locally (the value this runner
 * enforces) and mirrored to coord's device row (the value coord's grant mint
 * reads); both must admit for a remote create to happen. Directory and
 * intent-repo allowlists stay in `settings.json` under `remote_create`.
 */

interface CommandResponse<T> {
  success: boolean;
  message: string | null;
  data: T | null;
}

interface PreferencePayload {
  accept_remote_create: string;
  coord_mirror?: CoordMirror;
}

export function RemoteCreateSettings({ onLog }: { onLog: LogFunction }) {
  const [value, setValue] = useState<AcceptRemoteCreate | null>(null);
  const [mirror, setMirror] = useState<CoordMirror | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  // Set once the user acts, so a slow initial load cannot overwrite their pick.
  const touched = useRef(false);

  const apply = useCallback((r: CommandResponse<PreferencePayload>) => {
    if (!r.success || !r.data) throw new Error(r.message ?? "the runner returned no preference");
    const v = r.data.accept_remote_create;
    if (!isAcceptRemoteCreate(v)) throw new Error(`unrecognised value ${JSON.stringify(v)}`);
    setValue(v);
    setMirror(r.data.coord_mirror ?? null);
    setLoadError(null);
  }, []);

  useEffect(() => {
    let cancelled = false;
    invoke<CommandResponse<PreferencePayload>>("remote_create_preference_get")
      .then((r) => {
        if (!cancelled && !touched.current) apply(r);
      })
      .catch((err) => {
        if (!cancelled && !touched.current)
          setLoadError(describeThrown(err, "the runner did not answer"));
      });
    return () => {
      cancelled = true;
    };
  }, [apply]);

  const save = useCallback(
    async (next: AcceptRemoteCreate) => {
      touched.current = true;
      const prev = value;
      const prevMirror = mirror;
      setBusy(true);
      setStatus(null);
      setValue(next);
      // The old verdict describes the old value; never show it beside the new one.
      setMirror(null);
      try {
        const r = await invoke<CommandResponse<PreferencePayload>>("remote_create_preference_set", {
          acceptRemoteCreate: next,
        });
        apply(r);
        const consistent = r.data?.coord_mirror?.consistent === true;
        setStatus(r.message ?? "Saved.");
        onLog(
          consistent ? "success" : "warning",
          `Remote create: ${next}${consistent ? "" : " (coord mirror not verified)"}`,
        );
      } catch (err) {
        setValue(prev);
        setMirror(prevMirror);
        const why = describeThrown(err, "save refused");
        setStatus(`Not saved: ${why}`);
        onLog("error", `Remote create preference save failed: ${why}`);
      } finally {
        setBusy(false);
      }
    },
    [apply, mirror, onLog, value],
  );

  const reconcile = useCallback(async () => {
    touched.current = true;
    setBusy(true);
    setStatus(null);
    try {
      const r = await invoke<CommandResponse<PreferencePayload>>(
        "remote_create_preference_reconcile",
      );
      apply(r);
      setStatus(r.message ?? null);
    } catch (err) {
      const why = describeThrown(err, "reconcile refused");
      setStatus(`Check failed: ${why}`);
      onLog("error", `Remote create coord check failed: ${why}`);
    } finally {
      setBusy(false);
    }
  }, [apply, onLog]);

  const mirrorLine = describeCoordMirror(mirror);
  const mirrorClass =
    mirrorLine.tone === "ok"
      ? "text-green-400"
      : mirrorLine.tone === "warn"
        ? "text-yellow-400"
        : "text-muted-foreground";

  return (
    <div className="space-y-3 rounded-lg bg-card/50 p-4" data-ui-bridge-id="settings.remote-create">
      <div className="font-medium text-sm flex items-center gap-2">
        <TerminalSquare className="w-4 h-4 text-primary" />
        Remote session creation
      </div>
      <p className="text-xs text-muted-foreground">
        Whether other machines in this tenant may start a new terminal session on this machine.
        Separate from remote attach: a create spawns a new process here. Every create also needs a
        coord-minted grant, and this runner refuses any create its own setting does not allow.
      </p>
      {loadError && (
        <p className="text-xs text-red-400" data-ui-bridge-id="settings.remote-create-load-error">
          Current value unknown — {loadError}. Picking a value below still saves it.
        </p>
      )}
      <div className="space-y-1.5" role="radiogroup" aria-label="Accept remote session creation">
        {ACCEPT_REMOTE_CREATE_OPTIONS.map((opt) => (
          <label
            key={opt.value}
            className="flex items-start gap-2 text-xs cursor-pointer"
            data-ui-bridge-id={`settings.remote-create-${opt.value}`}
          >
            <input
              type="radio"
              name="accept_remote_create"
              value={opt.value}
              checked={value === opt.value}
              disabled={busy}
              onChange={() => void save(opt.value)}
              className="mt-0.5"
            />
            <span>
              <span className="font-medium">
                {opt.label}
                {opt.value === ACCEPT_REMOTE_CREATE_DEFAULT ? " (default)" : ""}
              </span>
              <span className="text-muted-foreground"> — {opt.explain}</span>
            </span>
          </label>
        ))}
      </div>
      {value !== null && (
        <div className="flex items-start gap-2">
          <p
            className={`text-xs flex-1 ${mirrorClass}`}
            data-ui-bridge-id="settings.remote-create-mirror"
          >
            {busy ? "Checking coord…" : mirrorLine.text}
          </p>
          <button
            type="button"
            onClick={() => void reconcile()}
            disabled={busy}
            className="btn-secondary text-xs px-2 py-1"
            data-ui-bridge-id="settings.remote-create-reconcile"
          >
            Check coord
          </button>
        </div>
      )}
      {(busy || status) && (
        <p
          className="text-xs text-muted-foreground"
          data-ui-bridge-id="settings.remote-create-status"
        >
          {busy ? "Working…" : status}
        </p>
      )}
    </div>
  );
}
