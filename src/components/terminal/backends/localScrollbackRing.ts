/**
 * The LOCAL runner's scrollback ring, read over the Tauri command
 * `terminal_get_scrollback`.
 *
 * This is the only module that names that command. Consumers that hold an
 * `ITerminalBackend` read the ring through `backend.readScrollbackRing(...)`,
 * which every shipped backend implements by delegating here. Two consumers
 * hold no backend BY DESIGN and call `readLocalScrollbackRing` directly:
 *
 *  - `TerminalBridgeProxies` — the proxy exists exactly while no backend is
 *    mounted for the pane (mount-independent UI Bridge element).
 *  - `resumeVerification` — the boot-restore probe addresses a tab id through
 *    `TerminalRefsMap`, which carries `TerminalInstanceHandle`s and no backend,
 *    and the tab may be virtualized away entirely while it polls.
 *
 * `scrollbackRingSeam.test.ts` pins both facts: a new
 * `invoke("terminal_get_scrollback")` anywhere else in `terminal/` goes red.
 *
 * Pre-Phase-3 seam of plan `2026-08-31-remote-session-tabs-in-runner-terminal`
 * (vet 2026-09-02, "The seam"): a remote pane's ring arrives over a different
 * transport, and until this module every consumer was hard-wired to this one.
 */

import { invoke } from "@tauri-apps/api/core";
import type { ScrollbackRingWindow } from "./types";

/** The runner command that serves the ring. Exported for the seam test. */
export const LOCAL_SCROLLBACK_RING_COMMAND = "terminal_get_scrollback";

/**
 * Wire shape of the command's `CommandResponse<ScrollbackData>`. `data` is
 * `null` when the runner holds no ring for the id; `data.data` is the ring
 * bytes base64-encoded.
 */
interface LocalScrollbackResponse {
  success?: boolean;
  data?: { data?: string; startOffset: number; endOffset: number } | null;
}

/** The IPC call, injectable for tests. Defaults to Tauri's `invoke`. */
export type ScrollbackInvoker = (cmd: string, args: { terminalId: string }) => Promise<unknown>;

/** Decode the ring's base64 payload to the raw PTY bytes it carries. */
export function decodeRingBytes(encoded: string): Uint8Array {
  const raw = atob(encoded);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes;
}

/**
 * Read the local runner's scrollback ring for `terminalId`.
 *
 * Resolves `null` when the runner answers without a ring (`success: false`,
 * `data: null`, or no encoded payload) — the terminal is gone or never had
 * one. Rejects only when the IPC itself fails, so callers can tell "no ring"
 * from "no answer" the way the pre-seam call sites did.
 */
export async function readLocalScrollbackRing(
  terminalId: string,
  invoker: ScrollbackInvoker = invoke,
): Promise<ScrollbackRingWindow | null> {
  const resp = (await invoker(LOCAL_SCROLLBACK_RING_COMMAND, { terminalId })) as
    | LocalScrollbackResponse
    | undefined;
  const data = resp?.data;
  if (!resp?.success || !data || typeof data.data !== "string") return null;
  return {
    bytes: decodeRingBytes(data.data),
    startOffset: data.startOffset,
    endOffset: data.endOffset,
  };
}
