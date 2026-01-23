/**
 * Check Library Picker
 *
 * Modal dialog for selecting a saved check from the Check Builder library
 * to add as a workflow step.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Search, X, Check as CheckIcon, AlertTriangle, AlignLeft, FileType, Shield, Terminal } from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// Check type from Check Builder
interface SavedCheck {
  id: string;
  name: string;
  description?: string;
  check_type: string;
  tool: string;
  command?: string;
  working_directory?: string;
  config_path?: string;
  auto_fix: boolean;
  fail_on_warning: boolean;
  timeout_seconds: number;
  is_critical: boolean;
  enabled: boolean;
  tags: string[];
  created_at: string;
  updated_at: string;
}

interface CheckLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (check: SavedCheck, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

const CHECK_TYPE_ICONS: Record<string, typeof AlertTriangle> = {
  lint: AlertTriangle,
  format: AlignLeft,
  typecheck: FileType,
  security: Shield,
  analyze: Search,
  custom_command: Terminal,
};

const CHECK_TYPE_COLORS: Record<string, string> = {
  lint: "text-amber-400",
  format: "text-blue-400",
  typecheck: "text-purple-400",
  security: "text-red-400",
  analyze: "text-indigo-400",
  custom_command: "text-gray-400",
};

export function CheckLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: CheckLibraryPickerProps) {
  const [checks, setChecks] = useState<SavedCheck[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterType, setFilterType] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  // Fetch checks from database
  useEffect(() => {
    if (!isOpen) return;

    const fetchChecks = async () => {
      setIsLoading(true);
      try {
        const response = await invoke<{
          success: boolean;
          data?: SavedCheck[];
          message?: string;
        }>("list_checks", { enabledOnly: false });

        if (response.success && response.data) {
          setChecks(response.data);
        }
      } catch (err) {
        console.error("Failed to fetch checks:", err);
      } finally {
        setIsLoading(false);
      }
    };

    fetchChecks();
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedId(null);
      setSearchQuery("");
      setFilterType(null);
    }
  }, [isOpen]);

  // Filter checks
  const filteredChecks = checks.filter((check) => {
    const matchesSearch =
      !searchQuery ||
      check.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      check.description?.toLowerCase().includes(searchQuery.toLowerCase()) ||
      check.tool.toLowerCase().includes(searchQuery.toLowerCase());

    const matchesType = !filterType || check.check_type === filterType;

    return matchesSearch && matchesType;
  });

  // Get unique check types for filter
  const checkTypes = [...new Set(checks.map((c) => c.check_type))];

  const handleSelect = useCallback(() => {
    const selected = checks.find((c) => c.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [checks, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (check: SavedCheck) => {
      onSelect(check, phase);
      onClose();
    },
    [onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        data-ui-id="dialog-check-library-picker"
        className="bg-neutral-900 border border-neutral-700 rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
          <div>
            <h2 className="text-lg font-semibold">Select Check from Library</h2>
            <p className="text-sm text-muted-foreground">
              Choose a saved check to add as a {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-neutral-700 rounded-md transition-colors"
            data-ui-id="workflow-builder-check-picker-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search and Filter */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-neutral-800">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search checks..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
              data-ui-id="workflow-builder-check-picker-search-input"
            />
          </div>
          <select
            value={filterType || ""}
            onChange={(e) => setFilterType(e.target.value || null)}
            className="px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="workflow-builder-check-picker-type-select"
          >
            <option value="">All Types</option>
            {checkTypes.map((type) => (
              <option key={type} value={type}>
                {type.replace("_", " ").replace(/\b\w/g, (c) => c.toUpperCase())}
              </option>
            ))}
          </select>
        </div>

        {/* Check List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading checks...</div>
          ) : filteredChecks.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {checks.length === 0
                ? "No checks saved yet. Create checks in the Check Builder first."
                : "No checks match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredChecks.map((check) => {
                const Icon = CHECK_TYPE_ICONS[check.check_type] || Terminal;
                const colorClass = CHECK_TYPE_COLORS[check.check_type] || "text-gray-400";
                const isSelected = selectedId === check.id;

                return (
                  <button
                    key={check.id}
                    onClick={() => setSelectedId(check.id)}
                    onDoubleClick={() => handleDoubleClick(check)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-cyan-500/15 border-2 border-cyan-500"
                          : "bg-neutral-800/50 border-2 border-transparent hover:bg-neutral-800 hover:border-neutral-600"
                      }
                    `}
                    data-ui-id={`workflow-builder-check-picker-item-${check.id}`}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div className={`p-2 rounded-md bg-neutral-900 ${colorClass}`}>
                      <Icon className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{check.name}</span>
                        {!check.enabled && (
                          <span className="text-xs px-1.5 py-0.5 bg-amber-500/20 text-amber-400 rounded">
                            disabled
                          </span>
                        )}
                      </div>
                      {check.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {check.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        <span className="capitalize">{check.check_type.replace("_", " ")}</span>
                        <span className="text-neutral-600">|</span>
                        <span>{check.tool}</span>
                        {check.working_directory && (
                          <>
                            <span className="text-neutral-600">|</span>
                            <span className="truncate max-w-[200px]">{check.working_directory}</span>
                          </>
                        )}
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-neutral-700 bg-neutral-800/50">
          <span className="text-sm text-muted-foreground">
            {filteredChecks.length} check{filteredChecks.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-neutral-700 transition-colors"
              data-ui-id="workflow-builder-check-picker-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="workflow-builder-check-picker-add-btn"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
