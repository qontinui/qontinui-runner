/**
 * Mounts the session search palette on the Terminal page and acts on what it
 * picks (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4). Mounted with the page — not inside the Session Manager panel — so
 * `Ctrl+Shift+Space` and `/sessions.find` work while the panel is hidden.
 *
 * Enter on an entry:
 *  - live → focus its session (the card-click path, `openSession`);
 *  - previous → resume it (the Previous view's resume path);
 *  - recently closed → reopen it (`reopenClosed`, which also puts it back in
 *    its group and answers whether that worked).
 *
 * The modifier (Shift+Enter / "New zone") does the same in a zone of its own
 * (`planNewZone`); when the grid has no zone to give, it says so rather than
 * quietly doing the plain action.
 */

import { useCallback, useEffect } from "react";

import { SessionSearchPalette } from "./SessionSearchPalette";
import { planNewZone } from "./sessionNewZone";
import type { PaletteEntry } from "./sessionSearchPalette";
import { useSessionSearchPaletteOpen } from "./sessionSearchPaletteStore";
import type { TerminalWorkMode } from "./terminalWorkMode";
import { usePastSessions, type PastSession } from "./usePastSessions";
import type { UnifiedSession, UseSessionManagerReturn } from "./useSessionManager";
import type { ZoneAssignments } from "./useZoneLayout";

/** The slice of `useZoneLayout()` a new-zone placement needs. */
export interface PaletteZoneLayout {
  workMode: TerminalWorkMode;
  layoutId: string;
  layout: { zones: readonly unknown[] };
  assignments: ZoneAssignments;
  setLayoutId: (id: string, request: { source: "spawn" }) => void;
  assignTabToZone: (zoneIndex: number, tabId: string) => void;
  setFocusedZone: (zoneIndex: number) => void;
  requestReveal: (tabId: string) => void;
}

export interface SessionSearchPaletteHostProps {
  manager: Pick<
    UseSessionManagerReturn,
    "liveFacts" | "allSessions" | "openSession" | "recentlyClosed" | "reopenClosed"
  >;
  zoneLayout: PaletteZoneLayout;
  onResumePastSession: (session: PastSession) => void;
  /** Report an outcome the operator would otherwise not see. */
  notify: (message: string, type: "success" | "error") => void;
}

/** The live tab a row shows in this window, if any. */
function tabIdOf(row: UnifiedSession): string | null {
  return row.source === "tab" ? row.sessionId : row.zoneTabId;
}

export function SessionSearchPaletteHost({
  manager,
  zoneLayout,
  onResumePastSession,
  notify,
}: SessionSearchPaletteHostProps) {
  const open = useSessionSearchPaletteOpen();
  const past = usePastSessions();
  const refreshPast = past.refresh;
  // Previous sessions are re-read each time the palette opens.
  useEffect(() => {
    if (open) void refreshPast();
  }, [open, refreshPast]);

  const { liveFacts, allSessions, openSession, recentlyClosed, reopenClosed } = manager;

  /**
   * Make room for a session about to be resumed: in grid mode, an empty zone
   * (growing the grid when every zone is taken) the new tab will fill.
   * Answers `false` when no zone could be made.
   */
  const makeRoomForResume = useCallback((): boolean => {
    const plan = planNewZone({
      workMode: zoneLayout.workMode,
      layoutId: zoneLayout.layoutId,
      zoneCount: zoneLayout.layout.zones.length,
      assignments: zoneLayout.assignments,
    });
    if (plan.kind === "grow") zoneLayout.setLayoutId(plan.layoutId, { source: "spawn" });
    return plan.kind !== "full";
  }, [zoneLayout]);

  const showInNewZone = useCallback(
    (row: UnifiedSession) => {
      const tabId = tabIdOf(row);
      if (!tabId) {
        notify("Not opened in a new zone: the session has no tab in this window", "error");
        openSession(row);
        return;
      }
      const plan = planNewZone({
        workMode: zoneLayout.workMode,
        layoutId: zoneLayout.layoutId,
        zoneCount: zoneLayout.layout.zones.length,
        assignments: zoneLayout.assignments,
      });
      if (plan.kind === "full") {
        notify("Not opened in a new zone: every zone is taken and the grid cannot grow", "error");
        openSession(row);
        return;
      }
      const zone = plan.kind === "tmux" ? 0 : plan.zone;
      if (plan.kind === "grow") zoneLayout.setLayoutId(plan.layoutId, { source: "spawn" });
      zoneLayout.assignTabToZone(zone, tabId);
      zoneLayout.setFocusedZone(zone);
      zoneLayout.requestReveal(tabId);
    },
    [zoneLayout, openSession, notify],
  );

  const onActivate = useCallback(
    (entry: PaletteEntry, { newZone }: { newZone: boolean }) => {
      if (entry.source === "live") {
        const row = allSessions.find((s) => s.sessionId === entry.rowId);
        if (!row) {
          notify("Not opened: the session is no longer in the Live list", "error");
          return;
        }
        if (newZone) showInNewZone(row);
        else openSession(row);
        return;
      }
      const roomNote =
        newZone && !makeRoomForResume() ? " — no new zone: every zone is taken" : "";
      if (entry.source === "previous") {
        const ps = past.sessions.find((p) => p.claudeSessionId === entry.claudeSessionId);
        if (!ps) {
          notify("Not resumed: the session is no longer in the history", "error");
          return;
        }
        onResumePastSession(ps);
        if (roomNote) notify(`Resumed ${entry.title}${roomNote}`, "error");
        return;
      }
      const closed = recentlyClosed.entries?.find((c) => c.sessionKey === entry.sessionKey);
      if (!closed) {
        notify("Not reopened: the session is no longer in Recently closed", "error");
        return;
      }
      void reopenClosed(closed).then((r) =>
        notify(`${entry.title}: ${r.detail}${roomNote}`, r.state === "done" ? "success" : "error"),
      );
    },
    [
      allSessions,
      openSession,
      showInNewZone,
      makeRoomForResume,
      past.sessions,
      onResumePastSession,
      recentlyClosed,
      reopenClosed,
      notify,
    ],
  );

  return (
    <SessionSearchPalette
      live={liveFacts}
      previous={past.sessions}
      closed={recentlyClosed.entries ?? []}
      onActivate={onActivate}
    />
  );
}
