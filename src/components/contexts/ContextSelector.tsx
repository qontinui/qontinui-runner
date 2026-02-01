/**
 * ContextSelector.tsx
 *
 * A component for selecting AI contexts to include in AI tasks.
 * Supports both auto-detection and manual selection of contexts.
 *
 * Features:
 * - Auto-detect toggle: Automatically select contexts based on rules
 * - Manual selection: Checkbox list of available contexts grouped by scope
 * - Preview: Show which contexts will be included and why
 * - Compact mode: For embedding in task dialogs (just shows selected count with expand)
 * - Expanded mode: Full list with checkboxes
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  ChevronDown,
  ChevronRight,
  Check,
  Zap,
  Settings2,
  Sparkles,
  BookOpen,
  User,
  Package,
  Loader2,
  AlertCircle,
  Info,
} from "lucide-react";
import type {
  ContextWithMetadata,
  ContextSelection,
  AutoDetectResult,
  ContextScope,
} from "../../types/context";
import { useContexts, groupContextsByScope } from "./hooks/useContexts";
import { getAccentColors, getStatusColors } from "@/design-system";

// ============================================================================
// Types
// ============================================================================

export interface ContextSelectorProps {
  /** Current selection state */
  selection: ContextSelection;
  /** Called when selection changes */
  onSelectionChange: (selection: ContextSelection) => void;

  /** For auto-detection evaluation */
  taskPrompt?: string;
  actionTypes?: string[];
  recentErrors?: string[];

  /** Display options */
  compact?: boolean;
  disabled?: boolean;

  /** Optional class name */
  className?: string;
}

interface AutoDetectState {
  loading: boolean;
  results: AutoDetectResult[];
  evaluated: boolean;
}

// ============================================================================
// Scope Icons
// ============================================================================

const SCOPE_ICONS: Record<ContextScope, React.ReactNode> = {
  builtin: <Package className="w-3.5 h-3.5" />,
  user: <User className="w-3.5 h-3.5" />,
  project: <BookOpen className="w-3.5 h-3.5" />,
};

const SCOPE_LABELS: Record<ContextScope, string> = {
  builtin: "Built-in",
  user: "User",
  project: "Project",
};

// ============================================================================
// Context Item Component
// ============================================================================

interface ContextItemProps {
  context: ContextWithMetadata;
  isSelected: boolean;
  isAutoDetected: boolean;
  autoDetectReason?: string;
  matchedTrigger?: string;
  onToggle: (id: string) => void;
  disabled?: boolean;
}

function ContextItem({
  context,
  isSelected,
  isAutoDetected,
  autoDetectReason,
  matchedTrigger,
  onToggle,
  disabled,
}: ContextItemProps) {
  const handleClick = () => {
    if (!disabled) {
      onToggle(context.id);
    }
  };

  const reasonLabel = useMemo(() => {
    switch (autoDetectReason) {
      case "taskMention":
        return `Matched: "${matchedTrigger}" in task`;
      case "actionType":
        return `Matched: "${matchedTrigger}" action type`;
      case "errorPattern":
        return `Matched: "${matchedTrigger}" in logs`;
      default:
        return null;
    }
  }, [autoDetectReason, matchedTrigger]);

  return (
    <div
      onClick={handleClick}
      className={`
        flex items-start gap-2 p-2 rounded-lg cursor-pointer transition-colors
        ${disabled ? "opacity-50 cursor-not-allowed" : "hover:bg-muted/50"}
        ${isSelected ? "bg-primary/10 border border-primary/30" : "border border-transparent"}
      `}
    >
      {/* Checkbox */}
      <div
        className={`
          w-4 h-4 mt-0.5 rounded flex items-center justify-center flex-shrink-0
          ${isSelected ? "bg-primary text-primary-foreground" : "border border-border"}
        `}
      >
        {isSelected && <Check className="w-3 h-3" />}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-medium text-sm truncate">{context.name}</span>
          {isAutoDetected && (
            <span
              className={`px-1.5 py-0.5 text-xs ${getAccentColors("amber").bg} ${getAccentColors("amber").text} rounded flex items-center gap-1`}
            >
              <Zap className="w-3 h-3" />
              auto
            </span>
          )}
        </div>
        {isAutoDetected && reasonLabel && (
          <p className="text-xs text-muted-foreground mt-0.5 truncate">{reasonLabel}</p>
        )}
        {context.category && !isAutoDetected && (
          <p className="text-xs text-muted-foreground mt-0.5 truncate">{context.category}</p>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Scope Section Component
// ============================================================================

interface ScopeSectionProps {
  scope: ContextScope;
  contexts: ContextWithMetadata[];
  selectedIds: string[];
  autoDetectedMap: Map<string, AutoDetectResult>;
  onToggle: (id: string) => void;
  disabled?: boolean;
}

function ScopeSection({
  scope,
  contexts,
  selectedIds,
  autoDetectedMap,
  onToggle,
  disabled,
}: ScopeSectionProps) {
  // Always call hooks unconditionally to satisfy React's Rules of Hooks
  const [isExpanded, setIsExpanded] = useState(true);

  // Early return after all hooks are called
  if (contexts.length === 0) {
    return null;
  }

  const selectedCount = contexts.filter((c) => selectedIds.includes(c.id)).length;

  return (
    <div className="space-y-1">
      {/* Section Header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="flex items-center gap-2 w-full px-2 py-1 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors"
      >
        {isExpanded ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        {SCOPE_ICONS[scope]}
        <span>{SCOPE_LABELS[scope]}</span>
        <span className="text-muted-foreground">
          ({selectedCount}/{contexts.length})
        </span>
      </button>

      {/* Context List */}
      {isExpanded && (
        <div className="space-y-1 pl-2">
          {contexts.map((context) => {
            const autoResult = autoDetectedMap.get(context.id);
            return (
              <ContextItem
                key={context.id}
                context={context}
                isSelected={selectedIds.includes(context.id)}
                isAutoDetected={!!autoResult}
                autoDetectReason={autoResult?.reason}
                matchedTrigger={autoResult?.matchedTrigger}
                onToggle={onToggle}
                disabled={disabled}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export function ContextSelector({
  selection,
  onSelectionChange,
  taskPrompt = "",
  actionTypes = [],
  recentErrors = [],
  compact = false,
  disabled = false,
  className = "",
}: ContextSelectorProps) {
  const { contexts, loading: contextsLoading, error, evaluateAutoInclude } = useContexts();

  const [isExpanded, setIsExpanded] = useState(!compact);
  const [autoDetectState, setAutoDetectState] = useState<AutoDetectState>({
    loading: false,
    results: [],
    evaluated: false,
  });

  // Group contexts by scope
  const groupedContexts = useMemo(() => groupContextsByScope(contexts), [contexts]);

  // Map of auto-detected context IDs to their results
  const autoDetectedMap = useMemo(() => {
    const map = new Map<string, AutoDetectResult>();
    for (const result of autoDetectState.results) {
      map.set(result.contextId, result);
    }
    return map;
  }, [autoDetectState.results]);

  /**
   * Run auto-detection evaluation
   */
  const runAutoDetection = useCallback(async () => {
    setAutoDetectState((prev) => ({ ...prev, loading: true }));

    try {
      const results = await evaluateAutoInclude(taskPrompt, actionTypes, recentErrors);
      setAutoDetectState({
        loading: false,
        results,
        evaluated: true,
      });

      // If auto-detect is enabled, update selection with auto-detected contexts
      if (selection.autoDetect) {
        const autoIds = results.map((r) => r.contextId);
        // Merge auto-detected with any manually selected
        const newSelectedIds = Array.from(new Set([...selection.selectedIds, ...autoIds]));
        onSelectionChange({
          ...selection,
          selectedIds: newSelectedIds,
        });
      }
    } catch (err) {
      console.error("[ContextSelector] Auto-detection failed:", err);
      setAutoDetectState((prev) => ({ ...prev, loading: false }));
    }
  }, [evaluateAutoInclude, taskPrompt, actionTypes, recentErrors, selection, onSelectionChange]);

  // Run auto-detection when enabled or when task context changes
  useEffect(() => {
    if (selection.autoDetect && !autoDetectState.evaluated && !autoDetectState.loading) {
      runAutoDetection();
    }
  }, [selection.autoDetect, autoDetectState.evaluated, autoDetectState.loading, runAutoDetection]);

  // Re-evaluate when task context changes
  const actionTypesKey = actionTypes.join(",");
  const recentErrorsKey = recentErrors.join(",");
  useEffect(() => {
    if (selection.autoDetect) {
      setAutoDetectState((prev) => ({ ...prev, evaluated: false }));
    }
  }, [selection.autoDetect, taskPrompt, actionTypesKey, recentErrorsKey]);

  /**
   * Toggle auto-detect mode
   */
  const handleAutoDetectToggle = () => {
    const newAutoDetect = !selection.autoDetect;

    if (newAutoDetect) {
      // Enable auto-detect: trigger evaluation
      setAutoDetectState((prev) => ({ ...prev, evaluated: false }));
    }

    onSelectionChange({
      ...selection,
      autoDetect: newAutoDetect,
    });
  };

  /**
   * Toggle a single context
   */
  const handleToggleContext = (contextId: string) => {
    const isSelected = selection.selectedIds.includes(contextId);
    const newSelectedIds = isSelected
      ? selection.selectedIds.filter((id) => id !== contextId)
      : [...selection.selectedIds, contextId];

    onSelectionChange({
      ...selection,
      selectedIds: newSelectedIds,
    });
  };

  /**
   * Select all contexts
   */
  const handleSelectAll = () => {
    const allIds = contexts.filter((c) => c.enabled).map((c) => c.id);
    onSelectionChange({
      ...selection,
      selectedIds: allIds,
    });
  };

  /**
   * Clear selection
   */
  const handleClearSelection = () => {
    onSelectionChange({
      ...selection,
      selectedIds: [],
    });
  };

  // Compact mode: just show a summary with expand button
  if (compact && !isExpanded) {
    return (
      <div className={`flex items-center gap-2 ${className}`}>
        <button
          onClick={() => setIsExpanded(true)}
          disabled={disabled}
          className={`
            flex items-center gap-2 px-3 py-1.5 text-sm rounded-lg border transition-colors
            ${disabled ? "opacity-50 cursor-not-allowed" : "hover:bg-muted cursor-pointer"}
            ${selection.selectedIds.length > 0 ? "border-primary/50 bg-primary/5" : "border-border"}
          `}
        >
          <Sparkles className="w-4 h-4 text-primary" />
          <span>Context</span>
          <span
            className={`
              px-1.5 py-0.5 text-xs rounded-full
              ${selection.autoDetect ? `${getAccentColors("amber").bg} ${getAccentColors("amber").text}` : "bg-muted text-muted-foreground"}
            `}
          >
            {selection.autoDetect ? "Auto" : "Manual"}
          </span>
          {selection.selectedIds.length > 0 && (
            <span className="text-muted-foreground">{selection.selectedIds.length} selected</span>
          )}
          <ChevronRight className="w-4 h-4 text-muted-foreground" />
        </button>
      </div>
    );
  }

  return (
    <div className={`space-y-3 ${className}`}>
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-primary" />
          <span className="font-medium text-sm">Context</span>
          {compact && (
            <button
              onClick={() => setIsExpanded(false)}
              className="p-1 hover:bg-muted rounded"
              title="Collapse"
            >
              <ChevronDown className="w-4 h-4" />
            </button>
          )}
        </div>

        {/* Auto-detect Toggle */}
        <button
          onClick={handleAutoDetectToggle}
          disabled={disabled}
          className={`
            flex items-center gap-1.5 px-2 py-1 text-xs rounded-md transition-colors
            ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}
            ${
              selection.autoDetect
                ? `${getAccentColors("amber").bg} ${getAccentColors("amber").text} hover:bg-amber-500/30`
                : "bg-muted text-muted-foreground hover:bg-muted/80"
            }
          `}
        >
          {autoDetectState.loading ? (
            <Loader2 className="w-3 h-3 animate-spin" />
          ) : selection.autoDetect ? (
            <Zap className="w-3 h-3" />
          ) : (
            <Settings2 className="w-3 h-3" />
          )}
          <span>{selection.autoDetect ? "Auto" : "Manual"}</span>
        </button>
      </div>

      {/* Error State */}
      {error && (
        <div
          className={`flex items-center gap-2 p-2 text-sm ${getStatusColors("error").text} ${getStatusColors("error").bg} rounded-lg`}
        >
          <AlertCircle className="w-4 h-4 flex-shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {/* Loading State */}
      {contextsLoading && (
        <div className="flex items-center justify-center py-4">
          <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
        </div>
      )}

      {/* Auto-detect Info */}
      {selection.autoDetect && autoDetectState.results.length > 0 && (
        <div
          className={`flex items-start gap-2 p-2 text-xs ${getAccentColors("amber").text} ${getAccentColors("amber").bg} rounded-lg`}
        >
          <Info className="w-3.5 h-3.5 mt-0.5 flex-shrink-0" />
          <span>
            {autoDetectState.results.length} context(s) auto-detected based on your task. You can
            add or remove contexts manually.
          </span>
        </div>
      )}

      {/* Actions */}
      {!contextsLoading && contexts.length > 0 && (
        <div className="flex items-center gap-2 text-xs">
          <button
            onClick={handleSelectAll}
            disabled={disabled}
            className="text-primary hover:underline disabled:opacity-50"
          >
            Select all
          </button>
          <span className="text-muted-foreground">|</span>
          <button
            onClick={handleClearSelection}
            disabled={disabled}
            className="text-muted-foreground hover:text-foreground disabled:opacity-50"
          >
            Clear
          </button>
          <span className="ml-auto text-muted-foreground">
            {selection.selectedIds.length} of {contexts.length} selected
          </span>
        </div>
      )}

      {/* Context List by Scope */}
      {!contextsLoading && (
        <div className="space-y-3 max-h-64 overflow-y-auto pr-1">
          {/* Built-in contexts first */}
          <ScopeSection
            scope="builtin"
            contexts={groupedContexts.builtin}
            selectedIds={selection.selectedIds}
            autoDetectedMap={autoDetectedMap}
            onToggle={handleToggleContext}
            disabled={disabled}
          />

          {/* User contexts */}
          <ScopeSection
            scope="user"
            contexts={groupedContexts.user}
            selectedIds={selection.selectedIds}
            autoDetectedMap={autoDetectedMap}
            onToggle={handleToggleContext}
            disabled={disabled}
          />

          {/* Project contexts */}
          <ScopeSection
            scope="project"
            contexts={groupedContexts.project}
            selectedIds={selection.selectedIds}
            autoDetectedMap={autoDetectedMap}
            onToggle={handleToggleContext}
            disabled={disabled}
          />
        </div>
      )}

      {/* Empty State */}
      {!contextsLoading && contexts.length === 0 && (
        <div className="text-center py-4 text-sm text-muted-foreground">
          <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>No contexts available</p>
          <p className="text-xs mt-1">
            Create contexts in Settings to provide AI with domain knowledge
          </p>
        </div>
      )}
    </div>
  );
}

export default ContextSelector;
