/**
 * Optimistic projection of Session Manager organization ops — what the
 * operator sees between clicking and the runner answering (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 2).
 *
 * This is a PREVIEW, not an authority: `useSessionOrg` always replaces it with
 * the state the runner returns. It mirrors the semantics of `apply_op` in
 * `src-tauri/src/session/session_org_store.rs` closely enough that the
 * reconcile is normally invisible, and every op the runner would refuse (blank
 * title, unknown group, blank key) is a no-op here so a refusal never flashes
 * a phantom group. It never mints anything the runner would mint differently:
 * `createGroup` must carry its `groupId` (the client fills one in), and an op
 * without one is skipped. Pure — returns a new org, never mutates its input.
 */

import {
  SESSION_GROUP_COLOR_ROLES,
  type MemberOrg,
  type SessionGroupColorRole,
  type SessionOrg,
  type SessionOrgOp,
} from "./sessionOrgTypes";

const RECENTLY_CLOSED_CAP = 200;

function emptyMember(): MemberOrg {
  return {
    groupId: null,
    order: 0,
    pinned: false,
    tags: [],
    muted: false,
    snoozedUntil: null,
    lastSeenAt: null,
  };
}

function isDefaultMember(m: MemberOrg): boolean {
  return (
    m.groupId === null &&
    m.order === 0 &&
    !m.pinned &&
    m.tags.length === 0 &&
    !m.muted &&
    m.snoozedUntil === null &&
    m.lastSeenAt === null
  );
}

function cleanKeys(keys: readonly string[]): string[] | null {
  const out: string[] = [];
  for (const raw of keys) {
    const key = raw.trim();
    if (!key) return null;
    if (!out.includes(key)) out.push(key);
  }
  return out;
}

/** Members of `groupId` (null = Ungrouped) in display order. */
export function orderedMemberKeys(org: SessionOrg, groupId: string | null): string[] {
  return Object.entries(org.members)
    .filter(([, m]) => m.groupId === groupId)
    .sort(([ka, a], [kb, b]) => a.order - b.order || (ka < kb ? -1 : ka > kb ? 1 : 0))
    .map(([k]) => k);
}

function member(org: SessionOrg, key: string): MemberOrg {
  const existing = org.members[key];
  if (existing) return existing;
  const fresh = emptyMember();
  org.members[key] = fresh;
  return fresh;
}

function renumber(org: SessionOrg, keys: readonly string[]): void {
  keys.forEach((key, i) => {
    member(org, key).order = i;
  });
}

function moveToEnd(org: SessionOrg, keys: readonly string[], target: string | null): void {
  const sources = new Set<string | null>();
  for (const key of keys) {
    const m = member(org, key);
    if (m.groupId !== target) sources.add(m.groupId);
  }
  const targetOrder = orderedMemberKeys(org, target).filter((k) => !keys.includes(k));
  targetOrder.push(...keys);
  for (const key of keys) member(org, key).groupId = target;
  renumber(org, targetOrder);
  for (const source of sources) renumber(org, orderedMemberKeys(org, source));
}

function nextColorRole(org: SessionOrg): SessionGroupColorRole {
  const used = new Set(org.groups.map((g) => g.colorRole));
  return (
    SESSION_GROUP_COLOR_ROLES.find((r) => !used.has(r)) ??
    SESSION_GROUP_COLOR_ROLES[org.groups.length % SESSION_GROUP_COLOR_ROLES.length]
  );
}

function renumberGroups(org: SessionOrg): void {
  org.groups.sort((a, b) => a.order - b.order);
  org.groups.forEach((g, i) => {
    g.order = i;
  });
}

function hasGroup(org: SessionOrg, groupId: string): boolean {
  return org.groups.some((g) => g.id === groupId);
}

function forEachMember(org: SessionOrg, keys: readonly string[], f: (m: MemberOrg) => void): void {
  const clean = cleanKeys(keys);
  if (!clean) return;
  for (const key of clean) f(member(org, key));
}

function cleanTags(tags: readonly string[]): string[] | null {
  const out = tags.map((t) => t.trim());
  return out.every((t) => t.length > 0) ? out : null;
}

function applyOne(org: SessionOrg, op: SessionOrgOp, now: number): void {
  switch (op.op) {
    case "createGroup": {
      const title = op.title.trim();
      const id = op.groupId?.trim();
      const keys = cleanKeys(op.sessionKeys ?? []);
      if (!title || !id || !keys || hasGroup(org, id)) return;
      const order = org.groups.reduce((max, g) => Math.max(max, g.order + 1), 0);
      org.groups.push({
        id,
        title,
        colorRole: op.colorRole ?? nextColorRole(org),
        collapsed: false,
        order,
        createdAt: now,
      });
      moveToEnd(org, keys, id);
      return;
    }
    case "renameGroup": {
      const title = op.title.trim();
      const g = org.groups.find((x) => x.id === op.groupId);
      if (g && title) g.title = title;
      return;
    }
    case "recolorGroup": {
      const g = org.groups.find((x) => x.id === op.groupId);
      if (g) g.colorRole = op.colorRole;
      return;
    }
    case "setGroupCollapsed": {
      const g = org.groups.find((x) => x.id === op.groupId);
      if (g) g.collapsed = op.collapsed;
      return;
    }
    case "moveGroup": {
      renumberGroups(org);
      const from = org.groups.findIndex((g) => g.id === op.groupId);
      if (from < 0) return;
      const [g] = org.groups.splice(from, 1);
      org.groups.splice(Math.min(Math.max(op.index, 0), org.groups.length), 0, g);
      org.groups.forEach((x, i) => {
        x.order = i;
      });
      return;
    }
    case "ungroupGroup":
      if (hasGroup(org, op.groupId)) moveToEnd(org, orderedMemberKeys(org, op.groupId), null);
      return;
    case "deleteGroup":
      if (!hasGroup(org, op.groupId)) return;
      moveToEnd(org, orderedMemberKeys(org, op.groupId), null);
      org.groups = org.groups.filter((g) => g.id !== op.groupId);
      renumberGroups(org);
      return;
    case "addMembers": {
      const keys = cleanKeys(op.sessionKeys);
      if (keys && hasGroup(org, op.groupId)) moveToEnd(org, keys, op.groupId);
      return;
    }
    case "removeMembers": {
      const keys = cleanKeys(op.sessionKeys);
      if (!keys) return;
      moveToEnd(
        org,
        keys.filter((k) => (org.members[k]?.groupId ?? null) !== null),
        null,
      );
      return;
    }
    case "moveMember": {
      const keys = cleanKeys([op.sessionKey]);
      if (!keys || (op.groupId !== null && !hasGroup(org, op.groupId))) return;
      moveToEnd(org, keys, op.groupId);
      const order = orderedMemberKeys(org, op.groupId).filter((k) => k !== keys[0]);
      order.splice(Math.min(Math.max(op.index, 0), order.length), 0, keys[0]);
      renumber(org, order);
      return;
    }
    case "setPinned":
      forEachMember(org, op.sessionKeys, (m) => {
        m.pinned = op.pinned;
      });
      return;
    case "addTags": {
      const tags = cleanTags(op.tags);
      if (!tags) return;
      forEachMember(org, op.sessionKeys, (m) => {
        m.tags = [...new Set([...m.tags, ...tags])].sort();
      });
      return;
    }
    case "removeTags": {
      const tags = cleanTags(op.tags);
      if (!tags) return;
      forEachMember(org, op.sessionKeys, (m) => {
        m.tags = m.tags.filter((t) => !tags.includes(t));
      });
      return;
    }
    case "setMuted":
      forEachMember(org, op.sessionKeys, (m) => {
        m.muted = op.muted;
      });
      return;
    case "snooze":
      forEachMember(org, op.sessionKeys, (m) => {
        m.snoozedUntil = op.until;
      });
      return;
    case "markSeen":
      forEachMember(org, [op.sessionKey], (m) => {
        if (m.lastSeenAt === null || op.at > m.lastSeenAt) m.lastSeenAt = op.at;
      });
      return;
    case "recordClosed": {
      const keys = cleanKeys([op.sessionKey]);
      if (!keys) return;
      org.recentlyClosed.unshift({
        sessionKey: keys[0],
        title: op.title.trim(),
        closedAt: op.closedAt,
        resumeCmd: op.resumeCmd ?? null,
        groupId: org.members[keys[0]]?.groupId ?? null,
      });
      org.recentlyClosed = org.recentlyClosed.slice(0, RECENTLY_CLOSED_CAP);
      org.recentlyClosedTotal += 1;
      return;
    }
    case "saveView": {
      const name = op.name.trim();
      if (!name) return;
      const query = op.query.trim();
      const existing = org.savedViews.find((v) => v.name === name);
      if (existing) {
        existing.query = query;
        existing.savedAt = now;
      } else {
        org.savedViews.push({ name, query, savedAt: now });
      }
      return;
    }
    case "deleteView":
      org.savedViews = org.savedViews.filter((v) => v.name !== op.name.trim());
      return;
    case "dismissSuggestion": {
      const key = op.key.trim();
      if (key && !org.dismissedSuggestions.includes(key)) {
        org.dismissedSuggestions = [...org.dismissedSuggestions, key].sort();
      }
      return;
    }
    case "resetAfterUnknown":
      return;
  }
}

/** Deep copy — the org is plain JSON. */
function cloneOrg(org: SessionOrg): SessionOrg {
  return structuredClone(org);
}

/** Project `ops` onto `org` without touching it. */
export function applyOpsOptimistic(
  org: SessionOrg,
  ops: readonly SessionOrgOp[],
  now: number = Date.now(),
): SessionOrg {
  const next = cloneOrg(org);
  for (const op of ops) applyOne(next, op, now);
  for (const [key, m] of Object.entries(next.members)) {
    if (isDefaultMember(m)) delete next.members[key];
  }
  return next;
}
