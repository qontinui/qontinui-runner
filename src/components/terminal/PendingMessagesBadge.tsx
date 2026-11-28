/**
 * Per-pane pending coord-message badge for the zone header (plan
 * `2026-11-27-coord-message-to-cross-account-pane-never-pushed`, item 4).
 *
 * Fed by the runner's OWN session-message poller, not by coord: every tick the
 * poller emits `session-messages-pending-view` with, per terminal, each pending
 * message it could not push there and why (`session_message_poller.rs`,
 * `render_pending_view`; also `GET /session-messages/pending-view`).
 *
 * Three states, never two: a fresh view with messages for this pane shows the
 * count; a fresh view with none shows nothing; a view that is unknown (the
 * runner is unpaired, the last poll failed, no tick has arrived yet, or the
 * last one is older than `staleAfterSecs`) shows a muted `?` chip, so an
 * unknown never reads as "nothing pending".
 */

import { useEffect, useState, useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { Mail } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

export const PENDING_VIEW_EVENT = "session-messages-pending-view";

export interface PendingViewEntry {
  messageId: string;
  priority: string;
  reason: string;
  blockedSince: string | null;
}

export interface PendingView {
  state: "fresh" | "unknown";
  unknownReason: string | null;
  polledAt: string | null;
  staleAfterSecs: number;
  byTerminal: Record<string, PendingViewEntry[]>;
}

/** Parse one event payload; anything malformed is `null` (ignored). */
export function parsePendingView(raw: unknown): PendingView | null {
  if (typeof raw !== "object" || raw === null) return null;
  const p = raw as Record<string, unknown>;
  if (p.state !== "fresh" && p.state !== "unknown") return null;
  const byTerminal: Record<string, PendingViewEntry[]> = {};
  if (typeof p.byTerminal === "object" && p.byTerminal !== null) {
    for (const [terminalId, list] of Object.entries(p.byTerminal as Record<string, unknown>)) {
      if (!Array.isArray(list)) continue;
      byTerminal[terminalId] = list.flatMap((e) => {
        if (typeof e !== "object" || e === null) return [];
        const r = e as Record<string, unknown>;
        if (typeof r.messageId !== "string" || typeof r.reason !== "string") return [];
        return [
          {
            messageId: r.messageId,
            priority: typeof r.priority === "string" ? r.priority : "normal",
            reason: r.reason,
            blockedSince: typeof r.blockedSince === "string" ? r.blockedSince : null,
          },
        ];
      });
    }
  }
  return {
    state: p.state,
    unknownReason: typeof p.unknownReason === "string" ? p.unknownReason : null,
    polledAt: typeof p.polledAt === "string" ? p.polledAt : null,
    staleAfterSecs: typeof p.staleAfterSecs === "number" ? p.staleAfterSecs : 30,
    byTerminal,
  };
}

/** What one pane's badge shows at `nowMs`. Pure; exported for the unit test. */
export type PaneBadge =
  | { kind: "unknown"; reason: string }
  | { kind: "none" }
  | { kind: "pending"; entries: PendingViewEntry[] };

export function paneBadge(view: PendingView | null, terminalId: string, nowMs: number): PaneBadge {
  if (!view) return { kind: "unknown", reason: "no poll has reported yet" };
  if (view.state === "unknown") {
    return { kind: "unknown", reason: view.unknownReason ?? "unknown" };
  }
  const polledMs = view.polledAt ? Date.parse(view.polledAt) : NaN;
  if (!Number.isFinite(polledMs) || nowMs - polledMs > view.staleAfterSecs * 1000) {
    return { kind: "unknown", reason: "the last poll is stale" };
  }
  const entries = view.byTerminal[terminalId] ?? [];
  return entries.length ? { kind: "pending", entries } : { kind: "none" };
}

/** The badge's title / UI Bridge label: every message id with its reason. */
export function paneBadgeText(badge: PaneBadge): string {
  switch (badge.kind) {
    case "unknown":
      return `Pending coord messages: unknown (${badge.reason})`;
    case "none":
      return "No pending coord messages";
    case "pending":
      return `${badge.entries.length} pending coord message${badge.entries.length === 1 ? "" : "s"}: ${badge.entries
        .map(
          (e) =>
            `${e.messageId} (${e.priority}, ${e.reason}${e.blockedSince ? `, since ${e.blockedSince}` : ""})`,
        )
        .join("; ")}`;
  }
}

export function pendingMessagesBadgeElementId(zoneIndex: number): string {
  return `terminal-pending-messages-${zoneIndex}`;
}

// One listener for every zone header.
let latest: PendingView | null = null;
const subscribers = new Set<() => void>();
let listening = false;

function ensureListening(): void {
  if (listening) return;
  listening = true;
  listen<unknown>(PENDING_VIEW_EVENT, (event) => {
    const view = parsePendingView(event.payload);
    if (!view) return;
    latest = view;
    subscribers.forEach((notify) => notify());
  }).catch((err) => {
    listening = false;
    console.error(`Failed to listen for ${PENDING_VIEW_EVENT}:`, err);
  });
}

function subscribe(notify: () => void): () => void {
  ensureListening();
  subscribers.add(notify);
  return () => subscribers.delete(notify);
}

function usePendingView(): PendingView | null {
  return useSyncExternalStore(subscribe, () => latest);
}

/** Re-render every few seconds so a view that stops arriving goes `unknown`. */
function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

export function PendingMessagesBadge({
  terminalId,
  zoneIndex,
}: {
  terminalId: string;
  zoneIndex: number;
}) {
  const view = usePendingView();
  const now = useNow(5000);
  const badge = paneBadge(view, terminalId, now);
  const text = paneBadgeText(badge);
  const elementId = pendingMessagesBadgeElementId(zoneIndex);
  // The UI Bridge DOM scan discovers interactive elements only; a plain chip
  // is in a snapshot only once registered (called before the early return:
  // hooks may not be conditional).
  const { ref } = useUIElement({ id: elementId, label: text, type: "generic" });
  if (badge.kind === "none") return null;
  const tone =
    badge.kind === "pending" ? "text-[#e0af68] bg-[#e0af68]/15" : "text-[#565f89] bg-[#565f89]/15";
  return (
    <span
      ref={ref}
      // `data-ui-bridge-id` is the runner's DOM-scan discovery attribute, so a
      // snapshot reads this chip and its label (message ids + reasons).
      data-ui-bridge-id={elementId}
      data-testid={elementId}
      data-pending-state={badge.kind}
      data-pending-message-ids={
        badge.kind === "pending" ? badge.entries.map((e) => e.messageId).join(",") : undefined
      }
      data-pending-reasons={
        badge.kind === "pending" ? badge.entries.map((e) => e.reason).join(",") : undefined
      }
      className={`flex items-center gap-0.5 shrink-0 text-[8px] px-1 py-0 rounded font-mono ${tone}`}
      title={text}
      aria-label={text}
    >
      <Mail className="w-2.5 h-2.5 shrink-0" aria-hidden />
      <span>{badge.kind === "pending" ? badge.entries.length : "?"}</span>
    </span>
  );
}
