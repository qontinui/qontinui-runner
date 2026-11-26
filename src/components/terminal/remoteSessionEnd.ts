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
  tenant: string | null = null,
): Promise<RemoteSessionEndResult> {
  return invoke<RemoteSessionEndResult>("remote_session_end", {
    deviceId,
    sessionId,
    force,
    // The tenant the row was LISTED under: coord resolves the session within
    // the presented principal's tenant only. null = the runner's default.
    tenant,
  });
}

/**
 * END a session on another device AS THE OPERATOR (plan
 * `2026-10-28-one-session-lifecycle-authority`, Phase 16 (c)): the twin that
 * only `RemoteSessionEndFlow` calls. It goes through the web backend's
 * user-session path under the operator's own sign-in; a `force` without that
 * sign-in is refused `operator_sign_in_required`. `terminalId` is the target
 * terminal the graceful answer reported (the force fallback's address), when
 * known. The bulk "Close all finished" keeps calling {@link remoteSessionEnd}
 * with `force: false`.
 */
export function operatorRemoteSessionEnd(
  deviceId: string,
  sessionId: string,
  force = false,
  tenant: string | null = null,
  terminalId: string | null = null,
): Promise<RemoteSessionEndResult> {
  return invoke<RemoteSessionEndResult>("operator_remote_session_end", {
    deviceId,
    sessionId,
    force,
    tenant,
    terminalId,
  });
}
