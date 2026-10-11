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
 * pinned Claude sessions to zones — resolve each session's account
 * (`resumeAccount.ts`), park them on `pendingProfileSessionsRef` (the
 * `PageSessionScope` effect types the `--resume` command once each zone has
 * an assignment), and give each session a tab: existing tabs first, then a
 * freshly spawned pane pinned to the session's account.
 */

import { useCallback, useEffect, useRef } from "react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { useTransitionEffects } from "./contexts";
import type { ZoneProfile } from "./zoneProfileStorage";
import { planProfileTabs, withResolvedResumeAccounts } from "./resumeAccount";

export interface ApplyZoneProfileDeps {
  /**
   * `useZoneActions().createAndAssignTerminal` — spawns a terminal AND drops
   * it into a zone (plain `createTerminal` would leave it unzoned).
   */
  createAndAssignTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
    claudeConfigDir?: string,
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

  // Monotonic apply counter — see the supersede check inside the callback.
  const generationRef = useRef(0);

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

    // A later apply supersedes this one at every await below: its sessions,
    // not this (older, slower) call's, are the ones to park.
    const generation = ++generationRef.current;
    // Drop sessions an earlier apply parked but never typed.
    pendingRef.current = null;

    // Resolve every session's account BEFORE parking it: the resume typed
    // for it (and the pin on a pane spawned for it) must name the account its
    // transcript is under. A profile saved without an account (null/undefined
    // `claudeConfigDir`) otherwise resumed under the spawn picker's account
    // and answered "No conversation found".
    const sessions = await withResolvedResumeAccounts(profile.sessions);
    if (generation !== generationRef.current) return;

    // Existing tabs are reused first, in order; every remaining session gets a
    // freshly spawned pane PINNED to its resolved account, placed in its own
    // zone explicitly. A session is parked only AFTER its own tab is placed,
    // bound to that tab (`expectedTabId`): a freshly spawned pane can be
    // auto-filled into ANOTHER session's zone before its explicit placement
    // lands, and a session parked earlier would have its resume typed into it
    // — then the pane's own session typed on top, into a live Claude.
    for (const step of planProfileTabs(
      sessions,
      liveTabs.map((t) => t.id),
    )) {
      let tabId: string | null | undefined = step.reuseTabId;
      if (!tabId) {
        // A new pane spawned NOW (not a restored session carrying a stored
        // tenant), so it records the current acting tenant like any other
        // new spawn.
        tabId = await createAndAssignTerminal(
          undefined,
          undefined,
          resolveTenantForSpawn(),
          step.pin,
        );
        if (generation !== generationRef.current) return;
        if (!tabId) continue;
      }
      zl.assignTabToZone(step.session.zoneIndex, tabId);
      pendingRef.current = [
        ...(pendingRef.current ?? []),
        { ...step.session, expectedTabId: tabId },
      ];
    }
  }, []);
}
