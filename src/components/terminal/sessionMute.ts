/**
 * Session mute — Chrome's "Mute site", applied to one session (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7).
 *
 * Mute suppresses exactly two things for the muted session: the needs-input
 * SOUND and the needs-input desktop NOTIFICATION (the "toast"). It hides no
 * state — the row still reads `Needs you`, the zone still flashes, the window
 * title still counts it — because a muted session that needs the operator
 * still needs him; mute only stops it interrupting.
 *
 * A muted flag the runner could not read is NOT muted: an unknown store must
 * never silence a session (served policy `verification-and-evidence`
 * `unknown-must-not-render-as-a-default` — silence would be a default).
 *
 * Pure: no React, no Tauri.
 */

import { sessionKeyFor } from "./sessionOrgTypes";
import type { SessionOrgView } from "./useSessionOrg";

/** The tab fields a tab's organization key is built from. */
export interface KeyedTab {
  id: string;
  title: string;
  spawnName?: string;
  workingDir?: string;
  claudeSessionId?: string;
  remote?: { deviceId: string };
}

export interface TabKeyContext {
  pageId: string;
  windowLabel: string;
}

/**
 * A tab's organization-store key: its Claude session id when it has one,
 * else its window-aware pane key. The one spelling the Session Manager's rows
 * and the notification surfaces share, so a mute set on a row reaches its tab.
 */
export function tabSessionKey(tab: KeyedTab, ctx: TabKeyContext): string {
  return sessionKeyFor(tab.claudeSessionId ?? null, {
    pageId: ctx.pageId,
    title: tab.spawnName ?? tab.title,
    workingDir: tab.workingDir ?? "",
    windowLabel: ctx.windowLabel,
    remoteDeviceId: tab.remote?.deviceId,
  });
}

/** The tab ids whose session is muted. Empty while the store is not known. */
export function mutedTabIds(
  tabs: readonly KeyedTab[],
  org: SessionOrgView,
  ctx: TabKeyContext,
): ReadonlySet<string> {
  const muted = new Set<string>();
  if (org.phase !== "known") return muted;
  for (const tab of tabs) {
    if (org.org.members[tabSessionKey(tab, ctx)]?.muted) muted.add(tab.id);
  }
  return muted;
}

/**
 * The tabs that just entered `needs-input` and may make a sound or raise a
 * notification — every one except the muted ones, in order.
 */
export function audibleNeedsInput(
  newNeedsInput: readonly string[],
  muted: ReadonlySet<string>,
): string[] {
  return newNeedsInput.filter((id) => !muted.has(id));
}
