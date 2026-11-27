/**
 * The `session-org` UI Bridge component: every Session Manager organization
 * control as an addressable action with its declared effect (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3). An agent or a driver organizes sessions by ROW id (the
 * `data-session-id` a card carries) without locating a menu item.
 *
 * Close and Close group end live conversation processes (D7), so they are
 * `destructive` — an autonomous walk never fires them. They run the same
 * graceful path as the dialog (never a kill) and answer the per-item results.
 * Every other action writes the organization store and is reversible through
 * its own inverse, so it is `write`. `list-groups` is the one `read`.
 *
 * Row-id lists are comma-separated text: action params are scalars
 * (`guardedHandler`).
 */

import { useUIComponent } from "@qontinui/ui-bridge";

import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";

import { textArg } from "./commands";
import type { CloseItemResult } from "./sessionClose";
import {
  SESSION_GROUP_COLOR_ROLES,
  type SessionGroupColorRole,
  type SessionOrgOp,
} from "./sessionOrgTypes";
import type { MoveToPageResult, UnifiedSession } from "./useSessionManager";
import type { SessionOrgView } from "./useSessionOrg";

export const SESSION_ORG_COMPONENT_ID = "session-org";

const GROUP_SCHEMA = { groupId: "string" } as const;
const CREATE_GROUP_SCHEMA = {
  title: "string",
  sessionIds: "string (optional; comma-separated row ids)",
} as const;
const RENAME_GROUP_SCHEMA = { groupId: "string", title: "string" } as const;
const RECOLOR_GROUP_SCHEMA = {
  groupId: "string",
  colorRole: `string (${SESSION_GROUP_COLOR_ROLES.join(" | ")})`,
} as const;
const COLLAPSE_GROUP_SCHEMA = { groupId: "string", collapsed: "string (true | false)" } as const;
const MOVE_GROUP_SCHEMA = { groupId: "string", index: "number (0-based position)" } as const;
const ADD_TO_GROUP_SCHEMA = {
  groupId: "string",
  sessionIds: "string (comma-separated row ids)",
} as const;
const SESSIONS_SCHEMA = { sessionIds: "string (comma-separated row ids)" } as const;
const FLAG_SCHEMA = {
  sessionIds: "string (comma-separated row ids)",
  value: "string (true | false)",
} as const;
const SNOOZE_SCHEMA = {
  sessionIds: "string (comma-separated row ids)",
  until: "number (optional; epoch ms — absent unsnoozes)",
} as const;
const MOVE_TO_PAGE_SCHEMA = {
  sessionIds: "string (comma-separated row ids)",
  pageId: "string",
} as const;

export interface SessionOrgBridgeDeps {
  org: SessionOrgView;
  /** Every Live row (unfiltered), so an agent can address any session. */
  rows: readonly UnifiedSession[];
  sessionKeyOf: (session: UnifiedSession) => string;
  applyOrg: (ops: SessionOrgOp[]) => Promise<void>;
  closeSessions: (sessions: readonly UnifiedSession[]) => Promise<CloseItemResult[]>;
  moveSessionsToPage: (
    sessions: readonly UnifiedSession[],
    pageId: string,
  ) => Promise<MoveToPageResult[]>;
}

function flag(args: Record<string, unknown>, field: string): boolean {
  const raw = textArg(args, field).trim().toLowerCase();
  if (raw === "true") return true;
  if (raw === "false") return false;
  throw new Error(`"${field}" must be "true" or "false", got "${raw}"`);
}

export function useSessionOrgBridge({
  org,
  rows,
  sessionKeyOf,
  applyOrg,
  closeSessions,
  moveSessionsToPage,
}: SessionOrgBridgeDeps): void {
  const rowsByIds = (raw: string): UnifiedSession[] => {
    const ids = raw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    if (ids.length === 0) throw new Error("sessionIds names no row");
    return ids.map((id) => {
      const row = rows.find((r) => r.sessionId === id);
      if (!row) throw new Error(`No Live row "${id}"`);
      return row;
    });
  };
  const keys = (args: Record<string, unknown>) =>
    rowsByIds(textArg(args, "sessionIds")).map(sessionKeyOf);
  const knownGroup = (groupId: string): string => {
    if (org.phase !== "known") throw new Error(`the session organization is ${org.phase}`);
    if (!org.org.groups.some((g) => g.id === groupId)) {
      throw new Error(
        `No group "${groupId}". Available: ${org.org.groups.map((g) => g.id).join(", ") || "none"}`,
      );
    }
    return groupId;
  };
  const group = (args: Record<string, unknown>) => knownGroup(textArg(args, "groupId"));
  const membersOf = (groupId: string): UnifiedSession[] => {
    if (org.phase !== "known") return [];
    return rows.filter((r) => org.org.members[sessionKeyOf(r)]?.groupId === groupId);
  };

  useUIComponent({
    id: SESSION_ORG_COMPONENT_ID,
    name: "Session organization",
    description:
      "Chrome-style session groups, pins, mute, snooze, page moves and close for the " +
      "Session Manager's Live rows, addressed by row id.",
    actions: [
      {
        id: "list-groups",
        label: "List Groups",
        // `read` — the org as this window holds it.
        effect: "read",
        handler: () =>
          org.phase === "known"
            ? {
                phase: org.phase,
                groups: [...org.org.groups]
                  .sort((a, b) => a.order - b.order)
                  .map((g) => ({
                    id: g.id,
                    title: g.title,
                    colorRole: g.colorRole,
                    collapsed: g.collapsed,
                    members: membersOf(g.id).map((r) => r.sessionId),
                  })),
              }
            : { phase: org.phase, groups: null },
      },
      {
        id: "create-group",
        label: "Create Group",
        paramSchema: CREATE_GROUP_SCHEMA,
        // `write` — a new group; reversible by delete-group.
        effect: "write",
        handler: guardedHandler("create-group", CREATE_GROUP_SCHEMA, (args) => {
          const ids = textArg(args, "sessionIds");
          return applyOrg([
            {
              op: "createGroup",
              title: textArg(args, "title"),
              sessionKeys: ids ? rowsByIds(ids).map(sessionKeyOf) : [],
            },
          ]);
        }),
      },
      {
        id: "rename-group",
        label: "Rename Group",
        paramSchema: RENAME_GROUP_SCHEMA,
        effect: "write",
        handler: guardedHandler("rename-group", RENAME_GROUP_SCHEMA, (args) =>
          applyOrg([{ op: "renameGroup", groupId: group(args), title: textArg(args, "title") }]),
        ),
      },
      {
        id: "recolor-group",
        label: "Recolor Group",
        paramSchema: RECOLOR_GROUP_SCHEMA,
        effect: "write",
        handler: guardedHandler("recolor-group", RECOLOR_GROUP_SCHEMA, (args) => {
          const role = textArg(args, "colorRole");
          if (!(SESSION_GROUP_COLOR_ROLES as readonly string[]).includes(role)) {
            throw new Error(`colorRole must be one of ${SESSION_GROUP_COLOR_ROLES.join(", ")}`);
          }
          return applyOrg([
            { op: "recolorGroup", groupId: group(args), colorRole: role as SessionGroupColorRole },
          ]);
        }),
      },
      {
        id: "set-group-collapsed",
        label: "Collapse / Expand Group",
        paramSchema: COLLAPSE_GROUP_SCHEMA,
        effect: "write",
        handler: guardedHandler("set-group-collapsed", COLLAPSE_GROUP_SCHEMA, (args) =>
          applyOrg([
            { op: "setGroupCollapsed", groupId: group(args), collapsed: flag(args, "collapsed") },
          ]),
        ),
      },
      {
        id: "move-group",
        label: "Move Group",
        paramSchema: MOVE_GROUP_SCHEMA,
        effect: "write",
        handler: guardedHandler("move-group", MOVE_GROUP_SCHEMA, (args) => {
          const index = Number(textArg(args, "index"));
          if (!Number.isInteger(index) || index < 0)
            throw new Error("index must be a whole number ≥ 0");
          return applyOrg([{ op: "moveGroup", groupId: group(args), index }]);
        }),
      },
      {
        id: "ungroup-group",
        label: "Ungroup",
        paramSchema: GROUP_SCHEMA,
        // `write` — releases the members; the group stays.
        effect: "write",
        handler: guardedHandler("ungroup-group", GROUP_SCHEMA, (args) =>
          applyOrg([{ op: "ungroupGroup", groupId: group(args) }]),
        ),
      },
      {
        id: "delete-group",
        label: "Delete Group",
        paramSchema: GROUP_SCHEMA,
        // `write` — removes the group only; its sessions stay, ungrouped.
        effect: "write",
        handler: guardedHandler("delete-group", GROUP_SCHEMA, (args) =>
          applyOrg([{ op: "deleteGroup", groupId: group(args) }]),
        ),
      },
      {
        id: "add-to-group",
        label: "Add To Group",
        paramSchema: ADD_TO_GROUP_SCHEMA,
        effect: "write",
        handler: guardedHandler("add-to-group", ADD_TO_GROUP_SCHEMA, (args) =>
          applyOrg([{ op: "addMembers", groupId: group(args), sessionKeys: keys(args) }]),
        ),
      },
      {
        id: "remove-from-group",
        label: "Remove From Group",
        paramSchema: SESSIONS_SCHEMA,
        effect: "write",
        handler: guardedHandler("remove-from-group", SESSIONS_SCHEMA, (args) =>
          applyOrg([{ op: "removeMembers", sessionKeys: keys(args) }]),
        ),
      },
      {
        id: "set-pinned",
        label: "Pin / Unpin",
        paramSchema: FLAG_SCHEMA,
        effect: "write",
        handler: guardedHandler("set-pinned", FLAG_SCHEMA, (args) =>
          applyOrg([{ op: "setPinned", sessionKeys: keys(args), pinned: flag(args, "value") }]),
        ),
      },
      {
        id: "set-muted",
        label: "Mute / Unmute",
        paramSchema: FLAG_SCHEMA,
        effect: "write",
        handler: guardedHandler("set-muted", FLAG_SCHEMA, (args) =>
          applyOrg([{ op: "setMuted", sessionKeys: keys(args), muted: flag(args, "value") }]),
        ),
      },
      {
        id: "snooze",
        label: "Snooze",
        paramSchema: SNOOZE_SCHEMA,
        effect: "write",
        handler: guardedHandler("snooze", SNOOZE_SCHEMA, (args) => {
          const raw = textArg(args, "until");
          const until = raw ? Number(raw) : null;
          if (until !== null && !Number.isFinite(until)) throw new Error("until must be epoch ms");
          return applyOrg([{ op: "snooze", sessionKeys: keys(args), until }]);
        }),
      },
      {
        id: "move-to-page",
        label: "Move To Page",
        paramSchema: MOVE_TO_PAGE_SCHEMA,
        // `write` — moves live tabs between pages; the PTY lives on.
        effect: "write",
        handler: guardedHandler("move-to-page", MOVE_TO_PAGE_SCHEMA, (args) =>
          moveSessionsToPage(rowsByIds(textArg(args, "sessionIds")), textArg(args, "pageId")),
        ),
      },
      {
        id: "close",
        label: "Close Sessions",
        description:
          "Close sessions through graceful_exit (never a kill); answers each one's outcome.",
        paramSchema: SESSIONS_SCHEMA,
        // `destructive` — ends live conversations; their in-flight context
        // cannot be reconstructed (D7).
        effect: "destructive",
        handler: guardedHandler("close", SESSIONS_SCHEMA, (args) =>
          closeSessions(rowsByIds(textArg(args, "sessionIds"))),
        ),
      },
      {
        id: "close-group",
        label: "Close Group",
        description: "Close every member of a group through graceful_exit; the group stays.",
        paramSchema: GROUP_SCHEMA,
        // `destructive` — as `close`, for every member.
        effect: "destructive",
        handler: guardedHandler("close-group", GROUP_SCHEMA, (args) =>
          closeSessions(membersOf(group(args))),
        ),
      },
    ],
  });
}
