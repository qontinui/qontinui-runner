import { invoke } from "@tauri-apps/api/core";
import type { RefObject } from "react";
import type { TerminalInstanceHandle } from "./TerminalInstance";

const encoder = new TextEncoder();

/** Encode a Uint8Array to base64 without stack overflow on large buffers. */
function uint8ToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, Math.min(i + CHUNK, bytes.length));
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

/**
 * Send `text` to a terminal by tab id, whether or not its `TerminalInstance`
 * is currently mounted.
 *
 * Under flow-grid virtualization (Phase 3) an assigned zone that is far
 * offscreen renders only a `CompactZoneCard` with NO mounted instance — so the
 * historical `terminalRefs.get(id)?.current?.writeToTerminal(...)` path is a
 * silent no-op for it. That would make quick-approve / reject / send-command on
 * a compact card 20 rows down do nothing.
 *
 * Resolution: prefer the mounted ref (identical byte path + local echo), and
 * when there is no live instance fall back to the backend `terminal_write`
 * command, which addresses the PTY session directly by id and is unaffected by
 * mount state. Both routes ultimately reach the same `TerminalSession::write`,
 * so typed-input observation (git-warn / resume sniff) fires either way.
 *
 * The Rust `terminal_write` handler (`src-tauri/src/commands/terminal.rs`)
 * takes `{ terminalId, data }` where `data` is base64 of the raw bytes — mirror
 * exactly what `TerminalInstance.writeToTerminal` sends.
 */
export function writeToTerminalById(
  terminalRefs: Map<string, RefObject<TerminalInstanceHandle | null>>,
  tabId: string,
  text: string,
): void {
  const mounted = terminalRefs.get(tabId)?.current;
  if (mounted) {
    mounted.writeToTerminal(text);
    return;
  }
  const bytes = encoder.encode(text);
  invoke("terminal_write", { terminalId: tabId, data: uint8ToBase64(bytes) }).catch(() => {
    // Fire-and-forget: a failed write only drops the keystroke; there is no
    // local echo to roll back and the operator can retry.
  });
}
