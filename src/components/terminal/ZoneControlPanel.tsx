/**
 * ZoneControlPanel
 *
 * Collapsible right-side panel for the Terminal page that shows an overview
 * of all zones — what each session is working on, its state, and quick actions.
 *
 * Features:
 * - Drag-and-drop zone reorder (swap tab assignments)
 * - Inline notes editor per zone
 * - Quick-filter by session state
 * - Session grouping / workspaces (save/load from localStorage)
 */

import React, { useMemo, useState, useCallback, useRef, useEffect } from "react";
import {
  LayoutGrid,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  X,
  Pin,
  PinOff,
  Plus,
  Circle,
  ExternalLink,
  GripVertical,
  StickyNote,
  Save,
  Trash2,
  FolderOpen,
} from "lucide-react";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface SavedWorkspace {
  name: string;
  layoutId: string;
  sessions: Array<{
    title: string;
    workingDir?: string;
    zoneIndex: number;
    label?: string;
    notes?: string;
    pinned?: boolean;
  }>;
  savedAt: number;
}

interface ZoneControlPanelProps {
  tabs: Array<{
    id: string;
    title: string;
    workingDir?: string;
    isAlive: boolean;
    exitCode: number | null;
    createdAt?: number;
  }>;
  assignments: Record<number, string>; // zoneIndex -> tabId
  sessionStates: Record<string, string>; // tabId -> state
  zoneLabels: Record<number, string>;
  zoneNotes: Record<number, string>;
  labelColorMap: Record<string, string>;
  focusedZone: number;
  zoneCount: number;
  lastOutputLines: Record<string, string[]>;
  onFocusZone: (zoneIndex: number) => void;
  onSetZoneLabel: (zoneIndex: number, label: string) => void;
  onSetZoneNotes?: (zoneIndex: number, notes: string) => void;
  onClose: () => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onCreateTerminal?: () => void;
  pinnedZones?: Set<number>;
  onTogglePin?: (zoneIndex: number) => void;
  onSwapZones?: (sourceZone: number, targetZone: number) => void;
  onLoadWorkspace?: (workspace: SavedWorkspace) => void;
  layoutId?: string;
}

type SessionState = "idle" | "working" | "needs-input" | "completed" | "error";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_LABELS: Record<SessionState, string> = {
  idle: "Idle",
  working: "Working",
  "needs-input": "Input",
  completed: "Done",
  error: "Error",
};

const ALL_STATES: SessionState[] = ["idle", "working", "needs-input", "completed", "error"];

const WORKSPACES_STORAGE_KEY = "qontinui-workspaces";
const MAX_WORKSPACES = 10;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatWorkingDir(dir: string | undefined): string {
  if (!dir) return "";
  const normalized = dir.replace(/\\/g, "/");
  const segments = normalized.split("/").filter(Boolean);
  if (segments.length <= 2) return segments.join("/");
  return segments.slice(-2).join("/");
}

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + "\u2026";
}

function getState(raw: string | undefined): SessionState {
  if (
    raw === "idle" ||
    raw === "working" ||
    raw === "needs-input" ||
    raw === "completed" ||
    raw === "error"
  ) {
    return raw;
  }
  return "idle";
}

function loadWorkspaces(): SavedWorkspace[] {
  try {
    const raw = localStorage.getItem(WORKSPACES_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed as SavedWorkspace[];
  } catch {
    return [];
  }
}

function saveWorkspacesToStorage(workspaces: SavedWorkspace[]): void {
  try {
    localStorage.setItem(WORKSPACES_STORAGE_KEY, JSON.stringify(workspaces));
  } catch {
    // localStorage full or unavailable — silently ignore
  }
}

function formatDate(ts: number): string {
  const d = new Date(ts);
  const month = d.getMonth() + 1;
  const day = d.getDate();
  const hours = d.getHours().toString().padStart(2, "0");
  const mins = d.getMinutes().toString().padStart(2, "0");
  return `${month}/${day} ${hours}:${mins}`;
}

// ---------------------------------------------------------------------------
// EditableLabel (inline double-click-to-edit)
// ---------------------------------------------------------------------------

function EditableLabel({
  value,
  onCommit,
  placeholder,
  className,
}: {
  value: string;
  onCommit: (v: string) => void;
  placeholder: string;
  className?: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) {
      setDraft(value);
      requestAnimationFrame(() => inputRef.current?.select());
    }
  }, [editing, value]);

  const commit = useCallback(() => {
    setEditing(false);
    const trimmed = draft.trim();
    if (trimmed !== value) onCommit(trimmed);
  }, [draft, value, onCommit]);

  if (editing) {
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
        className={`bg-[#1a1b26] border border-[#7aa2f7]/50 rounded px-1 py-0 text-[11px] text-[#c0caf5] outline-none w-full ${className ?? ""}`}
        autoFocus
      />
    );
  }

  return (
    <span
      onDoubleClick={() => setEditing(true)}
      className={`cursor-default select-none ${className ?? ""}`}
      title="Double-click to edit"
    >
      {value || <span className="text-[#414868] italic">{placeholder}</span>}
    </span>
  );
}

// ---------------------------------------------------------------------------
// InlineNotesEditor
// ---------------------------------------------------------------------------

function InlineNotesEditor({ notes, onSave }: { notes: string; onSave: (notes: string) => void }) {
  const [draft, setDraft] = useState(notes);
  const [prevNotes, setPrevNotes] = useState(notes);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Reset draft when notes prop changes (derived state computed during render)
  if (notes !== prevNotes) {
    setPrevNotes(notes);
    setDraft(notes);
  }

  useEffect(() => {
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, []);

  const save = useCallback(() => {
    const trimmed = draft.trim();
    if (trimmed !== notes) onSave(trimmed);
  }, [draft, notes, onSave]);

  return (
    <textarea
      ref={textareaRef}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={save}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          save();
        }
        if (e.key === "Escape") {
          save();
        }
      }}
      rows={3}
      placeholder="Add a note..."
      className="w-full bg-[#1a1b26] border border-[#2a2d3d] rounded px-2 py-1.5 text-[10px] text-[#c0caf5] outline-none resize-none placeholder-[#414868] focus:border-[#7aa2f7]/40 transition-colors"
      onClick={(e) => e.stopPropagation()}
    />
  );
}

// ---------------------------------------------------------------------------
// StateFilterBar
// ---------------------------------------------------------------------------

function StateFilterBar({
  stateCounts,
  activeFilters,
  onToggleFilter,
  onClearFilters,
}: {
  stateCounts: Record<SessionState, number>;
  activeFilters: Set<SessionState>;
  onToggleFilter: (state: SessionState) => void;
  onClearFilters: () => void;
}) {
  const hasActiveFilters = activeFilters.size > 0;

  return (
    <div className="flex items-center gap-1 px-3 py-1.5 border-b border-[#2a2d3d] bg-[#0d0e1a]/50 shrink-0 flex-wrap">
      {ALL_STATES.map((s) => {
        const count = stateCounts[s] ?? 0;
        const isActive = activeFilters.has(s);
        const color = STATE_COLORS[s];
        return (
          <button
            key={s}
            onClick={() => onToggleFilter(s)}
            className={`
              flex items-center gap-1 px-1.5 py-0.5 rounded-full text-[9px] font-medium
              transition-all duration-200
              ${isActive ? "ring-1" : "opacity-60 hover:opacity-100"}
            `}
            style={{
              backgroundColor: isActive ? color + "30" : color + "10",
              color: color,
              boxShadow: isActive ? `0 0 0 1px ${color}` : undefined,
            }}
            title={`${STATE_LABELS[s]} (${count})`}
          >
            <span
              className="w-1.5 h-1.5 rounded-full shrink-0"
              style={{ backgroundColor: color }}
            />
            <span>{count}</span>
          </button>
        );
      })}
      {hasActiveFilters && (
        <button
          onClick={onClearFilters}
          className="ml-auto px-1.5 py-0.5 rounded text-[9px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
        >
          Clear
        </button>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// WorkspacesSection
// ---------------------------------------------------------------------------

function WorkspacesSection({
  workspaces,
  onSave,
  onLoad,
  onDelete,
}: {
  workspaces: SavedWorkspace[];
  onSave: (name: string) => void;
  onLoad: (workspace: SavedWorkspace) => void;
  onDelete: (index: number) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [isNaming, setIsNaming] = useState(false);
  const [newName, setNewName] = useState("");
  const nameInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (isNaming) {
      requestAnimationFrame(() => nameInputRef.current?.focus());
    }
  }, [isNaming]);

  const handleSave = useCallback(() => {
    const trimmed = newName.trim();
    if (trimmed) {
      onSave(trimmed);
      setNewName("");
      setIsNaming(false);
    }
  }, [newName, onSave]);

  return (
    <div className="border-t border-[#2a2d3d] bg-[#0d0e1a]/30">
      {/* Collapsible header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 w-full px-3 py-1.5 text-[10px] font-semibold text-[#565f89] uppercase tracking-wider hover:text-[#c0caf5] transition-colors"
      >
        <ChevronDown
          className={`w-3 h-3 transition-transform duration-200 ${expanded ? "" : "-rotate-90"}`}
        />
        <FolderOpen className="w-3 h-3" />
        <span>Workspaces</span>
        {workspaces.length > 0 && (
          <span className="ml-auto text-[#414868] normal-case tracking-normal font-normal">
            {workspaces.length}
          </span>
        )}
      </button>

      {expanded && (
        <div className="px-2 pb-2 space-y-1">
          {/* Workspace list */}
          {workspaces.map((ws, idx) => (
            <div
              key={`${ws.name}-${ws.savedAt}`}
              className="flex items-center gap-1.5 px-2 py-1.5 rounded bg-[#1a1b26]/50 hover:bg-[#1a1b26] transition-colors group cursor-pointer"
              onClick={() => onLoad(ws)}
              title={`Load workspace "${ws.name}"`}
            >
              <div className="flex-1 min-w-0">
                <div className="text-[11px] text-[#c0caf5] truncate">{ws.name}</div>
                <div className="text-[9px] text-[#414868]">
                  {ws.sessions.length} zone{ws.sessions.length !== 1 ? "s" : ""} &middot;{" "}
                  {formatDate(ws.savedAt)}
                </div>
              </div>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDelete(idx);
                }}
                className="p-0.5 rounded text-[#414868] opacity-0 group-hover:opacity-100 hover:text-[#f7768e] hover:bg-[#2a2d3d] transition-all"
                title="Delete workspace"
              >
                <Trash2 className="w-3 h-3" />
              </button>
            </div>
          ))}

          {workspaces.length === 0 && (
            <div className="text-[10px] text-[#414868] italic px-2 py-1">No saved workspaces</div>
          )}

          {/* Save current */}
          {isNaming ? (
            <div className="flex items-center gap-1 px-1">
              <input
                ref={nameInputRef}
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onBlur={() => {
                  if (!newName.trim()) setIsNaming(false);
                  else handleSave();
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleSave();
                  if (e.key === "Escape") {
                    setIsNaming(false);
                    setNewName("");
                  }
                }}
                placeholder="Workspace name..."
                className="flex-1 bg-[#1a1b26] border border-[#7aa2f7]/50 rounded px-1.5 py-0.5 text-[10px] text-[#c0caf5] outline-none placeholder-[#414868]"
              />
              <button
                onClick={handleSave}
                disabled={!newName.trim()}
                className="p-1 rounded text-[#9ece6a] hover:bg-[#2a2d3d] transition-colors disabled:opacity-30"
                title="Save"
              >
                <Save className="w-3 h-3" />
              </button>
            </div>
          ) : (
            <button
              onClick={() => {
                if (workspaces.length >= MAX_WORKSPACES) return;
                setIsNaming(true);
              }}
              disabled={workspaces.length >= MAX_WORKSPACES}
              className="flex items-center gap-1 w-full px-2 py-1 rounded text-[10px] font-medium text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
              title={
                workspaces.length >= MAX_WORKSPACES
                  ? `Maximum ${MAX_WORKSPACES} workspaces`
                  : "Save current layout as workspace"
              }
            >
              <Save className="w-3 h-3" />
              Save Current
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// ZoneRow
// ---------------------------------------------------------------------------

interface ZoneRowProps {
  zoneIndex: number;
  tabId: string | undefined;
  tab: ZoneControlPanelProps["tabs"][number] | undefined;
  state: SessionState;
  label: string;
  notes: string;
  labelColor: string | undefined;
  isFocused: boolean;
  isPinned: boolean;
  lastOutput: string[];
  onFocus: () => void;
  onSetLabel: (label: string) => void;
  onSetNotes?: (notes: string) => void;
  onTogglePin?: () => void;
  // Drag-and-drop
  isDragging: boolean;
  isDropTarget: boolean;
  dropPosition: "above" | "below" | null;
  onDragStart: (e: React.DragEvent) => void;
  onDragEnd: () => void;
  onDragOver: (e: React.DragEvent) => void;
  onDragLeave: () => void;
  onDrop: (e: React.DragEvent) => void;
}

function ZoneRow({
  zoneIndex,
  tab,
  state,
  label,
  notes,
  labelColor,
  isFocused,
  isPinned,
  lastOutput,
  onFocus,
  onSetLabel,
  onSetNotes,
  onTogglePin,
  isDragging,
  isDropTarget,
  dropPosition,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDragLeave,
  onDrop,
}: ZoneRowProps) {
  const [hovered, setHovered] = useState(false);
  const [notesOpen, setNotesOpen] = useState(false);
  const stateColor = STATE_COLORS[state];
  const lastLine = lastOutput.length > 0 ? lastOutput[lastOutput.length - 1] : "";
  const hasNotes = notes.length > 0;

  return (
    <div className="relative" onDragOver={onDragOver} onDragLeave={onDragLeave} onDrop={onDrop}>
      {/* Drop indicator line — above */}
      {isDropTarget && dropPosition === "above" && (
        <div className="absolute top-0 left-1 right-1 h-0.5 bg-[#7aa2f7] rounded-full z-10" />
      )}

      <div
        draggable
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        className={`
          relative px-2.5 py-2 rounded-md cursor-pointer transition-all duration-200
          ${isFocused ? "bg-[#1a1b26]" : "bg-transparent hover:bg-[#1a1b26]/50"}
          ${isDragging ? "opacity-50" : "opacity-100"}
          ${isDropTarget ? "ring-1 ring-[#7aa2f7]/30" : ""}
        `}
        style={{
          borderLeft: isFocused ? `2px solid ${stateColor}` : "2px solid transparent",
        }}
        onClick={onFocus}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
      >
        {/* Row 1: drag handle, zone number, label, pin, state badge */}
        <div className="flex items-center gap-1.5 min-w-0">
          {/* Drag handle */}
          <span
            className={`shrink-0 cursor-grab active:cursor-grabbing transition-opacity duration-200 ${
              hovered ? "opacity-60" : "opacity-0"
            }`}
            title="Drag to reorder"
            onMouseDown={(e) => e.stopPropagation()}
          >
            <GripVertical className="w-3 h-3 text-[#565f89]" />
          </span>

          {/* State dot */}
          <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: stateColor }} />

          {/* Zone number */}
          <span className="text-[11px] font-mono font-bold text-[#a9b1d6] shrink-0">
            Z{zoneIndex + 1}
          </span>

          {/* Label */}
          <div className="flex-1 min-w-0 truncate">
            <EditableLabel
              value={label}
              onCommit={onSetLabel}
              placeholder="label"
              className={`text-[11px] font-medium ${label ? "" : "text-[#414868]"}`}
            />
          </div>

          {/* Notes indicator (always visible when notes exist) */}
          {hasNotes && !hovered && <StickyNote className="w-3 h-3 text-[#e0af68]/50 shrink-0" />}

          {/* Pin indicator */}
          {isPinned && <Pin className="w-3 h-3 text-[#bb9af7] shrink-0" />}

          {/* Label color swatch */}
          {label && labelColor && (
            <span
              className="w-2.5 h-2.5 rounded-sm shrink-0"
              style={{ backgroundColor: labelColor }}
            />
          )}

          {/* State badge */}
          <span
            className="px-1.5 py-0 rounded text-[9px] font-semibold uppercase tracking-wider shrink-0"
            style={{
              backgroundColor: stateColor + "20",
              color: stateColor,
            }}
          >
            {STATE_LABELS[state]}
          </span>
        </div>

        {/* Row 2: working directory */}
        {tab?.workingDir && (
          <div className="mt-0.5 pl-[34px] text-[10px] font-mono text-[#414868] truncate">
            {formatWorkingDir(tab.workingDir)}
          </div>
        )}

        {/* Row 3: last output line */}
        {lastLine && (
          <div className="mt-0.5 pl-[34px] text-[10px] text-[#414868] italic truncate">
            {truncate(lastLine, 40)}
          </div>
        )}

        {/* Row 4: hover actions */}
        {hovered && (
          <div className="absolute top-1.5 right-1.5 flex items-center gap-0.5">
            <button
              onClick={(e) => {
                e.stopPropagation();
                onFocus();
              }}
              className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
              title="Focus this zone"
            >
              <ExternalLink className="w-3 h-3" />
            </button>
            {onSetNotes && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setNotesOpen(!notesOpen);
                }}
                className={`p-0.5 rounded transition-colors ${
                  hasNotes || notesOpen
                    ? "text-[#e0af68] hover:text-[#e0af68]/80"
                    : "text-[#565f89] hover:text-[#c0caf5]"
                } hover:bg-[#2a2d3d]`}
                title={notesOpen ? "Close notes" : "Edit notes"}
              >
                <StickyNote className="w-3 h-3" />
              </button>
            )}
            {onTogglePin && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onTogglePin();
                }}
                className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
                title={isPinned ? "Unpin" : "Pin"}
              >
                {isPinned ? <PinOff className="w-3 h-3" /> : <Pin className="w-3 h-3" />}
              </button>
            )}
          </div>
        )}

        {/* Not-alive indicator */}
        {tab && !tab.isAlive && (
          <div className="mt-0.5 pl-[34px] text-[9px] text-[#f7768e]/60">
            exited ({tab.exitCode ?? "?"})
          </div>
        )}
      </div>

      {/* Inline notes editor (outside the draggable row, between rows) */}
      {notesOpen && onSetNotes && (
        <div className="px-2.5 py-1">
          <InlineNotesEditor
            notes={notes}
            onSave={(v) => {
              onSetNotes(v);
            }}
          />
        </div>
      )}

      {/* Drop indicator line — below */}
      {isDropTarget && dropPosition === "below" && (
        <div className="absolute bottom-0 left-1 right-1 h-0.5 bg-[#7aa2f7] rounded-full z-10" />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// ZoneControlPanel
// ---------------------------------------------------------------------------

export const ZoneControlPanel = React.memo(function ZoneControlPanel({
  tabs,
  assignments,
  sessionStates,
  zoneLabels,
  zoneNotes,
  labelColorMap,
  focusedZone,
  zoneCount,
  lastOutputLines,
  onFocusZone,
  onSetZoneLabel,
  onSetZoneNotes,
  onClose,
  collapsed,
  onToggleCollapsed,
  onCreateTerminal,
  pinnedZones,
  onTogglePin,
  onSwapZones,
  onLoadWorkspace,
  layoutId,
}: ZoneControlPanelProps) {
  // -----------------------------------------------------------------------
  // Drag-and-drop state
  // -----------------------------------------------------------------------
  const [draggedZone, setDraggedZone] = useState<number | null>(null);
  const [dropTargetZone, setDropTargetZone] = useState<number | null>(null);
  const [dropPosition, setDropPosition] = useState<"above" | "below" | null>(null);

  // -----------------------------------------------------------------------
  // State filter
  // -----------------------------------------------------------------------
  const [activeFilters, setActiveFilters] = useState<Set<SessionState>>(new Set());

  const toggleFilter = useCallback((state: SessionState) => {
    setActiveFilters((prev) => {
      const next = new Set(prev);
      if (next.has(state)) {
        next.delete(state);
      } else {
        next.add(state);
      }
      return next;
    });
  }, []);

  const clearFilters = useCallback(() => {
    setActiveFilters(new Set());
  }, []);

  // -----------------------------------------------------------------------
  // Workspaces
  // -----------------------------------------------------------------------
  const [workspaces, setWorkspaces] = useState<SavedWorkspace[]>(() => loadWorkspaces());

  const handleSaveWorkspace = useCallback(
    (name: string) => {
      const sessions = Array.from({ length: zoneCount }, (_, i) => {
        const tabId = assignments[i];
        const tab = tabId ? tabs.find((t) => t.id === tabId) : undefined;
        return {
          title: tab?.title ?? "",
          workingDir: tab?.workingDir,
          zoneIndex: i,
          label: zoneLabels[i] ?? undefined,
          notes: zoneNotes[i] ?? undefined,
          pinned: pinnedZones?.has(i) ?? false,
        };
      });

      const ws: SavedWorkspace = {
        name,
        layoutId: layoutId ?? "default",
        sessions,
        savedAt: Date.now(),
      };

      setWorkspaces((prev) => {
        const next = [ws, ...prev].slice(0, MAX_WORKSPACES);
        saveWorkspacesToStorage(next);
        return next;
      });
    },
    [zoneCount, assignments, tabs, zoneLabels, zoneNotes, pinnedZones, layoutId],
  );

  const handleDeleteWorkspace = useCallback((index: number) => {
    setWorkspaces((prev) => {
      const next = [...prev];
      next.splice(index, 1);
      saveWorkspacesToStorage(next);
      return next;
    });
  }, []);

  const handleLoadWorkspace = useCallback(
    (workspace: SavedWorkspace) => {
      onLoadWorkspace?.(workspace);
    },
    [onLoadWorkspace],
  );

  // -----------------------------------------------------------------------
  // Drag-and-drop handlers
  // -----------------------------------------------------------------------
  const handleDragStart = useCallback((zoneIndex: number, e: React.DragEvent) => {
    setDraggedZone(zoneIndex);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", String(zoneIndex));
  }, []);

  const handleDragEnd = useCallback(() => {
    setDraggedZone(null);
    setDropTargetZone(null);
    setDropPosition(null);
  }, []);

  const handleDragOver = useCallback(
    (zoneIndex: number, e: React.DragEvent) => {
      if (draggedZone === null || draggedZone === zoneIndex) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";

      const rect = e.currentTarget.getBoundingClientRect();
      const midY = rect.top + rect.height / 2;
      const pos = e.clientY < midY ? "above" : "below";

      setDropTargetZone(zoneIndex);
      setDropPosition(pos);
    },
    [draggedZone],
  );

  const handleDragLeave = useCallback(() => {
    setDropTargetZone(null);
    setDropPosition(null);
  }, []);

  const handleDrop = useCallback(
    (targetZone: number, e: React.DragEvent) => {
      e.preventDefault();
      if (draggedZone !== null && draggedZone !== targetZone && onSwapZones) {
        onSwapZones(draggedZone, targetZone);
      }
      setDraggedZone(null);
      setDropTargetZone(null);
      setDropPosition(null);
    },
    [draggedZone, onSwapZones],
  );

  // -----------------------------------------------------------------------
  // Build a lookup from tabId -> tab object
  // -----------------------------------------------------------------------
  const tabMap = useMemo(() => {
    const m = new Map<string, (typeof tabs)[number]>();
    for (const t of tabs) m.set(t.id, t);
    return m;
  }, [tabs]);

  // Compute zone data
  const zones = useMemo(() => {
    const result: Array<{
      zoneIndex: number;
      tabId: string | undefined;
      tab: (typeof tabs)[number] | undefined;
      state: SessionState;
      label: string;
      notes: string;
      labelColor: string | undefined;
      isPinned: boolean;
      lastOutput: string[];
    }> = [];

    for (let i = 0; i < zoneCount; i++) {
      const tabId = assignments[i];
      const tab = tabId ? tabMap.get(tabId) : undefined;
      const state = tabId ? getState(sessionStates[tabId]) : "idle";
      const label = zoneLabels[i] ?? "";
      const notes = zoneNotes[i] ?? "";
      const labelColor = label ? labelColorMap[label] : undefined;
      const isPinned = pinnedZones?.has(i) ?? false;
      const lastOutput = tabId ? (lastOutputLines[tabId] ?? []) : [];

      result.push({
        zoneIndex: i,
        tabId,
        tab,
        state,
        label,
        notes,
        labelColor,
        isPinned,
        lastOutput,
      });
    }

    return result;
  }, [
    zoneCount,
    assignments,
    tabMap,
    sessionStates,
    zoneLabels,
    zoneNotes,
    labelColorMap,
    pinnedZones,
    lastOutputLines,
  ]);

  // State counts for filter bar
  const stateCounts = useMemo(() => {
    const counts: Record<SessionState, number> = {
      idle: 0,
      working: 0,
      "needs-input": 0,
      completed: 0,
      error: 0,
    };
    for (const z of zones) {
      counts[z.state]++;
    }
    return counts;
  }, [zones]);

  // Filtered zones
  const filteredZones = useMemo(() => {
    if (activeFilters.size === 0) return zones;
    return zones.filter((z) => activeFilters.has(z.state));
  }, [zones, activeFilters]);

  // Find unassigned tabs
  const assignedTabIds = useMemo(() => new Set(Object.values(assignments)), [assignments]);
  const unassignedTabs = useMemo(
    () => tabs.filter((t) => !assignedTabIds.has(t.id)),
    [tabs, assignedTabIds],
  );

  // Total session count
  const totalSessions = tabs.length;

  // -----------------------------------------------------------------------
  // Collapsed view: narrow icon strip
  // -----------------------------------------------------------------------
  if (collapsed) {
    return (
      <div className="w-9 h-full shrink-0 flex flex-col bg-[#13141f] border-l border-[#2a2d3d]">
        {/* Expand button */}
        <div className="flex items-center justify-center py-2 border-b border-[#2a2d3d] bg-[#0d0e1a]">
          <button
            onClick={onToggleCollapsed}
            className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
            title="Expand control panel"
          >
            <ChevronLeft className="w-3.5 h-3.5" />
          </button>
        </div>

        {/* Zone state dots */}
        <div className="flex-1 flex flex-col items-center gap-1.5 py-2 overflow-y-auto">
          {zones.map((z) => (
            <button
              key={z.zoneIndex}
              onClick={() => {
                onFocusZone(z.zoneIndex);
                onToggleCollapsed();
              }}
              className={`
                w-5 h-5 rounded-full flex items-center justify-center text-[8px] font-mono font-bold
                transition-all hover:scale-110
                ${z.zoneIndex === focusedZone ? "ring-1 ring-[#c0caf5]" : ""}
              `}
              style={{
                backgroundColor: STATE_COLORS[z.state] + "30",
                color: STATE_COLORS[z.state],
              }}
              title={`Z${z.zoneIndex + 1}: ${STATE_LABELS[z.state]}${z.label ? ` — ${z.label}` : ""}`}
            >
              {z.zoneIndex + 1}
            </button>
          ))}
        </div>
      </div>
    );
  }

  // -----------------------------------------------------------------------
  // Expanded view
  // -----------------------------------------------------------------------
  return (
    <div
      className="h-full shrink-0 flex flex-col bg-[#13141f] border-l border-[#2a2d3d]"
      style={{ width: 280, transition: "width 200ms ease" }}
    >
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2.5 border-b border-[#2a2d3d] bg-[#0d0e1a] shrink-0">
        <LayoutGrid className="w-4 h-4 text-[#bb9af7]" />
        <span className="flex-1 text-sm font-semibold text-[#c0caf5]">Control Panel</span>
        <button
          onClick={onToggleCollapsed}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title="Collapse"
        >
          <ChevronRight className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={onClose}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title="Close"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* State filter bar */}
      <StateFilterBar
        stateCounts={stateCounts}
        activeFilters={activeFilters}
        onToggleFilter={toggleFilter}
        onClearFilters={clearFilters}
      />

      {/* Scrollable zone list */}
      <div className="flex-1 overflow-y-auto py-1.5 px-1.5 space-y-0.5 scrollbar-thin scrollbar-thumb-[#2a2d3d] scrollbar-track-transparent">
        {filteredZones.length === 0 && zones.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-[#565f89]">
            <Circle className="w-6 h-6 opacity-30 mb-2" />
            <span className="text-xs">No zones configured</span>
          </div>
        ) : filteredZones.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-[#565f89]">
            <span className="text-xs">No zones match filters</span>
            <button
              onClick={clearFilters}
              className="mt-1 text-[10px] text-[#7aa2f7] hover:underline"
            >
              Clear filters
            </button>
          </div>
        ) : (
          filteredZones.map((z) => (
            <ZoneRow
              key={z.zoneIndex}
              zoneIndex={z.zoneIndex}
              tabId={z.tabId}
              tab={z.tab}
              state={z.state}
              label={z.label}
              notes={z.notes}
              labelColor={z.labelColor}
              isFocused={z.zoneIndex === focusedZone}
              isPinned={z.isPinned}
              lastOutput={z.lastOutput}
              onFocus={() => onFocusZone(z.zoneIndex)}
              onSetLabel={(v) => onSetZoneLabel(z.zoneIndex, v)}
              onSetNotes={onSetZoneNotes ? (v) => onSetZoneNotes(z.zoneIndex, v) : undefined}
              onTogglePin={onTogglePin ? () => onTogglePin(z.zoneIndex) : undefined}
              // Drag-and-drop props
              isDragging={draggedZone === z.zoneIndex}
              isDropTarget={dropTargetZone === z.zoneIndex}
              dropPosition={dropTargetZone === z.zoneIndex ? dropPosition : null}
              onDragStart={(e) => handleDragStart(z.zoneIndex, e)}
              onDragEnd={handleDragEnd}
              onDragOver={(e) => handleDragOver(z.zoneIndex, e)}
              onDragLeave={handleDragLeave}
              onDrop={(e) => handleDrop(z.zoneIndex, e)}
            />
          ))
        )}

        {/* Unassigned tabs section */}
        {unassignedTabs.length > 0 && (
          <div className="mt-3 pt-2 border-t border-[#2a2d3d]/50">
            <div className="px-2 text-[10px] font-semibold text-[#414868] uppercase tracking-wider mb-1">
              Unassigned ({unassignedTabs.length})
            </div>
            {unassignedTabs.map((tab) => {
              const state = getState(sessionStates[tab.id]);
              return (
                <div
                  key={tab.id}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 rounded text-[#565f89]"
                >
                  <span
                    className="w-1.5 h-1.5 rounded-full shrink-0"
                    style={{ backgroundColor: STATE_COLORS[state] }}
                  />
                  <span className="flex-1 text-[11px] truncate">{tab.title}</span>
                  {!tab.isAlive && <span className="text-[9px] text-[#f7768e]/50">exited</span>}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Workspaces section (above footer) */}
      <WorkspacesSection
        workspaces={workspaces}
        onSave={handleSaveWorkspace}
        onLoad={handleLoadWorkspace}
        onDelete={handleDeleteWorkspace}
      />

      {/* Footer */}
      <div className="flex items-center gap-2 px-3 py-2 border-t border-[#2a2d3d] bg-[#0d0e1a] shrink-0">
        {onCreateTerminal && (
          <button
            onClick={onCreateTerminal}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] font-medium bg-[#7aa2f7]/10 text-[#7aa2f7] hover:bg-[#7aa2f7]/20 transition-colors"
          >
            <Plus className="w-3 h-3" />
            New Session
          </button>
        )}
        <span className="ml-auto text-[10px] text-[#414868]">
          {totalSessions} session{totalSessions !== 1 ? "s" : ""}
        </span>
      </div>
    </div>
  );
});
