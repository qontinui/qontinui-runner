import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  applyAttachWaiting,
  REMOTE_ATTACH_WAITING_EVENT,
  type RemoteAttachWaiting,
} from "./remoteTabs";

/**
 * Live attach WAITING states, keyed by coord session id.
 *
 * The runner re-presents the SAME attach grant to a target that has not
 * recorded it yet, for up to a whole catch-up poll tick. That wait is tens of
 * seconds, so the pending spinner every attach surface already shows would sit
 * there with nothing behind it and read as a hang — which would be a worse
 * defect than the refused attach it replaces. This hook carries the runner's
 * own progress so each surface can say what is being waited on and for how
 * long.
 *
 * The map is keyed on the session rather than on a local terminal id because
 * no tab exists yet while the attach is still being negotiated.
 */
export function useRemoteAttachWaiting(): Record<string, RemoteAttachWaiting> {
  const [waiting, setWaiting] = useState<Record<string, RemoteAttachWaiting>>({});

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    void listen<RemoteAttachWaiting>(REMOTE_ATTACH_WAITING_EVENT, (event) => {
      setWaiting((prev) => applyAttachWaiting(prev, event.payload));
    }).then((fn) => {
      // Unmounted before the listener was registered: drop it immediately
      // rather than leaking a subscription onto a dead component.
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return waiting;
}
