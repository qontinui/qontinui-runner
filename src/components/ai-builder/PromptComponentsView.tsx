/**
 * PromptComponentsView.tsx
 *
 * Component-based view for the AI prompt, showing structured sections
 * instead of raw text. Allows users to see exactly what's included
 * and toggle contexts on/off.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  FileText,
  Sparkles,
  ChevronDown,
  ChevronRight,
  Plus,
  Check,
  X,
  Layers,
  FileCode,
  Terminal,
  BookOpen,
  Eye,
  Settings2,
  Lightbulb,
  AlertCircle,
  Info,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { useContexts, groupContextsByScope } from "../contexts/hooks/useContexts";
import CollapsiblePanel from "../CollapsiblePanel";
import type { ContextWithMetadata, AutoDetectResult } from "../../types/context";
import type { LogSourceProfile } from "../../types/projectLogs";
import { getActiveProfile, getActiveLogSources } from "../../types/projectLogs";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Represents a prompt component section
 */
interface _PromptSection {
  id: string;
  type: "task" | "context" | "log-sources" | "system";
  title: string;
  content: string;
  enabled: boolean;
  reason?: string; // Why this section is included (auto-include reason)
  metadata?: {
    contextId?: string;
    scope?: string;
    isBuiltin?: boolean;
    isAutoIncluded?: boolean;
    autoIncludeReason?: string;
  };
}

/**
 * Context picker modal state
 */
interface ContextPickerState {
  isOpen: boolean;
  searchQuery: string;
}

export function PromptComponentsView() {
  const {
    goal,
    setGoal,
    currentSessionId,
    generatePrompt: _generatePrompt,
    projectLogs,
    executionSteps,
    captureInputValidation,
    // Context selection state from shared hook
    manuallyAddedContextIds,
    disabledContextIds,
    autoIncludeContexts,
    setAutoIncludeContexts,
    addContext: addContextToState,
    removeContext: removeContextFromState,
    toggleContextDisabled,
  } = useAiBuilder();

  const { contexts, loading: _contextsLoading, evaluateAutoInclude } = useContexts();

  // View mode: components or full
  const [viewMode, setViewMode] = useState<"components" | "full">("components");

  // Auto-detected contexts with reasons
  const [autoDetectedContexts, setAutoDetectedContexts] = useState<AutoDetectResult[]>([]);

  // Expanded sections
  const [expandedSections, setExpandedSections] = useState<Set<string>>(
    new Set(["task", "contexts"]),
  );

  // Info tooltip state
  const [showAutoIncludeInfo, setShowAutoIncludeInfo] = useState(false);

  // Context picker modal
  const [contextPicker, setContextPicker] = useState<ContextPickerState>({
    isOpen: false,
    searchQuery: "",
  });

  // Get action types from execution steps
  const actionTypes = useMemo(() => {
    return executionSteps.map((step) => step.type);
  }, [executionSteps]);

  // Evaluate auto-include when goal or action types change (only if auto-include is enabled)
  useEffect(() => {
    const evaluate = async () => {
      if (!autoIncludeContexts || !goal.trim()) {
        setAutoDetectedContexts([]);
        return;
      }
      const results = await evaluateAutoInclude(goal, actionTypes, []);
      setAutoDetectedContexts(results);
    };
    evaluate();
  }, [goal, actionTypes, evaluateAutoInclude, autoIncludeContexts]);

  // Get active log profile
  const activeLogProfile = useMemo((): LogSourceProfile | undefined => {
    if (!projectLogs.config) return undefined;
    return getActiveProfile(projectLogs.config);
  }, [projectLogs.config]);

  // Get active log sources
  const activeLogSources = useMemo(() => {
    if (!projectLogs.config) return [];
    return getActiveLogSources(projectLogs.config).filter((s) => s.enabled);
  }, [projectLogs.config]);

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
    for (const id of manuallyAddedContextIds) {
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

    // Always include Multi-Step Task Guide (builtin) unless disabled
    const multiStepGuide = contexts.find((c) => c.id === "builtin-multi-step-guide");
    if (multiStepGuide && !disabledContextIds.has(multiStepGuide.id)) {
      if (!result.some((r) => r.context.id === multiStepGuide.id)) {
        result.push({
          context: multiStepGuide,
          isAutoIncluded: true,
          autoIncludeReason: "Always included for runner sessions",
          isManuallyAdded: false,
        });
      }
    }

    // Include Input Validation Guide when capture is enabled
    if (captureInputValidation) {
      const inputValidationGuide = contexts.find((c) => c.id === "builtin-input-validation");
      if (inputValidationGuide && !disabledContextIds.has(inputValidationGuide.id)) {
        if (!result.some((r) => r.context.id === inputValidationGuide.id)) {
          result.push({
            context: inputValidationGuide,
            isAutoIncluded: true,
            autoIncludeReason: "Capture Input for Validation is enabled",
            isManuallyAdded: false,
          });
        }
      }
    }

    return result;
  }, [
    contexts,
    autoDetectedContexts,
    manuallyAddedContextIds,
    disabledContextIds,
    captureInputValidation,
  ]);

  // Toggle section expansion
  const toggleSection = useCallback((sectionId: string) => {
    setExpandedSections((prev) => {
      const next = new Set(prev);
      if (next.has(sectionId)) {
        next.delete(sectionId);
      } else {
        next.add(sectionId);
      }
      return next;
    });
  }, []);

  // Toggle context enabled/disabled
  const toggleContext = useCallback(
    (contextId: string, isAutoIncluded: boolean) => {
      if (isAutoIncluded) {
        // For auto-included contexts, toggle in disabled set
        toggleContextDisabled(contextId);
      } else {
        // For manually added contexts, remove from manual set
        removeContextFromState(contextId);
      }
    },
    [toggleContextDisabled, removeContextFromState],
  );

  // Add context manually
  const addContext = useCallback(
    (contextId: string) => {
      addContextToState(contextId);
      setContextPicker({ isOpen: false, searchQuery: "" });
    },
    [addContextToState],
  );

  // Compose the full prompt from components
  const composedPrompt = useMemo(() => {
    const sections: string[] = [];

    // 1. Contexts section
    if (includedContexts.length > 0) {
      sections.push("## Included Contexts\n");
      for (const { context } of includedContexts) {
        sections.push(`### ${context.name}\n\n${context.content}\n`);
      }
      sections.push("---\n");
    }

    // 2. Log sources section
    if (activeLogSources.length > 0) {
      sections.push("## Configured Log Sources\n");
      sections.push("The following log files have been configured for monitoring:\n");
      for (const source of activeLogSources) {
        const profileName = activeLogProfile?.name || "Default";
        sections.push(`- **${source.name}** (${profileName}): \`${source.path}\`\n`);
      }
      sections.push("\n---\n");
    }

    // 3. Task prompt
    if (goal.trim()) {
      sections.push("## Task\n\n");
      sections.push(goal.trim());
      sections.push("\n");
    } else {
      sections.push("## Task\n\n");
      sections.push("(No task specified)\n");
    }

    return sections.join("\n");
  }, [includedContexts, activeLogSources, activeLogProfile, goal]);

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
      <span className={`text-[10px] px-1.5 py-0.5 rounded ${colors[scope] || "bg-muted"}`}>
        {scope}
      </span>
    );
  };

  return (
    <CollapsiblePanel
      title="Prompt Preview"
      icon={<Sparkles className="w-4 h-4" />}
      defaultCollapsed={currentSessionId !== null}
      storageKey="ai-builder-prompt-components"
      headerExtra={
        <div className="flex items-center gap-2">
          <button
            onClick={(e) => {
              e.stopPropagation();
              setViewMode(viewMode === "components" ? "full" : "components");
            }}
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground px-2 py-1 rounded hover:bg-muted/50"
          >
            {viewMode === "components" ? (
              <>
                <Eye className="w-3 h-3" />
                View Full
              </>
            ) : (
              <>
                <Layers className="w-3 h-3" />
                Components
              </>
            )}
          </button>
        </div>
      }
    >
      {viewMode === "full" ? (
        // Full prompt view (composed from components)
        <div className="space-y-3">
          <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-96 whitespace-pre-wrap border border-border">
            {composedPrompt}
          </pre>
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>{composedPrompt.length} characters</span>
            <button
              onClick={() => navigator.clipboard.writeText(composedPrompt)}
              className="text-primary hover:underline"
            >
              Copy to clipboard
            </button>
          </div>
        </div>
      ) : (
        // Components view
        <div className="space-y-3">
          {/* Task Prompt Section (Editable) */}
          <div className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => toggleSection("task")}
              className="w-full flex items-center gap-2 px-3 py-2 bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              {expandedSections.has("task") ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <FileText className="w-4 h-4 text-primary" />
              <span className="font-medium text-sm">Task Prompt</span>
              <span className="ml-auto text-xs text-muted-foreground">
                {goal ? `${goal.length} chars` : "Required"}
              </span>
            </button>
            {expandedSections.has("task") && (
              <div className="p-3 bg-background">
                <textarea
                  value={goal}
                  onChange={(e) => setGoal(e.target.value)}
                  placeholder="Describe what you want to verify or achieve..."
                  className="w-full h-48 px-3 py-2 bg-muted/30 border border-border rounded-md text-sm resize-none focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>
            )}
          </div>

          {/* Included Contexts Section */}
          <div className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => toggleSection("contexts")}
              className="w-full flex items-center gap-2 px-3 py-2 bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              {expandedSections.has("contexts") ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <BookOpen className={`w-4 h-4 ${getAccentColors("purple").text}`} />
              <span className="font-medium text-sm">Included Contexts</span>
              <span className="ml-auto text-xs text-muted-foreground">
                {includedContexts.length} context{includedContexts.length !== 1 ? "s" : ""}
              </span>
            </button>
            {expandedSections.has("contexts") && (
              <div className="p-3 bg-background space-y-2">
                {/* Auto-include toggle */}
                <div className="flex items-center justify-between pb-2 border-b border-border">
                  <div className="flex items-center gap-2">
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={autoIncludeContexts}
                        onChange={(e) => setAutoIncludeContexts(e.target.checked)}
                        className="w-4 h-4 rounded border-border text-primary focus:ring-primary"
                      />
                      <span className="text-sm">Auto-include contexts</span>
                    </label>
                    <div className="relative">
                      <button
                        onClick={() => setShowAutoIncludeInfo(!showAutoIncludeInfo)}
                        className="text-muted-foreground hover:text-foreground"
                        title="What is auto-include?"
                      >
                        <Info className="w-4 h-4" />
                      </button>
                      {showAutoIncludeInfo && (
                        <div className="absolute left-0 top-full mt-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-xl z-50 text-sm">
                          <div className="flex items-start gap-2">
                            <Lightbulb
                              className={`w-4 h-4 ${getAccentColors("yellow").text} mt-0.5 flex-shrink-0`}
                            />
                            <div>
                              <p className="font-medium mb-1">Auto-Include Contexts</p>
                              <p className="text-muted-foreground text-xs">
                                When enabled, contexts are automatically included based on keywords
                                in your task prompt. For example, mentioning "error" or "runner"
                                will include relevant debugging contexts.
                              </p>
                              <p className="text-muted-foreground text-xs mt-2">
                                Disable this to only include contexts you manually add.
                              </p>
                            </div>
                          </div>
                          <button
                            onClick={() => setShowAutoIncludeInfo(false)}
                            className="absolute top-2 right-2 text-muted-foreground hover:text-foreground"
                          >
                            <X className="w-3 h-3" />
                          </button>
                        </div>
                      )}
                    </div>
                  </div>
                </div>

                {includedContexts.length === 0 ? (
                  <p className="text-sm text-muted-foreground italic">
                    No contexts included. Add contexts or set a goal to trigger auto-include.
                  </p>
                ) : (
                  <div className="space-y-2">
                    {includedContexts.map(({ context, isAutoIncluded, autoIncludeReason }) => (
                      <div
                        key={context.id}
                        className="flex items-start gap-2 p-2 rounded-md border border-border hover:bg-muted/30 group"
                      >
                        <button
                          onClick={() => toggleContext(context.id, isAutoIncluded)}
                          className={`mt-0.5 ${getStatusColors("success").text} hover:${getStatusColors("error").text}`}
                          title={isAutoIncluded ? "Disable this context" : "Remove this context"}
                        >
                          <Check className="w-4 h-4" />
                        </button>
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="font-medium text-sm truncate">{context.name}</span>
                            {renderScopeBadge(context.scope)}
                          </div>
                          {autoIncludeReason && (
                            <p className="text-xs text-muted-foreground mt-0.5 flex items-center gap-1">
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
                <div className="relative pt-2 border-t border-border">
                  <button
                    onClick={() => setContextPicker({ ...contextPicker, isOpen: true })}
                    className="flex items-center gap-1 text-sm text-primary hover:text-primary/80"
                  >
                    <Plus className="w-4 h-4" />
                    Add Context
                  </button>

                  {/* Context Picker Dropdown */}
                  {contextPicker.isOpen && (
                    <div className="absolute left-0 right-0 top-full mt-1 bg-background border border-border rounded-lg shadow-xl z-50 max-h-64 overflow-hidden">
                      <div className="p-2 border-b border-border">
                        <input
                          type="text"
                          value={contextPicker.searchQuery}
                          onChange={(e) =>
                            setContextPicker({ ...contextPicker, searchQuery: e.target.value })
                          }
                          placeholder="Search contexts..."
                          className="w-full bg-muted border-none rounded px-2 py-1 text-sm focus:outline-none focus:ring-1 focus:ring-primary"
                          autoFocus
                        />
                      </div>
                      <div className="overflow-auto max-h-48">
                        {Object.entries(groupedAvailableContexts).map(([scope, ctxs]) => {
                          if (ctxs.length === 0) return null;
                          return (
                            <div key={scope}>
                              <div className="px-3 py-1 text-xs font-medium text-muted-foreground bg-muted/50 capitalize">
                                {scope}
                              </div>
                              {ctxs.map((ctx) => (
                                <button
                                  key={ctx.id}
                                  onClick={() => addContext(ctx.id)}
                                  className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-muted transition-colors"
                                >
                                  <Plus className="w-3.5 h-3.5 text-muted-foreground" />
                                  <span className="text-sm truncate">{ctx.name}</span>
                                </button>
                              ))}
                            </div>
                          );
                        })}
                        {availableContextsForPicker.length === 0 && (
                          <div className="px-3 py-4 text-sm text-muted-foreground text-center">
                            No matching contexts found
                          </div>
                        )}
                      </div>
                      <div className="p-2 border-t border-border">
                        <button
                          onClick={() => setContextPicker({ isOpen: false, searchQuery: "" })}
                          className="w-full text-sm text-muted-foreground hover:text-foreground"
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

          {/* Log Sources Section */}
          <div className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => toggleSection("log-sources")}
              className="w-full flex items-center gap-2 px-3 py-2 bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              {expandedSections.has("log-sources") ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <Terminal className={`w-4 h-4 ${getAccentColors("orange").text}`} />
              <span className="font-medium text-sm">Log Sources</span>
              <span className="ml-auto text-xs text-muted-foreground">
                {activeLogSources.length > 0 ? (
                  <>
                    {activeLogSources.length} source{activeLogSources.length !== 1 ? "s" : ""}
                    {activeLogProfile && (
                      <span className={`ml-1 ${getAccentColors("orange").text}`}>
                        ({activeLogProfile.name})
                      </span>
                    )}
                  </>
                ) : (
                  "Not configured"
                )}
              </span>
            </button>
            {expandedSections.has("log-sources") && (
              <div className="p-3 bg-background">
                {activeLogSources.length > 0 ? (
                  <div className="space-y-1">
                    {activeLogSources.map((source) => (
                      <div
                        key={source.id}
                        className="flex items-center gap-2 text-sm text-muted-foreground"
                      >
                        <FileCode className="w-3.5 h-3.5" />
                        <span className="font-medium text-foreground">{source.name}:</span>
                        <code className="text-xs bg-muted px-1 rounded truncate flex-1">
                          {source.path}
                        </code>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="flex items-center gap-2 text-sm text-muted-foreground">
                    <AlertCircle className="w-4 h-4" />
                    <span>No log sources configured.</span>
                    <button
                      onClick={() => {
                        // Navigate to log sources configuration
                        // This could be handled by the parent component
                      }}
                      className="text-primary hover:underline"
                    >
                      Configure
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>

          {/* System Context Section */}
          <div className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => toggleSection("system")}
              className="w-full flex items-center gap-2 px-3 py-2 bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              {expandedSections.has("system") ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <Settings2 className="w-4 h-4 text-muted-foreground" />
              <span className="font-medium text-sm">System Context</span>
              <span className="ml-auto text-xs text-muted-foreground">Runner instructions</span>
            </button>
            {expandedSections.has("system") && (
              <div className="p-3 bg-background">
                <p className="text-xs text-muted-foreground">
                  System context includes runner-specific instructions such as:
                </p>
                <ul className="mt-2 space-y-1 text-xs text-muted-foreground list-disc list-inside">
                  <li>Supervisor restart instructions (how to safely restart the runner)</li>
                  <li>Session lifecycle management ([TASK_COMPLETE] marker usage)</li>
                  <li>Structured findings format</li>
                  <li>GUI automation tools (if config loaded)</li>
                </ul>
              </div>
            )}
          </div>

          {/* Summary */}
          <div className="text-xs text-muted-foreground pt-2 border-t border-border flex items-center justify-between">
            <span>
              Total: {includedContexts.length} context{includedContexts.length !== 1 ? "s" : ""} +{" "}
              {activeLogSources.length} log source{activeLogSources.length !== 1 ? "s" : ""}
            </span>
            <button onClick={() => setViewMode("full")} className="text-primary hover:underline">
              View full prompt →
            </button>
          </div>
        </div>
      )}
    </CollapsiblePanel>
  );
}
