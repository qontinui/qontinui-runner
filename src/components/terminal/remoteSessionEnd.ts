import { invoke } from "@tauri-apps/api/core";
import type { RemoteSessionEndResult } from "./remoteTabs";

/**
 * END a session on another device (plan
 * `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 3) — the
 * separate, named action beside a remote tab's close, which only detaches.
 *
 * `force: false` asks the target for a graceful `/exit` (refused at a busy
 * prompt or an unsent draft); `force: true` is a hard close and belongs behind
 * a second confirm. Resolves with the typed outcome; rejects only for an
 * argument that is not a session uuid. Waits up to 90 s for the target.
 */
export function remoteSessionEnd(
  deviceId: string,
  sessionId: string,
  force = false,
): Promise<RemoteSessionEndResult> {
  return invoke<RemoteSessionEndResult>("remote_session_end", {
    deviceId,
    sessionId,
    force,
  });
}
