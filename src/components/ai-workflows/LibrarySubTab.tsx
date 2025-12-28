/**
 * LibrarySubTab.tsx
 *
 * Unified Library view that combines:
 * - Prompts (from PromptLibraryTab)
 * - AI Workflows (from WorkflowLibrarySubTab)
 * - Playwright Scripts (from ScriptsSubTab)
 *
 * Each section can be expanded/collapsed. Items can be run directly,
 * which navigates to the Session tab.
 */

import { useState, useEffect, useCallback } from "react";
import {
  FileText,
  Sparkles,
  TestTube,
  Search,
  Play,
  Trash2,
  Copy,
  ChevronDown,
  ChevronUp,
  ChevronRight,
  Loader2,
  Plus,
  Clock,
  Tag,
  FolderOpen,
  Workflow,
  Eye,
  EyeOff,
  Settings,
  Target,
  List,
  Link,
  Code,
} from "lucide-react";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface LibrarySubTabProps {
  onLog: (level: LogLevel, message: string) => void;
  onNavigateToSession: () => void;
}

// Prompt types
interface SavedPrompt {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  max_iterations: number;
  workflow: {
    enabled: boolean;
    checkpoint_path: string;
    phase_field: string;
    completion_value: number;
    stall_threshold_secs: number;
    continuation_prompt: string;
    auto_continue: boolean;
  };
  created_at: string;
  modified_at: string;
}

// AI Workflow types
interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt";
  name: string;
  takeScreenshot: boolean;
}

interface SavedAiWorkflow {
  id: string;
  name: string;
  description: string;
  steps: ExecutionStep[];
  goal: string;
  max_iterations: number;
  persistent_session: boolean;
  capture_input_validation: boolean;
  category: string;
  tags: string[];
  created_at: string;
  modified_at: string;
}

// Playwright Script types
interface PlaywrightScript {
  id: string;
  name: string;
  description: string;
  target_url: string;
  script_content: string;
  created_at: string;
  modified_at: string;
}

type LibrarySection = "prompts" | "workflows" | "scripts";

export function LibrarySubTab({ onLog, onNavigateToSession }: LibrarySubTabProps) {
  // Data state
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [workflows, setWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);

  // Loading states
  const [loading, setLoading] = useState(true);
  const [runningId, setRunningId] = useState<string | null>(null);

  // UI state
  const [searchQuery, setSearchQuery] = useState("");
  const [expandedSections, setExpandedSections] = useState<Set<LibrarySection>>(
    new Set(["prompts", "workflows", "scripts"]),
  );
  const [expandedItemId, setExpandedItemId] = useState<string | null>(null);

  // Fetch all data on mount
  useEffect(() => {
    const fetchAll = async () => {
      setLoading(true);
      try {
        const [promptsRes, workflowsRes, scriptsRes] = await Promise.all([
          fetch("http://localhost:9876/prompts"),
          fetch("http://localhost:9876/ai-workflows"),
          fetch("http://localhost:9876/playwright/scripts"),
        ]);

        const [promptsData, workflowsData, scriptsData] = await Promise.all([
          promptsRes.json(),
          workflowsRes.json(),
          scriptsRes.json(),
        ]);

        if (promptsData.success) setPrompts(promptsData.data || []);
        if (workflowsData.success) setWorkflows(workflowsData.data || []);
        if (scriptsData.success) setScripts(scriptsData.data || []);
      } catch (error) {
        onLog("error", `Failed to fetch library data: ${error}`);
      } finally {
        setLoading(false);
      }
    };
    fetchAll();
  }, [onLog]);

  // Toggle section expansion
  const toggleSection = (section: LibrarySection) => {
    setExpandedSections((prev) => {
      const next = new Set(prev);
      if (next.has(section)) {
        next.delete(section);
      } else {
        next.add(section);
      }
      return next;
    });
  };

  // Toggle item expansion (show details)
  const toggleItemExpand = (id: string) => {
    setExpandedItemId((prev) => (prev === id ? null : id));
  };

  // Run a prompt using the unified sessions API
  const runPrompt = useCallback(
    async (prompt: SavedPrompt) => {
      setRunningId(prompt.id);
      try {
        // Use sessions API to run inline (not spawn external Claude window)
        const isWorkflow = prompt.workflow?.enabled;
        const requestBody = {
          session_type: isWorkflow ? "prompt_workflow" : "one_shot",
          name: prompt.name,
          prompt: prompt.content,
          continuation_prompt: isWorkflow ? prompt.workflow.continuation_prompt : null,
          total_phases: isWorkflow ? prompt.workflow.completion_value : prompt.max_iterations,
          uses_gui: false,
          timeout_seconds: 1800,
          // Multi-session workflow config (runner handles continuation)
          checkpoint_path: isWorkflow ? prompt.workflow.checkpoint_path : null,
          phase_field: isWorkflow ? prompt.workflow.phase_field : "current_phase",
          completion_value: isWorkflow ? prompt.workflow.completion_value : null,
        };
        console.log("[LibrarySubTab] Running prompt with config:", JSON.stringify(requestBody, null, 2));
        const response = await fetch("http://localhost:9876/sessions/start", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(requestBody),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started prompt: ${prompt.name}`);
          onNavigateToSession();
        } else {
          throw new Error(result.error || "Failed to run prompt");
        }
      } catch (error) {
        onLog("error", `Failed to run prompt: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToSession],
  );

  // Run an AI workflow
  const runWorkflow = useCallback(
    async (workflow: SavedAiWorkflow) => {
      setRunningId(workflow.id);
      try {
        // Generate prompt from workflow
        const prompt = generateWorkflowPrompt(workflow);

        const response = await fetch("http://localhost:9876/sessions/start", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            session_type: workflow.persistent_session ? "ai_builder" : "one_shot",
            name: workflow.name,
            prompt: prompt,
            total_phases: workflow.max_iterations,
            uses_gui: false,
            timeout_seconds: 1800,
          }),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started workflow: ${workflow.name}`);
          onNavigateToSession();
        } else {
          throw new Error(result.error || "Failed to run workflow");
        }
      } catch (error) {
        onLog("error", `Failed to run workflow: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToSession],
  );

  // Run a Playwright script
  const runScript = useCallback(
    async (script: PlaywrightScript) => {
      setRunningId(script.id);
      try {
        const response = await fetch(`http://localhost:9876/playwright/scripts/${script.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started script: ${script.name}`);
          onNavigateToSession();
        } else {
          throw new Error(result.error || "Failed to run script");
        }
      } catch (error) {
        onLog("error", `Failed to run script: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToSession],
  );

  // Delete handlers
  const deletePrompt = useCallback(
    async (id: string) => {
      try {
        const response = await fetch(`http://localhost:9876/prompts/${id}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          setPrompts((prev) => prev.filter((p) => p.id !== id));
          onLog("info", "Prompt deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete prompt: ${error}`);
      }
    },
    [onLog],
  );

  const deleteWorkflow = useCallback(
    async (id: string) => {
      try {
        const response = await fetch(`http://localhost:9876/ai-workflows/${id}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          setWorkflows((prev) => prev.filter((w) => w.id !== id));
          onLog("info", "Workflow deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete workflow: ${error}`);
      }
    },
    [onLog],
  );

  const deleteScript = useCallback(
    async (id: string) => {
      try {
        const response = await fetch(`http://localhost:9876/playwright/scripts/${id}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          setScripts((prev) => prev.filter((s) => s.id !== id));
          onLog("info", "Script deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete script: ${error}`);
      }
    },
    [onLog],
  );

  // Generate prompt from workflow
  const generateWorkflowPrompt = (workflow: SavedAiWorkflow): string => {
    const lines: string[] = [];
    lines.push(`# AI Automation Task: ${workflow.name}`);
    lines.push("");
    lines.push(`## Goal`);
    lines.push(workflow.goal || "(No goal specified)");
    lines.push("");
    lines.push(`## Execution Steps`);
    workflow.steps.forEach((step, index) => {
      lines.push(
        `${index + 1}. [${step.type}] ${step.name}${step.takeScreenshot ? " (screenshot)" : ""}`,
      );
    });
    lines.push("");
    lines.push(`## Settings`);
    lines.push(`- Max Iterations: ${workflow.max_iterations}`);
    lines.push(`- Mode: ${workflow.persistent_session ? "Persistent Session" : "Standard"}`);
    return lines.join("\n");
  };

  // Filter items based on search
  const filteredPrompts = prompts.filter(
    (p) =>
      p.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      p.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      p.category.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  const filteredWorkflows = workflows.filter(
    (w) =>
      w.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      w.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      w.goal.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  const filteredScripts = scripts.filter(
    (s) =>
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.target_url.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  const formatDate = (dateStr: string) => {
    const date = new Date(dateStr);
    return date.toLocaleDateString();
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="w-8 h-8 animate-spin text-primary" />
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-4 space-y-4">
      {/* Search Bar */}
      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
        <input
          type="text"
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          placeholder="Search prompts, workflows, and scripts..."
          className="w-full pl-9 pr-4 py-2 bg-background border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary"
        />
      </div>

      {/* Prompts Section */}
      <div className="card overflow-hidden">
        <button
          onClick={() => toggleSection("prompts")}
          className="w-full flex items-center justify-between p-4 hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <FileText className="w-5 h-5 text-amber-500" />
            <div className="text-left">
              <h3 className="font-semibold">Prompts</h3>
              <p className="text-xs text-muted-foreground">
                {filteredPrompts.length} prompt{filteredPrompts.length !== 1 ? "s" : ""}
              </p>
            </div>
          </div>
          {expandedSections.has("prompts") ? (
            <ChevronUp className="w-5 h-5" />
          ) : (
            <ChevronDown className="w-5 h-5" />
          )}
        </button>

        {expandedSections.has("prompts") && (
          <div className="border-t border-border">
            {filteredPrompts.length === 0 ? (
              <div className="p-4 text-center text-muted-foreground text-sm">No prompts found</div>
            ) : (
              <div className="divide-y divide-border">
                {filteredPrompts.map((prompt) => {
                  const isExpanded = expandedItemId === prompt.id;
                  return (
                    <div key={prompt.id} className="hover:bg-muted/20">
                      {/* Header row - clickable */}
                      <div className="flex items-center gap-3 p-3">
                        <button
                          onClick={() => toggleItemExpand(prompt.id)}
                          className="flex items-center gap-2 flex-1 min-w-0 text-left"
                        >
                          <ChevronRight
                            className={`w-4 h-4 text-muted-foreground transition-transform flex-shrink-0 ${
                              isExpanded ? "rotate-90" : ""
                            }`}
                          />
                          <FileText className="w-4 h-4 text-amber-500 flex-shrink-0" />
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium truncate">{prompt.name}</span>
                              {prompt.workflow.enabled && (
                                <span className="text-xs bg-orange-500/20 text-orange-600 px-1.5 py-0.5 rounded">
                                  <Workflow className="w-3 h-3 inline mr-1" />
                                  Workflow
                                </span>
                              )}
                              {prompt.category && (
                                <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                                  {prompt.category}
                                </span>
                              )}
                            </div>
                            {prompt.description && (
                              <p className="text-xs text-muted-foreground truncate">
                                {prompt.description}
                              </p>
                            )}
                          </div>
                        </button>
                        <div className="flex items-center gap-1 flex-shrink-0">
                          <button
                            onClick={() => runPrompt(prompt)}
                            disabled={runningId === prompt.id}
                            className="flex items-center gap-1 px-2 py-1 bg-amber-500/10 text-amber-600 rounded hover:bg-amber-500/20 transition-colors text-sm disabled:opacity-50"
                          >
                            {runningId === prompt.id ? (
                              <Loader2 className="w-3 h-3 animate-spin" />
                            ) : (
                              <Play className="w-3 h-3" />
                            )}
                            Run
                          </button>
                          <button
                            onClick={() => deletePrompt(prompt.id)}
                            className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
                          >
                            <Trash2 className="w-4 h-4" />
                          </button>
                        </div>
                      </div>

                      {/* Expanded details */}
                      {isExpanded && (
                        <div className="px-4 pb-4 pt-0 ml-10 space-y-3 border-t border-border/50 bg-muted/10">
                          {/* Tags */}
                          {prompt.tags && prompt.tags.length > 0 && (
                            <div className="flex items-start gap-2 pt-3">
                              <Tag className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex flex-wrap gap-1">
                                {prompt.tags.map((tag) => (
                                  <span
                                    key={tag}
                                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                                  >
                                    {tag}
                                  </span>
                                ))}
                              </div>
                            </div>
                          )}

                          {/* Settings */}
                          <div className="flex items-start gap-2">
                            <Settings className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                            <div className="text-xs text-muted-foreground space-y-1">
                              <div>Max iterations: {prompt.max_iterations}</div>
                              {prompt.workflow.enabled && (
                                <>
                                  <div>
                                    Auto-continue: {prompt.workflow.auto_continue ? "Yes" : "No"}
                                  </div>
                                  <div>
                                    Stall threshold: {prompt.workflow.stall_threshold_secs}s
                                  </div>
                                </>
                              )}
                            </div>
                          </div>

                          {/* Prompt Content */}
                          <div className="flex items-start gap-2">
                            <Code className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                            <div className="flex-1 min-w-0">
                              <div className="text-xs font-medium text-muted-foreground mb-1">
                                Prompt Content:
                              </div>
                              <pre className="text-xs bg-background border border-border rounded p-2 overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap break-words">
                                {prompt.content}
                              </pre>
                            </div>
                          </div>

                          {/* Workflow continuation prompt */}
                          {prompt.workflow.enabled && prompt.workflow.continuation_prompt && (
                            <div className="flex items-start gap-2">
                              <Workflow className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex-1 min-w-0">
                                <div className="text-xs font-medium text-muted-foreground mb-1">
                                  Continuation Prompt:
                                </div>
                                <pre className="text-xs bg-background border border-border rounded p-2 overflow-x-auto max-h-24 overflow-y-auto whitespace-pre-wrap break-words">
                                  {prompt.workflow.continuation_prompt}
                                </pre>
                              </div>
                            </div>
                          )}

                          {/* Timestamps */}
                          <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t border-border/50">
                            <span>Created: {formatDate(prompt.created_at)}</span>
                            <span>Modified: {formatDate(prompt.modified_at)}</span>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </div>

      {/* AI Workflows Section */}
      <div className="card overflow-hidden">
        <button
          onClick={() => toggleSection("workflows")}
          className="w-full flex items-center justify-between p-4 hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <Sparkles className="w-5 h-5 text-green-500" />
            <div className="text-left">
              <h3 className="font-semibold">AI Workflows</h3>
              <p className="text-xs text-muted-foreground">
                {filteredWorkflows.length} workflow{filteredWorkflows.length !== 1 ? "s" : ""}
              </p>
            </div>
          </div>
          {expandedSections.has("workflows") ? (
            <ChevronUp className="w-5 h-5" />
          ) : (
            <ChevronDown className="w-5 h-5" />
          )}
        </button>

        {expandedSections.has("workflows") && (
          <div className="border-t border-border">
            {filteredWorkflows.length === 0 ? (
              <div className="p-4 text-center text-muted-foreground text-sm">
                No workflows found
              </div>
            ) : (
              <div className="divide-y divide-border">
                {filteredWorkflows.map((workflow) => {
                  const isExpanded = expandedItemId === workflow.id;
                  return (
                    <div key={workflow.id} className="hover:bg-muted/20">
                      {/* Header row - clickable */}
                      <div className="flex items-center gap-3 p-3">
                        <button
                          onClick={() => toggleItemExpand(workflow.id)}
                          className="flex items-center gap-2 flex-1 min-w-0 text-left"
                        >
                          <ChevronRight
                            className={`w-4 h-4 text-muted-foreground transition-transform flex-shrink-0 ${
                              isExpanded ? "rotate-90" : ""
                            }`}
                          />
                          <Sparkles className="w-4 h-4 text-green-500 flex-shrink-0" />
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium truncate">{workflow.name}</span>
                              <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                                {workflow.steps.length} steps
                              </span>
                              {workflow.persistent_session && (
                                <span className="text-xs bg-blue-500/20 text-blue-600 px-1.5 py-0.5 rounded">
                                  Persistent
                                </span>
                              )}
                              {workflow.category && (
                                <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                                  {workflow.category}
                                </span>
                              )}
                            </div>
                            {workflow.description && (
                              <p className="text-xs text-muted-foreground truncate">
                                {workflow.description}
                              </p>
                            )}
                          </div>
                        </button>
                        <div className="flex items-center gap-1 flex-shrink-0">
                          <button
                            onClick={() => runWorkflow(workflow)}
                            disabled={runningId === workflow.id}
                            className="flex items-center gap-1 px-2 py-1 bg-green-500/10 text-green-600 rounded hover:bg-green-500/20 transition-colors text-sm disabled:opacity-50"
                          >
                            {runningId === workflow.id ? (
                              <Loader2 className="w-3 h-3 animate-spin" />
                            ) : (
                              <Play className="w-3 h-3" />
                            )}
                            Run
                          </button>
                          <button
                            onClick={() => deleteWorkflow(workflow.id)}
                            className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
                          >
                            <Trash2 className="w-4 h-4" />
                          </button>
                        </div>
                      </div>

                      {/* Expanded details */}
                      {isExpanded && (
                        <div className="px-4 pb-4 pt-0 ml-10 space-y-3 border-t border-border/50 bg-muted/10">
                          {/* Goal */}
                          {workflow.goal && (
                            <div className="flex items-start gap-2 pt-3">
                              <Target className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex-1 min-w-0">
                                <div className="text-xs font-medium text-muted-foreground mb-1">
                                  Goal:
                                </div>
                                <p className="text-sm">{workflow.goal}</p>
                              </div>
                            </div>
                          )}

                          {/* Tags */}
                          {workflow.tags && workflow.tags.length > 0 && (
                            <div className="flex items-start gap-2">
                              <Tag className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex flex-wrap gap-1">
                                {workflow.tags.map((tag) => (
                                  <span
                                    key={tag}
                                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                                  >
                                    {tag}
                                  </span>
                                ))}
                              </div>
                            </div>
                          )}

                          {/* Steps */}
                          <div className="flex items-start gap-2">
                            <List className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                            <div className="flex-1 min-w-0">
                              <div className="text-xs font-medium text-muted-foreground mb-1">
                                Execution Steps:
                              </div>
                              <div className="space-y-1">
                                {workflow.steps.map((step, index) => (
                                  <div
                                    key={step.id}
                                    className="flex items-center gap-2 text-xs bg-background border border-border rounded px-2 py-1"
                                  >
                                    <span className="text-muted-foreground font-mono">
                                      {index + 1}.
                                    </span>
                                    <span
                                      className={`px-1 py-0.5 rounded text-[10px] uppercase font-medium ${
                                        step.type === "workflow"
                                          ? "bg-green-500/20 text-green-600"
                                          : step.type === "playwright"
                                            ? "bg-purple-500/20 text-purple-600"
                                            : step.type === "prompt"
                                              ? "bg-amber-500/20 text-amber-600"
                                              : "bg-blue-500/20 text-blue-600"
                                      }`}
                                    >
                                      {step.type}
                                    </span>
                                    <span className="flex-1 truncate">{step.name}</span>
                                    {step.takeScreenshot && (
                                      <span className="text-muted-foreground">📸</span>
                                    )}
                                  </div>
                                ))}
                              </div>
                            </div>
                          </div>

                          {/* Settings */}
                          <div className="flex items-start gap-2">
                            <Settings className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                            <div className="text-xs text-muted-foreground space-y-1">
                              <div>Max iterations: {workflow.max_iterations}</div>
                              <div>
                                Session: {workflow.persistent_session ? "Persistent" : "Standard"}
                              </div>
                              <div>
                                Capture validation:{" "}
                                {workflow.capture_input_validation ? "Yes" : "No"}
                              </div>
                            </div>
                          </div>

                          {/* Timestamps */}
                          <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t border-border/50">
                            <span>Created: {formatDate(workflow.created_at)}</span>
                            <span>Modified: {formatDate(workflow.modified_at)}</span>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </div>

      {/* Scripts Section */}
      <div className="card overflow-hidden">
        <button
          onClick={() => toggleSection("scripts")}
          className="w-full flex items-center justify-between p-4 hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <TestTube className="w-5 h-5 text-purple-500" />
            <div className="text-left">
              <h3 className="font-semibold">Playwright Scripts</h3>
              <p className="text-xs text-muted-foreground">
                {filteredScripts.length} script{filteredScripts.length !== 1 ? "s" : ""}
              </p>
            </div>
          </div>
          {expandedSections.has("scripts") ? (
            <ChevronUp className="w-5 h-5" />
          ) : (
            <ChevronDown className="w-5 h-5" />
          )}
        </button>

        {expandedSections.has("scripts") && (
          <div className="border-t border-border">
            {filteredScripts.length === 0 ? (
              <div className="p-4 text-center text-muted-foreground text-sm">No scripts found</div>
            ) : (
              <div className="divide-y divide-border">
                {filteredScripts.map((script) => {
                  const isExpanded = expandedItemId === script.id;
                  return (
                    <div key={script.id} className="hover:bg-muted/20">
                      {/* Header row - clickable */}
                      <div className="flex items-center gap-3 p-3">
                        <button
                          onClick={() => toggleItemExpand(script.id)}
                          className="flex items-center gap-2 flex-1 min-w-0 text-left"
                        >
                          <ChevronRight
                            className={`w-4 h-4 text-muted-foreground transition-transform flex-shrink-0 ${
                              isExpanded ? "rotate-90" : ""
                            }`}
                          />
                          <TestTube className="w-4 h-4 text-purple-500 flex-shrink-0" />
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium truncate">{script.name}</span>
                            </div>
                            {script.target_url && (
                              <p className="text-xs text-muted-foreground truncate">
                                {script.target_url}
                              </p>
                            )}
                          </div>
                        </button>
                        <div className="flex items-center gap-1 flex-shrink-0">
                          <button
                            onClick={() => runScript(script)}
                            disabled={runningId === script.id}
                            className="flex items-center gap-1 px-2 py-1 bg-purple-500/10 text-purple-600 rounded hover:bg-purple-500/20 transition-colors text-sm disabled:opacity-50"
                          >
                            {runningId === script.id ? (
                              <Loader2 className="w-3 h-3 animate-spin" />
                            ) : (
                              <Play className="w-3 h-3" />
                            )}
                            Run
                          </button>
                          <button
                            onClick={() => deleteScript(script.id)}
                            className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
                          >
                            <Trash2 className="w-4 h-4" />
                          </button>
                        </div>
                      </div>

                      {/* Expanded details */}
                      {isExpanded && (
                        <div className="px-4 pb-4 pt-0 ml-10 space-y-3 border-t border-border/50 bg-muted/10">
                          {/* Description */}
                          {script.description && (
                            <div className="flex items-start gap-2 pt-3">
                              <FileText className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex-1 min-w-0">
                                <div className="text-xs font-medium text-muted-foreground mb-1">
                                  Description:
                                </div>
                                <p className="text-sm">{script.description}</p>
                              </div>
                            </div>
                          )}

                          {/* Target URL */}
                          {script.target_url && (
                            <div className="flex items-start gap-2">
                              <Link className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                              <div className="flex-1 min-w-0">
                                <div className="text-xs font-medium text-muted-foreground mb-1">
                                  Target URL:
                                </div>
                                <a
                                  href={script.target_url}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                  className="text-sm text-primary hover:underline break-all"
                                >
                                  {script.target_url}
                                </a>
                              </div>
                            </div>
                          )}

                          {/* Script Content */}
                          <div className="flex items-start gap-2">
                            <Code className="w-4 h-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                            <div className="flex-1 min-w-0">
                              <div className="text-xs font-medium text-muted-foreground mb-1">
                                Script Content:
                              </div>
                              <pre className="text-xs bg-background border border-border rounded p-2 overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap break-words font-mono">
                                {script.script_content}
                              </pre>
                            </div>
                          </div>

                          {/* Timestamps */}
                          <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t border-border/50">
                            <span>Created: {formatDate(script.created_at)}</span>
                            <span>Modified: {formatDate(script.modified_at)}</span>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </div>

      {/* Empty State */}
      {filteredPrompts.length === 0 &&
        filteredWorkflows.length === 0 &&
        filteredScripts.length === 0 && (
          <div className="text-center py-12 text-muted-foreground">
            <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p className="text-lg mb-2">No items in library</p>
            <p className="text-sm">Create prompts, workflows, or scripts using the Builder tab.</p>
          </div>
        )}
    </div>
  );
}
