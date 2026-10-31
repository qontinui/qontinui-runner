/**
 * The Session Manager's Live-list sections — every lens goes through
 * {@link groupSessions} (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3).
 *
 * ## Lenses
 *
 * - `groups` ("My groups", the default — D1): a Pinned section of compact
 *   rows, then each operator group in its stored order, then Ungrouped. A
 *   group with no row in the list still gets its header (Chrome keeps an
 *   empty group until it is closed), and a collapsed group keeps its header,
 *   its member count and its `Needs you` count — only the rows are hidden.
 * - `status` / `account` / `project` / `date`: the automatic lenses, one
 *   section per distinct key, in first-seen order of the (already sorted)
 *   rows.
 *
 * ## Order
 *
 * Inside a group, rows follow the stored member order — that is the order a
 * drag sets. Pinned and Ungrouped keep the incoming order, which is the sort
 * control's, so Ungrouped "can be sorted" (D1) and a drag inside it is a
 * no-op.
 *
 * ## Org unknown
 *
 * When the organization store is `loading` or `unknown`, `groups` cannot be
 * built honestly: no row may read "Ungrouped" when its group is simply
 * unread. The lens then answers ONE `org-unavailable` section holding every
 * row, which the panel labels with the reason (served policy
 * `verification-and-evidence` `unknown-must-not-render-as-a-default`).
 *
 * Also here: the drag-and-drop op builders ({@link memberDropOp},
 * {@link groupDropOp}). Both answer `null` for a drop that would leave
 * everything where it is, so a same-position drag sends nothing.
 *
 * Pure: no React, no I/O.
 */

import type { MemberOrg, SessionGroup, SessionOrg, SessionOrgOp } from "./sessionOrgTypes";
import { orderedMemberKeys } from "./sessionOrgOptimistic";
import { SESSION_STATUS_LABELS, type SessionActivity } from "./sessionStatusModel";

/** The Live list's grouping lenses; `groups` is "My groups". */
export const SESSION_GROUP_BY_LENSES = ["groups", "status", "account", "project", "date"] as const;

export type SessionGroupBy = (typeof SESSION_GROUP_BY_LENSES)[number];

/** The lens a window with nothing stored opens on (D1). */
export const DEFAULT_GROUP_BY: SessionGroupBy = "groups";

/** A stored lens value, or the default for anything else (a retired value included). */
export function parseGroupBy(raw: unknown): SessionGroupBy {
  return (SESSION_GROUP_BY_LENSES as readonly unknown[]).includes(raw)
    ? (raw as SessionGroupBy)
    : DEFAULT_GROUP_BY;
}

/** What a section needs from a row. `UnifiedSession` satisfies it. */
export interface GroupableSession {
  sessionId: string;
  accountLabel: string;
  projectLabel: string;
  lastModified: string;
  status: { activity: SessionActivity };
}

/** The org as the `groups` lens sees it — `SessionOrgView` narrows to it. */
export type GroupingOrg =
  | { phase: "known"; org: Pick<SessionOrg, "groups" | "members"> }
  | { phase: "loading" }
  | { phase: "unknown"; reason: string };

export type SessionSection<S> =
  | { kind: "pinned"; key: "pinned"; title: string; sessions: S[] }
  | {
      kind: "group";
      key: string;
      group: SessionGroup;
      /** The rows to render — empty when the group is collapsed. */
      sessions: S[];
      /** Rows in the list that belong to the group (pinned members excluded). */
      shownCount: number;
      /** Every stored member, including sessions this list does not show. */
      memberCount: number;
      /** Rows reading `Needs you`, counted whether or not the group is collapsed. */
      needsYouCount: number;
      collapsed: boolean;
    }
  | { kind: "ungrouped"; key: "ungrouped"; title: string; sessions: S[] }
  | { kind: "lens"; key: string; title: string; sessions: S[] }
  | { kind: "org-unavailable"; key: "org-unavailable"; title: string; sessions: S[] };

export const PINNED_SECTION_TITLE = "Pinned";
export const UNGROUPED_SECTION_TITLE = "Ungrouped";

export interface GroupSessionsInput<S> {
  sessions: readonly S[];
  groupBy: SessionGroupBy;
  /** Read only by the `groups` lens. */
  org: GroupingOrg;
  /** The organization-store key of a row (`sessionKeyFor`). */
  keyOf: (session: S) => string;
  /** "Today" for the date lens; injectable for tests. */
  now?: Date;
}

/** The sections of the Live list for `groupBy`. */
export function groupSessions<S extends GroupableSession>(
  input: GroupSessionsInput<S>,
): SessionSection<S>[] {
  if (input.groupBy === "groups") return groupByOrg(input);
  const now = input.now ?? new Date();
  const sections = new Map<string, S[]>();
  for (const session of input.sessions) {
    const key = lensKey(session, input.groupBy, now);
    const list = sections.get(key);
    if (list) list.push(session);
    else sections.set(key, [session]);
  }
  return [...sections.entries()].map(([key, sessions]) => ({
    kind: "lens",
    key,
    title: key,
    sessions,
  }));
}

function lensKey(
  session: GroupableSession,
  groupBy: Exclude<SessionGroupBy, "groups">,
  now: Date,
): string {
  switch (groupBy) {
    case "status":
      // The same word the card's dot carries — one label table for both.
      return SESSION_STATUS_LABELS.activity[session.status.activity];
    case "account":
      return session.accountLabel;
    case "project":
      return session.projectLabel || "Unknown";
    case "date":
      return formatDateGroupKey(session.lastModified, now);
  }
}

/** "Today" / "Yesterday" / "This Week" / a short date; "Unknown" when unparseable. */
export function formatDateGroupKey(iso: string, now: Date = new Date()): string {
  if (!iso) return "Unknown";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "Unknown";
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = Math.round((today.getTime() - target.getTime()) / 86400000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  if (diff > 1 && diff < 7) return "This Week";
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function groupByOrg<S extends GroupableSession>(input: GroupSessionsInput<S>): SessionSection<S>[] {
  const { org, sessions, keyOf } = input;
  if (org.phase !== "known") {
    return [
      {
        kind: "org-unavailable",
        key: "org-unavailable",
        title: org.phase === "loading" ? "Groups: loading" : `Groups unknown — ${org.reason}`,
        sessions: [...sessions],
      },
    ];
  }

  const members = org.org.members;
  const memberOf = (s: S): MemberOrg | undefined => members[keyOf(s)];
  const groupIds = new Set(org.org.groups.map((g) => g.id));

  const pinned: S[] = [];
  const ungrouped: S[] = [];
  const byGroup = new Map<string, S[]>();
  for (const session of sessions) {
    const m = memberOf(session);
    if (m?.pinned) {
      pinned.push(session);
      continue;
    }
    // A member naming a group the org no longer has reads as ungrouped — the
    // store releases such members itself; this only keeps the row visible.
    const groupId = m?.groupId != null && groupIds.has(m.groupId) ? m.groupId : null;
    if (groupId === null) {
      ungrouped.push(session);
      continue;
    }
    const list = byGroup.get(groupId);
    if (list) list.push(session);
    else byGroup.set(groupId, [session]);
  }

  const memberCounts = new Map<string, number>();
  for (const m of Object.values(members)) {
    if (m.groupId != null) memberCounts.set(m.groupId, (memberCounts.get(m.groupId) ?? 0) + 1);
  }

  const out: SessionSection<S>[] = [];
  if (pinned.length > 0) {
    out.push({ kind: "pinned", key: "pinned", title: PINNED_SECTION_TITLE, sessions: pinned });
  }
  const groups = [...org.org.groups].sort((a, b) => a.order - b.order);
  for (const group of groups) {
    const rows = [...(byGroup.get(group.id) ?? [])];
    // Stored member order; ties (no record yet) keep the incoming order,
    // because `Array.prototype.sort` is stable.
    rows.sort((a, b) => (memberOf(a)?.order ?? 0) - (memberOf(b)?.order ?? 0));
    out.push({
      kind: "group",
      key: `group:${group.id}`,
      group,
      sessions: group.collapsed ? [] : rows,
      shownCount: rows.length,
      memberCount: memberCounts.get(group.id) ?? 0,
      needsYouCount: rows.filter((s) => s.status.activity === "needs-you").length,
      collapsed: group.collapsed,
    });
  }
  out.push({
    kind: "ungrouped",
    key: "ungrouped",
    title: UNGROUPED_SECTION_TITLE,
    sessions: ungrouped,
  });
  return out;
}

// ── Drag and drop ────────────────────────────────────────────────────────────

/** Where a dragged session row is dropped. */
export interface MemberDropTarget {
  /** The group dropped into; `null` = Ungrouped. */
  groupId: string | null;
  /**
   * Insert before this member; `null` = at the end of the group. Ignored for
   * Ungrouped, which follows the sort control.
   */
  beforeKey: string | null;
}

/**
 * The op a session-row drop sends, or `null` when it would change nothing:
 * the row is already at that position in that group, or it is dropped back
 * into Ungrouped from Ungrouped.
 */
export function memberDropOp(
  org: Pick<SessionOrg, "groups" | "members">,
  sessionKey: string,
  target: MemberDropTarget,
): SessionOrgOp | null {
  const current = org.members[sessionKey]?.groupId ?? null;
  if (target.groupId === null) {
    return current === null ? null : { op: "removeMembers", sessionKeys: [sessionKey] };
  }
  if (!org.groups.some((g) => g.id === target.groupId)) return null;
  // Dropped onto itself: it stays where it is.
  if (target.beforeKey === sessionKey && current === target.groupId) return null;
  const others = orderedMemberKeys(org as SessionOrg, target.groupId).filter(
    (k) => k !== sessionKey,
  );
  const at = target.beforeKey === null ? -1 : others.indexOf(target.beforeKey);
  const index = at < 0 ? others.length : at;
  if (current === target.groupId) {
    const now = orderedMemberKeys(org as SessionOrg, current).indexOf(sessionKey);
    if (now === index) return null;
  }
  return { op: "moveMember", sessionKey, groupId: target.groupId, index };
}

/**
 * The op a group-header drop sends: move `groupId` to sit before
 * `beforeGroupId` (`null` = last). `null` when it is already there.
 */
export function groupDropOp(
  org: Pick<SessionOrg, "groups">,
  groupId: string,
  beforeGroupId: string | null,
): SessionOrgOp | null {
  const ordered = [...org.groups].sort((a, b) => a.order - b.order).map((g) => g.id);
  const from = ordered.indexOf(groupId);
  if (from < 0 || beforeGroupId === groupId) return null;
  const others = ordered.filter((id) => id !== groupId);
  const at = beforeGroupId === null ? -1 : others.indexOf(beforeGroupId);
  const index = at < 0 ? others.length : at;
  if (index === from) return null;
  return { op: "moveGroup", groupId, index };
}
