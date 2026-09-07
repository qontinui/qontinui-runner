import type { FleetSession } from "./useFleetSessions";

/**
 * Remote session tabs — the frontend half of plan
 * `2026-08-31-remote-session-tabs-in-runner-terminal` Phases 3c/4/5.
 *
 * Pure helpers only (the runner's vitest config is `environment: "node"`, no
 * jsdom), so the picker's button states, the tab badge, the tab-identity
 * bookkeeping and the restore filter are all unit-testable without rendering.
 */

/**
 * The identity a remote tab is keyed on, as `terminal_attach_remote` returns it
 * and the `terminal-remote-identity` event / `terminal_remote_identities`
 * command carry it (Rust `RemoteTabIdentity`, serde camelCase). The LOCAL
 * terminal id is fresh per attach; `(deviceId, sessionId)` is what survives a
 * reattach, a restart and a session leaving and re-entering the fleet list.
 */
export interface RemoteTabIdentity {
  deviceId: string;
  deviceLabel: string;
  sessionId: string;
  remoteTerminalId: string;
  /** True when the target still holds output OLDER than the attach seed. */
  historyAvailable: boolean;
}

/** Runner-local `TerminalInfo` + `remote`, as `terminal_attach_remote` returns. */
export interface RemoteTerminalInfoWire {
  id: string;
  title: string;
  pid?: number | null;
  cols: number;
  rows: number;
  workingDir: string;
  isAlive: boolean;
  exitCode?: number | null;
  createdAt: number;
  totalBytesProduced: number;
  pageId: string;
  remote: RemoteTabIdentity;
}

export function sameRemote(
  a: Pick<RemoteTabIdentity, "deviceId" | "sessionId"> | null | undefined,
  b: Pick<RemoteTabIdentity, "deviceId" | "sessionId"> | null | undefined,
): boolean {
  if (!a || !b) return false;
  return a.deviceId === b.deviceId && a.sessionId === b.sessionId;
}

// ---------------------------------------------------------------------------
// Picker: the Attach button
// ---------------------------------------------------------------------------

export interface AttachButtonState {
  /** True when the button must not fire. */
  disabled: boolean;
  /** The one-line tooltip saying WHY it is disabled; null when enabled. */
  reason: string | null;
}

export function fleetSessionAttachId(sessionId: string): string {
  return `terminal.fleet-session-attach.${sessionId}`;
}

/**
 * Whether a fleet row can be attached to, and why not when it cannot.
 *
 * Disabled for the caller's own device (a local session is a local tab — open
 * it from the Sessions view), for a closed session (nothing to attach to), and
 * for a row whose device id is missing or whose device identity columns coord
 * could not read (the attach mints a grant AGAINST that device id; a degraded
 * id is not evidence of where the session runs).
 */
export function attachButtonState(
  s: Pick<FleetSession, "isCallerDevice" | "closedAt" | "deviceId" | "state">,
  deviceIdentityColumnsPresent: boolean | null | undefined,
): AttachButtonState {
  if (s.isCallerDevice) {
    return {
      disabled: true,
      reason: "This session runs on this machine — open it from the Sessions view.",
    };
  }
  if (s.closedAt || s.state?.trim() === "closed") {
    return { disabled: true, reason: "This session is closed; there is nothing to attach to." };
  }
  if (!s.deviceId?.trim()) {
    return {
      disabled: true,
      reason: "coord did not report which device this session runs on — cannot address it.",
    };
  }
  if (deviceIdentityColumnsPresent === false) {
    return {
      disabled: true,
      reason:
        "coord could not read device identity for this read — the device id is unknown, not confirmed. Refresh and try again.",
    };
  }
  return { disabled: false, reason: null };
}

/**
 * Turn the string a failed `terminal_attach_remote` invoke rejects with into
 * the line shown inline in the row. The Rust side spells every failure
 * `remote_attach:<code>[:<reason>]: <detail>`; the code is what the operator
 * needs first, the detail is kept because it names the machine or grant.
 */
export function attachErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : typeof err === "string" ? err : String(err);
  const m = /^remote_attach:([a-z_]+)(?::([a-z_]+))?:\s*(.*)$/s.exec(raw.trim());
  if (!m) return raw.trim() || "attach failed (no reason given)";
  const code = m[2] ? `${m[1]} (${m[2]})` : m[1];
  const detail = m[3]?.trim();
  return detail ? `${code} — ${detail}` : code;
}

/** The label the picker sends as `sessionLabel` and the tab title's second half. */
export function remoteSessionLabel(
  s: Pick<FleetSession, "workUnitSlug" | "intent" | "repo" | "branch" | "sessionId">,
): string {
  const slug = s.workUnitSlug?.trim();
  if (slug) return slug;
  const intent = s.intent?.trim();
  if (intent) return intent.length > 40 ? `${intent.slice(0, 40)}…` : intent;
  const repo = s.repo?.trim();
  const branch = s.branch?.trim();
  if (repo && branch) return `${repo} @ ${branch}`;
  if (repo) return repo;
  return s.sessionId.slice(0, 8);
}

/** `"<device label>: <session label>"` — the same shape the Rust side titles the tab. */
export function remoteTabTitle(deviceLabel: string, sessionLabel: string): string {
  return `${deviceLabel}: ${sessionLabel}`;
}

// ---------------------------------------------------------------------------
// Tab badge + identity bookkeeping
// ---------------------------------------------------------------------------

/**
 * The device chip on a remote tab's header, or null for a local tab. The text
 * is the device label the picker showed at attach time; the title carries the
 * ids so a same-named machine can still be told apart.
 */
export function remoteBadgeLabel(
  remote: RemoteTabIdentity | null | undefined,
): { text: string; title: string } | null {
  if (!remote) return null;
  const label = remote.deviceLabel.trim() || remote.deviceId.slice(0, 8);
  return {
    text: label,
    title:
      `Remote tab — this session runs on device ${remote.deviceId} ` +
      `(coord session ${remote.sessionId}). Closing this tab detaches; it does not end the session.`,
  };
}

/**
 * Attach a remote identity to the tab with `terminalId`. Same race shape as
 * the worker / bypass marks in `useTerminalManager`: the Rust side emits
 * `terminal-remote-identity` right after `terminal-created`, but arrival order
 * at the webview is not guaranteed, so a mark whose tab is not there yet is
 * reported `buffered` for the caller to hold until the tab lands.
 */
export function applyRemoteMark<T extends { id: string; remote?: RemoteTabIdentity }>(
  tabs: T[],
  terminalId: string,
  remote: RemoteTabIdentity,
): { tabs: T[]; buffered: boolean } {
  const idx = tabs.findIndex((t) => t.id === terminalId);
  if (idx < 0) return { tabs, buffered: true };
  const have = tabs[idx].remote;
  if (have && sameRemote(have, remote) && have.historyAvailable === remote.historyAvailable) {
    return { tabs, buffered: false };
  }
  const next = tabs.slice();
  next[idx] = { ...tabs[idx], remote };
  return { tabs: next, buffered: false };
}

/**
 * Remote tabs whose pane has ended — the target exited, the reattach was
 * refused, or the grant expired. These are the tabs that offer "Reattach".
 * A live remote tab whose RELAY dropped is NOT here: the Rust pane keeps the
 * session open, writes an in-band notice, and reattaches by itself.
 */
export function detachedRemoteTabs<T extends { isAlive: boolean; remote?: RemoteTabIdentity }>(
  tabs: readonly T[],
): T[] {
  return tabs.filter((t) => !!t.remote && !t.isAlive);
}

/**
 * Split a remote tab's title back into its session half, for a reattach that
 * should keep the same title. Falls back to the whole title when it was
 * renamed away from the `"<device>: <label>"` shape.
 */
export function sessionLabelFromTitle(title: string, deviceLabel: string): string {
  const prefix = `${deviceLabel}: `;
  return title.startsWith(prefix) ? title.slice(prefix.length) : title;
}

// ---------------------------------------------------------------------------
// Restart: saved remote tabs come back as a placeholder card, never a PTY
// ---------------------------------------------------------------------------

/**
 * The saved remote sessions of a page that are NOT currently open as a tab —
 * what the restore card offers a "Reattach" for after a restart.
 *
 * Re-attach on demand, deliberately not at boot: a runner restart voids every
 * grant, the target may be offline, and auto-minting one grant per saved tab
 * would fail loudly for each dark device with nothing the operator asked for.
 * A saved remote tab is therefore never fed to `terminal_create` (that would
 * silently spawn a LOCAL shell under a remote title) and never respawned by
 * itself — it is listed here, one click each, with the outcome shown inline.
 */
export function savedRemoteSessionsToRestore<T extends { remote?: RemoteTabIdentity }>(
  saved: ReadonlyArray<{ remote?: RemoteTabIdentity; title: string }>,
  liveTabs: readonly T[],
): Array<{ remote: RemoteTabIdentity; title: string }> {
  const out: Array<{ remote: RemoteTabIdentity; title: string }> = [];
  for (const s of saved) {
    if (!s.remote) continue;
    if (liveTabs.some((t) => sameRemote(t.remote, s.remote))) continue;
    if (out.some((o) => sameRemote(o.remote, s.remote))) continue;
    out.push({ remote: s.remote, title: s.title });
  }
  return out;
}

/** The window event `RemoteTabActions` raises with fetched earlier output. */
export const REMOTE_HISTORY_EVENT = "qontinui:remote-history";

export interface RemoteHistoryDetail {
  terminalId: string;
  /** Raw target bytes for `[startOffset, endOffset)` of the TARGET's stream. */
  bytes: Uint8Array;
  startOffset: number;
  endOffset: number;
}

/** Decode the `terminal_remote_history_load` payload's base64 body. */
export function decodeHistoryBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
