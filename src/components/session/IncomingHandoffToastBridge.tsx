/**
 * IncomingHandoffToastBridge.tsx
 *
 * Mount-and-forget component that listens for `handoff_request` Tauri
 * `session-event` payloads and emits an in-app toast so the operator
 * notices a peer pushing a session to this device.
 *
 * The full receiver flow (claim re-acquire, scrollback replay, child
 * materialization) lives in `src-tauri/src/session/handoff.rs` — this
 * component is purely the user-visible "you got mail" surface.
 *
 * Lives at the App root so it sees events regardless of the active tab.
 */

import { useEffect, useState, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ArrowDownLeft, X } from "lucide-react";
import type { SessionEvent } from "@/contexts/SessionContext";
import { getAccentColors } from "@/design-system";

interface IncomingHandoff {
  /** Local id so we can dismiss/clear a specific toast. */
  id: string;
  sessionId: string;
  /** Origin device that triggered the handoff, if the payload carried it. */
  sourceDeviceId?: string;
  /** Operator-supplied purpose, when present in the wire payload. */
  purpose?: string;
  /** Wall-clock the toast was raised, for natural ordering. */
  raisedAt: number;
}

/** How long an unattended toast sticks before auto-dismissing. */
const AUTO_DISMISS_MS = 12_000;

let toastCounter = 0;

export function IncomingHandoffToastBridge() {
  const [toasts, setToasts] = useState<IncomingHandoff[]>([]);

  const dismiss = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    void listen<SessionEvent>("session-event", (event) => {
      const payload = event.payload;
      if (!payload || payload.kind !== "handoff_request") return;

      const sourceDeviceId =
        typeof payload.payload?.source_device_id === "string"
          ? payload.payload.source_device_id
          : undefined;
      const purpose =
        typeof payload.payload?.purpose === "string"
          ? payload.payload.purpose
          : undefined;

      const id = `handoff-${++toastCounter}`;
      const toast: IncomingHandoff = {
        id,
        sessionId: payload.session_id,
        sourceDeviceId,
        purpose,
        raisedAt: Date.now(),
      };
      setToasts((prev) => [...prev, toast]);

      // Auto-dismiss. Operator can also explicitly click X.
      setTimeout(() => dismiss(id), AUTO_DISMISS_MS);
    }).then((unl) => {
      unlisten = unl;
    });

    return () => {
      unlisten?.();
    };
  }, [dismiss]);

  if (toasts.length === 0) return null;

  const accent = getAccentColors("blue");

  return (
    <div
      data-page-element="incoming-handoff-toasts"
      className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm"
      role="region"
      aria-label="Incoming session handoff notifications"
    >
      {toasts.map((toast) => (
        <div
          key={toast.id}
          data-page-element="incoming-handoff-toast"
          data-session-id={toast.sessionId}
          className={`rounded-lg border ${accent.border} ${accent.bg} shadow-lg p-3`}
          role="status"
        >
          <div className="flex items-start gap-2">
            <ArrowDownLeft className={`w-4 h-4 mt-0.5 shrink-0 ${accent.text}`} />
            <div className="flex-1 min-w-0">
              <div className={`text-sm font-medium ${accent.text}`}>
                Incoming session handoff
              </div>
              {toast.purpose && (
                <div className="text-xs text-foreground mt-0.5 truncate">
                  {toast.purpose}
                </div>
              )}
              <div className="text-[11px] text-muted-foreground mt-1 font-mono truncate">
                session {toast.sessionId.slice(0, 8)}…
                {toast.sourceDeviceId
                  ? ` from ${toast.sourceDeviceId.slice(0, 8)}…`
                  : ""}
              </div>
            </div>
            <button
              type="button"
              onClick={() => dismiss(toast.id)}
              aria-label="Dismiss handoff notification"
              className="text-muted-foreground hover:text-foreground p-0.5"
            >
              <X className="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

export default IncomingHandoffToastBridge;
