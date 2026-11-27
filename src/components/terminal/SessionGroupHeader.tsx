/**
 * One operator group's header in the Session Manager's "My groups" lens (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3).
 *
 * Shows the color chip, the title (renamed inline), the member count and —
 * collapsed or not — how many members need you, so collapsing a group never
 * hides that it is waiting on the operator. The chevron collapses it; the
 * header itself is the group's drag handle and drop target (a session dropped
 * on it lands at the end of the group, a group dropped on it lands before it);
 * right-click opens the group menu.
 */

import { useEffect, useRef, useState, type DragEvent, type ReactNode } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import { SESSION_GROUP_COLOR_VAR } from "./sessionGroupColors";
import {
  GROUP_HEADER_ACTIONS,
  sessionOrgControlAttrs,
  sessionOrgControlId,
} from "./sessionOrgControls";
import type { SessionGroup } from "./sessionOrgTypes";
import { ACTIVITY_DOT } from "./sessionStatusModel";

/** The badge repeats the `Needs you` dot's color — the word is the signal. */
const NEEDS_YOU_COLOR = ACTIVITY_DOT["needs-you"].color;

/** "3" — or "2 of 3" when the list shows fewer rows than the group has members. */
export function groupCountLabel(shownCount: number, memberCount: number): string {
  return shownCount === memberCount ? `${memberCount}` : `${shownCount} of ${memberCount}`;
}

/** The header's needs-you badge text; `null` at zero. */
export function needsYouBadge(needsYouCount: number): string | null {
  return needsYouCount > 0 ? `${needsYouCount} need${needsYouCount === 1 ? "s" : ""} you` : null;
}

export interface SessionGroupHeaderProps {
  group: SessionGroup;
  shownCount: number;
  memberCount: number;
  needsYouCount: number;
  renaming: boolean;
  onStartRename: () => void;
  onRename: (title: string) => void;
  onCancelRename: () => void;
  onToggleCollapse: () => void;
  /** The group context menu's items; `close` dismisses it. */
  renderMenu: (close: () => void) => ReactNode;
  onDragStart: (e: DragEvent<HTMLDivElement>) => void;
  onDragEnd: (e: DragEvent<HTMLDivElement>) => void;
  onDragOver: (e: DragEvent<HTMLDivElement>) => void;
  onDragLeave: (e: DragEvent<HTMLDivElement>) => void;
  onDrop: (e: DragEvent<HTMLDivElement>) => void;
  /** A drop is hovering: `before` = a group lands above, `into` = a session joins. */
  dropIndicator: "before" | "into" | null;
}

export function SessionGroupHeader({
  group,
  shownCount,
  memberCount,
  needsYouCount,
  renaming,
  onStartRename,
  onRename,
  onCancelRename,
  onToggleCollapse,
  renderMenu,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDragLeave,
  onDrop,
  dropIndicator,
}: SessionGroupHeaderProps) {
  const headerId = sessionOrgControlId("groupHeader", group.id);
  const collapseId = sessionOrgControlId("groupCollapse", group.id);
  const { ref: headerRef } = useUIElement({
    id: headerId,
    type: "generic",
    label: `Session group ${group.title}`,
    actions: GROUP_HEADER_ACTIONS,
  });
  const { ref: collapseRef } = useUIElement({
    id: collapseId,
    type: "button",
    label: `${group.collapsed ? "Expand" : "Collapse"} group ${group.title}`,
  });
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const handler = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenu(null);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [menu]);

  const badge = needsYouBadge(needsYouCount);
  const color = SESSION_GROUP_COLOR_VAR[group.colorRole];

  return (
    <div
      ref={headerRef}
      {...sessionOrgControlAttrs("groupHeader", group.id)}
      data-group-id={group.id}
      data-group-title={group.title}
      data-group-color-role={group.colorRole}
      data-group-collapsed={group.collapsed ? "true" : "false"}
      data-group-member-count={memberCount}
      data-group-needs-you-count={needsYouCount}
      draggable={!renaming}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
      onDoubleClick={onStartRename}
      style={{ borderLeftColor: color }}
      className={`group/header relative flex items-center gap-1.5 pl-1 pr-2 pt-2 pb-1 border-l-[3px] select-none
        ${dropIndicator === "before" ? "border-t-2 border-t-brand-primary" : ""}
        ${dropIndicator === "into" ? "bg-brand-primary/10" : ""}`}
    >
      <button
        ref={collapseRef}
        {...sessionOrgControlAttrs("groupCollapse", group.id)}
        aria-expanded={!group.collapsed}
        aria-label={`${group.collapsed ? "Expand" : "Collapse"} group ${group.title}`}
        onClick={onToggleCollapse}
        className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
      >
        {group.collapsed ? (
          <ChevronRight className="w-3 h-3" />
        ) : (
          <ChevronDown className="w-3 h-3" />
        )}
      </button>
      <span
        aria-hidden
        className="w-2.5 h-2.5 rounded-sm shrink-0"
        style={{ backgroundColor: color }}
      />
      {renaming ? (
        <RenameInput group={group} onRename={onRename} onCancel={onCancelRename} />
      ) : (
        <span
          className="flex-1 min-w-0 truncate text-[11px] font-medium text-[#c0caf5]"
          title={group.title}
        >
          {group.title}
        </span>
      )}
      <span
        className="shrink-0 text-[10px] text-[#565f89]"
        title={
          shownCount === memberCount
            ? `${memberCount} member${memberCount === 1 ? "" : "s"}`
            : `${shownCount} shown of ${memberCount} members`
        }
      >
        {groupCountLabel(shownCount, memberCount)}
      </span>
      {badge && (
        <span
          data-testid="group-needs-you"
          className="shrink-0 px-1 rounded text-[9px] font-medium"
          style={{ color: NEEDS_YOU_COLOR, backgroundColor: `${NEEDS_YOU_COLOR}26` }}
          title={`${badge} in this group`}
        >
          {badge}
        </span>
      )}
      {menu && (
        <div
          ref={menuRef}
          role="menu"
          className="fixed z-50 py-1 min-w-[180px] bg-[#1a1b26] border border-[#2a2d3d] rounded-md shadow-xl"
          style={{ left: menu.x, top: menu.y }}
        >
          {renderMenu(() => setMenu(null))}
        </div>
      )}
    </div>
  );
}

/**
 * The inline rename field. Mounted fresh for each rename, so it starts from
 * the current title; it settles once — Enter or blur commits a non-blank
 * title, Escape (or a blank one) cancels.
 */
function RenameInput({
  group,
  onRename,
  onCancel,
}: {
  group: SessionGroup;
  onRename: (title: string) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState(group.title);
  const settled = useRef(false);
  const settle = (commit: boolean) => {
    if (settled.current) return;
    settled.current = true;
    if (commit && draft.trim()) onRename(draft.trim());
    else onCancel();
  };
  return (
    <input
      {...sessionOrgControlAttrs("groupRenameInput", group.id)}
      autoFocus
      value={draft}
      aria-label={`Rename group ${group.title}`}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => settle(true)}
      onKeyDown={(e) => {
        if (e.key === "Enter") settle(true);
        else if (e.key === "Escape") settle(false);
      }}
      className="flex-1 min-w-0 px-1 py-0 text-[11px] bg-[#13141f] border border-[#2a2d3d] rounded text-[#c0caf5] focus:outline-none focus:border-[#7aa2f7]/50"
    />
  );
}
