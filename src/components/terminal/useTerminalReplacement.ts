import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";
import { shouldIngestCreatedTerminal } from "./useTerminalManager";

const logger = createLogger("TerminalReplacement");

/**
 * Emitted by the runner when it replaces one pane with another in place —
 * today only the usage-limit account migration
 * (`terminal::account_migration::TERMINAL_REPLACED_EVENT`). Sent twice per
 * replacement: first with `newTerminalId: null` before the respawn (a
 * replacement is coming), then once settled, with the new pane's id, or with
 * `""` when the respawn failed and the old pane stays.
 */
export const TERMINAL_REPLACED_EVENT = "terminal-replaced";

export interface TerminalReplacedPayload {
  pageId?: string | null;
  oldTerminalId?: string | null;
  newTerminalId?: string | null;
}

/** What one `terminal-replaced` event asks of THIS page's zone layout. */
export type ReplacementStep =
  | { kind: "begin"; oldId: string }
  | { kind: "complete"; oldId: string; newId: string }
  | { kind: "abandon"; oldId: string };

/**
 * Pure: read one event for the page `pageId`. `null` for an event naming
 * another page, or one with no old pane id.
 */
export function replacementStepFor(
  payload: TerminalReplacedPayload | null | undefined,
  pageId: string,
): ReplacementStep | null {
  const oldId = payload?.oldTerminalId;
  if (!oldId) return null;
  if (!shouldIngestCreatedTerminal(payload?.pageId, pageId)) return null;
  const newId = payload?.newTerminalId;
  if (newId === null || newId === undefined) return { kind: "begin", oldId };
  if (newId === "") return { kind: "abandon", oldId };
  return { kind: "complete", oldId, newId };
}

/** The slice of `useZoneLayout()` a replacement drives. */
export interface ReplacementZoneLayout {
  beginReplacement: (oldId: string) => void;
  endReplacement: (oldId: string) => void;
  /** The new pane takes the old one's zone (tmux: its shown slot and grid zone). */
  replaceTab: (oldId: string, newId: string) => void;
}

/**
 * Put a runner-replaced pane's successor where the old pane was. Without it
 * the successor is placed like any new terminal — in the lowest empty zone, or
 * a zone a full grid grows for — and the old pane's close then leaves its own
 * zone empty.
 *
 * Completion keeps the hold (`beginReplacement`) in place: the old pane is
 * still listed until the runner closes it, and the hold stops counting once it
 * is gone.
 */
export function useTerminalReplacement(pageId: string, zoneLayout: ReplacementZoneLayout): void {
  const layoutRef = useRef(zoneLayout);
  useEffect(() => {
    layoutRef.current = zoneLayout;
  });

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<TerminalReplacedPayload>(TERMINAL_REPLACED_EVENT, (event) => {
      const step = replacementStepFor(event.payload, pageId);
      if (!step) return;
      const layout = layoutRef.current;
      switch (step.kind) {
        case "begin":
          layout.beginReplacement(step.oldId);
          break;
        case "complete":
          layout.beginReplacement(step.oldId);
          layout.replaceTab(step.oldId, step.newId);
          logger.info(`Terminal ${step.oldId} replaced in place by ${step.newId} [page ${pageId}]`);
          break;
        case "abandon":
          layout.endReplacement(step.oldId);
          break;
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [pageId]);
}
