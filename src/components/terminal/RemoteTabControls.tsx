import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { History, Monitor, RefreshCw } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { CommandResponse } from "./types";
import { useTerminalSession } from "./contexts/TerminalSessionContext";
import {
  attachErrorMessage,
  decodeHistoryBase64,
  remoteBadgeLabel,
  sessionLabelFromTitle,
  REMOTE_HISTORY_EVENT,
  type RemoteHistoryDetail,
  type RemoteTerminalInfoWire,
} from "./remoteTabs";

/**
 * Zone-header controls for a REMOTE tab (plan
 * `2026-08-31-remote-session-tabs-in-runner-terminal`, Phases 4/5):
 *
 * - the device badge — which machine this tab's session runs on;
 * - **Earlier output** — Phase 5 lazy scrollback: the attach shipped only the
 *   tail of the target's ring; this fetches the rest on demand and re-renders
 *   the pane (history first, then the local ring) — see `TerminalInstance`'s
 *   `REMOTE_HISTORY_EVENT` handler;
 * - **Reattach** — for a remote tab whose pane ENDED (target exit, refused
 *   reattach, expired grant): re-runs `terminal_attach_remote` for the same
 *   `(deviceId, sessionId)`, then closes the dead tab. A live tab whose relay
 *   merely dropped needs none of this: the Rust pane keeps the session open,
 *   writes an in-band notice, and reattaches by itself on reconnect.
 *
 * Renders nothing for a local tab. Every action acknowledges itself — busy
 * state while pending, the outcome shown inline and kept until the next
 * action (`ux-priorities` `an-action-must-acknowledge-itself`).
 */
export function RemoteTabControls({
  tab,
  compact = false,
}: {
  tab: TerminalTab;
  compact?: boolean;
}) {
  const badge = remoteBadgeLabel(tab.remote);
  const session = useTerminalSession();
  const [busy, setBusy] = useState<"history" | "reattach" | null>(null);
  const [note, setNote] = useState<string | null>(null);

  if (!badge || !tab.remote) return null;
  const remote = tab.remote;

  const loadHistory = async () => {
    if (busy) return;
    setBusy("history");
    setNote(null);
    try {
      const r = await invoke<CommandResponse>("terminal_remote_history_load", {
        terminalId: tab.id,
      });
      if (!r.success) {
        setNote(r.message ?? "no earlier output");
        return;
      }
      const d = r.data as { data: string; startOffset: number; endOffset: number };
      // The pane reports what it actually did. The listener is synchronous up
      // to its own early returns, so `reported` is set by the time dispatch
      // returns; a pane that is not mounted reports nothing at all, which is
      // itself a distinct (and honest) answer.
      let reported = false;
      const detail: RemoteHistoryDetail = {
        terminalId: tab.id,
        bytes: decodeHistoryBase64(d.data),
        startOffset: d.startOffset,
        endOffset: d.endOffset,
        report: (outcome) => {
          reported = true;
          setNote(
            outcome.rendered
              ? `loaded ${outcome.bytes} earlier bytes`
              : `earlier output not shown: ${outcome.reason}`,
          );
        },
      };
      window.dispatchEvent(new CustomEvent<RemoteHistoryDetail>(REMOTE_HISTORY_EVENT, { detail }));
      if (!reported) {
        setNote("earlier output not shown: this tab's pane is not listening");
      }
    } catch (err) {
      setNote(`earlier output failed: ${attachErrorMessage(err)}`);
    } finally {
      setBusy(null);
    }
  };

  const reattach = async () => {
    if (busy) return;
    setBusy("reattach");
    setNote(null);
    try {
      const info = await invoke<RemoteTerminalInfoWire>("terminal_attach_remote", {
        deviceId: remote.deviceId,
        sessionId: remote.sessionId,
        deviceLabel: remote.deviceLabel,
        sessionLabel: sessionLabelFromTitle(tab.title, remote.deviceLabel),
        workingDir: tab.workingDir ?? null,
        pageId: session.pageId !== "default" ? session.pageId : null,
      });
      // The new tab is already open (terminal-created + terminal-remote-identity);
      // retire the dead one it replaces.
      session.closeTerminal(tab.id);
      session.setActiveId(info.id);
    } catch (err) {
      setNote(`reattach failed: ${attachErrorMessage(err)}`);
    } finally {
      setBusy(null);
    }
  };

  const btn =
    "flex items-center gap-0.5 shrink-0 text-[8px] px-1 py-0 rounded transition-colors disabled:opacity-50 " +
    "text-[#7aa2f7] bg-[#7aa2f7]/10 hover:bg-[#7aa2f7]/25";

  return (
    <>
      <span
        data-ui-bridge-id={`terminal.remote-badge.${tab.id}`}
        className={`flex items-center gap-0.5 shrink-0 text-[8px] text-[#7aa2f7] bg-[#7aa2f7]/15 px-1 py-0 ${
          compact ? "rounded-full py-0.5" : "rounded"
        }`}
        title={badge.title}
        aria-label={`Remote tab: ${badge.title}`}
      >
        <Monitor className="w-2.5 h-2.5" />
        {badge.text}
      </span>
      {tab.isAlive && remote.historyAvailable && (
        <button
          type="button"
          data-ui-bridge-id={`terminal.remote-history.${tab.id}`}
          onClick={(e) => {
            e.stopPropagation();
            void loadHistory();
          }}
          disabled={busy !== null}
          className={btn}
          title="The attach delivered only the newest part of the remote scrollback. Load what came before it (re-renders this pane)."
        >
          <History className={`w-2.5 h-2.5 ${busy === "history" ? "animate-spin" : ""}`} />
          {busy === "history" ? "loading…" : "earlier output"}
        </button>
      )}
      {!tab.isAlive && (
        <button
          type="button"
          data-ui-bridge-id={`terminal.remote-reattach.${tab.id}`}
          onClick={(e) => {
            e.stopPropagation();
            void reattach();
          }}
          disabled={busy !== null}
          className={btn}
          title="This remote pane ended (the session exited, or the grant expired). Mint a fresh grant and reattach to the same session."
        >
          <RefreshCw className={`w-2.5 h-2.5 ${busy === "reattach" ? "animate-spin" : ""}`} />
          {busy === "reattach" ? "reattaching…" : "reattach"}
        </button>
      )}
      {note && (
        <span
          data-ui-bridge-id={`terminal.remote-note.${tab.id}`}
          className="text-[8px] text-[#e0af68] truncate max-w-[14rem]"
          title={note}
        >
          {note}
        </span>
      )}
    </>
  );
}
