/**
 * Native HTML5 drag for the Session Manager's Live list (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3; the house pattern is `TerminalPageTabBar.tsx`):
 *
 * - drag a row onto another row in a group → reorder / move into that group
 *   before it; onto a group header → append to that group; onto Ungrouped →
 *   leave its group;
 * - drag a group header onto another header → reorder the groups; onto
 *   Ungrouped → move it last;
 * - drag either onto a Terminal page tab → move its live tabs to that page
 *   (`TerminalPageTabBar` reads the payload).
 *
 * The ops come from `memberDropOp` / `groupDropOp`, which answer `null` for a
 * drop that changes nothing — a same-position drag sends no op at all. The
 * dragged item is held in a ref as well as on the `DataTransfer`, because
 * `dragover` may read only the transfer's TYPES.
 */

import { useCallback, useMemo, useRef, useState, type DragEvent } from "react";

import { groupDropOp, memberDropOp } from "./sessionGrouping";
import {
  SESSION_ORG_DRAG_MIME,
  encodeSessionOrgDrag,
  type SessionOrgDragPayload,
} from "./sessionOrgDrag";
import type { SessionOrg, SessionOrgOp } from "./sessionOrgTypes";
import type { SessionCardDrag } from "./SessionCard";
import type { UnifiedSession } from "./useSessionManager";

/** Where a drop would land right now — drives the insertion indicators. */
export type DropHover =
  | { kind: "row"; sessionId: string }
  | { kind: "group"; groupId: string; mode: "before" | "into" }
  | { kind: "ungrouped" }
  | null;

export interface SessionOrgDragDeps {
  /** `null` when the org is not known: rows still drag to page tabs, nothing reorders. */
  org: Pick<SessionOrg, "groups" | "members"> | null;
  sessionKeyOf: (session: UnifiedSession) => string;
  /** The live tab a row would move to another page, or null. */
  liveTabOf: (session: UnifiedSession) => string | null;
  /** Live tabs of a group's rows (for a group dragged to a page tab). */
  groupTabIds: (groupId: string) => string[];
  apply: (ops: SessionOrgOp[]) => void;
  /** Row drops reorder only in the "My groups" lens. */
  reorderEnabled: boolean;
}

export function useSessionOrgDrag(deps: SessionOrgDragDeps) {
  const { org, sessionKeyOf, liveTabOf, groupTabIds, apply, reorderEnabled } = deps;
  const dragged = useRef<SessionOrgDragPayload | null>(null);
  const [hover, setHover] = useState<DropHover>(null);

  const begin = useCallback((payload: SessionOrgDragPayload, e: DragEvent) => {
    dragged.current = payload;
    e.dataTransfer.setData(SESSION_ORG_DRAG_MIME, encodeSessionOrgDrag(payload));
    e.dataTransfer.effectAllowed = "move";
  }, []);

  const end = useCallback(() => {
    dragged.current = null;
    setHover(null);
  }, []);

  const send = useCallback(
    (op: SessionOrgOp | null) => {
      if (op) apply([op]);
    },
    [apply],
  );

  /** The group a row sits in, as the lens renders it. */
  const groupOfRow = useCallback(
    (session: UnifiedSession): string | null =>
      org?.members[sessionKeyOf(session)]?.groupId ?? null,
    [org, sessionKeyOf],
  );
  /** A pinned row renders in Pinned, outside its group: it is no reorder target. */
  const isPinnedRow = useCallback(
    (session: UnifiedSession): boolean => org?.members[sessionKeyOf(session)]?.pinned === true,
    [org, sessionKeyOf],
  );

  const card: SessionCardDrag = useMemo(
    () => ({
      start: (session, e) => {
        const tab = liveTabOf(session);
        begin({ kind: "session", sessionKey: sessionKeyOf(session), tabIds: tab ? [tab] : [] }, e);
      },
      end,
      over: (session, e) => {
        if (!reorderEnabled || !org || dragged.current?.kind !== "session") return;
        if (isPinnedRow(session)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setHover((h) =>
          h?.kind === "row" && h.sessionId === session.sessionId
            ? h
            : { kind: "row", sessionId: session.sessionId },
        );
      },
      drop: (session, e) => {
        const d = dragged.current;
        end();
        if (!reorderEnabled || !org || d?.kind !== "session" || isPinnedRow(session)) return;
        e.preventDefault();
        const groupId = groupOfRow(session);
        send(
          memberDropOp(org, d.sessionKey, {
            groupId,
            beforeKey: groupId === null ? null : sessionKeyOf(session),
          }),
        );
      },
    }),
    [begin, end, liveTabOf, sessionKeyOf, reorderEnabled, org, groupOfRow, isPinnedRow, send],
  );

  const groupHeader = useMemo(
    () => ({
      start: (groupId: string, e: DragEvent) =>
        begin({ kind: "group", groupId, tabIds: groupTabIds(groupId) }, e),
      end,
      over: (groupId: string, e: DragEvent) => {
        const d = dragged.current;
        if (!org || !d || (d.kind === "group" && d.groupId === groupId)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setHover({ kind: "group", groupId, mode: d.kind === "group" ? "before" : "into" });
      },
      leave: () => setHover(null),
      drop: (groupId: string, e: DragEvent) => {
        const d = dragged.current;
        end();
        if (!org || !d) return;
        e.preventDefault();
        send(
          d.kind === "group"
            ? groupDropOp(org, d.groupId, groupId)
            : memberDropOp(org, d.sessionKey, { groupId, beforeKey: null }),
        );
      },
    }),
    [begin, end, groupTabIds, org, send],
  );

  const ungrouped = useMemo(
    () => ({
      over: (e: DragEvent) => {
        if (!org || !dragged.current) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setHover({ kind: "ungrouped" });
      },
      leave: () => setHover(null),
      drop: (e: DragEvent) => {
        const d = dragged.current;
        end();
        if (!org || !d) return;
        e.preventDefault();
        send(
          d.kind === "group"
            ? groupDropOp(org, d.groupId, null)
            : memberDropOp(org, d.sessionKey, { groupId: null, beforeKey: null }),
        );
      },
    }),
    [end, org, send],
  );

  return { card, groupHeader, ungrouped, hover };
}
