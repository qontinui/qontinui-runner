/**
 * Which terminals currently have a pane that RENDERS their output — and
 * therefore acks their bytes to the Rust reader itself (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 2, root cause A4).
 *
 * The backend gates webview emission on acked bytes (`EmissionGate` in
 * `src-tauri/src/terminal/session.rs`). The page-level output tap
 * (`TerminalSessionContext`) proxy-acks bytes for tabs nobody is rendering, so
 * an unwatched session's emission never wedges at the high watermark and the
 * tap — which keeps session-state tracking (chips, sparklines) mount-independent
 * — never goes blind. That mechanism is deliberate and documented; Phase 2 only
 * corrects the *predicate* it uses.
 *
 * It used to be "no mounted `TerminalInstance`". A hidden pane is mounted but
 * stops writing to xterm and stops its ack timer, so it renders nothing and
 * acks nothing — under the old predicate its emission would stall at the high
 * watermark and the tap would go blind with it. The predicate is now "no
 * mounted AND VISIBLE instance", a strict superset of the tabs the tap used to
 * cover.
 *
 * A visible pane registers as soon as its effect runs — NOT when its backend
 * finishes loading. That timing is load-bearing: a freshly mounted pane
 * buffers pre-backend bytes and writes them itself, so if the tap also
 * proxy-acked them during the (up to ~1 s) init the terminal would be
 * double-acked and its emission backpressure would be disabled for the rest of
 * the session (`bytes_acked` only ever grows; the gate compares
 * `sent - acked`).
 *
 * Ref-counted rather than a plain set: a tab moving between windows/zones can
 * transiently have two mounts, and the departing one's release must not clear
 * the survivor's registration. Note the refcount governs the TAP's predicate
 * only — two panes rendering the same terminal would each ack their own
 * writes, which is pre-existing and prevented upstream by the
 * exactly-one-or-zero mount-owner invariant in `classifyTabs`.
 */

const consumers = new Map<string, number>();

/**
 * Register this pane as the terminal's render-ack consumer. Returns a release
 * function; calling it twice is a no-op.
 */
export function retainRenderConsumer(terminalId: string): () => void {
  consumers.set(terminalId, (consumers.get(terminalId) ?? 0) + 1);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const next = (consumers.get(terminalId) ?? 0) - 1;
    if (next <= 0) consumers.delete(terminalId);
    else consumers.set(terminalId, next);
  };
}

/** True when some mounted, visible pane is rendering (and acking) this terminal. */
export function hasRenderConsumer(terminalId: string): boolean {
  return consumers.has(terminalId);
}

/** Test-only. */
export function __renderConsumerIds(): string[] {
  return [...consumers.keys()];
}

/** Test-only. */
export function __resetRenderConsumersForTest(): void {
  consumers.clear();
}
