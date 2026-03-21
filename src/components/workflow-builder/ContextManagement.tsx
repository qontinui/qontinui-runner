/**
 * ContextManagement.tsx
 *
 * UI component for managing AI contexts in the workflow builder.
 * Allows users to:
 * - Add/remove contexts manually
 * - Enable/disable auto-include based on task description
 * - See which contexts would be auto-included
 */

import React, { useState, useEffect, useMemo, useCallback } from "react";
import {
  BookOpen,
  Plus,
  X,
  Check,
  Lightbulb,
  Info,
  ChevronDown,
  ChevronRight,
  Search,
} from "lucide-react";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { useContexts, groupContextsByScope } from "../contexts/hooks/useContexts";
import type { ContextWithMetadata, AutoDetectResult } from "../../types/context";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Context picker modal state
 */
interface ContextPickerState {
  isOpen: boolean;
  searchQuery: string;
}

/**
 * ContextManagement component for the workflow builder
 */
export function ContextManagement() {
  const { state, updateWorkflow } = useWorkflowBuilder();
  const { workflow } = state;
  const { contexts, loading: contextsLoading, evaluateAutoInclude } = useContexts();

  // Local state
  const [isExpanded, setIsExpanded] = useState(true);
  const [showAutoIncludeInfo, setShowAutoIncludeInfo] = useState(false);
  const [autoDetectedContexts, setAutoDetectedContexts] = useState<AutoDetectResult[]>([]);
  const [contextPicker, setContextPicker] = useState<ContextPickerState>({
    isOpen: false,
    searchQuery: "",
  });

  // Get context management values from workflow
  const contextIds = useMemo(() => workflow.context_ids ?? [], [workflow.context_ids]);
  const disabledContextIds = useMemo(
    () => new Set(workflow.disabled_context_ids ?? []),
    [workflow.disabled_context_ids],
  );
  const autoIncludeContexts = workflow.auto_include_contexts ?? true;

  // Helper to update context IDs
  const setContextIds = useCallback(
    (ids: string[]) => {
      updateWorkflow({ context_ids: ids });
    },
    [updateWorkflow],
  );

  // Helper to update disabled context IDs
  const setDisabledContextIds = useCallback(
    (ids: string[]) => {
      updateWorkflow({ disabled_context_ids: ids });
    },
    [updateWorkflow],
  );

  // Helper to update auto-include setting
  const setAutoIncludeContexts = useCallback(
    (value: boolean) => {
      updateWorkflow({ auto_include_contexts: value });
    },
    [updateWorkflow],
  );

  // Add a context manually
  const addContext = useCallback(
    (contextId: string) => {
      if (!contextIds.includes(contextId)) {
        setContextIds([...contextIds, contextId]);
      }
      setContextPicker({ isOpen: false, searchQuery: "" });
    },
    [contextIds, setContextIds],
  );

  // Remove a context from manual list
  const removeContext = useCallback(
    (contextId: string) => {
      setContextIds(contextIds.filter((id) => id !== contextId));
    },
    [contextIds, setContextIds],
  );

  // Toggle a context as disabled (for auto-include)
  const toggleContextDisabled = useCallback(
    (contextId: string) => {
      const disabled = new Set(disabledContextIds);
      if (disabled.has(contextId)) {
        disabled.delete(contextId);
      } else {
        disabled.add(contextId);
      }
      setDisabledContextIds(Array.from(disabled));
    },
    [disabledContextIds, setDisabledContextIds],
  );

  // Toggle a context (remove if manual, disable if auto-included)
  const toggleContext = useCallback(
    (contextId: string, isAutoIncluded: boolean) => {
      if (isAutoIncluded) {
        toggleContextDisabled(contextId);
      } else {
        removeContext(contextId);
      }
    },
    [toggleContextDisabled, removeContext],
  );

  // Evaluate auto-include when description changes (only if enabled)
  useEffect(() => {
    const evaluate = async () => {
      if (!autoIncludeContexts || !workflow.description?.trim()) {
        setAutoDetectedContexts([]);
        return;
      }
      const results = await evaluateAutoInclude(workflow.description, [], []);
      setAutoDetectedContexts(results);
    };
    evaluate();
  }, [workflow.description, evaluateAutoInclude, autoIncludeContexts]);

  // Build the list of included contexts
  const includedContexts = useMemo(() => {
    const result: Array<{
      context: ContextWithMetadata;
      isAutoIncluded: boolean;
      autoIncludeReason?: string;
      isManuallyAdded: boolean;
    }> = [];

    // Add auto-detected contexts (if not disabled)
    for (const autoResult of autoDetectedContexts) {
      if (!disabledContextIds.has(autoResult.contextId)) {
        const ctx = contexts.find((c) => c.id === autoResult.contextId);
        if (ctx) {
          result.push({
            context: ctx,
            isAutoIncluded: true,
            autoIncludeReason: `${autoResult.reason}: "${autoResult.matchedTrigger}"`,
            isManuallyAdded: false,
          });
        }
      }
    }

    // Add manually added contexts
    for (const id of contextIds) {
      // Don't add if already in auto-detected
      if (result.some((r) => r.context.id === id)) continue;

      const ctx = contexts.find((c) => c.id === id);
      if (ctx) {
        result.push({
          context: ctx,
          isAutoIncluded: false,
          isManuallyAdded: true,
        });
      }
    }

    return result;
  }, [contexts, autoDetectedContexts, contextIds, disabledContextIds]);

  // Available contexts for picker (not already included)
  const availableContextsForPicker = useMemo(() => {
    const includedIds = new Set(includedContexts.map((c) => c.context.id));
    return contexts
      .filter((c) => !includedIds.has(c.id) && c.enabled)
      .filter((c) => {
        if (!contextPicker.searchQuery) return true;
        const query = contextPicker.searchQuery.toLowerCase();
        return (
          c.name.toLowerCase().includes(query) ||
          c.content.toLowerCase().includes(query) ||
          c.category?.toLowerCase().includes(query)
        );
      });
  }, [contexts, includedContexts, contextPicker.searchQuery]);

  // Group available contexts by scope
  const groupedAvailableContexts = useMemo(() => {
    return groupContextsByScope(availableContextsForPicker);
  }, [availableContextsForPicker]);

  // Render the scope badge
  const renderScopeBadge = (scope: string) => {
    const colors: Record<string, string> = {
      builtin: `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`,
      user: `${getStatusColors("success").bg} ${getAccentColors("green").text}`,
      project: `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`,
    };
    return (
      <span
        data-content-role="badge"
        className={`text-[10px] px-1.5 py-0.5 rounded ${colors[scope] || "bg-zinc-700"}`}
      >
        {scope}
      </span>
    );
  };

  if (contextsLoading) {
    return (
      <div className="p-4 border-t border-zinc-700">
        <div className="flex items-center gap-2 text-zinc-400">
          <BookOpen className="w-4 h-4" />
          <span className="text-sm">Loading contexts...</span>
        </div>
      </div>
    );
  }

  return (
    <div className="border-t border-zinc-700">
      {/* Header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-center gap-2 px-4 py-3 hover:bg-zinc-800/50 transition-colors"
      >
        {isExpanded ? (
          <ChevronDown className="w-4 h-4 text-zinc-400" />
        ) : (
          <ChevronRight className="w-4 h-4 text-zinc-400" />
        )}
        <BookOpen className={`w-4 h-4 ${getAccentColors("purple").text}`} />
        <span className="font-medium text-sm text-zinc-300">AI Contexts</span>
        <span className="ml-auto text-xs text-zinc-500">
          {includedContexts.length} context{includedContexts.length !== 1 ? "s" : ""}
        </span>
      </button>

      {isExpanded && (
        <div className="px-4 pb-4 space-y-3">
          {/* Auto-include toggle */}
          <div className="flex items-center justify-between pb-2 border-b border-zinc-700">
            <div className="flex items-center gap-2">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={autoIncludeContexts}
                  onChange={(e) => setAutoIncludeContexts(e.target.checked)}
                  className="w-4 h-4 rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                />
                <span className="text-sm text-zinc-300">Auto-include contexts</span>
              </label>
              <div className="relative">
                <button
                  onClick={() => setShowAutoIncludeInfo(!showAutoIncludeInfo)}
                  className="text-zinc-500 hover:text-zinc-300"
                  title="What is auto-include?"
                >
                  <Info className="w-4 h-4" />
                </button>
                {showAutoIncludeInfo && (
                  <div className="absolute left-0 top-full mt-2 w-72 p-3 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl z-50 text-sm">
                    <div className="flex items-start gap-2">
                      <Lightbulb
                        className={`w-4 h-4 ${getAccentColors("yellow").text} mt-0.5 shrink-0`}
                      />
                      <div>
                        <p className="font-medium text-zinc-200 mb-1">Auto-Include Contexts</p>
                        <p className="text-zinc-400 text-xs">
                          When enabled, contexts are automatically included based on keywords in
                          your workflow description. For example, mentioning "error" or "runner"
                          will include relevant debugging contexts.
                        </p>
                        <p className="text-zinc-400 text-xs mt-2">
                          Disable this to only include contexts you manually add.
                        </p>
                      </div>
                    </div>
                    <button
                      onClick={() => setShowAutoIncludeInfo(false)}
                      className="absolute top-2 right-2 text-zinc-500 hover:text-zinc-300"
                    >
                      <X className="w-3 h-3" />
                    </button>
                  </div>
                )}
              </div>
            </div>
          </div>

          {/* Included contexts list */}
          {includedContexts.length === 0 ? (
            <p className="text-sm text-zinc-500 italic">
              No contexts included. Add contexts manually or set a description to trigger
              auto-include.
            </p>
          ) : (
            <div className="space-y-2">
              {includedContexts.map(({ context, isAutoIncluded, autoIncludeReason }) => (
                <div
                  key={context.id}
                  className="flex items-start gap-2 p-2 rounded-md border border-zinc-700 hover:bg-zinc-800/50 group"
                >
                  <button
                    onClick={() => toggleContext(context.id, isAutoIncluded)}
                    className="mt-0.5 text-green-500 hover:text-red-500"
                    title={isAutoIncluded ? "Disable this context" : "Remove this context"}
                  >
                    <Check className="w-4 h-4" />
                  </button>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span
                        data-content-role="label"
                        data-content-label="context name"
                        className="font-medium text-sm text-zinc-200 truncate"
                      >
                        {context.name}
                      </span>
                      {renderScopeBadge(context.scope)}
                    </div>
                    {autoIncludeReason && (
                      <p className="text-xs text-zinc-500 mt-0.5 flex items-center gap-1">
                        <Lightbulb className="w-3 h-3" />
                        Auto: {autoIncludeReason}
                      </p>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Add Context Button */}
          <div className="relative pt-2 border-t border-zinc-700">
            <button
              onClick={() => setContextPicker({ ...contextPicker, isOpen: true })}
              className="flex items-center gap-1 text-sm text-blue-400 hover:text-blue-300"
            >
              <Plus className="w-4 h-4" />
              Add Context
            </button>

            {/* Context Picker Dropdown */}
            {contextPicker.isOpen && (
              <div className="absolute left-0 right-0 top-full mt-1 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl z-50 max-h-64 overflow-hidden">
                <div className="p-2 border-b border-zinc-700">
                  <div className="relative">
                    <Search className="absolute left-2 top-1/2 transform -translate-y-1/2 w-4 h-4 text-zinc-500" />
                    <input
                      type="text"
                      value={contextPicker.searchQuery}
                      onChange={(e) =>
                        setContextPicker({ ...contextPicker, searchQuery: e.target.value })
                      }
                      placeholder="Search contexts..."
                      className="w-full bg-zinc-700 border-none rounded pl-8 pr-2 py-1 text-sm text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-1 focus:ring-blue-500"
                      autoFocus
                    />
                  </div>
                </div>
                <div className="overflow-auto max-h-48">
                  {Object.entries(groupedAvailableContexts).map(([scope, ctxs]) => {
                    if (ctxs.length === 0) return null;
                    return (
                      <div key={scope}>
                        <div
                          data-content-role="heading"
                          className="px-3 py-1 text-xs font-medium text-zinc-500 bg-zinc-800/50 capitalize"
                        >
                          {scope}
                        </div>
                        {ctxs.map((ctx) => (
                          <button
                            key={ctx.id}
                            onClick={() => addContext(ctx.id)}
                            className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-zinc-700 transition-colors"
                          >
                            <Plus className="w-3.5 h-3.5 text-zinc-500" />
                            <span className="text-sm text-zinc-200 truncate">{ctx.name}</span>
                          </button>
                        ))}
                      </div>
                    );
                  })}
                  {availableContextsForPicker.length === 0 && (
                    <div className="px-3 py-4 text-sm text-zinc-500 text-center">
                      No matching contexts found
                    </div>
                  )}
                </div>
                <div className="p-2 border-t border-zinc-700">
                  <button
                    onClick={() => setContextPicker({ isOpen: false, searchQuery: "" })}
                    className="w-full text-sm text-zinc-500 hover:text-zinc-300"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default ContextManagement;
