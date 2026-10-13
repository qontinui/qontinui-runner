/**
 * Wire types for the Session Manager organization store — the TypeScript
 * mirror of `src-tauri/src/session/session_org_store.rs` (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 2). Field names are the serde camelCase spelling the Tauri commands
 * (`session_org_get` / `session_org_apply`), the loopback twins
 * (`GET/POST /terminal/session-org`) and the `session-org-changed` event all
 * carry.
 */

/** The event every window listens on; mirrors `SESSION_ORG_CHANGED_EVENT`. */
export const SESSION_ORG_CHANGED_EVENT = "session-org-changed";

/**
 * A group color as a ROLE name, never a color value (D4). The six
 * non-attention roles, each named for the `@qontinui/design-tokens` token it
 * resolves from; red and amber are reserved to the attention layer. Mirrors
 * `ColorRole`, in the order a new group takes the next unused one.
 */
export const SESSION_GROUP_COLOR_ROLES = [
  "brand-primary",
  "brand-secondary",
  "brand-success",
  "info",
  "border-strong",
  "text-muted",
] as const;

export type SessionGroupColorRole = (typeof SESSION_GROUP_COLOR_ROLES)[number];

export interface SessionGroup {
  id: string;
  title: string;
  colorRole: SessionGroupColorRole;
  collapsed: boolean;
  /** Position among groups. */
  order: number;
  /** Unix millis. */
  createdAt: number;
}

export interface MemberOrg {
  groupId: string | null;
  /** Position within its group, or within Ungrouped. */
  order: number;
  pinned: boolean;
  /** Sorted, deduplicated. */
  tags: string[];
  muted: boolean;
  /** Unix millis; null = not snoozed. */
  snoozedUntil: number | null;
  /** Unix millis of the last focus. */
  lastSeenAt: number | null;
}

export interface ClosedSession {
  sessionKey: string;
  title: string;
  closedAt: number;
  resumeCmd: string | null;
  /** The group the session was in when it closed. */
  groupId: string | null;
}

export interface SavedView {
  name: string;
  query: string;
  savedAt: number;
}

export interface SessionOrg {
  version: number;
  /** Bumped by every apply that changed something. */
  revision: number;
  groups: SessionGroup[];
  members: Record<string, MemberOrg>;
  /** Newest first; bounded — `recentlyClosed.length` of `recentlyClosedTotal`. */
  recentlyClosed: ClosedSession[];
  recentlyClosedTotal: number;
  savedViews: SavedView[];
  dismissedSuggestions: string[];
}

/** `org` is null exactly when `status` is "unknown" — never an empty org. */
export interface SessionOrgSnapshot {
  status: "known" | "unknown";
  org: SessionOrg | null;
  reason: string | null;
  preservedAt: string | null;
}

/** One mutation; mirrors `SessionOrgOp`. Each names only the fields it changes. */
export type SessionOrgOp =
  | {
      op: "createGroup";
      groupId?: string;
      title: string;
      colorRole?: SessionGroupColorRole;
      sessionKeys?: string[];
    }
  | { op: "renameGroup"; groupId: string; title: string }
  | { op: "recolorGroup"; groupId: string; colorRole: SessionGroupColorRole }
  | { op: "setGroupCollapsed"; groupId: string; collapsed: boolean }
  | { op: "moveGroup"; groupId: string; index: number }
  /** Release every member, keep the empty group. */
  | { op: "ungroupGroup"; groupId: string }
  /** Remove the group; its members become ungrouped, never removed. */
  | { op: "deleteGroup"; groupId: string }
  | { op: "addMembers"; groupId: string; sessionKeys: string[] }
  | { op: "removeMembers"; sessionKeys: string[] }
  | { op: "moveMember"; sessionKey: string; groupId: string | null; index: number }
  | { op: "setPinned"; sessionKeys: string[]; pinned: boolean }
  | { op: "addTags"; sessionKeys: string[]; tags: string[] }
  | { op: "removeTags"; sessionKeys: string[]; tags: string[] }
  | { op: "setMuted"; sessionKeys: string[]; muted: boolean }
  | { op: "snooze"; sessionKeys: string[]; until: number | null }
  | { op: "markSeen"; sessionKey: string; at: number }
  | {
      op: "recordClosed";
      sessionKey: string;
      title: string;
      closedAt: number;
      resumeCmd?: string | null;
    }
  | { op: "saveView"; name: string; query: string }
  | { op: "deleteView"; name: string }
  | { op: "dismissSuggestion"; key: string }
  /** The one op accepted while the org is unknown: start a fresh org. */
  | { op: "resetAfterUnknown" };

export interface SessionOrgOpResult {
  ok: boolean;
  /** Machine-readable refusal code when `ok` is false. */
  error: string | null;
  /** The id of a group this op created. */
  groupId: string | null;
}

export interface SessionOrgApplyResponse {
  results: SessionOrgOpResult[];
  snapshot: SessionOrgSnapshot;
}

// ── Session keys ─────────────────────────────────────────────────────────────

/** Unit separator — `PaneKey`'s field delimiter (`session/pane_store.rs`). */
const PANE_KEY_SEP = "\u001f";
/** `crate::window_assignments::MAIN_WINDOW_LABEL`. */
const MAIN_WINDOW_LABEL = "main";

export interface PaneIdentity {
  pageId: string;
  title: string;
  workingDir: string;
  /** Terminal window label; empty or "main" is the main window. */
  windowLabel?: string;
  /** Set for a tab attached to a session on another device. */
  remoteDeviceId?: string;
}

/**
 * The string form of `PaneKey::from_create_in_window` / `from_create_remote`.
 * Byte-for-byte the Rust key — both test suites pin the same literal.
 */
export function paneKeyString(pane: PaneIdentity): string {
  const base = [pane.pageId.trim(), pane.title.trim(), pane.workingDir.trim()];
  if (pane.remoteDeviceId !== undefined) {
    return [...base, "remote", pane.remoteDeviceId.trim()].join(PANE_KEY_SEP);
  }
  const label = (pane.windowLabel ?? "").trim();
  if (label === "" || label === MAIN_WINDOW_LABEL) return base.join(PANE_KEY_SEP);
  return [...base, label].join(PANE_KEY_SEP);
}

/**
 * The key a session's organization is stored under: the Claude session id
 * when known, else `pane:` + the window-aware pane key. Terminal ids are never
 * a key — they are a fresh uuid per launch. Mirrors Rust `session_key`.
 */
export function sessionKeyFor(
  claudeSessionId: string | null | undefined,
  pane: PaneIdentity,
): string {
  const id = claudeSessionId?.trim();
  if (id) return id;
  return `pane:${paneKeyString(pane)}`;
}
