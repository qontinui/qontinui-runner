/**
 * Check Group Library Picker
 *
 * Modal dialog for selecting a saved check group from the Check Builder library
 * to add as a workflow step. Running a check group executes all checks in
 * the group as a single step.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Search, X, Check as CheckIcon, FolderOpen, Layers } from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// Check group type from Check Builder
interface CheckGroup {
  id: string;
  name: string;
  description?: string;
  color?: string;
  enabled: boolean;
  run_in_parallel: boolean;
  stop_on_failure: boolean;
  tags: string[];
  created_at: string;
  updated_at: string;
}

// Check type for displaying group contents
interface SavedCheck {
  id: string;
  name: string;
  check_type: string;
  tool: string;
}

interface CheckGroupLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (group: CheckGroup, checksInGroup: SavedCheck[], phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

export function CheckGroupLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: CheckGroupLibraryPickerProps) {
  const [groups, setGroups] = useState<CheckGroup[]>([]);
  const [groupChecks, setGroupChecks] = useState<Record<string, SavedCheck[]>>({});
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [isLoading, setIsLoading] = useState(false);

  // Fetch check groups and their checks
  useEffect(() => {
    if (!isOpen) return;

    const fetchGroups = async () => {
      setIsLoading(true);
      try {
        // Fetch groups
        const groupsResponse = await invoke<{
          success: boolean;
          data?: CheckGroup[];
          message?: string;
        }>("list_check_groups", { enabledOnly: false });

        if (groupsResponse.success && groupsResponse.data) {
          setGroups(groupsResponse.data);

          // Fetch checks for each group
          const checksMap: Record<string, SavedCheck[]> = {};
          for (const group of groupsResponse.data) {
            try {
              const checksResponse = await invoke<{
                success: boolean;
                data?: SavedCheck[];
                message?: string;
              }>("get_checks_in_group", { groupId: group.id });

              if (checksResponse.success && checksResponse.data) {
                checksMap[group.id] = checksResponse.data;
              } else {
                checksMap[group.id] = [];
              }
            } catch {
              checksMap[group.id] = [];
            }
          }
          setGroupChecks(checksMap);
        }
      } catch (err) {
        console.error("Failed to fetch check groups:", err);
      } finally {
        setIsLoading(false);
      }
    };

    fetchGroups();
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedId(null);
      setSearchQuery("");
    }
  }, [isOpen]);

  // Filter groups
  const filteredGroups = groups.filter((group) => {
    const matchesSearch =
      !searchQuery ||
      group.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      group.description?.toLowerCase().includes(searchQuery.toLowerCase());

    return matchesSearch;
  });

  const handleSelect = useCallback(() => {
    const selected = groups.find((g) => g.id === selectedId);
    if (selected) {
      onSelect(selected, groupChecks[selected.id] || [], phase);
      onClose();
    }
  }, [groups, groupChecks, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (group: CheckGroup) => {
      onSelect(group, groupChecks[group.id] || [], phase);
      onClose();
    },
    [groupChecks, onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        data-ui-id="dialog-check-group-library-picker"
        className="bg-card border border-border rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <div>
            <h2 className="text-lg font-semibold">Select Check Group</h2>
            <p className="text-sm text-muted-foreground">
              Run all checks in a group as a single {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-muted/80 rounded-md transition-colors"
            data-ui-id="workflow-builder-check-group-picker-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search check groups..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted border border-border rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
              data-ui-id="workflow-builder-check-group-picker-search-input"
            />
          </div>
        </div>

        {/* Group List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading check groups...</div>
          ) : filteredGroups.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {groups.length === 0
                ? "No check groups created yet. Create groups in the Check Builder first."
                : "No groups match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredGroups.map((group) => {
                const isSelected = selectedId === group.id;
                const checks = groupChecks[group.id] || [];

                return (
                  <button
                    key={group.id}
                    onClick={() => setSelectedId(group.id)}
                    onDoubleClick={() => handleDoubleClick(group)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-cyan-500/15 border-2 border-cyan-500"
                          : "bg-muted/50 border-2 border-transparent hover:bg-muted hover:border-border"
                      }
                    `}
                    data-ui-id={`workflow-builder-check-group-picker-item-${group.id}`}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div
                      className="p-2 rounded-md bg-card"
                      style={{ color: group.color || "#6366f1" }}
                    >
                      <FolderOpen className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{group.name}</span>
                        {!group.enabled && (
                          <span className="text-xs px-1.5 py-0.5 bg-amber-500/20 text-amber-400 rounded">
                            disabled
                          </span>
                        )}
                      </div>
                      {group.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {group.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        <span className="flex items-center gap-1">
                          <Layers className="w-3 h-3" />
                          {checks.length} check{checks.length !== 1 ? "s" : ""}
                        </span>
                        {group.run_in_parallel && <span className="text-cyan-400">parallel</span>}
                        {group.stop_on_failure && (
                          <span className="text-amber-400">stop on failure</span>
                        )}
                      </div>
                      {/* Show check names */}
                      {checks.length > 0 && (
                        <div className="mt-2 flex flex-wrap gap-1">
                          {checks.slice(0, 5).map((check) => (
                            <span
                              key={check.id}
                              className="text-xs px-1.5 py-0.5 bg-muted/80 rounded"
                            >
                              {check.name}
                            </span>
                          ))}
                          {checks.length > 5 && (
                            <span className="text-xs px-1.5 py-0.5 text-muted-foreground">
                              +{checks.length - 5} more
                            </span>
                          )}
                        </div>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-border bg-muted/50">
          <span
            data-content-role="metric"
            data-content-label="available group count"
            className="text-sm text-muted-foreground"
          >
            {filteredGroups.length} group{filteredGroups.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-muted/80 transition-colors"
              data-ui-id="workflow-builder-check-group-picker-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="workflow-builder-check-group-picker-add-btn"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
