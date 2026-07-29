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
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<RestoreZoneProfileDetail>).detail;
      if (!detail?.profileName) return;
      void restoreProfileByName(detail.profileName, { pageId: detail.pageId });
    };
    window.addEventListener(RESTORE_ZONE_PROFILE_EVENT, handler as EventListener);
    return () => window.removeEventListener(RESTORE_ZONE_PROFILE_EVENT, handler as EventListener);
  }, [restoreProfileByName]);

  return restoreProfileByName;
}

/**
 * Fire-and-forget restore request from outside the terminal tree (e.g. the
 * Projects page's activation flow). No-op when the terminal page is not
 * mounted — an activation must not depend on it.
 */
export function requestZoneProfileRestore(detail: RestoreZoneProfileDetail): void {
  window.dispatchEvent(new CustomEvent(RESTORE_ZONE_PROFILE_EVENT, { detail }));
}
