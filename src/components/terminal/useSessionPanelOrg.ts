/**
 * The Session Manager panel's organization state and actions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3), kept out of the panel's render: group colors and member rows,
 * Move to page with its outcome notice, the shared card org menu, drag, the
 * `session-org` bridge component, the D7 close request, inline rename,
 * "New session in group" and "Suggest groups".
 */

import { useCallback, useMemo, useState } from "react";

import { sessionCardName, sessionCardStatusDisplay, type SessionCardOrgMenu } from "./SessionCard";
import type { CloseTarget } from "./SessionCloseDialog";
import { SESSION_GROUP_COLOR_VAR } from "./sessionGroupColors";
import { suggestGroups, type GroupSuggestion } from "./sessionGroupSuggestions";
import type { MenuPage } from "./SessionOrgMenus";
import type { SessionGroup, SessionOrgOp } from "./sessionOrgTypes";
import { buildSuggestionSessions, loadFleetSuggestionFacts } from "./sessionSuggestionInputs";
import type { UnifiedSession, UseSessionManagerReturn } from "./useSessionManager";
import { memberOf } from "./useSessionOrg";
import { useSessionOrgBridge } from "./useSessionOrgBridge";
import { useSessionOrgDrag } from "./useSessionOrgDrag";

/** What "New session in group" asks the page to spawn. */
export interface NewSessionInGroupRequest {
  /** The group's last working dir. */
  workingDir: string;
  /** The account (Claude config dir) of the same member. */
  configDir: string;
  /** Called with the new session's pinned Claude id, to file it in the group. */
  onPinned: (claudeSessionId: string) => void;
}

/** The close confirmation's subject: its heading and the rows it names. */
export interface CloseRequest {
  heading: string;
  sessionIds: string[];
}

/** Drop from an open suggestion list the ones a batch just accepted or dismissed. */
export function withoutAppliedSuggestions(
  list: readonly GroupSuggestion[],
  ops: readonly SessionOrgOp[],
): GroupSuggestion[] {
  const same = (a: readonly string[] | undefined, b: readonly string[]) =>
    a !== undefined && a.length === b.length && a.every((k, i) => k === b[i]);
  return list.filter(
    (s) =>
      !ops.some(
        (op) =>
          (op.op === "dismissSuggestion" && op.key === s.id) ||
          (op.op === "createGroup" &&
            same(
              op.sessionKeys,
              s.members.map((m) => m.sessionKey),
            )),
      ),
  );
}

export interface SessionPanelOrgDeps {
  manager: UseSessionManagerReturn;
  pages: readonly MenuPage[];
  currentPageId: string;
  onNewSessionInGroup?: (request: NewSessionInGroupRequest) => void;
  /** Row drops reorder only in the "My groups" lens. */
  groupLens: boolean;
}

export function useSessionPanelOrg({
  manager,
  pages,
  currentPageId,
  onNewSessionInGroup,
  groupLens,
}: SessionPanelOrgDeps) {
  const {
    sessions,
    allSessions,
    org,
    applyOrg,
    sessionKeyOf,
    closableTabId,
    closeSessions,
    moveSessionsToPage,
  } = manager;
  const knownOrg = org.phase === "known" ? org.org : null;

  const apply = useCallback((ops: SessionOrgOp[]) => void applyOrg(ops), [applyOrg]);
  const otherPages = useMemo(
    () => pages.filter((p) => p.id !== currentPageId),
    [pages, currentPageId],
  );
  const groupColorOf = useCallback(
    (session: UnifiedSession): string | null => {
      const groupId = memberOf(org, sessionKeyOf(session))?.groupId ?? null;
      const group = groupId ? knownOrg?.groups.find((g) => g.id === groupId) : undefined;
      return group ? SESSION_GROUP_COLOR_VAR[group.colorRole] : null;
    },
    [org, knownOrg, sessionKeyOf],
  );
  const groupRows = useCallback(
    (groupId: string): UnifiedSession[] =>
      allSessions.filter((s) => memberOf(org, sessionKeyOf(s))?.groupId === groupId),
    [allSessions, org, sessionKeyOf],
  );
  const groupTabIds = useCallback(
    (groupId: string): string[] =>
      groupRows(groupId)
        .map((s) => closableTabId(s))
        .filter((t): t is string => t !== null),
    [groupRows, closableTabId],
  );

  // A Move-to-page outcome stays on screen until the next one: a move that
  // failed for some rows must not read as done.
  const [moveNotice, setMoveNotice] = useState<string | null>(null);
  const dismissMoveNotice = useCallback(() => setMoveNotice(null), []);
  const moveToPage = useCallback(
    (rows: readonly UnifiedSession[], pageId: string) => {
      const pageName = pages.find((p) => p.id === pageId)?.name ?? pageId;
      setMoveNotice(`Moving ${rows.length} to ${pageName}…`);
      void moveSessionsToPage(rows, pageId).then((results) => {
        const failed = results.filter((r) => !r.ok);
        setMoveNotice(
          failed.length === 0
            ? `Moved ${results.length} to ${pageName}`
            : `Moved ${results.length - failed.length} of ${results.length} to ${pageName} — ${failed
                .map((r) => r.detail)
                .join("; ")}`,
        );
      });
    },
    [moveSessionsToPage, pages],
  );

  const orgMenu: SessionCardOrgMenu | undefined = useMemo(
    () =>
      knownOrg
        ? {
            groups: knownOrg.groups,
            pages: otherPages,
            apply,
            moveToPage: (session, pageId) => moveToPage([session], pageId),
          }
        : undefined,
    [knownOrg, otherPages, apply, moveToPage],
  );

  const drag = useSessionOrgDrag({
    org: knownOrg,
    sessionKeyOf,
    liveTabOf: closableTabId,
    groupTabIds,
    apply,
    reorderEnabled: groupLens,
  });

  useSessionOrgBridge({
    org,
    rows: allSessions,
    sessionKeyOf,
    applyOrg,
    closeSessions,
    moveSessionsToPage,
  });

  // ── Close (D7) ───────────────────────────────────────────────────────────

  const [closeRequest, setCloseRequest] = useState<CloseRequest | null>(null);
  const dismissClose = useCallback(() => setCloseRequest(null), []);
  const requestCloseGroup = useCallback(
    (group: SessionGroup) =>
      setCloseRequest({
        heading: `Close group ${group.title}?`,
        sessionIds: groupRows(group.id).map((s) => s.sessionId),
      }),
    [groupRows],
  );
  const requestClose = useCallback((session: UnifiedSession) => {
    setCloseRequest({
      heading: `Close ${sessionCardName(session)}?`,
      sessionIds: [session.sessionId],
    });
  }, []);
  // Re-read LIVE while the dialog is open: a row that turned `Needs you`
  // since it was asked for grows its second-tick requirement.
  const closeTargets: CloseTarget[] = useMemo(() => {
    if (!closeRequest) return [];
    return closeRequest.sessionIds.flatMap((id) => {
      const row = allSessions.find((s) => s.sessionId === id);
      if (!row) return [];
      return [
        {
          sessionId: row.sessionId,
          title: sessionCardName(row),
          activity: row.status.activity,
          activityWord: sessionCardStatusDisplay(row.status).activityLabel,
          closable: closableTabId(row) !== null,
        },
      ];
    });
  }, [closeRequest, allSessions, closableTabId]);
  const rowsOf = useCallback(
    (ids: readonly string[]) => allSessions.filter((s) => ids.includes(s.sessionId)),
    [allSessions],
  );

  // ── Groups: rename, new session, suggestions ─────────────────────────────

  const [renamingGroupId, setRenamingGroupId] = useState<string | null>(null);

  /** "New session in group": the most recently active member with a dir and an account. */
  const newSessionFor = useCallback(
    (groupId: string): { run: (() => void) | null; reason: string } => {
      if (!onNewSessionInGroup) return { run: null, reason: "Not available on this page" };
      const source = [...groupRows(groupId)]
        .filter((s) => s.projectPath && s.configDir)
        .sort((a, b) => b.lastModified.localeCompare(a.lastModified))[0];
      if (!source) {
        return { run: null, reason: "No member has a known working dir and account to spawn in" };
      }
      return {
        reason: "",
        run: () =>
          onNewSessionInGroup({
            workingDir: source.projectPath,
            configDir: source.configDir,
            onPinned: (claudeSessionId) =>
              apply([{ op: "addMembers", groupId, sessionKeys: [claudeSessionId] }]),
          }),
      };
    },
    [onNewSessionInGroup, groupRows, apply],
  );

  const [suggestions, setSuggestions] = useState<{
    list: GroupSuggestion[];
    note: string | null;
  } | null>(null);
  const openSuggestions = useCallback(async () => {
    if (!knownOrg) return;
    const { facts, unavailable } = await loadFleetSuggestionFacts();
    const inputs = buildSuggestionSessions(
      sessions.map((s) => ({
        sessionKey: sessionKeyOf(s),
        title: sessionCardName(s),
        claudeSessionId: s.source === "transcript" ? s.sessionId : null,
        workingDir: s.projectPath,
      })),
      facts,
    );
    setSuggestions({ list: suggestGroups(inputs, knownOrg), note: unavailable });
  }, [knownOrg, sessions, sessionKeyOf]);

  const applySuggestionOps = useCallback(
    (ops: SessionOrgOp[]) => {
      apply(ops);
      // Accepted or dismissed suggestions leave the open list at once.
      setSuggestions((prev) =>
        prev ? { ...prev, list: withoutAppliedSuggestions(prev.list, ops) } : prev,
      );
    },
    [apply],
  );
  const closeSuggestions = useCallback(() => setSuggestions(null), []);

  return {
    apply,
    otherPages,
    groupColorOf,
    groupRows,
    groupTabIds,
    moveNotice,
    dismissMoveNotice,
    moveToPage,
    orgMenu,
    drag,
    closeRequest,
    requestClose,
    requestCloseGroup,
    dismissClose,
    closeTargets,
    rowsOf,
    renamingGroupId,
    setRenamingGroupId,
    newSessionFor,
    suggestions,
    openSuggestions,
    applySuggestionOps,
    closeSuggestions,
  };
}
