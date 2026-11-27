/**
 * The organization items of the Session Manager's two context menus (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3):
 *
 * - **Session** ({@link SessionOrgMenuItems}): Add to new group, Add to
 *   group ▸, Remove from group, Pin/Unpin, Move to page ▸, Mute, Snooze ▸,
 *   tags. Close stays in the card's own menu, below these.
 * - **Group header** ({@link GroupMenuItems}): Rename, Color ▸, Collapse,
 *   New session in group, Move group to page ▸, Ungroup, Close group…,
 *   Delete group.
 *
 * A "▸" item opens its choices inline beneath it rather than as a flyout, so
 * the menu never extends past the panel edge. Every item renders its
 * `terminal.session-org.*` id and declared effect (`sessionOrgControls.ts`).
 * The menus compute nothing beyond the op batch each click sends.
 */

import { useState, type ReactNode } from "react";
import {
  BellOff,
  Bell,
  ChevronRight,
  Clock,
  FolderInput,
  FolderMinus,
  FolderPlus,
  Layers,
  Palette,
  Pencil,
  Pin,
  Plus,
  Tag,
  Trash2,
  Ungroup,
  X,
  ChevronsDownUp,
  ChevronsUpDown,
} from "lucide-react";

import { SESSION_GROUP_COLOR_VAR } from "./sessionGroupColors";
import { sessionOrgControlAttrs, type SessionOrgControlName } from "./sessionOrgControls";
import {
  SESSION_GROUP_COLOR_ROLES,
  type MemberOrg,
  type SessionGroup,
  type SessionOrgOp,
} from "./sessionOrgTypes";

/** A Terminal page a session can be moved to. */
export interface MenuPage {
  id: string;
  name: string;
}

/** The word each color role reads as in the Color ▸ list. */
export const COLOR_ROLE_LABELS: Readonly<
  Record<(typeof SESSION_GROUP_COLOR_ROLES)[number], string>
> = {
  "brand-primary": "Primary",
  "brand-secondary": "Secondary",
  "brand-success": "Teal",
  info: "Info",
  "border-strong": "Grey",
  "text-muted": "Muted",
};

/** The Snooze ▸ choices, from `now`: one hour, four hours, tomorrow 09:00. */
export function snoozePresets(now: number): { label: string; until: number }[] {
  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  tomorrow.setHours(9, 0, 0, 0);
  return [
    { label: "1 hour", until: now + 60 * 60 * 1000 },
    { label: "4 hours", until: now + 4 * 60 * 60 * 1000 },
    { label: "Tomorrow 09:00", until: tomorrow.getTime() },
  ];
}

const ITEM_CLASS =
  "w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors disabled:opacity-40 disabled:hover:bg-transparent text-left";
const SUB_ITEM_CLASS =
  "w-full flex items-center gap-2 pl-7 pr-3 py-1 text-[11px] text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors text-left";

function MenuItem({
  control,
  itemId,
  icon,
  children,
  onClick,
  disabled,
  title,
  danger,
}: {
  control: SessionOrgControlName;
  itemId?: string;
  icon: ReactNode;
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  title?: string;
  danger?: boolean;
}) {
  return (
    <button
      {...sessionOrgControlAttrs(control, itemId)}
      role="menuitem"
      disabled={disabled}
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={`${ITEM_CLASS} ${danger ? "text-[#f7768e]" : ""}`}
    >
      {icon}
      <span className="flex-1 truncate">{children}</span>
    </button>
  );
}

/** A "▸" item: toggles its choices open beneath it. */
function SubMenu({
  control,
  itemId,
  icon,
  label,
  disabled,
  title,
  children,
}: {
  control: SessionOrgControlName;
  itemId?: string;
  icon: ReactNode;
  label: string;
  disabled?: boolean;
  title?: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        {...sessionOrgControlAttrs(control, itemId)}
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        title={title}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        className={ITEM_CLASS}
      >
        {icon}
        <span className="flex-1 truncate">{label}</span>
        <ChevronRight className={`w-3 h-3 transition-transform ${open ? "rotate-90" : ""}`} />
      </button>
      {open && <div role="menu">{children}</div>}
    </>
  );
}

function SubItem({
  control,
  itemId,
  children,
  onClick,
  title,
}: {
  control: SessionOrgControlName;
  itemId: string;
  children: ReactNode;
  onClick: () => void;
  title?: string;
}) {
  return (
    <button
      {...sessionOrgControlAttrs(control, itemId)}
      role="menuitem"
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={SUB_ITEM_CLASS}
    >
      {children}
    </button>
  );
}

/** A one-line text entry inside a menu (new group name, tag). */
function MenuTextEntry({
  control,
  itemId,
  placeholder,
  initial = "",
  onSubmit,
  onCancel,
}: {
  control: SessionOrgControlName;
  itemId?: string;
  placeholder: string;
  initial?: string;
  onSubmit: (text: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initial);
  return (
    <div className="px-2 py-1">
      <input
        {...sessionOrgControlAttrs(control, itemId)}
        autoFocus
        value={value}
        aria-label={placeholder}
        placeholder={placeholder}
        onClick={(e) => e.stopPropagation()}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && value.trim()) onSubmit(value.trim());
          else if (e.key === "Escape") onCancel();
        }}
        className="w-full px-2 py-1 text-xs bg-[#13141f] border border-[#2a2d3d] rounded text-[#a9b1d6] placeholder-[#414868] focus:outline-none focus:border-[#7aa2f7]/50"
      />
    </div>
  );
}

function ColorChip({ role }: { role: SessionGroup["colorRole"] }) {
  return (
    <span
      aria-hidden
      className="w-2.5 h-2.5 rounded-sm shrink-0"
      style={{ backgroundColor: SESSION_GROUP_COLOR_VAR[role] }}
    />
  );
}

export interface SessionOrgMenuItemsProps {
  /** The row id, for per-item control ids. */
  sessionId: string;
  sessionKey: string;
  member: MemberOrg | null;
  groups: readonly SessionGroup[];
  /** Pages other than this one; empty = no Move to page. */
  pages: readonly MenuPage[];
  /** The row has a live tab here — the precondition of Move to page. */
  hasLiveTab: boolean;
  onApply: (ops: SessionOrgOp[]) => void;
  onMoveToPage: (pageId: string) => void;
  /** Dismiss the menu. */
  close: () => void;
  now?: number;
}

export function SessionOrgMenuItems({
  sessionId,
  sessionKey,
  member,
  groups,
  pages,
  hasLiveTab,
  onApply,
  onMoveToPage,
  close,
  now: nowProp,
}: SessionOrgMenuItemsProps) {
  // The menu mounts each time it opens, so its first render is "now".
  const [openedAt] = useState(() => Date.now());
  const now = nowProp ?? openedAt;
  const [entry, setEntry] = useState<"none" | "new-group" | "tag">("none");
  const keys = [sessionKey];
  const act = (ops: SessionOrgOp[]) => {
    onApply(ops);
    close();
  };
  const inGroup = member?.groupId ?? null;
  const otherGroups = [...groups].sort((a, b) => a.order - b.order).filter((g) => g.id !== inGroup);
  const snoozed = member?.snoozedUntil != null && member.snoozedUntil > now;

  if (entry === "new-group") {
    return (
      <MenuTextEntry
        control="sessionMenuNewGroup"
        itemId={sessionId}
        placeholder="New group name…"
        onSubmit={(title) => act([{ op: "createGroup", title, sessionKeys: keys }])}
        onCancel={() => setEntry("none")}
      />
    );
  }
  if (entry === "tag") {
    return (
      <MenuTextEntry
        control="sessionMenuTag"
        itemId={sessionId}
        placeholder="Tag…"
        onSubmit={(tag) => act([{ op: "addTags", sessionKeys: keys, tags: [tag] }])}
        onCancel={() => setEntry("none")}
      />
    );
  }

  return (
    <>
      <MenuItem
        control="sessionMenuNewGroup"
        itemId={sessionId}
        icon={<FolderPlus className="w-3 h-3 text-[#7aa2f7]" />}
        onClick={() => setEntry("new-group")}
      >
        Add to new group
      </MenuItem>
      <SubMenu
        control="sessionMenuAddToGroup"
        itemId={sessionId}
        icon={<FolderInput className="w-3 h-3 text-[#7aa2f7]" />}
        label="Add to group"
        disabled={otherGroups.length === 0}
        title={otherGroups.length === 0 ? "No other group exists yet" : undefined}
      >
        {otherGroups.map((g) => (
          <SubItem
            key={g.id}
            control="sessionMenuAddToGroup"
            itemId={`${sessionId}-${g.id}`}
            title={g.title}
            onClick={() => act([{ op: "addMembers", groupId: g.id, sessionKeys: keys }])}
          >
            <ColorChip role={g.colorRole} />
            <span className="truncate">{g.title}</span>
          </SubItem>
        ))}
      </SubMenu>
      {inGroup !== null && (
        <MenuItem
          control="sessionMenuRemoveFromGroup"
          itemId={sessionId}
          icon={<FolderMinus className="w-3 h-3" />}
          onClick={() => act([{ op: "removeMembers", sessionKeys: keys }])}
        >
          Remove from group
        </MenuItem>
      )}
      <MenuItem
        control="sessionMenuPin"
        itemId={sessionId}
        icon={<Pin className={`w-3 h-3 ${member?.pinned ? "text-[#e0af68]" : ""}`} />}
        onClick={() => act([{ op: "setPinned", sessionKeys: keys, pinned: !member?.pinned }])}
      >
        {member?.pinned ? "Unpin" : "Pin"}
      </MenuItem>
      <SubMenu
        control="sessionMenuMoveToPage"
        itemId={sessionId}
        icon={<Layers className="w-3 h-3" />}
        label="Move to page"
        disabled={!hasLiveTab || pages.length === 0}
        title={
          !hasLiveTab
            ? "Only a session with a live tab in this window can move"
            : pages.length === 0
              ? "There is no other page"
              : undefined
        }
      >
        {pages.map((p) => (
          <SubItem
            key={p.id}
            control="sessionMenuMoveToPage"
            itemId={`${sessionId}-${p.id}`}
            onClick={() => {
              onMoveToPage(p.id);
              close();
            }}
          >
            <span className="truncate">{p.name}</span>
          </SubItem>
        ))}
      </SubMenu>
      <MenuItem
        control="sessionMenuMute"
        itemId={sessionId}
        icon={member?.muted ? <Bell className="w-3 h-3" /> : <BellOff className="w-3 h-3" />}
        title="Mute silences this session's needs-input toast and sound; its row still reads Needs you"
        onClick={() => act([{ op: "setMuted", sessionKeys: keys, muted: !member?.muted }])}
      >
        {member?.muted ? "Unmute" : "Mute"}
      </MenuItem>
      <SubMenu
        control="sessionMenuSnooze"
        itemId={sessionId}
        icon={<Clock className="w-3 h-3" />}
        label={snoozed ? "Snoozed" : "Snooze"}
      >
        {snoozePresets(now).map((p) => (
          <SubItem
            key={p.label}
            control="sessionMenuSnooze"
            itemId={`${sessionId}-${p.label.replace(/\W+/g, "-").toLowerCase()}`}
            onClick={() => act([{ op: "snooze", sessionKeys: keys, until: p.until }])}
          >
            {p.label}
          </SubItem>
        ))}
        {snoozed && (
          <SubItem
            control="sessionMenuSnooze"
            itemId={`${sessionId}-unsnooze`}
            onClick={() => act([{ op: "snooze", sessionKeys: keys, until: null }])}
          >
            Unsnooze
          </SubItem>
        )}
      </SubMenu>
      <MenuItem
        control="sessionMenuTag"
        itemId={sessionId}
        icon={<Tag className="w-3 h-3 text-[#73daca]" />}
        onClick={() => setEntry("tag")}
      >
        Add tag
      </MenuItem>
      {(member?.tags ?? []).map((tag) => (
        <MenuItem
          key={tag}
          control="sessionMenuTag"
          itemId={`${sessionId}-remove-${tag}`}
          icon={<X className="w-3 h-3" />}
          onClick={() => act([{ op: "removeTags", sessionKeys: keys, tags: [tag] }])}
        >
          Remove tag “{tag}”
        </MenuItem>
      ))}
    </>
  );
}

export interface GroupMenuItemsProps {
  group: SessionGroup;
  pages: readonly MenuPage[];
  /** Live tabs among the group's rows — what Move/Close group act on. */
  liveTabCount: number;
  /** `null` when no member names an account to spawn in (the reason is shown). */
  onNewSession: (() => void) | null;
  newSessionUnavailable?: string;
  onStartRename: () => void;
  onApply: (ops: SessionOrgOp[]) => void;
  onMoveToPage: (pageId: string) => void;
  onCloseGroup: () => void;
  close: () => void;
}

export function GroupMenuItems({
  group,
  pages,
  liveTabCount,
  onNewSession,
  newSessionUnavailable,
  onStartRename,
  onApply,
  onMoveToPage,
  onCloseGroup,
  close,
}: GroupMenuItemsProps) {
  const id = group.id;
  const act = (ops: SessionOrgOp[]) => {
    onApply(ops);
    close();
  };
  return (
    <>
      <MenuItem
        control="groupMenuRename"
        itemId={id}
        icon={<Pencil className="w-3 h-3" />}
        onClick={() => {
          onStartRename();
          close();
        }}
      >
        Rename
      </MenuItem>
      <SubMenu
        control="groupMenuColor"
        itemId={id}
        icon={<Palette className="w-3 h-3" />}
        label="Color"
      >
        {SESSION_GROUP_COLOR_ROLES.map((role) => (
          <SubItem
            key={role}
            control="groupMenuColor"
            itemId={`${id}-${role}`}
            onClick={() => act([{ op: "recolorGroup", groupId: id, colorRole: role }])}
          >
            <ColorChip role={role} />
            <span className="flex-1">{COLOR_ROLE_LABELS[role]}</span>
            {group.colorRole === role && <span className="text-[9px] text-[#565f89]">current</span>}
          </SubItem>
        ))}
      </SubMenu>
      <MenuItem
        control="groupMenuCollapse"
        itemId={id}
        icon={
          group.collapsed ? (
            <ChevronsUpDown className="w-3 h-3" />
          ) : (
            <ChevronsDownUp className="w-3 h-3" />
          )
        }
        onClick={() => act([{ op: "setGroupCollapsed", groupId: id, collapsed: !group.collapsed }])}
      >
        {group.collapsed ? "Expand" : "Collapse"}
      </MenuItem>
      <MenuItem
        control="groupMenuNewSession"
        itemId={id}
        icon={<Plus className="w-3 h-3 text-[#9ece6a]" />}
        disabled={onNewSession === null}
        title={
          onNewSession === null ? newSessionUnavailable : "Spawn in the group's last working dir"
        }
        onClick={() => {
          onNewSession?.();
          close();
        }}
      >
        New session in group
      </MenuItem>
      <SubMenu
        control="groupMenuMoveToPage"
        itemId={id}
        icon={<Layers className="w-3 h-3" />}
        label="Move group to page"
        disabled={liveTabCount === 0 || pages.length === 0}
        title={
          liveTabCount === 0
            ? "No member has a live tab in this window"
            : pages.length === 0
              ? "There is no other page"
              : undefined
        }
      >
        {pages.map((p) => (
          <SubItem
            key={p.id}
            control="groupMenuMoveToPage"
            itemId={`${id}-${p.id}`}
            onClick={() => {
              onMoveToPage(p.id);
              close();
            }}
          >
            <span className="truncate">{p.name}</span>
          </SubItem>
        ))}
      </SubMenu>
      <div className="my-1 h-px bg-[#2a2d3d]" />
      <MenuItem
        control="groupMenuUngroup"
        itemId={id}
        icon={<Ungroup className="w-3 h-3" />}
        title="Release every member; the empty group stays"
        onClick={() => act([{ op: "ungroupGroup", groupId: id }])}
      >
        Ungroup
      </MenuItem>
      <MenuItem
        control="groupMenuCloseGroup"
        itemId={id}
        icon={<X className="w-3 h-3" />}
        danger
        disabled={liveTabCount === 0}
        title={liveTabCount === 0 ? "No member has a live tab in this window" : undefined}
        onClick={() => {
          onCloseGroup();
          close();
        }}
      >
        Close group…
      </MenuItem>
      <MenuItem
        control="groupMenuDelete"
        itemId={id}
        icon={<Trash2 className="w-3 h-3" />}
        title="Remove the group; its sessions stay, ungrouped"
        onClick={() => act([{ op: "deleteGroup", groupId: id }])}
      >
        Delete group
      </MenuItem>
    </>
  );
}
