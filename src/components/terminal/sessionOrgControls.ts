/**
 * Every Session Manager organization control's UI Bridge id and its declared
 * effect, in one table (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3; served policy `testing`
 * `an-actions-safety-class-is-declared-not-re-derived`).
 *
 * Each control renders `data-ui-bridge-id={id}` and `data-effect={effect}` so
 * a driver reads the class off the element it is about to click, and the
 * `session-org` component (`useSessionOrgBridge`) declares the same class on
 * the matching action. Close and Close group end live conversation processes
 * (D7), so they — and Force close — are `destructive`.
 *
 * Per-row and per-group controls append the row's / group's id to the base
 * (`sessionOrgControlId`), the convention `sessionCardElementId` set.
 */

import type { IREffect } from "@qontinui/ui-bridge";

export interface SessionOrgControl {
  id: string;
  effect: IREffect;
}

const control = (name: string, effect: IREffect): SessionOrgControl => ({
  id: `terminal.session-org.${name}`,
  effect,
});

export const SESSION_ORG_CONTROLS = {
  // Panel
  railToggle: control("rail-toggle", "read"),
  resizeHandle: control("resize-handle", "read"),
  suggestGroups: control("suggest-groups", "read"),
  pinnedSection: control("pinned-section", "read"),
  ungroupedSection: control("ungrouped-section", "read"),
  railItem: control("rail-item", "read"),
  // Group header
  groupHeader: control("group-header", "read"),
  groupCollapse: control("group-collapse", "write"),
  groupRenameInput: control("group-rename-input", "write"),
  groupMenuRename: control("group-menu.rename", "write"),
  groupMenuColor: control("group-menu.color", "write"),
  groupMenuCollapse: control("group-menu.collapse", "write"),
  groupMenuNewSession: control("group-menu.new-session", "write"),
  groupMenuMoveToPage: control("group-menu.move-to-page", "write"),
  groupMenuUngroup: control("group-menu.ungroup", "write"),
  groupMenuCloseGroup: control("group-menu.close-group", "destructive"),
  groupMenuDelete: control("group-menu.delete", "write"),
  // Session context menu
  sessionMenuNewGroup: control("session-menu.new-group", "write"),
  sessionMenuAddToGroup: control("session-menu.add-to-group", "write"),
  sessionMenuRemoveFromGroup: control("session-menu.remove-from-group", "write"),
  sessionMenuPin: control("session-menu.pin", "write"),
  sessionMenuMoveToPage: control("session-menu.move-to-page", "write"),
  sessionMenuMute: control("session-menu.mute", "write"),
  sessionMenuSnooze: control("session-menu.snooze", "write"),
  sessionMenuTag: control("session-menu.tag", "write"),
  sessionMenuClose: control("session-menu.close", "destructive"),
  // Close confirmation
  closeDialog: control("close-dialog", "read"),
  closeTick: control("close-tick", "read"),
  closeConfirm: control("close-confirm", "destructive"),
  closeCancel: control("close-cancel", "read"),
  forceClose: control("force-close", "read"),
  forceCloseConfirm: control("force-close-confirm", "destructive"),
  closeInclude: control("close-include", "read"),
  // Triage (Phase 5): the list, the bulk bar, Ready to close, Recently closed
  sessionList: control("session-list", "read"),
  bulkBar: control("bulk-bar", "read"),
  bulkClose: control("bulk.close", "destructive"),
  bulkFinish: control("bulk.finish", "write"),
  bulkAddToGroup: control("bulk.add-to-group", "write"),
  bulkNewGroup: control("bulk.new-group", "write"),
  bulkPin: control("bulk.pin", "write"),
  bulkUnpin: control("bulk.unpin", "write"),
  bulkMute: control("bulk.mute", "write"),
  bulkSnooze: control("bulk.snooze", "write"),
  bulkMoveToPage: control("bulk.move-to-page", "write"),
  bulkResume: control("bulk.resume", "write"),
  bulkClear: control("bulk.clear", "read"),
  bulkResults: control("bulk-results", "read"),
  readyToClose: control("ready-to-close", "read"),
  readyToCloseReview: control("ready-to-close.review", "destructive"),
  recentlyClosed: control("recently-closed", "read"),
  recentlyClosedResume: control("recently-closed.resume", "write"),
} as const satisfies Record<string, SessionOrgControl>;

export type SessionOrgControlName = keyof typeof SESSION_ORG_CONTROLS;

/** A per-item control id: the base id plus the row's or group's id. */
export function sessionOrgControlId(name: SessionOrgControlName, itemId: string): string {
  return `${SESSION_ORG_CONTROLS[name].id}-${itemId}`;
}

/** The two attributes every control renders. */
export function sessionOrgControlAttrs(
  name: SessionOrgControlName,
  itemId?: string,
): { "data-ui-bridge-id": string; "data-effect": IREffect } {
  return {
    "data-ui-bridge-id":
      itemId === undefined ? SESSION_ORG_CONTROLS[name].id : sessionOrgControlId(name, itemId),
    "data-effect": SESSION_ORG_CONTROLS[name].effect,
  };
}
