/**
 * Shared constants for the lock-yield protocol UI (lock-yield-protocol-plan
 * SHIPPED 2026-05-12, archived in qontinui-dev-notes/plans/).
 *
 * Hoisted out of WaitingLockBanner.tsx and FileActivityPanel.tsx so both
 * surfaces — the per-tab waiting banner in `components/terminal/` and the
 * dashboard row action in `components/productivity/` — share one source of
 * truth without crossing the productivity ↔ terminal folder boundary.
 */

/**
 * Cooldown applied after a "Request yield" click, in milliseconds.
 * Prevents the requester from spamming yield-request POSTs at the holder.
 *
 * The same value is asserted by tripwire tests in both
 * `WaitingLockBanner.test.tsx` and `FileActivityPanel.test.tsx` — if you
 * change this, update both tripwires in lockstep.
 */
export const REQUEST_YIELD_COOLDOWN_MS = 30_000;
