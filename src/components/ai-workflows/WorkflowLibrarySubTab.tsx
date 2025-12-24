/**
 * WorkflowLibrarySubTab.tsx
 *
 * Displays and manages saved AI workflows from the AI Workflows library.
 */

import { useState, useEffect } from "react";
import {
  Sparkles,
  Trash2,
  Play,
  Search,
  Clock,
  ChevronDown,
  ChevronUp,
  Edit2,
  Copy,
  Loader2,
} from "lucide-react";

/** Execution step in a workflow */
interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt";
  name: string;
  takeScreenshot: boolean;
  playwrightScriptId?: string;
  promptId?: string;
  promptContent?: string;
}

/** Saved AI Workflow */
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

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface WorkflowLibrarySubTabProps {
  onLog: (level: LogLevel, message: string) => void;
}

export function WorkflowLibrarySubTab({ onLog }: WorkflowLibrarySubTabProps) {
  const [workflows, setWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);

  // Fetch workflows on mount
  useEffect(() => {
    fetchWorkflows();
  }, []);

  const fetchWorkflows = async () => {
    setLoading(true);
    try {
      const response = await fetch("http://localhost:9876/ai-workflows");
      const result = await response.json();
      if (result.success && result.data) {
        setWorkflows(result.data);
      }
    } catch (error) {
      onLog("error", `Failed to fetch workflows: ${error}`);
    } finally {
      setLoading(false);
    }
  };

  const deleteWorkflow = async (id: string) => {
    try {
      const response = await fetch(`http://localhost:9876/ai-workflows/${id}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        setWorkflows((prev) => prev.filter((w) => w.id !== id));
        onLog("info", "Workflow deleted");
      } else {
        throw new Error(result.error);
      }
    } catch (error) {
      onLog("error", `Failed to delete workflow: ${error}`);
    }
  };

  const runWorkflow = async (workflow: SavedAiWorkflow) => {
    setRunningId(workflow.id);
    try {
      // Generate the prompt from the workflow
      const prompt = generatePromptFromWorkflow(workflow);

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
        onLog("info", `Started workflow: ${workflow.name}`);
      } else {
        throw new Error(result.error);
      }
    } catch (error) {
      onLog("error", `Failed to run workflow: ${error}`);
    } finally {
      setRunningId(null);
    }
  };

  const duplicateWorkflow = async (workflow: SavedAiWorkflow) => {
    try {
      const response = await fetch("http://localhost:9876/ai-workflows", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: `${workflow.name} (Copy)`,
          description: workflow.description,
          steps: workflow.steps,
          goal: workflow.goal,
          max_iterations: workflow.max_iterations,
          persistent_session: workflow.persistent_session,
          capture_input_validation: workflow.capture_input_validation,
          category: workflow.category,
          tags: workflow.tags,
        }),
      });

      const result = await response.json();
      if (result.success) {
        await fetchWorkflows();
        onLog("info", "Workflow duplicated");
      } else {
        throw new Error(result.error);
      }
    } catch (error) {
      onLog("error", `Failed to duplicate workflow: ${error}`);
    }
  };

  // Simple prompt generator (matches the format in WorkflowBuilderSubTab)
  const generatePromptFromWorkflow = (workflow: SavedAiWorkflow): string => {
    const lines: string[] = [];
    lines.push(`# AI Automation Task: ${workflow.name}`);
    lines.push("");
    lines.push(`## Goal`);
    lines.push(workflow.goal || "(No goal specified)");
    lines.push("");
    lines.push(`## Execution Steps`);
    workflow.steps.forEach((step, index) => {
      lines.push(`${index + 1}. [${step.type}] ${step.name}${step.takeScreenshot ? " (screenshot)" : ""}`);
    });
    lines.push("");
    lines.push(`## Settings`);
    lines.push(`- Max Iterations: ${workflow.max_iterations}`);
    lines.push(`- Mode: ${workflow.persistent_session ? "Persistent Session" : "Standard"}`);
    return lines.join("\n");
  };

  const filteredWorkflows = workflows.filter(
    (w) =>
      w.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      w.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      w.goal.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  const formatDate = (dateStr: string) => {
    const date = new Date(dateStr);
    return date.toLocaleDateString() + " " + date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="w-8 h-8 animate-spin text-primary" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Sparkles className="w-6 h-6 text-green-500" />
          <div>
            <h2 className="text-xl font-semibold">AI Workflows</h2>
            <p className="text-sm text-muted-foreground">
              {workflows.length} saved workflow{workflows.length !== 1 ? "s" : ""}
            </p>
          </div>
        </div>

        {/* Search */}
        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search workflows..."
            className="pl-9 pr-4 py-2 bg-background border border-border rounded-md text-sm w-64 focus:outline-none focus:ring-2 focus:ring-primary"
          />
        </div>
      </div>

      {/* Workflow List */}
      {filteredWorkflows.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          {searchQuery ? (
            <p>No workflows match your search.</p>
          ) : (
            <>
              <Sparkles className="w-12 h-12 mx-auto mb-4 opacity-50" />
              <p className="text-lg mb-2">No saved workflows yet</p>
              <p className="text-sm">Create workflows in the Builder tab and save them here.</p>
            </>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          {filteredWorkflows.map((workflow) => (
            <div
              key={workflow.id}
              className="card border border-border/50 overflow-hidden"
            >
              {/* Workflow Header */}
              <div className="p-4 flex items-center gap-4">
                <Sparkles className="w-5 h-5 text-green-500 flex-shrink-0" />

                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <h3 className="font-medium truncate">{workflow.name}</h3>
                    <span className="text-xs bg-muted px-2 py-0.5 rounded">
                      {workflow.steps.length} step{workflow.steps.length !== 1 ? "s" : ""}
                    </span>
                    {workflow.persistent_session && (
                      <span className="text-xs bg-orange-500/20 text-orange-600 px-2 py-0.5 rounded">
                        Persistent
                      </span>
                    )}
                  </div>
                  {workflow.description && (
                    <p className="text-sm text-muted-foreground truncate mt-0.5">
                      {workflow.description}
                    </p>
                  )}
                </div>

                {/* Actions */}
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => runWorkflow(workflow)}
                    disabled={runningId === workflow.id}
                    className="flex items-center gap-1 px-3 py-1.5 bg-green-500/10 text-green-600 rounded hover:bg-green-500/20 transition-colors disabled:opacity-50"
                  >
                    {runningId === workflow.id ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Play className="w-4 h-4" />
                    )}
                    Run
                  </button>
                  <button
                    onClick={() => duplicateWorkflow(workflow)}
                    className="p-1.5 text-muted-foreground hover:text-foreground transition-colors"
                    title="Duplicate"
                  >
                    <Copy className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => deleteWorkflow(workflow.id)}
                    className="p-1.5 text-muted-foreground hover:text-red-500 transition-colors"
                    title="Delete"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => setExpandedId(expandedId === workflow.id ? null : workflow.id)}
                    className="p-1.5 text-muted-foreground hover:text-foreground transition-colors"
                    title="Show details"
                  >
                    {expandedId === workflow.id ? (
                      <ChevronUp className="w-4 h-4" />
                    ) : (
                      <ChevronDown className="w-4 h-4" />
                    )}
                  </button>
                </div>
              </div>

              {/* Expanded Details */}
              {expandedId === workflow.id && (
                <div className="px-4 pb-4 pt-0 border-t border-border/50 space-y-4">
                  {/* Goal */}
                  {workflow.goal && (
                    <div>
                      <h4 className="text-xs font-semibold text-muted-foreground uppercase mb-1">Goal</h4>
                      <p className="text-sm">{workflow.goal}</p>
                    </div>
                  )}

                  {/* Steps */}
                  <div>
                    <h4 className="text-xs font-semibold text-muted-foreground uppercase mb-2">Steps</h4>
                    <div className="space-y-1">
                      {workflow.steps.map((step, index) => (
                        <div
                          key={step.id}
                          className="flex items-center gap-2 text-sm p-2 bg-background rounded"
                        >
                          <span className="text-xs text-muted-foreground w-5">{index + 1}.</span>
                          <span
                            className={`text-xs px-1.5 py-0.5 rounded ${
                              step.type === "workflow"
                                ? "bg-blue-500/20 text-blue-600"
                                : step.type === "state"
                                  ? "bg-purple-500/20 text-purple-600"
                                  : step.type === "playwright"
                                    ? "bg-green-500/20 text-green-600"
                                    : "bg-amber-500/20 text-amber-600"
                            }`}
                          >
                            {step.type}
                          </span>
                          <span className="truncate">{step.name}</span>
                          {step.takeScreenshot && (
                            <span className="text-xs text-muted-foreground">(screenshot)</span>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>

                  {/* Metadata */}
                  <div className="flex items-center gap-4 text-xs text-muted-foreground">
                    <div className="flex items-center gap-1">
                      <Clock className="w-3 h-3" />
                      Created: {formatDate(workflow.created_at)}
                    </div>
                    {workflow.modified_at !== workflow.created_at && (
                      <div className="flex items-center gap-1">
                        <Edit2 className="w-3 h-3" />
                        Modified: {formatDate(workflow.modified_at)}
                      </div>
                    )}
                    <div>Max iterations: {workflow.max_iterations}</div>
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
