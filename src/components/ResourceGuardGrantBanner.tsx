/**
 * ResourceGuardGrantBanner — the always-visible face of a live resource-guard
 * grant (plan
 * `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-dialog-says-low-memory`
 * Phase 3).
 *
 * "Start anyway" on the blocking dialog admits up to `GRANT_SPAWN_LIMIT` more
 * refused starts on that lane without asking (see `ResourceGuardGrant` in
 * `lib/resourceGuard.ts`). An override the operator cannot see is a surprise, so
 * while a grant is live this banner names its lane, the starts and seconds it
 * has left and what it was given for, and offers Revoke. There is deliberately
 * no "hide" that leaves the grant running: the banner goes away when the grant
 * does — spent, lapsed, or revoked — and not before.
 *
 * Mounted once at the App root beside `ResourceGuardDialog`, prop-less, for the
 * same reason the dialog is: the store is module-level and any spawn surface
 * can create a grant.
 */

import { useEffect, useState } from "react";
import { ShieldOff } from "lucide-react";
import { revokeResourceGuardGrant, useResourceGuardGrants } from "@/lib/resourceGuard";
import { grantAnnouncement, grantBannerText } from "@/lib/resourceGuardCopy";

export function ResourceGuardGrantBanner() {
  const grants = useResourceGuardGrants();
  const [now, setNow] = useState(() => Date.now());

  // Tick only while a grant is live: the countdown is the banner's whole point,
  // and an idle interval on every runner window is not. The zero-delay first
  // tick refreshes a clock that sat still while no grant was live.
  useEffect(() => {
    if (grants.length === 0) return;
    const tick = () => setNow(Date.now());
    const first = setTimeout(tick, 0);
    const id = setInterval(tick, 1000);
    return () => {
      clearTimeout(first);
      clearInterval(id);
    };
  }, [grants.length]);

  if (grants.length === 0) return null;

  return (
    <div className="fixed top-3 left-1/2 -translate-x-1/2 z-40 flex flex-col gap-2">
      {grants.map((grant) => (
        // The visible row is not a live region: its countdown changes every
        // second, and a polite region would read it aloud every second. The
        // screen-reader-only status below speaks when the grant appears or a
        // start is spent.
        <div
          key={grant.metric}
          className="flex items-center gap-3 px-4 py-2 rounded-lg border border-amber-500/40 bg-zinc-900/95 text-amber-300 text-sm shadow-lg"
        >
          <ShieldOff className="w-4 h-4 shrink-0" />
          <span aria-hidden="true">{grantBannerText(grant, now)}</span>
          <span className="sr-only" role="status">
            {grantAnnouncement(grant)}
          </span>
          <button
            type="button"
            onClick={() => revokeResourceGuardGrant(grant.metric)}
            className="px-2 py-0.5 text-xs font-medium rounded border border-amber-500/40 hover:bg-amber-500/10 transition-colors"
          >
            Revoke
          </button>
        </div>
      ))}
    </div>
  );
}
