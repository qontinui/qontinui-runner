import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Monitor, RefreshCw, X } from "lucide-react";
import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { useSessionPersistence, type SavedSessionConfig } from "./useSessionPersistence";
import {
  attachErrorMessage,
  savedRemoteSessionsToRestore,
  sessionLabelFromTitle,
  type RemoteTabIdentity,
  type RemoteTerminalInfoWire,
} from "./remoteTabs";

/**
 * After a restart, the remote tabs this page had are offered here as
 * placeholders with a Reattach action — never re-created as terminals (plan
 * `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 4).
 *
 * Why a card and not an automatic reattach: a runner restart voids every
 * attach grant, the target may be offline, and minting one grant per saved tab
 * at boot would fail loudly for each dark device with nothing the operator
 * asked for. Why not a tab: feeding a saved remote entry to `terminal_create`
 * (what the local restore does) would spawn a LOCAL shell under a remote
 * title — the silent-respawn the plan forbids. So the saved identity is
 * listed, one click each, with the outcome shown inline.
 *
 * Lives in the same top-right advisory column as `ResumeFailedBanner`.
 */
export function RemoteRestoreBanner() {
  const { tabs, pageId, setActiveId } = useTerminalSession();
  const persistence = useSessionPersistence(pageId);
  const [saved, setSaved] = useState<SavedSessionConfig[]>([]);
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});

  // Read the snapshot ONCE per page mount: what this page had before the
  // restart. Later saves (which the live tabs drive) must not re-populate it.
  useEffect(() => {
    setSaved(persistence.hasSavedLayout() ? (persistence.getSavedLayout()?.sessions ?? []) : []);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pageId]);

  const pending = useMemo(
    () =>
      savedRemoteSessionsToRestore(saved, tabs).filter(
        (p) => !dismissed.has(`${p.remote.deviceId}/${p.remote.sessionId}`),
      ),
    [saved, tabs, dismissed],
  );

  const key = (r: RemoteTabIdentity) => `${r.deviceId}/${r.sessionId}`;

  const reattach = useCallback(
    async (remote: RemoteTabIdentity, title: string) => {
      const k = key(remote);
      setBusy(k);
      setErrors((e) => ({ ...e, [k]: "" }));
      try {
        const info = await invoke<RemoteTerminalInfoWire>("terminal_attach_remote", {
          deviceId: remote.deviceId,
          sessionId: remote.sessionId,
          deviceLabel: remote.deviceLabel,
          sessionLabel: sessionLabelFromTitle(title, remote.deviceLabel),
          workingDir: null,
          pageId: pageId !== "default" ? pageId : null,
        });
        setActiveId(info.id);
        // The live tab now carries this identity, so `pending` drops it.
      } catch (err) {
        setErrors((e) => ({ ...e, [k]: attachErrorMessage(err) }));
      } finally {
        setBusy(null);
      }
    },
    [pageId, setActiveId],
  );

  if (pending.length === 0) return null;

  return (
    <div
      data-ui-bridge-id="terminal.remote-restore-banner"
      className="rounded border border-[#7aa2f7]/40 bg-[#1a1b26]/95 px-3 py-2 text-[11px] text-[#c0caf5] shadow-lg max-w-sm"
    >
      <div className="flex items-center gap-1.5 mb-1">
        <Monitor className="w-3 h-3 text-[#7aa2f7]" />
        <span className="font-medium">
          {pending.length} remote tab{pending.length !== 1 ? "s" : ""} from before the restart
        </span>
      </div>
      <p className="text-[10px] text-[#565f89] mb-1.5">
        Remote tabs are not reopened automatically — a restart voids their grants. Reattach mints a
        fresh grant for the same session; the tab returns when the target answers.
      </p>
      <ul className="space-y-1">
        {pending.map(({ remote, title }) => {
          const k = key(remote);
          const err = errors[k];
          return (
            <li
              key={k}
              data-ui-bridge-id={`terminal.remote-restore.${remote.sessionId}`}
              className="flex flex-col gap-0.5"
            >
              <div className="flex items-center gap-1.5">
                <span
                  className="truncate flex-1"
                  title={`${remote.deviceId} · ${remote.sessionId}`}
                >
                  {title}
                </span>
                <button
                  type="button"
                  data-ui-bridge-id={`terminal.remote-restore-reattach.${remote.sessionId}`}
                  onClick={() => void reattach(remote, title)}
                  disabled={busy !== null}
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-50 transition-colors"
                >
                  <RefreshCw className={`w-2.5 h-2.5 ${busy === k ? "animate-spin" : ""}`} />
                  {busy === k ? "reattaching…" : "Reattach"}
                </button>
                <button
                  type="button"
                  data-ui-bridge-id={`terminal.remote-restore-dismiss.${remote.sessionId}`}
                  onClick={() => setDismissed((d) => new Set([...d, k]))}
                  disabled={busy === k}
                  className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
                  title="Forget this remote tab for now (it stays in the fleet picker)"
                >
                  <X className="w-3 h-3" />
                </button>
              </div>
              {err && <span className="text-[10px] text-[#f7768e] break-words">{err}</span>}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
