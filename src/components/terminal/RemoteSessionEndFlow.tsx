import { useCallback, useState } from "react";
import { createPortal } from "react-dom";
import { Power } from "lucide-react";

import { ConfirmDialog } from "../ui/ConfirmDialog";
import { remoteSessionEnd } from "./remoteSessionEnd";
import {
  describeEndResult,
  endInvokeFailure,
  REMOTE_END_CANCEL_ID,
  REMOTE_END_CONFIRM_ID,
  REMOTE_END_DIALOG_ID,
  REMOTE_END_FORCE_CONFIRM_ID,
  REMOTE_END_FORCE_ID,
  REMOTE_END_RESULT_ID,
  remoteTabEndId,
  type EndResultView,
} from "./remoteSessionEndView";
import { sessionLabelFromTitle, type RemoteSessionEndResult } from "./remoteTabs";
import type { TerminalTab } from "./useTerminalManager";

/**
 * The single-session "End on remote" flow (plan
 * `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 5a/5b):
 * confirm → graceful end → result, and — only on `refused` / `still_running` —
 * an explicit "Force end" behind a SECOND confirm.
 *
 * Rendered through a portal: the tab's hover cluster is an `opacity-0` overlay
 * that fades out when the pointer leaves the cell, and a dialog nested in it
 * would fade with it.
 */

export interface RemoteEndTarget {
  deviceId: string;
  deviceLabel: string;
  sessionId: string;
  sessionLabel: string;
}

type Phase =
  | { step: "confirm" }
  | { step: "running"; force: boolean }
  | { step: "result"; result: RemoteSessionEndResult; view: EndResultView }
  | { step: "force-confirm"; result: RemoteSessionEndResult; view: EndResultView };

export function RemoteSessionEndFlow({
  target,
  onClose,
}: {
  target: RemoteEndTarget;
  /** Called when the dialog closes, with the last result (null when none ran). */
  onClose: (result: RemoteSessionEndResult | null, view: EndResultView | null) => void;
}) {
  const [phase, setPhase] = useState<Phase>({ step: "confirm" });

  const run = useCallback(
    async (force: boolean) => {
      setPhase({ step: "running", force });
      let result: RemoteSessionEndResult;
      try {
        result = await remoteSessionEnd(target.deviceId, target.sessionId, force);
      } catch (err) {
        result = endInvokeFailure(err, target.deviceId, target.sessionId);
      }
      setPhase({ step: "result", result, view: describeEndResult(result, force) });
    },
    [target.deviceId, target.sessionId],
  );

  const who = `${target.sessionLabel} on ${target.deviceLabel}`;
  // Cancelling the force step still reports the graceful result it followed.
  const close = () =>
    phase.step === "result" || phase.step === "force-confirm"
      ? onClose(phase.result, phase.view)
      : onClose(null, null);

  let dialog;
  switch (phase.step) {
    case "confirm":
      dialog = (
        <ConfirmDialog
          open
          title="End remote session"
          message={`End ${who}?`}
          description={
            "This asks that machine to exit Claude gracefully (/exit) and close the terminal. " +
            "A busy prompt or an unsent draft is refused, not interrupted. Closing the tab " +
            "only detaches — this ends the session."
          }
          variant="warning"
          confirmText="End session"
          dialogId={REMOTE_END_DIALOG_ID}
          confirmId={REMOTE_END_CONFIRM_ID}
          cancelId={REMOTE_END_CANCEL_ID}
          onClose={close}
          onConfirm={() => void run(false)}
        />
      );
      break;
    case "running":
      dialog = (
        <ConfirmDialog
          open
          title="End remote session"
          message={
            phase.force
              ? `Force-closing ${who}…`
              : `Ending ${who} — waiting for the remote (up to 90 s)…`
          }
          isLoading
          dialogId={REMOTE_END_DIALOG_ID}
          confirmId={REMOTE_END_CONFIRM_ID}
          cancelId={REMOTE_END_CANCEL_ID}
          onClose={() => {}}
          onConfirm={() => {}}
        />
      );
      break;
    case "result": {
      const v = phase.view;
      dialog = (
        <ConfirmDialog
          open
          title={v.isEnded ? "Remote session ended" : "Remote session not ended"}
          message={v.headline}
          variant={v.isEnded ? "info" : "warning"}
          confirmText="Force end…"
          cancelText="Close"
          hideConfirm={!v.offerForce}
          dialogId={REMOTE_END_DIALOG_ID}
          confirmId={REMOTE_END_FORCE_ID}
          cancelId={REMOTE_END_CANCEL_ID}
          onClose={close}
          onConfirm={() => setPhase({ step: "force-confirm", result: phase.result, view: v })}
        >
          <div
            data-ui-bridge-id={REMOTE_END_RESULT_ID}
            data-end-outcome={v.outcome}
            role="status"
            className="text-sm space-y-1"
          >
            <div>
              <span className={v.toneClass}>{v.label}</span>
              <span className="text-zinc-500"> · {who}</span>
            </div>
            {v.detail && <div className="text-zinc-400 break-words">Reason: {v.detail}</div>}
          </div>
        </ConfirmDialog>
      );
      break;
    }
    case "force-confirm":
      dialog = (
        <ConfirmDialog
          open
          title="Force end remote session"
          message={`Force-close ${who}?`}
          description={
            "This kills the remote terminal immediately — an in-flight Claude turn or an " +
            "unsent draft is lost. It cannot be undone."
          }
          variant="danger"
          confirmText="Force end"
          dialogId={REMOTE_END_DIALOG_ID}
          confirmId={REMOTE_END_FORCE_CONFIRM_ID}
          cancelId={REMOTE_END_CANCEL_ID}
          onClose={close}
          onConfirm={() => void run(true)}
        >
          <div className="text-sm text-zinc-400 break-words">
            The graceful end said: {phase.view.headline}
            {phase.view.detail ? ` (${phase.view.detail})` : ""}
          </div>
        </ConfirmDialog>
      );
      break;
  }

  if (typeof document === "undefined") return dialog;
  return createPortal(dialog, document.body);
}

/**
 * The "End on remote session" button for a remote tab's hover cluster, with
 * the flow it opens. Self-contained (its own open state) so the cluster it
 * sits in needs nothing but the tab.
 *
 * An `ended` needs no action here: the target's PTY close reaches this runner
 * as `remote_terminal_exit`, which ends the tab's pane through the existing
 * path — adding a second retire path would race it.
 */
export function RemoteTabEndAction({ tab }: { tab: TerminalTab }) {
  const [open, setOpen] = useState(false);
  const remote = tab.remote;
  if (!remote) return null;
  const deviceLabel = remote.deviceLabel.trim() || remote.deviceId.slice(0, 8);
  return (
    <>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen(true);
        }}
        data-ui-bridge-id={remoteTabEndId(tab.id)}
        className="p-1 rounded text-[#565f89] hover:text-[#e0af68] hover:bg-[#e0af68]/10 transition-colors"
        title={`End this session on ${deviceLabel} (graceful /exit, confirmed first). Close only detaches.`}
        aria-label="End on remote session"
      >
        <Power className="w-3 h-3" />
      </button>
      {open && (
        <RemoteSessionEndFlow
          target={{
            deviceId: remote.deviceId,
            deviceLabel,
            sessionId: remote.sessionId,
            sessionLabel: sessionLabelFromTitle(tab.title, remote.deviceLabel),
          }}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}
