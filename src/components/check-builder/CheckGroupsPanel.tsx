/**
 * Check Groups Panel
 *
 * Displays and manages check groups - collections of checks that can be
 * run together as a single step in workflows.
 */

import { useState, useCallback } from "react";
import {
  FolderOpen,
  Plus,
  Play,
  MoreVertical,
  Trash2,
  ChevronDown,
  ChevronRight,
  Settings,
  Layers,
  CheckCircle,
  XCircle,
  Clock,
} from "lucide-react";
import { useCheckBuilder } from "./CheckBuilderContext";
import type { CheckGroup, Check } from "./types";

// Group list item component
interface GroupListItemProps {
  group: CheckGroup;
  checks: Check[];
  isSelected: boolean;
  isExpanded: boolean;
  onSelect: () => void;
  onToggleExpand: () => void;
  onDelete: () => void;
  onExecute: () => void;
}

function GroupListItem({
  group,
  checks,
  isSelected,
  isExpanded,
  onSelect,
  onToggleExpand,
  onDelete,
  onExecute,
}: GroupListItemProps) {
  const [showMenu, setShowMenu] = useState(false);

  return (
    <div>
      <div
        className={`
          group flex items-center gap-2 px-3 py-2 rounded-md cursor-pointer
          transition-colors
          ${
            isSelected
              ? "bg-cyan-500/15 text-cyan-400 border-l-2 border-cyan-400 ml-[-1px]"
              : "hover:bg-muted/30"
          }
        `}
        onClick={onSelect}
      >
        <button
          className="p-0.5 hover:bg-muted/50 rounded"
          onClick={(e) => {
            e.stopPropagation();
            onToggleExpand();
          }}
        >
          {isExpanded ? (
            <ChevronDown className="w-4 h-4" />
          ) : (
            <ChevronRight className="w-4 h-4" />
          )}
        </button>
        <div
          className="w-3 h-3 rounded-full flex-shrink-0"
          style={{ backgroundColor: group.color || "#6366f1" }}
        />
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium truncate">{group.name}</div>
          <div className="text-xs text-muted-foreground">
            {checks.length} check{checks.length !== 1 ? "s" : ""}
            {!group.enabled && (
              <span className="ml-1 text-amber-500">(disabled)</span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-1">
          <button
            className="p-1 opacity-0 group-hover:opacity-100 hover:bg-muted rounded transition-opacity"
            onClick={(e) => {
              e.stopPropagation();
              onExecute();
            }}
            title="Run all checks in this group"
          >
            <Play className="w-4 h-4 text-cyan-500" />
          </button>
          <div className="relative">
            <button
              className="p-1 opacity-0 group-hover:opacity-100 hover:bg-muted rounded transition-opacity"
              onClick={(e) => {
                e.stopPropagation();
                setShowMenu(!showMenu);
              }}
            >
              <MoreVertical className="w-4 h-4" />
            </button>
            {showMenu && (
              <>
                <div className="fixed inset-0 z-10" onClick={() => setShowMenu(false)} />
                <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[120px]">
                  <button
                    className="w-full px-3 py-1.5 text-sm text-left hover:bg-muted flex items-center gap-2 text-destructive"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDelete();
                      setShowMenu(false);
                    }}
                  >
                    <Trash2 className="w-4 h-4" />
                    Delete
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Expanded check list */}
      {isExpanded && checks.length > 0 && (
        <div className="ml-6 pl-2 border-l border-border/50 space-y-0.5 mt-1">
          {checks.map((check) => (
            <div
              key={check.id}
              className="flex items-center gap-2 px-2 py-1.5 text-sm text-muted-foreground"
            >
              <div className="w-2 h-2 rounded-full bg-muted-foreground/50" />
              <span className="truncate">{check.name}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// Create group modal
interface CreateGroupModalProps {
  isOpen: boolean;
  onClose: () => void;
  onCreate: (name: string, description: string, color: string) => void;
}

function CreateGroupModal({ isOpen, onClose, onCreate }: CreateGroupModalProps) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [color, setColor] = useState("#6366f1");

  const colors = [
    "#ef4444", // red
    "#f97316", // orange
    "#eab308", // yellow
    "#22c55e", // green
    "#14b8a6", // teal
    "#06b6d4", // cyan
    "#3b82f6", // blue
    "#6366f1", // indigo
    "#8b5cf6", // violet
    "#d946ef", // fuchsia
    "#ec4899", // pink
  ];

  const handleCreate = () => {
    if (name.trim()) {
      onCreate(name.trim(), description.trim(), color);
      setName("");
      setDescription("");
      setColor("#6366f1");
      onClose();
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-card border border-border rounded-lg shadow-xl w-[400px] max-w-[90vw]">
        <div className="p-4 border-b border-border">
          <h2 className="text-lg font-semibold">Create Check Group</h2>
        </div>
        <div className="p-4 space-y-4">
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-muted-foreground">Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g., Python Checks, Pre-commit"
              className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
              data-ui-id="check-builder-create-group-name-input"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-muted-foreground">Description</label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Optional description"
              rows={2}
              className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50 resize-none"
              data-ui-id="check-builder-create-group-description-input"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-muted-foreground">Color</label>
            <div className="flex flex-wrap gap-2">
              {colors.map((c) => (
                <button
                  key={c}
                  onClick={() => setColor(c)}
                  className={`w-6 h-6 rounded-full transition-transform ${
                    color === c ? "ring-2 ring-offset-2 ring-offset-card ring-white scale-110" : ""
                  }`}
                  style={{ backgroundColor: c }}
                />
              ))}
            </div>
          </div>
        </div>
        <div className="p-4 border-t border-border flex justify-end gap-2">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm rounded-md hover:bg-muted transition-colors"
            data-ui-id="check-builder-create-group-cancel-btn"
          >
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!name.trim()}
            className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                     transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            data-ui-id="check-builder-create-group-confirm-btn"
          >
            Create Group
          </button>
        </div>
      </div>
    </div>
  );
}

// Assign checks to group modal
interface AssignChecksModalProps {
  isOpen: boolean;
  group: CheckGroup | null;
  currentCheckIds: string[];
  allChecks: Check[];
  onClose: () => void;
  onSave: (checkIds: string[]) => void;
}

function AssignChecksModal({
  isOpen,
  group,
  currentCheckIds,
  allChecks,
  onClose,
  onSave,
}: AssignChecksModalProps) {
  const [selectedIds, setSelectedIds] = useState<string[]>(currentCheckIds);

  // Reset selection when modal opens
  useState(() => {
    setSelectedIds(currentCheckIds);
  });

  const toggleCheck = (checkId: string) => {
    setSelectedIds((prev) =>
      prev.includes(checkId) ? prev.filter((id) => id !== checkId) : [...prev, checkId],
    );
  };

  const handleSave = () => {
    onSave(selectedIds);
    onClose();
  };

  if (!isOpen || !group) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-card border border-border rounded-lg shadow-xl w-[500px] max-w-[90vw] max-h-[80vh] flex flex-col">
        <div className="p-4 border-b border-border">
          <h2 className="text-lg font-semibold">Assign Checks to "{group.name}"</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Select which checks should be included in this group
          </p>
        </div>
        <div className="flex-1 overflow-y-auto p-4">
          {allChecks.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              No checks available. Create some checks first.
            </div>
          ) : (
            <div className="space-y-1">
              {allChecks.map((check) => (
                <label
                  key={check.id}
                  className="flex items-center gap-3 px-3 py-2 rounded-md hover:bg-muted/30 cursor-pointer"
                >
                  <input
                    type="checkbox"
                    checked={selectedIds.includes(check.id)}
                    onChange={() => toggleCheck(check.id)}
                    className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
                    data-ui-id={`check-builder-assign-check-${check.id}-checkbox`}
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium">{check.name}</div>
                    <div className="text-xs text-muted-foreground">
                      {check.tool} - {check.check_type}
                    </div>
                  </div>
                </label>
              ))}
            </div>
          )}
        </div>
        <div className="p-4 border-t border-border flex justify-between items-center">
          <span className="text-sm text-muted-foreground">
            {selectedIds.length} check{selectedIds.length !== 1 ? "s" : ""} selected
          </span>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-muted transition-colors"
              data-ui-id="check-builder-assign-checks-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSave}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md transition-colors"
              data-ui-id="check-builder-assign-checks-save-btn"
            >
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// Group editor panel (shown when a group is selected)
interface GroupEditorPanelProps {
  group: CheckGroup;
  checks: Check[];
  onUpdate: (updates: { name?: string; description?: string; color?: string; enabled?: boolean; run_in_parallel?: boolean; stop_on_failure?: boolean }) => void;
  onAssignChecks: () => void;
  onExecute: () => void;
  isExecuting?: boolean;
}

function GroupEditorPanel({
  group,
  checks,
  onUpdate,
  onAssignChecks,
  onExecute,
  isExecuting,
}: GroupEditorPanelProps) {
  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 bg-neutral-800 border-b border-neutral-700">
        <div className="flex items-center gap-3">
          <div
            className="w-4 h-4 rounded-full"
            style={{ backgroundColor: group.color || "#6366f1" }}
          />
          <div>
            <h3 className="text-sm font-medium">{group.name}</h3>
            <p className="text-xs text-muted-foreground">
              {checks.length} check{checks.length !== 1 ? "s" : ""}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={onAssignChecks}
            className="px-3 py-1.5 text-sm bg-neutral-700 hover:bg-neutral-600 rounded-md flex items-center gap-1.5 transition-colors"
            data-ui-id="check-builder-manage-checks-btn"
          >
            <Settings className="w-3.5 h-3.5" />
            Manage Checks
          </button>
          <button
            onClick={onExecute}
            disabled={isExecuting || checks.length === 0}
            className="px-3 py-1.5 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md flex items-center gap-1.5 transition-colors disabled:opacity-50"
            data-ui-id="check-builder-run-group-btn"
          >
            <Play className="w-3.5 h-3.5" />
            {isExecuting ? "Running..." : "Run Group"}
          </button>
        </div>
      </div>

      {/* Configuration */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Name */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Name</label>
          <input
            type="text"
            value={group.name}
            onChange={(e) => onUpdate({ name: e.target.value })}
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="check-builder-group-name-input"
          />
        </div>

        {/* Description */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Description</label>
          <textarea
            value={group.description || ""}
            onChange={(e) => onUpdate({ description: e.target.value || undefined })}
            placeholder="Optional description"
            rows={2}
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50 resize-none"
            data-ui-id="check-builder-group-description-input"
          />
        </div>

        {/* Toggles */}
        <div className="space-y-3 pt-2">
          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-sm">Enabled</span>
            <input
              type="checkbox"
              checked={group.enabled}
              onChange={(e) => onUpdate({ enabled: e.target.checked })}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
              data-ui-id="check-builder-group-enabled-checkbox"
            />
          </label>
          <label className="flex items-center justify-between cursor-pointer">
            <div>
              <span className="text-sm">Run in Parallel</span>
              <p className="text-xs text-muted-foreground">Run all checks simultaneously</p>
            </div>
            <input
              type="checkbox"
              checked={group.run_in_parallel}
              onChange={(e) => onUpdate({ run_in_parallel: e.target.checked })}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
              data-ui-id="check-builder-group-parallel-checkbox"
            />
          </label>
          <label className="flex items-center justify-between cursor-pointer">
            <div>
              <span className="text-sm">Stop on Failure</span>
              <p className="text-xs text-muted-foreground">Stop running checks if one fails</p>
            </div>
            <input
              type="checkbox"
              checked={group.stop_on_failure}
              onChange={(e) => onUpdate({ stop_on_failure: e.target.checked })}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
              data-ui-id="check-builder-group-stop-on-failure-checkbox"
            />
          </label>
        </div>

        {/* Checks in group */}
        <div className="pt-4 border-t border-neutral-700">
          <div className="flex items-center justify-between mb-3">
            <h4 className="text-sm font-medium">Checks in this group</h4>
            <button
              onClick={onAssignChecks}
              className="text-xs text-cyan-500 hover:text-cyan-400"
            >
              Edit
            </button>
          </div>
          {checks.length === 0 ? (
            <div className="text-sm text-muted-foreground text-center py-4 border border-dashed border-neutral-700 rounded-md">
              No checks assigned yet
            </div>
          ) : (
            <div className="space-y-1">
              {checks.map((check) => (
                <div
                  key={check.id}
                  className="flex items-center gap-2 px-3 py-2 bg-neutral-800/50 rounded-md"
                >
                  <Layers className="w-4 h-4 text-muted-foreground" />
                  <span className="text-sm flex-1">{check.name}</span>
                  <span className="text-xs text-muted-foreground">{check.tool}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// Main CheckGroupsPanel component
export function CheckGroupsPanel() {
  const {
    checkGroups,
    checks,
    selectedGroupId,
    selectGroup,
    createCheckGroup,
    updateCheckGroup,
    deleteCheckGroup,
    getChecksInGroup,
    setChecksInGroup,
    executeCheckGroup,
    lastGroupExecutionResult,
  } = useCheckBuilder();

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [showAssignModal, setShowAssignModal] = useState(false);
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const [isExecuting, setIsExecuting] = useState(false);

  const selectedGroup = selectedGroupId
    ? checkGroups.find((g) => g.id === selectedGroupId) || null
    : null;
  const selectedGroupChecks = selectedGroupId ? getChecksInGroup(selectedGroupId) : [];

  const handleCreateGroup = useCallback(
    async (name: string, description: string, color: string) => {
      await createCheckGroup({ name, description, color });
    },
    [createCheckGroup],
  );

  const handleDeleteGroup = useCallback(
    async (id: string) => {
      if (window.confirm("Are you sure you want to delete this group?")) {
        await deleteCheckGroup(id);
      }
    },
    [deleteCheckGroup],
  );

  const handleExecuteGroup = useCallback(
    async (groupId: string) => {
      setIsExecuting(true);
      try {
        await executeCheckGroup(groupId);
      } finally {
        setIsExecuting(false);
      }
    },
    [executeCheckGroup],
  );

  const toggleExpanded = (groupId: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) {
        next.delete(groupId);
      } else {
        next.add(groupId);
      }
      return next;
    });
  };

  return (
    <div className="h-full flex bg-neutral-900/50">
      {/* Left: Groups list */}
      <div className="w-64 flex-shrink-0 flex flex-col border-r border-border/50 bg-card">
        {/* Header */}
        <div className="p-4 border-b border-border/50">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-semibold flex items-center gap-2">
              <FolderOpen className="w-4 h-4" />
              Check Groups
            </h2>
            <button
              className="p-1.5 hover:bg-muted rounded-md transition-colors"
              onClick={() => setShowCreateModal(true)}
              title="Create new group"
              data-ui-id="check-builder-new-group-btn"
            >
              <Plus className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Groups list */}
        <div className="flex-1 overflow-y-auto p-2 space-y-1">
          {checkGroups.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground text-sm">
              No groups yet.
              <br />
              <button
                onClick={() => setShowCreateModal(true)}
                className="text-cyan-500 hover:text-cyan-400 mt-2"
              >
                Create your first group
              </button>
            </div>
          ) : (
            checkGroups.map((group) => (
              <GroupListItem
                key={group.id}
                group={group}
                checks={getChecksInGroup(group.id)}
                isSelected={selectedGroupId === group.id}
                isExpanded={expandedGroups.has(group.id)}
                onSelect={() => selectGroup(group.id)}
                onToggleExpand={() => toggleExpanded(group.id)}
                onDelete={() => handleDeleteGroup(group.id)}
                onExecute={() => handleExecuteGroup(group.id)}
              />
            ))
          )}
        </div>

        {/* Footer */}
        <div className="p-3 border-t border-border/50">
          <span className="text-xs text-muted-foreground">
            {checkGroups.length} group{checkGroups.length !== 1 ? "s" : ""} total
          </span>
        </div>
      </div>

      {/* Right: Group editor or empty state */}
      {selectedGroup ? (
        <GroupEditorPanel
          group={selectedGroup}
          checks={selectedGroupChecks}
          onUpdate={(updates) => updateCheckGroup(selectedGroup.id, updates)}
          onAssignChecks={() => setShowAssignModal(true)}
          onExecute={() => handleExecuteGroup(selectedGroup.id)}
          isExecuting={isExecuting}
        />
      ) : (
        <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground">
          <FolderOpen className="w-16 h-16 mb-4 opacity-20" />
          <p className="text-sm">Select a group to configure</p>
          <p className="text-xs text-muted-foreground/70 mt-2">
            or create a new one from the sidebar
          </p>
        </div>
      )}

      {/* Execution results */}
      {lastGroupExecutionResult && selectedGroupId === lastGroupExecutionResult.group.id && (
        <div className="absolute bottom-0 left-64 right-0 bg-neutral-900 border-t border-neutral-700">
          <div className="px-4 py-3 flex items-center justify-between">
            <div className="flex items-center gap-4">
              <span className="text-sm font-medium">Last Run Results</span>
              <div className="flex items-center gap-3 text-xs">
                <span className="flex items-center gap-1 text-green-400">
                  <CheckCircle className="w-3 h-3" />
                  {lastGroupExecutionResult.summary.passed} passed
                </span>
                {lastGroupExecutionResult.summary.failed > 0 && (
                  <span className="flex items-center gap-1 text-red-400">
                    <XCircle className="w-3 h-3" />
                    {lastGroupExecutionResult.summary.failed} failed
                  </span>
                )}
                <span className="flex items-center gap-1 text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {lastGroupExecutionResult.summary.duration_ms}ms
                </span>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Modals */}
      <CreateGroupModal
        isOpen={showCreateModal}
        onClose={() => setShowCreateModal(false)}
        onCreate={handleCreateGroup}
      />
      <AssignChecksModal
        isOpen={showAssignModal}
        group={selectedGroup}
        currentCheckIds={selectedGroupId ? (getChecksInGroup(selectedGroupId).map((c) => c.id)) : []}
        allChecks={checks}
        onClose={() => setShowAssignModal(false)}
        onSave={(checkIds) => selectedGroupId && setChecksInGroup(selectedGroupId, checkIds)}
      />
    </div>
  );
}
