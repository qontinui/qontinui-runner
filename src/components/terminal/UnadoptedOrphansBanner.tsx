/**
 * UnadoptedOrphansBanner — says which sessions a closed terminal page left
 * that no page could adopt (`useTerminalInitialization().unadoptedOrphans`:
 * the orphan-adoption pass gave up). Plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5.
 *
 * Its own banner, not the page's shared notification: withdrawing it when a
 * restore later adopts the orphans must never clear an unrelated notice that
 * replaced it meanwhile. It is withdrawn when the list empties. Dismissed, it
 * stays dismissed while the list only shrinks (orphans adopted or closed); it
 * shows again for an orphan not listed when it was dismissed — a new one, or
 * one that left the list and came back.
 */

import { useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import { unadoptedOrphansNotice, type UnadoptedOrphans } from "./useTerminalInitialization";

/** One key per orphan the banner shows: a session id, or a page whose orphans were never read. */
function orphanKeys(entries: readonly UnadoptedOrphans[]): string[] {
  return entries.flatMap((u) =>
    u.sessionIds === null
      ? u.removedPageIds.map((page) => `page:${page}`)
      : u.sessionIds.map((id) => `session:${id}`),
  );
}

export function UnadoptedOrphansBanner({ entries }: { entries: readonly UnadoptedOrphans[] }) {
  // The orphans shown when the operator dismissed the banner.
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());
  const keys = orphanKeys(entries);
  // An orphan that leaves the list (adopted, closed, withdrawn) leaves the
  // dismissed set too, so its return — a later give-up over it — is news
  // again. A guarded state update during render: React's "adjusting state
  // when a prop changes".
  const listed = new Set(keys);
  if ([...dismissed].some((k) => !listed.has(k))) {
    setDismissed(new Set([...dismissed].filter((k) => listed.has(k))));
  }
  if (keys.length === 0 || keys.every((k) => dismissed.has(k))) return null;
  return (
    <div
      role="alert"
      className="fixed bottom-4 left-1/2 -translate-x-1/2 z-40 flex items-start gap-3 max-w-2xl px-4 py-2 rounded-lg border border-red-500/40 bg-zinc-900/95 text-red-300 text-sm shadow-lg"
    >
      <AlertTriangle className="w-4 h-4 mt-0.5 shrink-0" />
      <div className="flex flex-col gap-1">
        {entries.map((u, i) => (
          <span key={i}>{unadoptedOrphansNotice(u)}</span>
        ))}
      </div>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={() => setDismissed(new Set([...dismissed, ...keys]))}
        className="p-0.5 rounded hover:bg-red-500/10 transition-colors"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}
