/**
 * WorkflowBuilderTab.tsx
 *
 * Main component for the unified Workflow Builder.
 * Organizes steps into three phases: Setup, Verification, Agentic.
 */

import React, { useState, useCallback, useEffect, useRef } from "react";
import {
  Plus,
  Save,
  Play,
  Square,
  Settings,
  FileText,
  Loader2,
  Search,
  Sparkles,
  Info,
  Trash2,
  Check,
  Download,
  ChevronDown,
  ChevronUp,
  AlertCircle,
} from "lucide-react";
import type {
  WorkflowPhase,
  UnifiedStep,
  UnifiedWorkflow,
  WorkflowExport,
  SavedPrompt,
  SavedShellCommand,
  LogSourceSelection,
  HealthCheckUrl,
} from "../../types";
import { generateStepId } from "../../types";
import { useGlobalLogSources } from "../../hooks/useGlobalLogSources";
import { WorkflowBuilderProvider, useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PhaseSection } from "./PhaseSection";
import { StepItem } from "./StepItem";
import { AddStepDropdown, AddStepButton } from "./AddStepDropdown";
import { StepConfigPanel } from "./StepConfigPanel";
import { PromptLibraryPicker } from "./PromptLibraryPicker";
import { ShellCommandLibraryPicker } from "./ShellCommandLibraryPicker";
import { CheckLibraryPicker } from "./CheckLibraryPicker";
import { CheckGroupLibraryPicker } from "./CheckGroupLibraryPicker";
import { MacroLibraryPicker } from "./MacroLibraryPicker";
import { PlaywrightScriptLibraryPicker } from "./PlaywrightScriptLibraryPicker";
import { TestLibraryPicker } from "./TestLibraryPicker";
import { WorkflowLibraryPicker } from "./WorkflowLibraryPicker";
import { StateLibraryPicker } from "./StateLibraryPicker";
import { McpServerToolPicker } from "./McpServerToolPicker";
import { AiGenerateWorkflowModal } from "./AiGenerateWorkflowModal";
import { GenerateFromStatesModal } from "./GenerateFromStatesModal";
import { AddStateStepsModal } from "./AddStateStepsModal";
import { LiveBrowserPanel } from "./LiveBrowserPanel";
import { PromptTemplateEditor } from "./PromptTemplateEditor";
import { ContextManagement } from "./ContextManagement";
import { PageTutorialMenu } from "../tutorial";
import { getAccentColors } from "@/design-system";
import { BatchDeleteDialog, ConfirmDialog } from "../ui";
import { BuilderToolbar, toolbarActions } from "../ui/BuilderToolbar";

const API_BASE = "http://localhost:9876";

// =============================================================================
// AI Provider/Model Options
// =============================================================================

/** Provider options for workflow-level AI override */
const PROVIDER_OPTIONS = [
  { value: "", label: "Use Default (from Settings)" },
  { value: "claude_cli", label: "Claude CLI" },
  { value: "anthropic_api", label: "Anthropic API" },
  { value: "openai_api", label: "OpenAI API" },
  { value: "gemini_api", label: "Gemini API" },
];

/** Model options per provider - shown when a specific provider is selected */
const MODELS_BY_PROVIDER: Record<string, { value: string; label: string }[]> = {
  claude_cli: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
    { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  ],
  anthropic_api: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
    { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  ],
  openai_api: [
    { value: "", label: "Default" },
    { value: "gpt-4o", label: "GPT-4o" },
    { value: "gpt-4o-mini", label: "GPT-4o Mini" },
    { value: "o1", label: "o1" },
    { value: "o1-mini", label: "o1-mini" },
  ],
  gemini_api: [
    { value: "", label: "Default" },
    { value: "gemini-3-flash-preview", label: "Gemini 3 Flash (Fast)" },
    { value: "gemini-3-pro-preview", label: "Gemini 3 Pro" },
    { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
    { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  ],
};

// =============================================================================
// Empty State Component
// =============================================================================

function EmptyState({ onAddStep }: { onAddStep: (phase: WorkflowPhase) => void }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 px-4 text-center">
      <FileText className="w-12 h-12 text-zinc-600 mb-4" />
      <h3 className="text-lg font-medium text-zinc-300 mb-2">Start Building</h3>
      <p className="text-sm text-zinc-500 mb-6 max-w-md">
        Add steps to build your workflow. Execution order:
        <br />
        <span className="text-blue-400">Setup</span> (once) {"->"} [
        <span className="text-green-400">Verification</span> {"<->"}{" "}
        <span className="text-amber-400">Agentic</span>] (loop) {"->"}{" "}
        <span className="text-purple-400">Completion</span> (once)
      </p>

      <div className="flex flex-wrap gap-3 justify-center">
        <button
          data-tutorial-id="setup-phase"
          onClick={() => onAddStep("setup")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-blue-500/20 hover:bg-blue-500/30 text-blue-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Setup</span>
        </button>
        <button
          data-tutorial-id="verification-phase"
          onClick={() => onAddStep("verification")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-green-500/20 hover:bg-green-500/30 text-green-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Verification</span>
        </button>
        <button
          data-tutorial-id="agentic-phase"
          onClick={() => onAddStep("agentic")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-amber-500/20 hover:bg-amber-500/30 text-amber-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Agentic</span>
        </button>
        <button
          data-tutorial-id="completion-phase"
          onClick={() => onAddStep("completion")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-purple-500/20 hover:bg-purple-500/30 text-purple-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Completion</span>
        </button>
      </div>
    </div>
  );
}

// =============================================================================
// Settings Panel Component
// =============================================================================

interface SettingsPanelProps {
  nameInputRef?: React.RefObject<HTMLInputElement | null>;
}

function SettingsPanel({ nameInputRef }: SettingsPanelProps) {
  const { state, updateWorkflow, features } = useWorkflowBuilder();
  const { workflow } = state;
  const { settings: logSourceSettings } = useGlobalLogSources();

  // Get current log source selection value for display
  const getLogSourceDisplayValue = (): string => {
    const selection = workflow.log_source_selection;
    if (!selection || selection === "default") return "default";
    if (selection === "ai") return "ai";
    if (selection === "all") return "all";
    if (typeof selection === "object" && "profile_id" in selection) {
      return `profile:${selection.profile_id}`;
    }
    return "default";
  };

  // Handle log source selection change
  const handleLogSourceChange = (value: string) => {
    let selection: LogSourceSelection;
    if (value === "default") {
      selection = "default";
    } else if (value === "ai") {
      selection = "ai";
    } else if (value === "all") {
      selection = "all";
    } else if (value.startsWith("profile:")) {
      selection = { profile_id: value.replace("profile:", "") };
    } else {
      selection = "default";
    }
    updateWorkflow({ log_source_selection: selection });
  };

  return (
    <div className="p-4 border-t border-zinc-700 space-y-4">
      {/* Always show name and description */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Name</label>
        <input
          ref={nameInputRef}
          type="text"
          value={workflow.name}
          onChange={(e) => updateWorkflow({ name: e.target.value })}
          placeholder="Workflow name..."
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-name-input"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Description</label>
        <textarea
          value={workflow.description}
          onChange={(e) => updateWorkflow({ description: e.target.value })}
          placeholder="What does this workflow do?"
          rows={2}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 resize-none"
          data-ui-id="workflow-builder-description-input"
        />
      </div>

      {/* Show iteration settings when there are agentic steps */}
      {features.showIterationSettings && (
        <div className="space-y-4">
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Max Iterations</label>
            <input
              type="number"
              value={workflow.max_iterations ?? 10}
              onChange={(e) => updateWorkflow({ max_iterations: parseInt(e.target.value) || 10 })}
              min={1}
              max={100}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-iterations-input"
            />
            <p className="text-xs text-zinc-500 mt-1">
              Maximum number of verification {"<->"} agentic loops
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              AI Session Timeout
            </label>
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="timeout-enabled"
                checked={workflow.timeout_seconds != null}
                onChange={(e) => {
                  if (e.target.checked) {
                    updateWorkflow({ timeout_seconds: 300 }); // Default 5 minutes
                  } else {
                    updateWorkflow({ timeout_seconds: null });
                  }
                }}
                className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
                data-ui-id="workflow-builder-timeout-checkbox"
              />
              <label htmlFor="timeout-enabled" className="text-sm text-zinc-400">
                Enable timeout
              </label>
              {workflow.timeout_seconds != null && (
                <input
                  type="number"
                  value={workflow.timeout_seconds}
                  onChange={(e) =>
                    updateWorkflow({
                      timeout_seconds: Math.max(60, parseInt(e.target.value) || 300),
                    })
                  }
                  min={60}
                  max={7200}
                  className="w-24 px-2 py-1 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50 text-sm"
                  data-ui-id="workflow-builder-timeout-input"
                />
              )}
              {workflow.timeout_seconds != null && (
                <span className="text-sm text-zinc-500">seconds</span>
              )}
            </div>
            <p className="text-xs text-zinc-500 mt-1">
              {workflow.timeout_seconds != null
                ? `Kill AI session after ${workflow.timeout_seconds}s of inactivity`
                : "No timeout - runs until completion or manual stop (recommended)"}
            </p>
          </div>
        </div>
      )}

      {/* Show provider/model settings when there are agentic steps */}
      {features.showIterationSettings && (
        <div className="p-3 bg-zinc-800/50 rounded-md space-y-3">
          <div className="flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-purple-400" />
            <span className="text-sm font-medium text-zinc-300">AI Provider Override</span>
            <div className="relative group">
              <Info className="w-3.5 h-3.5 text-zinc-500 hover:text-zinc-300 cursor-help" />
              <div className="absolute left-0 bottom-full mb-2 w-64 p-3 bg-zinc-800 border border-zinc-600 rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
                <p className="text-xs text-zinc-300">
                  Override the default AI provider for this workflow. Use Gemini Flash for fast,
                  cost-effective tasks. Leave empty to use the provider from Settings.
                </p>
              </div>
            </div>
          </div>
          <div className="flex gap-3">
            <select
              value={workflow.provider ?? ""}
              onChange={(e) => {
                updateWorkflow({
                  provider: e.target.value || undefined,
                  model: undefined, // Reset model when provider changes
                });
              }}
              className="flex-1 px-3 py-2 bg-zinc-700 border border-zinc-600 rounded-md text-zinc-200 text-sm focus:outline-none focus:ring-2 focus:ring-purple-500/50"
              data-ui-id="workflow-builder-provider-select"
            >
              {PROVIDER_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            {workflow.provider && MODELS_BY_PROVIDER[workflow.provider] && (
              <select
                value={workflow.model ?? ""}
                onChange={(e) => updateWorkflow({ model: e.target.value || undefined })}
                className="flex-1 px-3 py-2 bg-zinc-700 border border-zinc-600 rounded-md text-zinc-200 text-sm focus:outline-none focus:ring-2 focus:ring-purple-500/50"
                data-ui-id="workflow-builder-model-select"
              >
                {MODELS_BY_PROVIDER[workflow.provider].map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            )}
          </div>
        </div>
      )}

      {/* AI Summary toggle - show when workflow has any AI prompts */}
      {features.hasAiPrompts && (
        <div className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md">
          <div>
            <label className="block text-sm font-medium text-zinc-300">AI Summary</label>
            <p className="text-xs text-zinc-500">
              Generate an AI summary of the workflow execution
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={!workflow.skip_ai_summary}
            onClick={() => updateWorkflow({ skip_ai_summary: !workflow.skip_ai_summary })}
            className={`
              relative inline-flex h-6 w-11 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
              transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
              ${!workflow.skip_ai_summary ? "bg-blue-600" : "bg-zinc-600"}
            `}
            data-ui-id="workflow-builder-ai-summary-toggle"
          >
            <span
              className={`
                pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
                transition duration-200 ease-in-out
                ${!workflow.skip_ai_summary ? "translate-x-5" : "translate-x-0"}
              `}
            />
          </button>
        </div>
      )}

      {/* Health Check toggle - automatic server health checks */}
      <div className="space-y-2">
        <div className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md">
          <div>
            <label className="block text-sm font-medium text-zinc-300">Server Health Checks</label>
            <p className="text-xs text-zinc-500">
              Verify configured servers are running before verification
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={workflow.health_check_enabled ?? true}
            onClick={() =>
              updateWorkflow({ health_check_enabled: !(workflow.health_check_enabled ?? true) })
            }
            className={`
              relative inline-flex h-6 w-11 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
              transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
              ${(workflow.health_check_enabled ?? true) ? "bg-blue-600" : "bg-zinc-600"}
            `}
            data-ui-id="workflow-builder-health-check-toggle"
          >
            <span
              className={`
                pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
                transition duration-200 ease-in-out
                ${(workflow.health_check_enabled ?? true) ? "translate-x-5" : "translate-x-0"}
              `}
            />
          </button>
        </div>

        {/* Health Check URLs - shown when health checks are enabled */}
        {(workflow.health_check_enabled ?? true) && (
          <div className="pl-3 space-y-2">
            <div className="text-xs text-zinc-500">
              {(workflow.health_check_urls ?? []).length === 0
                ? "No health check URLs configured. Add URLs to check server availability."
                : `${(workflow.health_check_urls ?? []).length} health check URL(s) configured`}
            </div>

            {/* List existing health check URLs with inline editing */}
            {(workflow.health_check_urls ?? []).map((hc, index) => (
              <HealthCheckUrlEditor
                key={index}
                healthCheck={hc}
                onChange={(updated) => {
                  const urls = [...(workflow.health_check_urls ?? [])];
                  urls[index] = updated;
                  updateWorkflow({ health_check_urls: urls });
                }}
                onDelete={() => {
                  const urls = [...(workflow.health_check_urls ?? [])];
                  urls.splice(index, 1);
                  updateWorkflow({ health_check_urls: urls });
                }}
              />
            ))}

            {/* Add new health check URL */}
            <button
              type="button"
              onClick={() => {
                const newUrl: HealthCheckUrl = {
                  name: "New Health Check",
                  url: "http://localhost:8000/health",
                  expected_status: 200,
                  timeout_seconds: 5,
                  is_critical: true,
                };
                updateWorkflow({
                  health_check_urls: [...(workflow.health_check_urls ?? []), newUrl],
                });
              }}
              className="flex items-center gap-1 text-xs text-blue-400 hover:text-blue-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add Health Check URL
            </button>
          </div>
        )}
      </div>

      {/* Log Watch toggle - automatic log error detection */}
      <div className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md">
        <div>
          <label className="block text-sm font-medium text-zinc-300">
            Automatic Log Error Detection
          </label>
          <p className="text-xs text-zinc-500">
            Scan backend and frontend logs for errors before each verification
          </p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={workflow.log_watch_enabled ?? true}
          onClick={() =>
            updateWorkflow({ log_watch_enabled: !(workflow.log_watch_enabled ?? true) })
          }
          className={`
            relative inline-flex h-6 w-11 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
            transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
            ${(workflow.log_watch_enabled ?? true) ? "bg-blue-600" : "bg-zinc-600"}
          `}
          data-ui-id="workflow-builder-log-watch-toggle"
        >
          <span
            className={`
              pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
              transition duration-200 ease-in-out
              ${(workflow.log_watch_enabled ?? true) ? "translate-x-5" : "translate-x-0"}
            `}
          />
        </button>
      </div>

      {/* Developer Prompt Template - show when workflow has agentic steps */}
      {features.showIterationSettings && (
        <PromptTemplateEditor
          workflowTemplate={workflow.prompt_template}
          onWorkflowTemplateChange={(template) => updateWorkflow({ prompt_template: template })}
          hasAgenticSteps={true}
        />
      )}

      {/* Log Source Selection */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Log Sources</label>
        <select
          value={getLogSourceDisplayValue()}
          onChange={(e) => handleLogSourceChange(e.target.value)}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-log-sources-select"
        >
          <option value="default">Default (use global setting)</option>
          <option value="ai">AI-based selection</option>
          <option value="all">All enabled sources</option>
          {logSourceSettings?.profiles.map((profile) => (
            <option key={profile.id} value={`profile:${profile.id}`}>
              Profile: {profile.name}
            </option>
          ))}
        </select>
        <p className="text-xs text-zinc-500 mt-1">
          Which log sources to include when running this workflow
        </p>
      </div>

      {/* AI Contexts - show when workflow has agentic steps */}
      {features.hasAiPrompts && <ContextManagement />}

      {/* Category and tags */}
      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Category</label>
          <input
            type="text"
            value={workflow.category}
            onChange={(e) => updateWorkflow({ category: e.target.value })}
            placeholder="general"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-category-input"
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Tags</label>
          <input
            type="text"
            value={workflow.tags.join(", ")}
            onChange={(e) =>
              updateWorkflow({
                tags: e.target.value
                  .split(",")
                  .map((t) => t.trim())
                  .filter(Boolean),
              })
            }
            placeholder="tag1, tag2"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-tags-input"
          />
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Health Check URL Editor Component
// =============================================================================

interface HealthCheckUrlEditorProps {
  healthCheck: HealthCheckUrl;
  onChange: (updated: HealthCheckUrl) => void;
  onDelete: () => void;
}

function HealthCheckUrlEditor({ healthCheck, onChange, onDelete }: HealthCheckUrlEditorProps) {
  const [isExpanded, setIsExpanded] = useState(false);

  const updateField = <K extends keyof HealthCheckUrl>(field: K, value: HealthCheckUrl[K]) => {
    onChange({ ...healthCheck, [field]: value });
  };

  return (
    <div className="bg-zinc-800 rounded border border-zinc-700 overflow-hidden">
      {/* Collapsed view - clickable header */}
      <div
        className="flex items-center gap-2 p-2 cursor-pointer hover:bg-zinc-750 transition-colors"
        onClick={() => setIsExpanded(!isExpanded)}
      >
        <button
          type="button"
          className="p-0.5 text-zinc-400 hover:text-zinc-300 transition-colors"
          onClick={(e) => {
            e.stopPropagation();
            setIsExpanded(!isExpanded);
          }}
        >
          {isExpanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
        </button>

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-zinc-300 truncate">{healthCheck.name}</span>
            {(healthCheck.is_critical ?? true) && (
              <span className="flex items-center gap-0.5 px-1.5 py-0.5 text-[10px] font-medium bg-red-500/20 text-red-400 rounded">
                <AlertCircle className="w-3 h-3" />
                Critical
              </span>
            )}
          </div>
          <div className="text-xs text-zinc-500 truncate">{healthCheck.url}</div>
        </div>

        <div className="flex items-center gap-1 text-xs text-zinc-500">
          <span>{healthCheck.expected_status ?? 200}</span>
          <span className="text-zinc-600">|</span>
          <span>{healthCheck.timeout_seconds ?? 5}s</span>
        </div>

        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          className="p-1 text-zinc-500 hover:text-red-400 transition-colors"
          title="Remove health check"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      </div>

      {/* Expanded view - inline editing */}
      {isExpanded && (
        <div className="p-3 pt-0 space-y-3 border-t border-zinc-700">
          {/* Name field */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">Name</label>
            <input
              type="text"
              value={healthCheck.name}
              onChange={(e) => updateField("name", e.target.value)}
              className="w-full px-2 py-1.5 text-sm bg-zinc-900 border border-zinc-600 rounded
                         text-zinc-200 placeholder-zinc-500 focus:outline-none focus:border-blue-500"
              placeholder="e.g., Backend Server"
            />
          </div>

          {/* URL field */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">URL</label>
            <input
              type="text"
              value={healthCheck.url}
              onChange={(e) => updateField("url", e.target.value)}
              className="w-full px-2 py-1.5 text-sm bg-zinc-900 border border-zinc-600 rounded
                         text-zinc-200 placeholder-zinc-500 focus:outline-none focus:border-blue-500 font-mono"
              placeholder="http://localhost:8000/health"
            />
          </div>

          {/* Expected Status and Timeout - side by side */}
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs font-medium text-zinc-400 mb-1">
                Expected Status
              </label>
              <input
                type="number"
                value={healthCheck.expected_status ?? 200}
                onChange={(e) => updateField("expected_status", parseInt(e.target.value) || 200)}
                className="w-full px-2 py-1.5 text-sm bg-zinc-900 border border-zinc-600 rounded
                           text-zinc-200 focus:outline-none focus:border-blue-500"
                min={100}
                max={599}
              />
            </div>
            <div>
              <label className="block text-xs font-medium text-zinc-400 mb-1">
                Timeout (seconds)
              </label>
              <input
                type="number"
                value={healthCheck.timeout_seconds ?? 5}
                onChange={(e) => updateField("timeout_seconds", parseInt(e.target.value) || 5)}
                className="w-full px-2 py-1.5 text-sm bg-zinc-900 border border-zinc-600 rounded
                           text-zinc-200 focus:outline-none focus:border-blue-500"
                min={1}
                max={300}
              />
            </div>
          </div>

          {/* Critical toggle */}
          <div className="flex items-center justify-between py-1">
            <div>
              <label className="block text-xs font-medium text-zinc-400">Critical</label>
              <p className="text-[10px] text-zinc-500">Stop workflow if this check fails</p>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={healthCheck.is_critical ?? true}
              onClick={() => updateField("is_critical", !(healthCheck.is_critical ?? true))}
              className={`
                relative inline-flex h-5 w-9 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
                transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
                ${(healthCheck.is_critical ?? true) ? "bg-red-600" : "bg-zinc-600"}
              `}
            >
              <span
                className={`
                  pointer-events-none inline-block h-4 w-4 transform rounded-full bg-white shadow ring-0
                  transition duration-200 ease-in-out
                  ${(healthCheck.is_critical ?? true) ? "translate-x-4" : "translate-x-0"}
                `}
              />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Main Content Component
// =============================================================================

function WorkflowBuilderContent({
  onOpenLibrary: _onOpenLibrary,
  onNavigateToActive,
}: {
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const {
    state,
    isEmpty,
    hasUnsavedChanges,
    addStep,
    removeStep,
    moveStep,
    selectStep,
    getSelectedStep,
    showAddDropdown: _showAddDropdown,
    resetToNew,
    saveWorkflow,
    loadWorkflow,
    updateWorkflow,
  } = useWorkflowBuilder();

  const selectedStep = getSelectedStep();

  const [showSettings, setShowSettings] = useState(false);
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const [dropdownPhase, setDropdownPhase] = useState<WorkflowPhase | null>(null);

  // Prompt library picker state
  const [promptPickerOpen, setPromptPickerOpen] = useState(false);
  const [promptPickerPhase, setPromptPickerPhase] = useState<WorkflowPhase>("setup");

  // Shell command library picker state
  const [shellCommandPickerOpen, setShellCommandPickerOpen] = useState(false);
  const [shellCommandPickerPhase, setShellCommandPickerPhase] = useState<WorkflowPhase>("setup");

  // Check library picker state
  const [checkPickerOpen, setCheckPickerOpen] = useState(false);
  const [checkPickerPhase, setCheckPickerPhase] = useState<WorkflowPhase>("verification");

  // Test library picker state
  const [testPickerOpen, setTestPickerOpen] = useState(false);
  const [testPickerPhase, setTestPickerPhase] = useState<WorkflowPhase>("verification");

  // Check group library picker state
  const [checkGroupPickerOpen, setCheckGroupPickerOpen] = useState(false);
  const [checkGroupPickerPhase, setCheckGroupPickerPhase] = useState<WorkflowPhase>("verification");

  // Macro library picker state
  const [macroPickerOpen, setMacroPickerOpen] = useState(false);
  const [macroPickerPhase, setMacroPickerPhase] = useState<WorkflowPhase>("setup");

  // Playwright script library picker state
  const [playwrightScriptPickerOpen, setPlaywrightScriptPickerOpen] = useState(false);
  const [playwrightScriptPickerPhase, setPlaywrightScriptPickerPhase] =
    useState<WorkflowPhase>("verification");

  // Workflow library picker state
  const [workflowPickerOpen, setWorkflowPickerOpen] = useState(false);
  const [workflowPickerPhase, setWorkflowPickerPhase] = useState<WorkflowPhase>("setup");

  // State library picker state
  const [statePickerOpen, setStatePickerOpen] = useState(false);
  const [statePickerPhase, setStatePickerPhase] = useState<WorkflowPhase>("setup");

  // MCP server tool picker state
  const [mcpPickerOpen, setMcpPickerOpen] = useState(false);
  const [mcpPickerPhase, setMcpPickerPhase] = useState<WorkflowPhase>("setup");

  // AI generate workflow modal state
  const [aiGenerateModalOpen, setAiGenerateModalOpen] = useState(false);

  // Generate from states modal state
  const [generateFromStatesModalOpen, setGenerateFromStatesModalOpen] = useState(false);

  // Add state steps modal state
  const [addStateStepsModalOpen, setAddStateStepsModalOpen] = useState(false);

  // Live browser panel state
  const [liveBrowserPanelOpen, setLiveBrowserPanelOpen] = useState(false);

  // Ref for focusing the name input when creating new workflow
  const nameInputRef = useRef<HTMLInputElement | null>(null);

  // Workflow library state
  const [savedWorkflows, setSavedWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [workflowsLoading, setWorkflowsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");

  // Workflow selection mode state (for batch delete)
  const [isWorkflowSelectionMode, setIsWorkflowSelectionMode] = useState(false);
  const [selectedWorkflowIds, setSelectedWorkflowIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeletingWorkflows, setIsDeletingWorkflows] = useState(false);

  // Workflow import/export state
  const [isExportingWorkflow, setIsExportingWorkflow] = useState(false);
  const [workflowImportError, setWorkflowImportError] = useState<string | null>(null);

  // Handle creating a new workflow with visual feedback
  const handleNewWorkflow = useCallback(() => {
    resetToNew();
    setShowSettings(true);
    // Focus the name input after state updates
    setTimeout(() => {
      nameInputRef.current?.focus();
      nameInputRef.current?.select();
    }, 50);
  }, [resetToNew]);

  const accentColors = getAccentColors("green");

  // Fetch saved workflows
  const fetchWorkflows = useCallback(async () => {
    setWorkflowsLoading(true);
    try {
      const response = await fetch(`${API_BASE}/unified-workflows`);
      const result = await response.json();
      if (result.success && result.data) {
        setSavedWorkflows(result.data);
      } else if (Array.isArray(result)) {
        setSavedWorkflows(result);
      } else {
        setSavedWorkflows([]);
      }
    } catch (error) {
      console.error("Failed to fetch workflows:", error);
    } finally {
      setWorkflowsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchWorkflows();
  }, [fetchWorkflows]);

  // Refresh workflow list after save
  useEffect(() => {
    if (!state.isSaving && state.workflow.id) {
      fetchWorkflows();
    }
  }, [state.isSaving, state.workflow.id, fetchWorkflows]);

  // Toggle workflow selection for batch delete
  const toggleWorkflowSelection = useCallback((workflowId: string) => {
    setSelectedWorkflowIds((prev) => {
      const next = new Set(prev);
      if (next.has(workflowId)) {
        next.delete(workflowId);
      } else {
        next.add(workflowId);
      }
      return next;
    });
  }, []);

  // Exit workflow selection mode
  const exitWorkflowSelectionMode = useCallback(() => {
    setIsWorkflowSelectionMode(false);
    setSelectedWorkflowIds(new Set());
  }, []);

  // Delete selected workflows
  const deleteSelectedWorkflows = useCallback(async () => {
    if (selectedWorkflowIds.size === 0) return;

    setIsDeletingWorkflows(true);
    try {
      const deletePromises = Array.from(selectedWorkflowIds).map((id) =>
        fetch(`${API_BASE}/unified-workflows/${id}`, { method: "DELETE" }),
      );
      await Promise.all(deletePromises);

      // Refresh the list
      await fetchWorkflows();

      // If the currently edited workflow was deleted, reset to new
      if (state.workflow.id && selectedWorkflowIds.has(state.workflow.id)) {
        resetToNew();
      }

      // Exit selection mode
      exitWorkflowSelectionMode();
      setShowBatchDeleteDialog(false);
    } catch (error) {
      console.error("Failed to delete workflows:", error);
    } finally {
      setIsDeletingWorkflows(false);
    }
  }, [
    selectedWorkflowIds,
    fetchWorkflows,
    state.workflow.id,
    resetToNew,
    exitWorkflowSelectionMode,
  ]);

  // Get names of selected workflows for the delete dialog
  const getSelectedWorkflowNames = useCallback((): string[] => {
    return savedWorkflows.filter((w) => selectedWorkflowIds.has(w.id)).map((w) => w.name);
  }, [savedWorkflows, selectedWorkflowIds]);

  // Export the current workflow to JSON file
  const handleExportWorkflow = useCallback(async () => {
    if (!state.workflow.id) return;

    setIsExportingWorkflow(true);
    try {
      const response = await fetch(`${API_BASE}/unified-workflows/${state.workflow.id}/export`);
      const data = await response.json();
      if (data.success && data.data) {
        const exportData = data.data as WorkflowExport;
        const blob = new Blob([JSON.stringify(exportData, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `workflow-${exportData.workflow.name.replace(/[^a-zA-Z0-9]/g, "-")}.json`;
        a.click();
        URL.revokeObjectURL(url);
      } else {
        console.error("Failed to export workflow:", data.error);
      }
    } catch (error) {
      console.error("Failed to export workflow:", error);
    } finally {
      setIsExportingWorkflow(false);
    }
  }, [state.workflow.id]);

  // Import a workflow from JSON file
  const handleImportWorkflow = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      setWorkflowImportError(null);
      setWorkflowsLoading(true);

      try {
        const text = await file.text();
        const data = JSON.parse(text);

        // Validate the import format
        let workflowData: UnifiedWorkflow;
        if (data.manifest && data.workflow) {
          // Full export format
          workflowData = data.workflow;
        } else if (data.setup_steps !== undefined) {
          // Direct workflow object
          workflowData = data;
        } else {
          throw new Error("Invalid workflow file format");
        }

        // Call the import API
        const response = await fetch(`${API_BASE}/unified-workflows/import`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            workflow: workflowData,
            conflict_strategy: "generate",
          }),
        });

        const result = await response.json();
        if (result.success && result.data) {
          // Refresh the list and select the imported workflow
          await fetchWorkflows();
          await loadWorkflow(result.data.workflow.id);
        } else {
          setWorkflowImportError(result.error || "Failed to import workflow");
        }
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : "Failed to import workflow";
        setWorkflowImportError(errorMsg);
        console.error("Failed to import workflow:", error);
      } finally {
        setWorkflowsLoading(false);
        // Reset the file input via event target
        event.target.value = "";
      }
    },
    [fetchWorkflows, loadWorkflow],
  );

  // Filter workflows
  const filteredWorkflows = savedWorkflows.filter((w) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      w.name.toLowerCase().includes(query) ||
      w.description?.toLowerCase().includes(query) ||
      w.category?.toLowerCase().includes(query)
    );
  });

  // Select a workflow for editing
  const selectWorkflow = async (workflow: UnifiedWorkflow) => {
    await loadWorkflow(workflow.id);
  };

  // Handle save
  const handleSave = useCallback(async () => {
    const success = await saveWorkflow();
    if (success) {
      console.log("Workflow saved successfully");
    }
  }, [saveWorkflow]);

  // Execution state
  const [isExecuting, setIsExecuting] = useState(false);
  const [executionError, setExecutionError] = useState<string | null>(null);
  const [executionSuccess, setExecutionSuccess] = useState<string | null>(null);
  const [showSaveBeforeRunDialog, setShowSaveBeforeRunDialog] = useState(false);
  const [isSavingBeforeRun, setIsSavingBeforeRun] = useState(false);

  // Execute the workflow run (called after save confirmation)
  const executeWorkflowRun = useCallback(
    async (workflowId: string) => {
      setIsExecuting(true);
      setExecutionError(null);
      setExecutionSuccess(null);

      try {
        console.log("[WorkflowBuilder] Running workflow:", workflowId, state.workflow.name);

        const runUrl = `${API_BASE}/unified-workflows/${workflowId}/run`;
        const requestBody = JSON.stringify({
          monitor_index: 0,
          // No timeout_seconds - runs until completion or manual stop
        });

        fetch(runUrl, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: requestBody,
        })
          .then((response) => {
            console.log("[WorkflowBuilder] Workflow run response:", response.status);
          })
          .catch((error) => {
            console.error("[WorkflowBuilder] Workflow run error:", error);
          });

        setTimeout(() => {
          onNavigateToActive?.();
        }, 100);

        setExecutionSuccess("Workflow started! Check the Active tab to monitor progress.");
        setTimeout(() => setExecutionSuccess(null), 5000);
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : "Failed to start workflow";
        setExecutionError(errorMsg);
      } finally {
        setIsExecuting(false);
      }
    },
    [state.workflow.name, onNavigateToActive],
  );

  // Handle save and run (called from dialog confirmation)
  const handleSaveAndRun = useCallback(async () => {
    setIsSavingBeforeRun(true);
    try {
      const savedWorkflow = await saveWorkflow();
      if (!savedWorkflow) {
        setExecutionError("Failed to save workflow. Cannot run.");
        setShowSaveBeforeRunDialog(false);
        return;
      }
      setShowSaveBeforeRunDialog(false);
      await executeWorkflowRun(savedWorkflow.id);
    } finally {
      setIsSavingBeforeRun(false);
    }
  }, [saveWorkflow, executeWorkflowRun]);

  // Handle run workflow
  const handleRun = useCallback(async () => {
    if (isExecuting) return;

    // Check if workflow has steps
    if (isEmpty) {
      setExecutionError("Cannot run an empty workflow. Add some steps first.");
      return;
    }

    // Check if workflow is saved (must have an ID to run)
    if (!state.workflow.id || hasUnsavedChanges) {
      setShowSaveBeforeRunDialog(true);
      return;
    }

    // Workflow is already saved, run it directly
    await executeWorkflowRun(state.workflow.id);
  }, [state.workflow.id, isEmpty, hasUnsavedChanges, isExecuting, executeWorkflowRun]);

  // Handle stop execution
  const handleStop = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/stop-execution`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        setIsExecuting(false);
        console.log("[WorkflowBuilder] Execution stopped");
      }
    } catch (error) {
      console.error("[WorkflowBuilder] Failed to stop execution:", error);
    }
  }, []);

  const handleAddStep = useCallback((phase: WorkflowPhase) => {
    setDropdownPhase(phase);
    setDropdownOpen(true);
  }, []);

  const handleStepAdded = useCallback(
    (step: UnifiedStep, phase: WorkflowPhase) => {
      addStep(step, phase);
      setDropdownOpen(false);
    },
    [addStep],
  );

  // Handler to open the prompt library picker
  const handleOpenPromptLibrary = useCallback((phase: WorkflowPhase) => {
    setPromptPickerPhase(phase);
    setPromptPickerOpen(true);
  }, []);

  // Handler for when a prompt is selected from the library
  const handlePromptSelected = useCallback(
    (prompt: SavedPrompt, phase: WorkflowPhase) => {
      const promptNames: Record<WorkflowPhase, string> = {
        setup: "AI Setup Task",
        verification: "AI Verification",
        agentic: "AI Prompt",
        completion: "AI Completion Task",
      };

      const step: UnifiedStep = {
        id: generateStepId(),
        type: "prompt",
        phase: phase as "setup" | "verification" | "agentic" | "completion",
        name: prompt.name || promptNames[phase],
        content: prompt.content,
        prompt_id: prompt.id,
        provider: prompt.provider ?? undefined,
        model: prompt.model ?? undefined,
        is_blocking: phase === "verification" ? true : undefined,
      };

      addStep(step, phase);
      setPromptPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the shell command library picker
  const handleOpenShellCommandLibrary = useCallback((phase: WorkflowPhase) => {
    setShellCommandPickerPhase(phase);
    setShellCommandPickerOpen(true);
  }, []);

  // Handler for when a shell command is selected from the library
  const handleShellCommandSelected = useCallback(
    (command: SavedShellCommand, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "shell_command",
        phase: phase as "setup" | "completion",
        name: command.name || (phase === "setup" ? "Setup Command" : "Completion Command"),
        command: command.command,
        shell_command_id: command.id,
        working_directory: command.working_directory ?? undefined,
        timeout_seconds: command.timeout_seconds,
        fail_on_error: command.fail_on_error,
      };

      addStep(step, phase);
      setShellCommandPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the check library picker
  const handleOpenCheckLibrary = useCallback((phase: WorkflowPhase) => {
    setCheckPickerPhase(phase);
    setCheckPickerOpen(true);
  }, []);

  // Type for saved check from CheckLibraryPicker
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
  }

  // Handler for when a check is selected from the library
  const handleCheckSelected = useCallback(
    (check: SavedCheck, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "check",
        phase: "verification",
        name: check.name,
        check_type: check.check_type as
          | "lint"
          | "format"
          | "typecheck"
          | "analyze"
          | "security"
          | "custom_command",
        check_id: check.id,
        command: check.command,
        working_directory: check.working_directory,
        config_path: check.config_path,
        auto_fix: check.auto_fix,
        fail_on_warning: check.fail_on_warning,
        timeout_seconds: check.timeout_seconds,
        is_blocking: check.is_critical,
      };

      addStep(step, phase);
      setCheckPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the test library picker
  const handleOpenTestLibrary = useCallback((phase: WorkflowPhase) => {
    setTestPickerPhase(phase);
    setTestPickerOpen(true);
  }, []);

  // Type for saved test from TestLibraryPicker
  interface SavedTest {
    id: string;
    name: string;
    description?: string;
    test_type: "playwright_cdp" | "qontinui_vision" | "python_script" | "repository_test";
    category?: string;
    timeout_seconds: number;
    is_critical: boolean;
    enabled: boolean;
    tags: string[];
  }

  // Handler for when a test is selected from the library
  const handleTestSelected = useCallback(
    (test: SavedTest, phase: WorkflowPhase) => {
      // Map test_type to the workflow step test_type format
      const testTypeMap: Record<
        string,
        "playwright" | "qontinui_vision" | "python" | "repository" | "custom_command"
      > = {
        playwright_cdp: "playwright",
        qontinui_vision: "qontinui_vision",
        python_script: "python",
        repository_test: "repository",
      };

      const step: UnifiedStep = {
        id: generateStepId(),
        type: "test",
        phase: phase as "setup" | "verification" | "completion",
        name: test.name,
        test_type: testTypeMap[test.test_type] || "custom_command",
        test_id: test.id,
        timeout_seconds: test.timeout_seconds,
        is_critical: test.is_critical,
      };

      addStep(step, phase);
      setTestPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the check group library picker
  const handleOpenCheckGroupLibrary = useCallback((phase: WorkflowPhase) => {
    setCheckGroupPickerPhase(phase);
    setCheckGroupPickerOpen(true);
  }, []);

  // Type for check group from CheckGroupLibraryPicker
  interface CheckGroup {
    id: string;
    name: string;
    description?: string;
    color?: string;
    enabled: boolean;
    run_in_parallel: boolean;
    stop_on_failure: boolean;
    tags: string[];
  }

  // Handler for when a check group is selected from the library
  const handleCheckGroupSelected = useCallback(
    (group: CheckGroup, checksInGroup: { id: string; name: string }[], phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "check_group",
        phase: "verification",
        name: group.name,
        check_group_id: group.id,
        is_blocking: true, // Check groups are blocking by default
      };

      addStep(step, phase);
      setCheckGroupPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the macro library picker
  const handleOpenMacroLibrary = useCallback((phase: WorkflowPhase) => {
    setMacroPickerPhase(phase);
    setMacroPickerOpen(true);
  }, []);

  // Type for saved macro from MacroLibraryPicker
  interface SavedMacro {
    id: string;
    name: string;
    description: string;
    steps: { id: string; action_type: string; description: string }[];
    category: string;
    tags: string[];
  }

  // Handler for when a macro is selected from the library
  const handleMacroSelected = useCallback(
    (macro: SavedMacro, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "macro",
        phase: "setup",
        name: macro.name || "Run Macro",
        macro_id: macro.id,
      };

      addStep(step, phase);
      setMacroPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the playwright script library picker
  const handleOpenPlaywrightScriptLibrary = useCallback((phase: WorkflowPhase) => {
    setPlaywrightScriptPickerPhase(phase);
    setPlaywrightScriptPickerOpen(true);
  }, []);

  // Type for saved playwright script from PlaywrightScriptLibraryPicker
  interface SavedPlaywrightScript {
    id: string;
    name: string;
    description: string;
    target_url: string;
    script_content: string;
    category: string;
    tags: string[];
    timeout_seconds: number;
    display_mode: string;
    browser: string;
    headless: boolean;
  }

  // Handler for when a playwright script is selected from the library
  const handlePlaywrightScriptSelected = useCallback(
    (script: SavedPlaywrightScript, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "test",
        phase: "verification",
        name: script.name || "Playwright Test",
        test_type: "playwright",
        script_id: script.id,
        script_content: script.script_content,
        target_url: script.target_url,
        timeout_seconds: script.timeout_seconds,
        is_critical: true,
      };

      addStep(step, phase);
      setPlaywrightScriptPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the workflow library picker
  const handleOpenWorkflowLibrary = useCallback((phase: WorkflowPhase) => {
    setWorkflowPickerPhase(phase);
    setWorkflowPickerOpen(true);
  }, []);

  // Handler for when a workflow is selected from the library
  const handleWorkflowSelected = useCallback(
    (workflow: UnifiedWorkflow, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "workflow_ref",
        phase: phase as "setup" | "verification" | "completion",
        name: workflow.name || "Run Workflow",
        workflow_id: workflow.id,
      };

      addStep(step, phase);
      setWorkflowPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the state library picker
  const handleOpenStateLibrary = useCallback((phase: WorkflowPhase) => {
    setStatePickerPhase(phase);
    setStatePickerOpen(true);
  }, []);

  // Type for state from StateLibraryPicker
  interface StateInfo {
    id: string;
    name: string;
    description?: string;
    tags?: string[];
    is_initial?: boolean;
    is_terminal?: boolean;
  }

  // Handler for when a state is selected from the library
  const handleStateSelected = useCallback(
    (stateInfo: StateInfo, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "state",
        phase: phase as "setup" | "verification",
        name: stateInfo.name || "Navigate to State",
        state_id: stateInfo.id,
      };

      addStep(step, phase);
      setStatePickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the MCP server tool picker
  const handleOpenMcpServerToolPicker = useCallback((phase: WorkflowPhase) => {
    setMcpPickerPhase(phase);
    setMcpPickerOpen(true);
  }, []);

  // Types for MCP server and tool from McpServerToolPicker
  interface McpServerConfig {
    id: string;
    name: string;
    description?: string;
    server_type: string;
    command?: string;
    url?: string;
    enabled: boolean;
    auto_start: boolean;
    tags?: string[];
  }

  interface McpToolInfo {
    name: string;
    description?: string;
    input_schema?: Record<string, unknown>;
  }

  // Handler for when an MCP server tool is selected
  const handleMcpToolSelected = useCallback(
    (server: McpServerConfig, tool: McpToolInfo, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "mcp_call",
        phase: phase as "setup" | "verification" | "completion",
        name: `${server.name}: ${tool.name}`,
        server_id: server.id,
        tool_name: tool.name,
        tool_description: tool.description,
      };

      addStep(step, phase);
      setMcpPickerOpen(false);
    },
    [addStep],
  );

  // Handler for when an AI-generated workflow is ready to load
  const handleAiWorkflowGenerated = useCallback(
    (workflow: UnifiedWorkflow) => {
      // Load the generated workflow into the builder
      // The workflow will be treated as a new workflow (not yet saved)
      resetToNew();
      updateWorkflow({
        name: workflow.name,
        description: workflow.description,
        category: workflow.category,
        tags: workflow.tags,
        setup_steps: workflow.setup_steps,
        verification_steps: workflow.verification_steps,
        agentic_steps: workflow.agentic_steps,
        completion_steps: workflow.completion_steps,
        max_iterations: workflow.max_iterations,
        provider: workflow.provider ?? undefined,
        model: workflow.model ?? undefined,
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: workflow.log_source_selection,
      });
      setShowSettings(true);
      console.log("[WorkflowBuilder] Loaded AI-generated workflow:", workflow.name);
    },
    [resetToNew, updateWorkflow],
  );

  const renderStep = useCallback(
    (
      step: UnifiedStep,
      index: number,
      phase: WorkflowPhase,
      steps: UnifiedStep[],
      isSelectionMode: boolean = false,
      isSelectedForDelete: boolean = false,
      onToggleSelect?: () => void,
    ) => (
      <StepItem
        key={step.id}
        step={step}
        phase={phase}
        index={index}
        isFirst={index === 0}
        isLast={index === steps.length - 1}
        isSelected={state.selectedStepId === step.id}
        onMoveUp={() => moveStep(step.id, phase, "up")}
        onMoveDown={() => moveStep(step.id, phase, "down")}
        onDelete={() => removeStep(step.id, phase)}
        onClick={() => selectStep(step.id)}
        isSelectionMode={isSelectionMode}
        isSelectedForDelete={isSelectedForDelete}
        onToggleSelect={onToggleSelect}
      />
    ),
    [state.selectedStepId, moveStep, removeStep, selectStep],
  );

  // Get step count for a workflow
  const getStepCount = (workflow: UnifiedWorkflow): number => {
    return (
      (workflow.setup_steps?.length || 0) +
      (workflow.verification_steps?.length || 0) +
      (workflow.agentic_steps?.length || 0) +
      (workflow.completion_steps?.length || 0)
    );
  };

  return (
    <div className="h-full flex">
      {/* Left Panel - Workflow List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          {/* Title row */}
          <div className="flex items-center justify-between mb-2">
            <h2
              className="text-lg font-semibold flex items-center gap-2"
              data-ui-id="workflow-builder-title"
            >
              <Sparkles className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Workflows
            </h2>
            <PageTutorialMenu page="unified-workflow-builder" variant="compact" />
          </div>

          {/* Action buttons row */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.ai(() => setAiGenerateModalOpen(true)),
              toolbarActions.liveBrowser(() => setLiveBrowserPanelOpen(true)),
              toolbarActions.stateMachine(() => setGenerateFromStatesModalOpen(true)),
              toolbarActions.addFromState(() => setAddStateStepsModalOpen(true)),
              toolbarActions.new(handleNewWorkflow, "New"),
              toolbarActions.import(handleImportWorkflow, ".json", workflowsLoading),
              toolbarActions.delete(
                () => setIsWorkflowSelectionMode(!isWorkflowSelectionMode),
                isWorkflowSelectionMode,
              ),
            ]}
          />

          {/* Import error message */}
          {workflowImportError && (
            <div className="mx-4 mb-2 p-2 bg-red-500/10 border border-red-500/30 rounded-md text-red-400 text-xs flex items-center justify-between">
              <span>{workflowImportError}</span>
              <button
                onClick={() => setWorkflowImportError(null)}
                className="p-0.5 hover:bg-red-500/20 rounded"
              >
                <span className="text-red-400">×</span>
              </button>
            </div>
          )}

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search workflows..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
              data-ui-id="workflow-builder-search-input"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isWorkflowSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">{selectedWorkflowIds.size} selected</span>
            <div className="flex items-center gap-2">
              {selectedWorkflowIds.size > 0 && (
                <button
                  onClick={() => setShowBatchDeleteDialog(true)}
                  className="flex items-center gap-1 px-3 py-1 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  Delete
                </button>
              )}
              <button
                onClick={exitWorkflowSelectionMode}
                className="px-3 py-1 text-sm text-neutral-400 hover:text-neutral-200 transition-colors"
              >
                Cancel
              </button>
            </div>
          </div>
        )}

        {/* Workflow List */}
        <div className="flex-1 overflow-y-auto p-2">
          {workflowsLoading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredWorkflows.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No workflows found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredWorkflows.map((workflow) => (
                <button
                  key={workflow.id}
                  data-ui-id={`workflow-builder-list-item-${workflow.id}`}
                  onClick={() => {
                    if (isWorkflowSelectionMode) {
                      toggleWorkflowSelection(workflow.id);
                    } else {
                      selectWorkflow(workflow);
                    }
                  }}
                  className={`w-full text-left p-3 rounded-lg transition-colors flex items-start gap-3 ${
                    isWorkflowSelectionMode && selectedWorkflowIds.has(workflow.id)
                      ? "bg-red-500/20 border border-red-500/50"
                      : state.workflow.id === workflow.id
                        ? "bg-neutral-700"
                        : "hover:bg-neutral-800"
                  } ${isWorkflowSelectionMode ? "border" : ""} ${
                    isWorkflowSelectionMode && !selectedWorkflowIds.has(workflow.id)
                      ? "border-transparent"
                      : ""
                  }`}
                >
                  {/* Checkbox in selection mode */}
                  {isWorkflowSelectionMode && (
                    <div
                      className={`flex-shrink-0 w-5 h-5 mt-0.5 rounded border-2 flex items-center justify-center transition-colors ${
                        selectedWorkflowIds.has(workflow.id)
                          ? "bg-red-500 border-red-500"
                          : "border-neutral-500"
                      }`}
                    >
                      {selectedWorkflowIds.has(workflow.id) && (
                        <Check className="w-3 h-3 text-white" />
                      )}
                    </div>
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-sm truncate">{workflow.name}</div>
                    {workflow.description && (
                      <div className="text-xs text-neutral-400 truncate mt-0.5">
                        {workflow.description}
                      </div>
                    )}
                    <div className="flex items-center gap-2 mt-1.5">
                      <span className="text-xs text-neutral-500">
                        {getStepCount(workflow)} steps
                      </span>
                      {workflow.category && (
                        <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                          {workflow.category}
                        </span>
                      )}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-zinc-900">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-zinc-700">
          <div className="flex items-center gap-4">
            <h1 className="text-lg font-semibold text-zinc-100">
              {state.workflow.name || "New Workflow"}
              {hasUnsavedChanges && <span className="text-zinc-500 ml-2">*</span>}
            </h1>
          </div>

          <div className="flex items-center gap-2">
            <button
              data-tutorial-id="workflow-settings"
              data-ui-id="workflow-builder-settings-btn"
              onClick={() => setShowSettings(!showSettings)}
              className={`
                flex items-center gap-2 px-3 py-1.5 rounded-md transition-colors
                ${showSettings ? "bg-zinc-700 text-zinc-200" : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"}
              `}
              title="Settings"
            >
              <Settings className="w-4 h-4" />
              <span className="text-sm">Settings</span>
            </button>

            <button
              data-tutorial-id="save-workflow-button"
              data-ui-id="workflow-builder-save-btn"
              onClick={handleSave}
              disabled={state.isSaving || (!hasUnsavedChanges && !!state.originalWorkflow)}
              className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              title="Save workflow"
            >
              {state.isSaving ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Save className="w-4 h-4" />
              )}
              <span className="text-sm">{state.isSaving ? "Saving..." : "Save"}</span>
            </button>

            <button
              onClick={handleExportWorkflow}
              disabled={!state.workflow.id || isExportingWorkflow}
              className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              title="Export workflow to file"
              data-ui-id="workflow-builder-export-btn"
            >
              {isExportingWorkflow ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Download className="w-4 h-4" />
              )}
              <span className="text-sm">Export</span>
            </button>

            {isExecuting ? (
              <button
                onClick={handleStop}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-white transition-colors"
                title="Stop execution"
                data-ui-id="workflow-builder-stop-btn"
              >
                <Square className="w-4 h-4" />
                <span className="text-sm">Stop</span>
              </button>
            ) : (
              <button
                onClick={handleRun}
                disabled={isEmpty}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-blue-600 hover:bg-blue-500 text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                title="Run workflow"
                data-ui-id="workflow-builder-run-btn"
              >
                <Play className="w-4 h-4" />
                <span className="text-sm">Run</span>
              </button>
            )}
          </div>
        </div>

        {/* Status display */}
        {isExecuting && (
          <div className="px-4 py-2 bg-blue-500/20 border-b border-blue-500/30 text-blue-400 text-sm flex items-center gap-2">
            <Loader2 className="w-4 h-4 animate-spin" />
            Running workflow: {state.workflow.name}...
          </div>
        )}
        {executionSuccess && !isExecuting && (
          <div className="px-4 py-2 bg-green-500/20 border-b border-green-500/30 text-green-400 text-sm">
            {executionSuccess}
          </div>
        )}
        {(state.error || executionError) && !isExecuting && (
          <div className="px-4 py-2 bg-red-500/20 border-b border-red-500/30 text-red-400 text-sm flex items-center justify-between gap-2">
            <span className="flex-1">{state.error || executionError}</span>
            <button
              onClick={() => setExecutionError(null)}
              className="text-red-400 hover:text-red-300 p-1"
              title="Dismiss"
            >
              ✕
            </button>
          </div>
        )}

        {/* Settings Panel (collapsible) */}
        {showSettings && <SettingsPanel nameInputRef={nameInputRef} />}

        {/* Main Content with Split Layout */}
        <div className="flex-1 flex overflow-hidden">
          {/* Left: Workflow Steps */}
          <div
            className={`flex-1 overflow-y-auto p-4 ${selectedStep ? "border-r border-zinc-700" : ""}`}
          >
            {isEmpty ? (
              <EmptyState onAddStep={handleAddStep} />
            ) : (
              <div className="space-y-4 max-w-3xl mx-auto">
                {/* Setup Phase */}
                <PhaseSection
                  phase="setup"
                  steps={state.workflow.setup_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "setup",
                      state.workflow.setup_steps,
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Verification Phase */}
                <PhaseSection
                  phase="verification"
                  steps={state.workflow.verification_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "verification",
                      state.workflow.verification_steps,
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Agentic Phase */}
                <PhaseSection
                  phase="agentic"
                  steps={state.workflow.agentic_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "agentic",
                      state.workflow.agentic_steps,
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Completion Phase */}
                <PhaseSection
                  phase="completion"
                  steps={state.workflow.completion_steps ?? []}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "completion",
                      state.workflow.completion_steps ?? [],
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Global Add Step Button */}
                <div className="relative flex justify-center pt-4">
                  <AddStepButton onClick={() => handleAddStep("setup")} />
                </div>
              </div>
            )}

            {/* Add Step Dropdown */}
            {dropdownOpen && (
              <div className="fixed inset-0 z-40" onClick={() => setDropdownOpen(false)}>
                <div
                  className="absolute top-1/2 left-1/2 transform -translate-x-1/2 -translate-y-1/2"
                  onClick={(e) => e.stopPropagation()}
                >
                  <AddStepDropdown
                    filterPhase={dropdownPhase ?? undefined}
                    onAddStep={handleStepAdded}
                    isOpen={dropdownOpen}
                    onClose={() => setDropdownOpen(false)}
                    onOpenPromptLibrary={handleOpenPromptLibrary}
                    onOpenShellCommandLibrary={handleOpenShellCommandLibrary}
                    onOpenCheckLibrary={handleOpenCheckLibrary}
                    onOpenCheckGroupLibrary={handleOpenCheckGroupLibrary}
                    onOpenMacroLibrary={handleOpenMacroLibrary}
                    onOpenPlaywrightScriptLibrary={handleOpenPlaywrightScriptLibrary}
                    onOpenTestLibrary={handleOpenTestLibrary}
                    onOpenWorkflowLibrary={handleOpenWorkflowLibrary}
                    onOpenStateLibrary={handleOpenStateLibrary}
                    onOpenMcpServerToolPicker={handleOpenMcpServerToolPicker}
                  />
                </div>
              </div>
            )}

            {/* Prompt Library Picker */}
            <PromptLibraryPicker
              isOpen={promptPickerOpen}
              onClose={() => setPromptPickerOpen(false)}
              onSelect={handlePromptSelected}
              phase={promptPickerPhase}
            />

            {/* Shell Command Library Picker */}
            <ShellCommandLibraryPicker
              isOpen={shellCommandPickerOpen}
              onClose={() => setShellCommandPickerOpen(false)}
              onSelect={handleShellCommandSelected}
              phase={shellCommandPickerPhase}
            />

            {/* Check Library Picker */}
            <CheckLibraryPicker
              isOpen={checkPickerOpen}
              onClose={() => setCheckPickerOpen(false)}
              onSelect={handleCheckSelected}
              phase={checkPickerPhase}
            />

            {/* Test Library Picker */}
            <TestLibraryPicker
              isOpen={testPickerOpen}
              onClose={() => setTestPickerOpen(false)}
              onSelect={handleTestSelected}
              phase={testPickerPhase}
            />

            {/* Check Group Library Picker */}
            <CheckGroupLibraryPicker
              isOpen={checkGroupPickerOpen}
              onClose={() => setCheckGroupPickerOpen(false)}
              onSelect={handleCheckGroupSelected}
              phase={checkGroupPickerPhase}
            />

            {/* Macro Library Picker */}
            <MacroLibraryPicker
              isOpen={macroPickerOpen}
              onClose={() => setMacroPickerOpen(false)}
              onSelect={handleMacroSelected}
              phase={macroPickerPhase}
            />

            {/* Playwright Script Library Picker */}
            <PlaywrightScriptLibraryPicker
              isOpen={playwrightScriptPickerOpen}
              onClose={() => setPlaywrightScriptPickerOpen(false)}
              onSelect={handlePlaywrightScriptSelected}
              phase={playwrightScriptPickerPhase}
            />

            {/* Workflow Library Picker */}
            <WorkflowLibraryPicker
              isOpen={workflowPickerOpen}
              onClose={() => setWorkflowPickerOpen(false)}
              onSelect={handleWorkflowSelected}
              phase={workflowPickerPhase}
              excludeWorkflowId={state.workflow.id}
            />

            {/* State Library Picker */}
            <StateLibraryPicker
              isOpen={statePickerOpen}
              onClose={() => setStatePickerOpen(false)}
              onSelect={handleStateSelected}
              phase={statePickerPhase}
            />

            {/* MCP Server Tool Picker */}
            <McpServerToolPicker
              isOpen={mcpPickerOpen}
              onClose={() => setMcpPickerOpen(false)}
              onSelect={handleMcpToolSelected}
              phase={mcpPickerPhase}
            />

            {/* AI Generate Workflow Modal */}
            <AiGenerateWorkflowModal
              isOpen={aiGenerateModalOpen}
              onClose={() => setAiGenerateModalOpen(false)}
              onWorkflowGenerated={handleAiWorkflowGenerated}
            />

            {/* Generate from State Machine Modal */}
            <GenerateFromStatesModal
              isOpen={generateFromStatesModalOpen}
              onClose={() => setGenerateFromStatesModalOpen(false)}
              onWorkflowGenerated={handleAiWorkflowGenerated}
            />

            {/* Add Steps from State Modal */}
            <AddStateStepsModal
              isOpen={addStateStepsModalOpen}
              onClose={() => setAddStateStepsModalOpen(false)}
              addStep={addStep}
              onStepsAdded={(result) => {
                console.log(
                  "[WorkflowBuilder] Added steps from state:",
                  result.verificationSteps.length,
                  "verification,",
                  result.agenticStep ? "1 agentic" : "0 agentic",
                );
              }}
            />

            {/* Live Browser Panel */}
            <LiveBrowserPanel
              isOpen={liveBrowserPanelOpen}
              onClose={() => setLiveBrowserPanelOpen(false)}
              addStep={addStep}
              onStepsAdded={(result) => {
                console.log(
                  "[WorkflowBuilder] Added steps from live browser:",
                  result.verificationSteps.length,
                  "verification,",
                  result.agenticStep ? "1 agentic" : "0 agentic",
                );
              }}
            />

            {/* Batch Delete Workflows Dialog */}
            <BatchDeleteDialog
              open={showBatchDeleteDialog}
              title="Delete Workflows"
              itemType="workflow"
              itemNames={getSelectedWorkflowNames()}
              isDeleting={isDeletingWorkflows}
              onClose={() => setShowBatchDeleteDialog(false)}
              onConfirm={deleteSelectedWorkflows}
            />

            {/* Save Before Run Confirmation Dialog */}
            <ConfirmDialog
              open={showSaveBeforeRunDialog}
              title="Save Required"
              message="To run a workflow, you need to save it first."
              description="Would you like to save the workflow now and then run it?"
              variant="info"
              confirmText="Save & Run"
              cancelText="Cancel"
              isLoading={isSavingBeforeRun}
              onClose={() => setShowSaveBeforeRunDialog(false)}
              onConfirm={handleSaveAndRun}
            />
          </div>

          {/* Right: Step Configuration Panel */}
          {selectedStep && (
            <div className="w-96 flex-shrink-0">
              <StepConfigPanel onClose={() => selectStep(null)} />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Inner Content Wrapper (handles loading workflow by ID)
// =============================================================================

function WorkflowBuilderInner({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const { loadWorkflow, state } = useWorkflowBuilder();

  // Load workflow if editWorkflowId is provided
  useEffect(() => {
    if (editWorkflowId && !state.isLoading) {
      loadWorkflow(editWorkflowId);
    }
  }, [editWorkflowId, loadWorkflow]); // eslint-disable-line react-hooks/exhaustive-deps

  if (state.isLoading) {
    return (
      <div className="flex items-center justify-center h-full bg-zinc-900">
        <div className="flex flex-col items-center gap-3">
          <Loader2 className="w-8 h-8 animate-spin text-zinc-400" />
          <span className="text-zinc-400">Loading workflow...</span>
        </div>
      </div>
    );
  }

  return (
    <WorkflowBuilderContent onOpenLibrary={onOpenLibrary} onNavigateToActive={onNavigateToActive} />
  );
}

// =============================================================================
// Main Export
// =============================================================================

interface WorkflowBuilderTabProps {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}

export function WorkflowBuilderTab({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: WorkflowBuilderTabProps) {
  return (
    <div data-tutorial-id="workflow-builder-nav" className="h-full">
      <WorkflowBuilderProvider>
        <WorkflowBuilderInner
          editWorkflowId={editWorkflowId}
          onOpenLibrary={onOpenLibrary}
          onNavigateToActive={onNavigateToActive}
        />
      </WorkflowBuilderProvider>
    </div>
  );
}

export default WorkflowBuilderTab;
