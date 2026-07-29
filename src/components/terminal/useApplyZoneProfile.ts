/**
 * `useApplyZoneProfile` — apply a {@link ZoneProfile} to the active terminal
 * page.
 *
 * Extracted from `TerminalPage`'s inline `ZoneProfilePicker.onLoadProfile`
 * closure (its previous only home) so the SAME application logic backs both
 * entry points:
 *
 *   1. the picker / its `load-profile` UI Bridge action, and
 *   2. `useZoneProfileRestore`, which restores a profile by NAME — the path a
 *      project activation uses to reinstate its `zone_profile`
 *      (projects-dashboard plan §7.2 step 3).
 *
 * Behaviour is unchanged from the inline version: set the layout, replace
 * labels / notes / pins / auto-approve patterns, then — when the profile
 * pinned Claude sessions to zones — park them on
 * `pendingProfileSessionsRef` (the `PageSessionScope` effect types the
 * `--resume` command once each zone has an assignment) and spawn just enough
 * blank terminals to fill the profile's zones, or, when there are already
 * enough tabs, assign the existing ones.
 */

import { useCallback, useEffect, useRef } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { useTransitionEffects } from "./contexts";
import type { ZoneProfile } from "./zoneProfileStorage";

export interface ApplyZoneProfileDeps {
  /**
   * `useZoneActions().createAndAssignTerminal` — spawns a terminal AND drops
   * it into a zone (plain `createTerminal` would leave it unzoned).
   */
  createAndAssignTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
  ) => Promise<string | null>;
  /** `TerminalPage`'s spawn-tenant precedence resolver. */
  resolveTenantForSpawn: (explicit?: string) => string | undefined;
}

/**
 * Returns a STABLE `applyProfile(profile)` callback.
 *
 * Everything it needs is read through a ref refreshed in an effect rather than
 * captured in a dependency list, for two reasons:
 *
 *   - `resolveTenantForSpawn` (and `createAndAssignTerminal`'s enclosing
 *     render) change identity on most `TerminalPage` renders, so listing them
 *     would produce a new callback every render — and `ZoneProfilePicker`
 *     stores this callback in a ref and can invoke it from its auto-apply
 *     effect long after the render that produced it;
 *   - the ref-in-effect (not ref-during-render) shape is the same one
 *     `ZoneProfilePicker.onLoadProfileRef` and `TerminalSessionContext`'s
 *     cross-hook setters already use.
 */
export function useApplyZoneProfile(
  deps: ApplyZoneProfileDeps,
): (profile: ZoneProfile) => Promise<void> {
  const { tabs, zoneLayout, labelsAndTags, pendingProfileSessionsRef } = useTerminalSession();
  const transitionEffects = useTransitionEffects();

  const latest = useRef({
    deps,
    tabs,
    zoneLayout,
    labelsAndTags,
    transitionEffects,
    pendingProfileSessionsRef,
  });
  useEffect(() => {
    latest.current = {
      deps,
      tabs,
      zoneLayout,
      labelsAndTags,
      transitionEffects,
      pendingProfileSessionsRef,
    };
  });

  return useCallback(async (profile: ZoneProfile) => {
    const {
      deps: { createAndAssignTerminal, resolveTenantForSpawn },
      tabs: liveTabs,
      zoneLayout: zl,
      labelsAndTags: lt,
      transitionEffects: te,
      pendingProfileSessionsRef: pendingRef,
    } = latest.current;

    zl.setLayoutId(profile.layoutId);
    lt.setZoneLabels(profile.labels);
    lt.setZoneNotes(profile.notes);
    lt.setPinnedZones(new Set(profile.pins));
    te.setAutoApprovePatterns(profile.autoApprovePatterns);

    if (!profile.sessions || profile.sessions.length === 0) return;

    pendingRef.current = profile.sessions;
    const toCreate = Math.max(0, profile.sessions.length - liveTabs.length);
    for (let i = 0; i < toCreate; i++) {
      // Brand-new blank terminals spawned NOW to fill the profile's zones (not
      // restored sessions carrying a stored tenant), so they record the current
      // acting tenant like any other new spawn.
      await createAndAssignTerminal(undefined, undefined, resolveTenantForSpawn());
    }
    if (toCreate === 0) {
      const assignedTabs = new Set<string>();
      for (const s of profile.sessions) {
        const candidate = liveTabs.find((t) => !assignedTabs.has(t.id));
        if (!candidate) break;
        assignedTabs.add(candidate.id);
        zl.assignTabToZone(s.zoneIndex, candidate.id);
      }
    }
  }, []);
}
