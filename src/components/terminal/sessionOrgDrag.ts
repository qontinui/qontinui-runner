/**
 * The Session Manager's native HTML5 drag payload (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3; the house drag pattern is `TerminalPageTabBar.tsx`).
 *
 * A session row or a group header is dragged with ONE custom MIME type, so
 * the drop targets that understand it — another row, a group header,
 * Ungrouped, a Terminal page tab — recognise it in `dragover` (where only
 * `dataTransfer.types` is readable) and everything else ignores it. The
 * payload carries the live tab ids it would move, so a page tab can move
 * them without knowing the organization store.
 */

export const SESSION_ORG_DRAG_MIME = "application/x-qontinui-session-org";

export type SessionOrgDragPayload =
  | { kind: "session"; sessionKey: string; tabIds: string[] }
  | { kind: "group"; groupId: string; tabIds: string[] };

export function encodeSessionOrgDrag(payload: SessionOrgDragPayload): string {
  return JSON.stringify(payload);
}

/** The payload, or `null` for anything that is not one. */
export function decodeSessionOrgDrag(raw: string | null | undefined): SessionOrgDragPayload | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (parsed === null || typeof parsed !== "object") return null;
  const p = parsed as Record<string, unknown>;
  const tabIds = Array.isArray(p.tabIds)
    ? p.tabIds.filter((t): t is string => typeof t === "string")
    : [];
  if (p.kind === "session" && typeof p.sessionKey === "string" && p.sessionKey) {
    return { kind: "session", sessionKey: p.sessionKey, tabIds };
  }
  if (p.kind === "group" && typeof p.groupId === "string" && p.groupId) {
    return { kind: "group", groupId: p.groupId, tabIds };
  }
  return null;
}

/** Does a `dragover` carry a session-org payload? (Its data is unreadable there.) */
export function isSessionOrgDrag(types: readonly string[] | DOMStringList | undefined): boolean {
  if (!types) return false;
  return Array.from(types as ArrayLike<string>).includes(SESSION_ORG_DRAG_MIME);
}
