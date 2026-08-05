/**
 * `useZoneProfileRestore` — apply a saved zone profile BY NAME.
 *
 * Projects-dashboard plan §7.2 step 3: activating a project must "restore the
 * project's `zone_profile` if set". The project record stores a profile
 * *name* (`SavedProject.zone_profile`), so activation needs the picker's
 * load-by-name behaviour without the picker's dropdown. This hook composes
 * the two extracted pieces rather than re-implementing either:
 *
 *   - `zoneProfileStorage` — the profile map + its page-namespaced settings
 *     keys (the picker reads/writes the exact same functions);
 *   - `useApplyZoneProfile` — the application logic, lifted out of
 *     `TerminalPage`'s inline `onLoadProfile`.
 *
 * The active-profile pointer is written on success, so a restore is
 * indistinguishable from the operator loading the profile by hand — including
 * the picker re-rendering, since `setting_set` emits `setting-changed` and the
 * picker refetches on it.
 *
 * A window `CustomEvent` bridge is also registered so a caller OUTSIDE the
 * terminal provider tree (the Projects page, which cannot call
 * `useTerminalSession()`) can trigger a restore. This mirrors the existing
 * `terminal-open-page` idiom in `useTerminalPages` rather than prop-drilling a
 * restore function up through `App`.
 */

import { useCallback, useEffect } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { loadProfilesFromDb, saveActiveProfileToDb, type ZoneProfile } from "./zoneProfileStorage";

/** Window event a non-terminal surface dispatches to request a restore. */
export const RESTORE_ZONE_PROFILE_EVENT = "terminal-restore-zone-profile";

export interface RestoreZoneProfileDetail {
  /** The saved profile's name (`SavedProject.zone_profile`). */
  profileName: string;
  /** Defaults to the active terminal page. */
  pageId?: string;
}

export type RestoreProfileByName = (
  profileName: string,
  options?: { pageId?: string },
) => Promise<boolean>;

/**
 * The most recent restore request no page has consumed yet.
 *
 * A window event only reaches listeners that are ALREADY mounted, and a
 * project activation dispatches one in the same commit that switches to the
 * project's page. When that page is brand-new (or was re-created from a
 * dangling id), `TerminalSessionProvider` is still showing its loading gate at
 * that moment — `TerminalPage`, and therefore this hook's listener, is not
 * mounted yet, so the event lands on nobody. That case is not hypothetical:
 * zone profiles live in the runner's Rust settings, page-namespaced, so a
 * re-created page CAN own profiles even though `instanceStorage` has never
 * heard of it.
 *
 * So the request is also queued here. `useZoneProfileRestore`'s effect drains
 * it on mount and on every page switch, which is exactly when the missing
 * listener appears. Module-level (not React state) because the producer is
 * outside the terminal tree entirely.
 */
let pendingRestore: RestoreZoneProfileDetail | null = null;

/**
 * Take the queued request when it targets `pageId` (or names no page at all).
 * Consuming CLEARS it — a restore is one-shot, never replayed on the next
 * page switch. Exported so the hand-off contract is unit-testable without
 * React.
 */
export function takePendingZoneProfileRestore(pageId: string): RestoreZoneProfileDetail | null {
  if (!pendingRestore) return null;
  const target = pendingRestore.pageId?.trim();
  if (target && target !== pageId) return null;
  const taken = pendingRestore;
  pendingRestore = null;
  return taken;
}

/**
 * Drop any queued request. Activating a project that has NO `zone_profile`
 * calls this, so a previous project's undelivered restore can never surface
 * later on an unrelated page switch.
 */
export function cancelZoneProfileRestore(): void {
  pendingRestore = null;
}

/**
 * Returns `restoreProfileByName(name)` → `true` when a profile with that name
 * existed on the target page and was applied, `false` when it did not.
 *
 * Never throws on a missing profile: a project can name a profile the operator
 * has since deleted, and that must degrade to "nothing to restore", not to a
 * failed activation — the same tolerance the dangling-page-id path has.
 */
export function useZoneProfileRestore(
  applyProfile: (profile: ZoneProfile) => Promise<void>,
): RestoreProfileByName {
  const { pageId } = useTerminalSession();

  const restoreProfileByName = useCallback<RestoreProfileByName>(
    async (profileName, options) => {
      const name = profileName?.trim();
      if (!name) return false;
      const targetPage = options?.pageId?.trim() || pageId;
      const profiles = await loadProfilesFromDb(targetPage);
      const profile = profiles[name];
      if (!profile) return false;
      await applyProfile(profile);
      // Mirror the picker: a loaded profile becomes the page's active one, so
      // a later boot auto-applies it and the picker's chip shows the name.
      await saveActiveProfileToDb(name, targetPage);
      return true;
    },
    [pageId, applyProfile],
  );

  useEffect(() => {
    // Drain a request that was dispatched before this page's tree existed
    // (see `pendingRestore`). `restoreProfileByName` changes identity with
    // `pageId`, so this re-runs on every page switch — which is precisely when
    // the previously-missing listener appears.
    const queued = takePendingZoneProfileRestore(pageId);
    if (queued?.profileName) {
      void restoreProfileByName(queued.profileName, { pageId: queued.pageId });
    }

    const handler = (e: Event) => {
      const detail = (e as CustomEvent<RestoreZoneProfileDetail>).detail;
      if (!detail?.profileName) return;
      // Claim it out of the queue so the mount-drain above cannot apply the
      // same profile a second time on the next page switch.
      takePendingZoneProfileRestore(detail.pageId?.trim() || pageId);
      void restoreProfileByName(detail.profileName, { pageId: detail.pageId });
    };
    window.addEventListener(RESTORE_ZONE_PROFILE_EVENT, handler as EventListener);
    return () => window.removeEventListener(RESTORE_ZONE_PROFILE_EVENT, handler as EventListener);
  }, [restoreProfileByName, pageId]);

  return restoreProfileByName;
}

/**
 * Fire-and-forget restore request from outside the terminal tree (e.g. the
 * Projects page's activation flow). Never throws and never blocks — an
 * activation must not depend on the terminal tree being mounted.
 *
 * The request is QUEUED as well as dispatched: when the target page's tree is
 * not mounted yet the event lands on nobody, and the queue is what lets the
 * page pick it up the moment it mounts (see `pendingRestore`).
 */
export function requestZoneProfileRestore(detail: RestoreZoneProfileDetail): void {
  pendingRestore = detail;
  window.dispatchEvent(new CustomEvent(RESTORE_ZONE_PROFILE_EVENT, { detail }));
}
