/**
 * WorkflowBuilderTab.tsx
 *
 * Main component for the unified Workflow Builder.
 * Organizes steps into three phases: Setup, Verification, Agentic.
 */

import React, { useState, useCallback, useEffect, useMemo, useRef } from "react";
import {
  Plus,
  Save,
  Play,
  Square,
  Settings,
  Loader2,
  Search,
  Sparkles,
  Info,
  Trash2,
  Check,
  Download,
  Upload,
  ChevronDown,
  ChevronUp,
  AlertCircle,
  Terminal,
  Puzzle,
  Layers,
  Zap,
  Bot,
  TestTube2,
  Monitor,
  Rocket,
  Globe,
  ShieldCheck,
  CheckCircle2,
  Activity,
  ScanSearch,
  HeartPulse,
  MoreHorizontal,
  ExternalLink,
  PanelLeft,
  Star,
  Pencil,
} from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type {
  WorkflowPhase,
  UnifiedStep,
  UnifiedWorkflow,
  WorkflowExport,
  WorkflowStep,
  SavedPrompt,
  SavedShellCommand,
  LogSourceSelection,
  HealthCheckUrl,
  ModelOverrideConfig,
  ModelOverrides,
} from "../../types";
import { generateStepId } from "../../types";
import { getTotalStepCount } from "../../types/unified-workflow";
import { useGlobalLogSources } from "../../hooks/useGlobalLogSources";
import {
  type SettingDef,
  type BooleanSettingDef,
  type NumberSettingDef,
  type SelectSettingDef,
  type CustomSettingDef,
  type SettingsSection,
  WORKFLOW_SETTINGS_CONFIG,
  PROVIDER_OPTIONS,
  MODELS_BY_PROVIDER,
  MODEL_OVERRIDE_PHASES,
  MODEL_PRESETS,
  detectPreset,
  getVisibleSections,
  getBooleanDisplayValue,
  toBooleanStoredValue,
  getLogSourceValue as _getLogSourceValue,
  parseLogSourceValue as _parseLogSourceValue,
  resolveModelForPhase,
} from "@qontinui/workflow-utils";
import { WorkflowBuilderProvider, useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PhaseSection } from "./PhaseSection";
import { StepItem } from "./StepItem";
import { AddStepDropdown, AddStepButton } from "./AddStepDropdown";
import { StepConfigPanel } from "./StepConfigPanel";
import { PromptLibraryPicker } from "./PromptLibraryPicker";
import { ShellCommandLibraryPicker } from "./ShellCommandLibraryPicker";
import { PipelineConfigPanel } from "./PipelineConfigPanel";
import { CheckLibraryPicker } from "./CheckLibraryPicker";
import { CheckGroupLibraryPicker } from "./CheckGroupLibraryPicker";
import { TestLibraryPicker } from "./TestLibraryPicker";
import { McpServerToolPicker } from "./McpServerToolPicker";
import { WorkflowLibraryPicker } from "./WorkflowLibraryPicker";
import { AiGenerateWorkflowModal } from "./AiGenerateWorkflowModal";
import { AiGeneratePanel } from "./AiGeneratePanel";
import { StageSelector } from "./StageSelector";
import { PromptTemplateEditor } from "./PromptTemplateEditor";
import { ContextManagement } from "./ContextManagement";
import { ConstraintOverridesEditor } from "./ConstraintOverridesEditor";
import { ConstraintsPanel } from "./constraints";
import { PageTutorialMenu } from "../tutorial";
import { getAccentColors } from "@/design-system";
import { BatchDeleteDialog, ConfirmDialog } from "../ui";
import { registerUserSkills } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import { CompositionBuilderDialog } from "./CompositionBuilderDialog";
import { SkillExportDialog } from "./SkillExportDialog";
import { SkillImportDialog } from "./SkillImportDialog";
import { RunOptionsDialog } from "./RunOptionsDialog";
import { createLogger } from "@/lib/logger";

const log = createLogger("WorkflowBuilder");

// Minimal icon resolver for composition builder (maps icon IDs to lucide components)
const SKILL_ICON_MAP: Record<string, React.ComponentType<{ className?: string }>> = {
  terminal: Terminal,
  puzzle: Puzzle,
  layers: Layers,
  zap: Zap,
  bot: Bot,
  "test-tube-2": TestTube2,
  monitor: Monitor,
  rocket: Rocket,
  globe: Globe,
  "shield-check": ShieldCheck,
  "check-circle-2": CheckCircle2,
  activity: Activity,
  "scan-search": ScanSearch,
  "heart-pulse": HeartPulse,
  sparkles: Sparkles,
};
function resolveSkillIcon(iconId: string): React.ComponentType<{ className?: string }> {
  return SKILL_ICON_MAP[iconId] || Puzzle;
}

// EmptyState removed — replaced by AiGeneratePanel as the default empty view

// =============================================================================
// Editable Workflow Title
// =============================================================================

function EditableWorkflowTitle({
  name,
  onChange,
  hasUnsavedChanges,
}: {
  name: string;
  onChange: (name: string) => void;
  hasUnsavedChanges: boolean;
}) {
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState(name);
  const inputRef = useRef<HTMLInputElement>(null);

  const displayName = name || "New Workflow";

  const startEditing = useCallback(() => {
    setEditValue(name);
    setIsEditing(true);
  }, [name]);

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  const commitEdit = useCallback(() => {
    setIsEditing(false);
    const trimmed = editValue.trim();
    if (trimmed !== name) {
      onChange(trimmed);
    }
  }, [editValue, name, onChange]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        commitEdit();
      } else if (e.key === "Escape") {
        setIsEditing(false);
        setEditValue(name);
      }
    },
    [commitEdit, name],
  );

  if (isEditing) {
    return (
      <div className="flex items-center gap-1">
        <input
          ref={inputRef}
          type="text"
          value={editValue}
          onChange={(e) => setEditValue(e.target.value)}
          onBlur={commitEdit}
          onKeyDown={handleKeyDown}
          placeholder="Workflow name..."
          className="text-lg font-semibold text-zinc-100 bg-transparent border-b border-zinc-500 focus:border-blue-500 outline-hidden px-0 py-0 min-w-[120px]"
        />
        {hasUnsavedChanges && <span className="text-zinc-500 ml-1">*</span>}
      </div>
    );
  }

  return (
    <button
      onClick={startEditing}
      className="flex items-center gap-2 group cursor-text text-left"
      title="Click to rename workflow"
    >
      <h1 className="text-lg font-semibold text-zinc-100">
        {displayName}
        {hasUnsavedChanges && <span className="text-zinc-500 ml-2">*</span>}
      </h1>
      <Pencil className="w-3.5 h-3.5 text-zinc-500 opacity-0 group-hover:opacity-100 transition-opacity" />
    </button>
  );
}

// =============================================================================
// Settings Panel Component (config-driven via @qontinui/workflow-utils)
// =============================================================================

interface SettingsPanelProps {
  nameInputRef?: React.RefObject<HTMLInputElement | null>;
}

/** Toggle switch component for the runner settings panel */
function ToggleSwitch({
  checked,
  onChange,
  dataUiId: _dataUiId,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  dataUiId?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`
        relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 border-transparent
        transition-colors duration-200 ease-in-out focus:outline-hidden focus:ring-2 focus:ring-blue-500/50
        ${checked ? "bg-blue-600" : "bg-zinc-600"}
      `}
    >
      <span
        className={`
          pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
          transition duration-200 ease-in-out
          ${checked ? "translate-x-5" : "translate-x-0"}
        `}
      />
    </button>
  );
}

function SettingsPanel({ nameInputRef }: SettingsPanelProps) {
  const { state, updateWorkflow, features } = useWorkflowBuilder();
  const { workflow } = state;
  const { settings: logSourceSettings } = useGlobalLogSources();

  const visibleSections = getVisibleSections(WORKFLOW_SETTINGS_CONFIG, features);

  const inputClass =
    "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50";
  const selectClass =
    "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50";

  // ─── Setting Renderers ──────────────────────────────────────────────

  function renderBooleanSetting(def: BooleanSettingDef) {
    const displayValue = getBooleanDisplayValue(def, (workflow as never)[def.key]);

    // Use toggle switch style for most boolean settings (runner uses switches)
    if (
      def.key === "skip_ai_summary" ||
      def.key === "health_check_enabled" ||
      def.key === "log_watch_enabled"
    ) {
      return (
        <div
          key={def.key}
          className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md"
        >
          <div>
            <label className="block text-sm font-medium text-zinc-300">{def.label}</label>
            {def.tooltip && <p className="text-xs text-zinc-500">{def.tooltip}</p>}
          </div>
          <ToggleSwitch
            checked={displayValue}
            onChange={(v) => updateWorkflow({ [def.key]: toBooleanStoredValue(def, v) })}
            dataUiId={`workflow-builder-${def.key.replace(/_/g, "-")}-toggle`}
          />
        </div>
      );
    }

    // Checkbox style for reflection mode, stop_on_failure
    return (
      <label key={def.key} className="flex items-center gap-2 text-sm text-zinc-400">
        <input
          type="checkbox"
          checked={displayValue}
          onChange={(e) =>
            updateWorkflow({ [def.key]: toBooleanStoredValue(def, e.target.checked) })
          }
          className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
        />
        {def.label}
        {def.tooltip && (
          <span className="relative group">
            <svg
              className="w-3.5 h-3.5 text-zinc-500 cursor-help"
              fill="none"
              viewBox="0 0 24 24"
              stroke="currentColor"
              strokeWidth={2}
            >
              <circle cx="12" cy="12" r="10" />
              <path d="M12 16v-4M12 8h.01" />
            </svg>
            <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-2 hidden group-hover:block w-64 p-2.5 bg-zinc-700 border border-zinc-600 rounded-lg text-[11px] text-zinc-300 leading-relaxed z-50 shadow-lg pointer-events-none">
              {def.tooltip}
            </span>
          </span>
        )}
      </label>
    );
  }

  function renderNumberSetting(def: NumberSettingDef) {
    if (def.key === "timeout_seconds") {
      // Special timeout rendering with enable/disable checkbox
      return (
        <div key={def.key}>
          <label className="block text-sm font-medium text-zinc-400 mb-1">AI Session Timeout</label>
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="timeout-enabled"
              checked={workflow.timeout_seconds != null}
              onChange={(e) => {
                updateWorkflow({ timeout_seconds: e.target.checked ? 300 : null });
              }}
              className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="timeout-enabled" className="text-sm text-zinc-400">
              Enable timeout
            </label>
            {workflow.timeout_seconds != null && (
              <>
                <input
                  type="number"
                  value={workflow.timeout_seconds}
                  onChange={(e) =>
                    updateWorkflow({
                      timeout_seconds: Math.max(def.min ?? 60, parseInt(e.target.value) || 300),
                    })
                  }
                  min={def.min}
                  max={def.max}
                  className="w-24 px-2 py-1 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 text-sm"
                />
                <span className="text-sm text-zinc-500">seconds</span>
              </>
            )}
          </div>
          <p className="text-xs text-zinc-500 mt-1">
            {workflow.timeout_seconds != null
              ? `Kill AI session after ${workflow.timeout_seconds}s of inactivity`
              : "No timeout - runs until completion or manual stop (recommended)"}
          </p>
        </div>
      );
    }

    return (
      <div key={def.key}>
        <label className="block text-sm font-medium text-zinc-400 mb-1">{def.label}</label>
        <input
          type="number"
          value={(workflow as never)[def.key] ?? def.defaultValue ?? ""}
          onChange={(e) =>
            updateWorkflow({ [def.key]: parseInt(e.target.value) || def.defaultValue || 10 })
          }
          min={def.min}
          max={def.max}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        />
        {def.description && <p className="text-xs text-zinc-500 mt-1">{def.description}</p>}
      </div>
    );
  }

  function renderCustomSetting(def: CustomSettingDef) {
    switch (def.customType) {
      case "name_input":
        return (
          <div key={def.key}>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Name</label>
            <input
              ref={nameInputRef}
              type="text"
              value={workflow.name}
              onChange={(e) => updateWorkflow({ name: e.target.value })}
              placeholder="Workflow name..."
              className={inputClass}
            />
          </div>
        );

      case "description_input":
        return (
          <div key={def.key}>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Description</label>
            <textarea
              value={workflow.description}
              onChange={(e) => updateWorkflow({ description: e.target.value })}
              placeholder="What does this workflow do?"
              rows={2}
              className={`${inputClass} resize-none`}
            />
          </div>
        );

      case "pipeline_config":
        if (workflow.workflow_architecture !== "multi_agent_pipeline") return null;
        return (
          <PipelineConfigPanel
            key={def.key}
            config={workflow.multi_agent_pipeline_config as Record<string, unknown> | undefined}
            onChange={(c) => updateWorkflow({ multi_agent_pipeline_config: c })}
          />
        );

      case "model_select": {
        if (!workflow.provider || !MODELS_BY_PROVIDER[workflow.provider]) return null;
        // Rendered inline with provider in the ai section
        return null;
      }

      case "log_source_select":
        return (
          <div key={def.key}>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Log Sources</label>
            <select
              value={_getLogSourceValue(
                workflow.log_source_selection as LogSourceSelection | undefined,
              )}
              onChange={(e) => {
                const parsed = _parseLogSourceValue(e.target.value);
                updateWorkflow({ log_source_selection: parsed ?? "default" });
              }}
              className={selectClass}
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
        );

      case "health_check_urls": {
        if (!(workflow.health_check_enabled ?? true)) return null;
        return (
          <div key={def.key} className="pl-3 space-y-2">
            <div className="text-xs text-zinc-500">
              {(workflow.health_check_urls ?? []).length === 0
                ? "No health check URLs configured. Add URLs to check server availability."
                : `${(workflow.health_check_urls ?? []).length} health check URL(s) configured`}
            </div>
            {(workflow.health_check_urls ?? []).map((hc, index) => (
              <HealthCheckUrlEditor
                key={`hc-${hc.url ?? ""}-${index}`}
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
        );
      }

      case "prompt_template":
        return (
          <PromptTemplateEditor
            key={def.key}
            workflowTemplate={workflow.prompt_template}
            onWorkflowTemplateChange={(template) => updateWorkflow({ prompt_template: template })}
            hasAgenticSteps={true}
          />
        );

      case "context_management":
        return <ContextManagement key={def.key} />;

      case "constraint_overrides":
        return <ConstraintOverridesEditor key={def.key} />;

      case "per_phase_model_select":
        return <PerPhaseModelSelect key={def.key} />;

      case "resolved_model_preview":
        return <ResolvedModelPreview key={def.key} />;

      case "category_input":
        return (
          <div key={def.key}>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Category</label>
            <input
              type="text"
              value={workflow.category}
              onChange={(e) => updateWorkflow({ category: e.target.value })}
              placeholder="general"
              className={inputClass}
            />
          </div>
        );

      case "tags_input":
        return (
          <div key={def.key}>
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
              className={inputClass}
            />
          </div>
        );

      default:
        return null;
    }
  }

  function renderSelectSetting(def: SelectSettingDef) {
    return (
      <div key={def.key}>
        <label className="block text-sm font-medium text-zinc-400 mb-1">{def.label}</label>
        <select
          value={((workflow as never)[def.key] as string) ?? def.defaultValue}
          onChange={(e) =>
            updateWorkflow({
              [def.key]: e.target.value || undefined,
              ...(def.key === "provider" ? { model: undefined } : {}),
            })
          }
          className={selectClass}
        >
          {def.options.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
        {def.description && <p className="text-xs text-zinc-500 mt-1">{def.description}</p>}
      </div>
    );
  }

  // ─── Setting Dispatch ───────────────────────────────────────────────

  function renderSetting(def: SettingDef) {
    switch (def.type) {
      case "boolean":
        return renderBooleanSetting(def);
      case "number":
        return renderNumberSetting(def);
      case "select":
        return renderSelectSetting(def);
      case "custom":
        return renderCustomSetting(def);
    }
  }

  // ─── Section Layout ─────────────────────────────────────────────────

  function renderSection(section: SettingsSection) {
    if (section.id === "identity") {
      return (
        <React.Fragment key={section.id}>{section.settings.map(renderSetting)}</React.Fragment>
      );
    }

    if (section.id === "metadata") {
      return (
        <div key={section.id} className="grid grid-cols-2 gap-4">
          {section.settings.map(renderSetting)}
        </div>
      );
    }

    if (section.id === "iteration") {
      return (
        <div key={section.id} className="space-y-4">
          {section.settings.map(renderSetting)}
        </div>
      );
    }

    if (section.id === "ai") {
      const providerDef = section.settings.find((s) => s.key === "provider");
      const _modelKey = section.settings.find((s) => s.key === "model");
      const booleans = section.settings.filter((s) => s.type === "boolean");
      const otherCustom = section.settings.filter(
        (s) => s.key !== "provider" && s.key !== "model" && s.type !== "boolean",
      );

      return (
        <React.Fragment key={section.id}>
          {/* Reflection mode and other booleans */}
          {booleans.map(renderSetting)}
          {/* Provider/Model override block */}
          {providerDef && (
            <div className="p-3 bg-zinc-800/50 rounded-md space-y-3">
              <div className="flex items-center gap-2">
                <Sparkles className="w-4 h-4 text-purple-400" />
                <span className="text-sm font-medium text-zinc-300">AI Provider Override</span>
                <div className="relative group">
                  <Info className="w-3.5 h-3.5 text-zinc-500 hover:text-zinc-300 cursor-help" />
                  <div className="absolute left-0 bottom-full mb-2 w-64 p-3 bg-zinc-800 border border-zinc-600 rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
                    <p className="text-xs text-zinc-300">
                      Override the default AI provider for this workflow.
                    </p>
                  </div>
                </div>
              </div>
              <div className="flex gap-3">
                <select
                  value={workflow.provider ?? ""}
                  onChange={(e) =>
                    updateWorkflow({ provider: e.target.value || undefined, model: undefined })
                  }
                  className="flex-1 px-3 py-2 bg-zinc-700 border border-zinc-600 rounded-md text-zinc-200 text-sm focus:outline-hidden focus:ring-2 focus:ring-purple-500/50"
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
                    className="flex-1 px-3 py-2 bg-zinc-700 border border-zinc-600 rounded-md text-zinc-200 text-sm focus:outline-hidden focus:ring-2 focus:ring-purple-500/50"
                  >
                    {MODELS_BY_PROVIDER[workflow.provider]!.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                )}
              </div>
            </div>
          )}
          {otherCustom.map(renderSetting)}
        </React.Fragment>
      );
    }

    // Default section rendering
    return <React.Fragment key={section.id}>{section.settings.map(renderSetting)}</React.Fragment>;
  }

  return (
    <div className="p-4 border-t border-zinc-700 space-y-4">
      {visibleSections.map(renderSection)}
    </div>
  );
}

// =============================================================================
// Resolved Model Preview Component
// =============================================================================

function ResolvedModelPreview() {
  const { state } = useWorkflowBuilder();
  const { workflow } = state;
  const overrides: ModelOverrides = (workflow.model_overrides as ModelOverrides) ?? {};

  const rows = MODEL_OVERRIDE_PHASES.map((phase) => ({
    ...phase,
    resolved: resolveModelForPhase(phase.key, overrides, workflow.model, undefined),
  }));

  const badgeClass = (source: string) => {
    switch (source) {
      case "phase":
        return "bg-purple-500/20 text-purple-400";
      case "workflow":
        return "bg-blue-500/20 text-blue-400";
      case "smart":
        return "bg-green-500/20 text-green-400";
      default:
        return "bg-zinc-500/20 text-zinc-400";
    }
  };

  return (
    <div className="bg-zinc-800/50 rounded-md p-3">
      <p className="text-xs font-medium text-zinc-300 mb-2">Effective Model Preview</p>
      <div className="space-y-1">
        {rows.map((row) => (
          <div key={row.key} className="flex items-center gap-2 text-xs">
            <span className="text-zinc-400 w-28 shrink-0 truncate">{row.label}</span>
            <span className="text-zinc-200 flex-1 truncate">{row.resolved.model}</span>
            <span
              className={`px-1.5 py-0.5 text-[10px] font-medium rounded ${badgeClass(row.resolved.source)}`}
            >
              {row.resolved.source}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

// =============================================================================
// Per-Phase Model Select Component
// =============================================================================

function PerPhaseModelSelect() {
  const { state, updateWorkflow } = useWorkflowBuilder();
  const { workflow } = state;
  const [isExpanded, setIsExpanded] = useState(false);

  const overrides: ModelOverrides = (workflow.model_overrides as ModelOverrides) ?? {};
  const hasOverrides = MODEL_OVERRIDE_PHASES.some((phase) => {
    const cfg = overrides[phase.key as keyof ModelOverrides];
    return cfg?.provider || cfg?.model;
  });

  const currentPreset = detectPreset(overrides);

  const updatePhaseOverride = (phaseKey: string, field: "provider" | "model", value: string) => {
    const current = { ...overrides };
    const phaseCfg: ModelOverrideConfig = {
      ...(current[phaseKey as keyof ModelOverrides] ?? {}),
    };
    if (value) {
      phaseCfg[field] = value;
    } else {
      delete phaseCfg[field];
    }
    // Remove phase entry if both fields are empty
    if (!phaseCfg.provider && !phaseCfg.model) {
      delete current[phaseKey as keyof ModelOverrides];
    } else {
      (current as Record<string, ModelOverrideConfig>)[phaseKey] = phaseCfg;
    }
    updateWorkflow({ model_overrides: Object.keys(current).length > 0 ? current : undefined });
  };

  const applyPreset = (presetId: string) => {
    if (presetId === "custom") return;
    const preset = MODEL_PRESETS.find((p) => p.id === presetId);
    if (preset) {
      updateWorkflow({
        model_overrides: Object.keys(preset.overrides).length > 0 ? preset.overrides : undefined,
      });
    }
  };

  const resetAll = () => {
    updateWorkflow({ model_overrides: undefined });
  };

  const copyFromLastGeneration = () => {
    const parsed = instanceStorage.getJSON<Record<string, unknown> | null>(
      "last-generation-model-overrides",
      null,
    );
    if (parsed && typeof parsed === "object") {
      updateWorkflow({ model_overrides: parsed });
    }
  };

  const hasLastGeneration = (() => {
    const parsed = instanceStorage.getJSON<Record<string, unknown> | null>(
      "last-generation-model-overrides",
      null,
    );
    return parsed && Object.keys(parsed).length > 0;
  })();

  const selectClass =
    "flex-1 px-2 py-1.5 bg-zinc-700 border border-zinc-600 rounded text-zinc-200 text-xs focus:outline-hidden focus:ring-2 focus:ring-purple-500/50";

  return (
    <div className="bg-zinc-800/50 rounded-md overflow-hidden">
      <button
        type="button"
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-center gap-2 p-3 hover:bg-zinc-700/30 transition-colors"
      >
        {isExpanded ? (
          <ChevronUp className="w-4 h-4 text-zinc-400" />
        ) : (
          <ChevronDown className="w-4 h-4 text-zinc-400" />
        )}
        <span className="text-sm font-medium text-zinc-300">Per-Phase Model Selection</span>
        {hasOverrides && (
          <span className="px-1.5 py-0.5 text-[10px] font-medium bg-purple-500/20 text-purple-400 rounded">
            {currentPreset !== "custom"
              ? (MODEL_PRESETS.find((p) => p.id === currentPreset)?.name ?? "Active")
              : "Custom"}
          </span>
        )}
      </button>
      {isExpanded && (
        <div className="px-3 pb-3 space-y-2">
          {/* Preset selector + Reset */}
          <div className="flex items-center gap-2">
            <select
              value={currentPreset}
              onChange={(e) => applyPreset(e.target.value)}
              className="flex-1 px-2 py-1.5 bg-zinc-700 border border-zinc-600 rounded text-zinc-200 text-xs focus:outline-hidden focus:ring-2 focus:ring-purple-500/50"
            >
              <option value="custom">Custom</option>
              {MODEL_PRESETS.map((preset) => (
                <option key={preset.id} value={preset.id}>
                  {preset.name} — {preset.description}
                </option>
              ))}
            </select>
            {hasLastGeneration && (
              <button
                type="button"
                onClick={copyFromLastGeneration}
                className="px-2 py-1.5 text-xs text-zinc-400 hover:text-blue-400 border border-zinc-600 rounded hover:border-blue-500/30 transition-colors"
                title="Copy model overrides from the last AI generation run"
              >
                From Generation
              </button>
            )}
            {hasOverrides && (
              <button
                type="button"
                onClick={resetAll}
                className="px-2 py-1.5 text-xs text-zinc-400 hover:text-red-400 border border-zinc-600 rounded hover:border-red-500/30 transition-colors"
              >
                Reset
              </button>
            )}
          </div>

          <p className="text-xs text-zinc-500">
            Override provider/model for individual phases. Empty = inherit from workflow-level
            setting.
          </p>
          {MODEL_OVERRIDE_PHASES.map((phase) => {
            const cfg = overrides[phase.key as keyof ModelOverrides];
            const provider = cfg?.provider ?? "";
            const model = cfg?.model ?? "";
            return (
              <div key={phase.key} className="flex items-center gap-2">
                <span
                  className="text-xs text-zinc-400 w-28 shrink-0 truncate"
                  title={phase.label}
                >
                  {phase.label}
                </span>
                <select
                  value={provider}
                  onChange={(e) => {
                    updatePhaseOverride(phase.key, "provider", e.target.value);
                    // Clear model when provider changes
                    if (e.target.value !== provider) {
                      updatePhaseOverride(phase.key, "model", "");
                    }
                  }}
                  className={selectClass}
                >
                  <option value="">Inherit</option>
                  {PROVIDER_OPTIONS.filter((p) => p.value !== "").map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
                {provider && MODELS_BY_PROVIDER[provider] ? (
                  <select
                    value={model}
                    onChange={(e) => updatePhaseOverride(phase.key, "model", e.target.value)}
                    className={selectClass}
                  >
                    {MODELS_BY_PROVIDER[provider]!.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                ) : (
                  <div className="flex-1" />
                )}
              </div>
            );
          })}
        </div>
      )}
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
        role="button"
        tabIndex={0}
        className="flex items-center gap-2 p-2 cursor-pointer hover:bg-zinc-750 transition-colors"
        onClick={() => setIsExpanded(!isExpanded)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") setIsExpanded(!isExpanded);
        }}
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
                         text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:border-blue-500"
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
                         text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:border-blue-500 font-mono"
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
                           text-zinc-200 focus:outline-hidden focus:border-blue-500"
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
                           text-zinc-200 focus:outline-hidden focus:border-blue-500"
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
                relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full border-2 border-transparent
                transition-colors duration-200 ease-in-out focus:outline-hidden focus:ring-2 focus:ring-blue-500/50
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
    updateStep,
    showAddDropdown: _showAddDropdown,
    resetToNew,
    setWorkflow,
    saveWorkflow,
    loadWorkflow,
    updateWorkflow,
    getActiveSteps,
  } = useWorkflowBuilder();

  const selectedStep = getSelectedStep();

  const [showSettings, setShowSettings] = useState(false);
  const [showConstraints, setShowConstraints] = useState(false);
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

  // MCP server tool picker state
  const [mcpPickerOpen, setMcpPickerOpen] = useState(false);
  const [mcpPickerPhase, setMcpPickerPhase] = useState<WorkflowPhase>("setup");

  // Workflow library picker state
  const [workflowPickerOpen, setWorkflowPickerOpen] = useState(false);
  const [workflowPickerPhase, setWorkflowPickerPhase] = useState<WorkflowPhase>("setup");

  // AI generate workflow modal state
  const [aiGenerateModalOpen, setAiGenerateModalOpen] = useState(false);

  // Library panel visibility (persisted to storage, default hidden)
  const [showLibrary, setShowLibrary] = useState(() => {
    return instanceStorage.getItem("qontinui-workflow-library-visible") === "true";
  });
  const toggleLibrary = useCallback(() => {
    setShowLibrary((prev) => {
      const next = !prev;
      instanceStorage.setItem("qontinui-workflow-library-visible", String(next));
      return next;
    });
  }, []);

  // Ref for focusing the name input when creating new workflow
  const nameInputRef = useRef<HTMLInputElement | null>(null);

  // Workflow library state
  const [savedWorkflows, setSavedWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [workflowsLoading, setWorkflowsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");

  // Favorites filter
  const [showFavoritesOnly, setShowFavoritesOnly] = useState(false);

  // Workflow selection mode state (for batch delete)
  const [isWorkflowSelectionMode, setIsWorkflowSelectionMode] = useState(false);
  const [selectedWorkflowIds, setSelectedWorkflowIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeletingWorkflows, setIsDeletingWorkflows] = useState(false);

  // Workflow import/export state
  const [isExportingWorkflow, setIsExportingWorkflow] = useState(false);
  const [workflowImportError, setWorkflowImportError] = useState<string | null>(null);

  // Skill import/export state
  const [isExportingSkills, setIsExportingSkills] = useState(false);
  const [isImportingSkills, setIsImportingSkills] = useState(false);
  const skillFileInputRef = useRef<HTMLInputElement>(null);
  const workflowFileInputRef = useRef<HTMLInputElement>(null);
  const [showSkillImportDialog, setShowSkillImportDialog] = useState(false);
  const [pendingImportSkills, setPendingImportSkills] = useState<unknown[] | null>(null);
  const [importConflictMode, setImportConflictMode] = useState<"skip" | "overwrite">("skip");
  const [showSkillExportDialog, setShowSkillExportDialog] = useState(false);
  const [availableSkillsForExport, setAvailableSkillsForExport] = useState<
    Array<{ id: string; name: string; source: string }>
  >([]);
  const [selectedSkillIdsForExport, setSelectedSkillIdsForExport] = useState<Set<string>>(
    new Set(),
  );
  const [showCompositionBuilder, setShowCompositionBuilder] = useState(false);

  // Handle creating a new workflow — resets to empty state which shows the AI generation panel
  const handleNewWorkflow = useCallback(() => {
    resetToNew();
  }, [resetToNew]);

  const accentColors = getAccentColors("green");

  // Fetch saved workflows
  const fetchWorkflows = useCallback(async () => {
    setWorkflowsLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/unified-workflows`);
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

  // Toggle favorite on a workflow (optimistic update with rollback)
  const toggleFavorite = useCallback(async (workflowId: string) => {
    setSavedWorkflows((prev) =>
      prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
    );
    try {
      const response = await tracedFetch(
        `${getApiBase()}/unified-workflows/${workflowId}/favorite`,
        { method: "POST" },
      );
      const result = await response.json();
      if (result.success) {
        const newState = result.data.is_favorite;
        setSavedWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: newState } : w)),
        );
      } else {
        setSavedWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle favorite:", error);
      setSavedWorkflows((prev) =>
        prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
      );
    }
  }, []);

  const hasFavorites = useMemo(() => savedWorkflows.some((w) => w.is_favorite), [savedWorkflows]);

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
        tracedFetch(`${getApiBase()}/unified-workflows/${id}`, { method: "DELETE" }),
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
      const response = await tracedFetch(
        `${getApiBase()}/unified-workflows/${state.workflow.id}/export`,
      );
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
        const response = await tracedFetch(`${getApiBase()}/unified-workflows/import`, {
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

  // Open the export dialog with a list of available skills
  const handleOpenExportDialog = useCallback(async () => {
    try {
      const res = await tracedFetch(`${getApiBase()}/skills`);
      const data = await res.json();
      const allSkills = data.data ?? data ?? [];
      const nonBuiltin = allSkills
        .filter((s: { source: string }) => s.source !== "builtin")
        .map((s: { id: string; name: string; source: string }) => ({
          id: s.id,
          name: s.name,
          source: s.source,
        }));
      if (nonBuiltin.length === 0) {
        log.debug("No user or community skills to export.");
        return;
      }
      setAvailableSkillsForExport(nonBuiltin);
      setSelectedSkillIdsForExport(new Set(nonBuiltin.map((s: { id: string }) => s.id)));
      setShowSkillExportDialog(true);
    } catch (error) {
      console.error("Failed to load skills for export:", error);
    }
  }, []);

  // Execute the export with selected skill IDs
  const handleExportSkills = useCallback(async () => {
    setIsExportingSkills(true);
    setShowSkillExportDialog(false);
    try {
      const skillIds = Array.from(selectedSkillIdsForExport);
      const response = await tracedFetch(`${getApiBase()}/skills/export`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ skill_ids: skillIds }),
      });
      const data = await response.json();
      if (data.success && data.data) {
        const exportData = data.data;
        const blob = new Blob([JSON.stringify(exportData, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        const date = new Date().toISOString().split("T")[0];
        a.download = `qontinui-skills-${date}.json`;
        a.click();
        URL.revokeObjectURL(url);
      } else {
        console.error("Failed to export skills:", data.error);
      }
    } catch (error) {
      console.error("Failed to export skills:", error);
    } finally {
      setIsExportingSkills(false);
    }
  }, [selectedSkillIdsForExport]);

  // Read the import file and show the conflict mode dialog
  const handleImportFileSelected = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      try {
        const text = await file.text();
        const data = JSON.parse(text);

        if (!data.manifest || data.manifest.content_type !== "skills" || !data.skills) {
          throw new Error("Invalid skill file format. Expected a skill export file.");
        }

        setPendingImportSkills(data.skills);
        setImportConflictMode("skip");
        setShowSkillImportDialog(true);
      } catch (error) {
        console.error("Failed to read skill file:", error);
      } finally {
        event.target.value = "";
      }
    },
    [],
  );

  // Execute the import with the chosen conflict mode
  const handleConfirmImport = useCallback(async () => {
    if (!pendingImportSkills) return;

    setShowSkillImportDialog(false);
    setIsImportingSkills(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/import`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          skills: pendingImportSkills,
          conflict_mode: importConflictMode,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        const { imported, skipped, overwritten, errors } = result.data;
        log.debug(
          `Skills imported: ${imported}, skipped: ${skipped}, overwritten: ${overwritten}`,
          errors.length > 0 ? `Errors: ${errors.join(", ")}` : "",
        );

        // Refresh the skill registry so imported skills appear immediately
        if (imported > 0 || overwritten > 0) {
          try {
            const skillsRes = await tracedFetch(`${getApiBase()}/skills`);
            const skillsData = await skillsRes.json();
            const allSkills = skillsData.data ?? skillsData ?? [];
            const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
            registerUserSkills(nonBuiltin);
          } catch {
            // Skill refresh is non-critical
          }
        }
      } else {
        console.error("Failed to import skills:", result.error);
      }
    } catch (error) {
      console.error("Failed to import skills:", error);
    } finally {
      setIsImportingSkills(false);
      setPendingImportSkills(null);
    }
  }, [pendingImportSkills, importConflictMode]);

  // === Skill Fork ===
  const _handleForkSkill = useCallback(async (skillId: string, newName?: string) => {
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/${skillId}/fork`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ new_name: newName || undefined }),
      });
      if (!response.ok) {
        const err = await response.json();
        console.error("Fork failed:", err);
        return;
      }
      // Refresh skills after fork
      const allResponse = await tracedFetch(`${getApiBase()}/skills`);
      if (allResponse.ok) {
        const allSkills = await allResponse.json();
        const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
        registerUserSkills(nonBuiltin);
      }
    } catch (err) {
      console.error("Fork error:", err);
    }
  }, []);

  // === Skill Approval ===
  const _handleApproveSkill = useCallback(async (skillId: string, status: string) => {
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/${skillId}/approve`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ status }),
      });
      if (!response.ok) {
        const err = await response.json();
        console.error("Approval failed:", err);
        return;
      }
      // Refresh skills after approval change
      const allResponse = await tracedFetch(`${getApiBase()}/skills`);
      if (allResponse.ok) {
        const allSkills = await allResponse.json();
        const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
        registerUserSkills(nonBuiltin);
      }
    } catch (err) {
      console.error("Approval error:", err);
    }
  }, []);

  // Filter workflows
  const filteredWorkflows = savedWorkflows
    .filter((w) => {
      if (showFavoritesOnly && !w.is_favorite) return false;
      if (!searchQuery) return true;
      const query = searchQuery.toLowerCase();
      return (
        w.name.toLowerCase().includes(query) ||
        w.description?.toLowerCase().includes(query) ||
        w.category?.toLowerCase().includes(query)
      );
    })
    .sort((a, b) => {
      const favA = a.is_favorite ? 1 : 0;
      const favB = b.is_favorite ? 1 : 0;
      return favB - favA;
    });

  // Select a workflow for editing
  const selectWorkflow = async (workflow: UnifiedWorkflow) => {
    await loadWorkflow(workflow.id);
  };

  // Handle save
  const handleSave = useCallback(async () => {
    const success = await saveWorkflow();
    if (success) {
      log.debug("Workflow saved successfully");
    }
  }, [saveWorkflow]);

  // Execution state
  const [isExecuting, setIsExecuting] = useState(false);
  const [executionError, setExecutionError] = useState<string | null>(null);
  const [executionSuccess, setExecutionSuccess] = useState<string | null>(null);
  const [showSaveBeforeRunDialog, setShowSaveBeforeRunDialog] = useState(false);
  const [isSavingBeforeRun, setIsSavingBeforeRun] = useState(false);
  const [showRunOptions, setShowRunOptions] = useState(false);
  const [pendingOverrides, setPendingOverrides] = useState<Record<string, unknown> | null>(null);

  // Execute the workflow run (called after save confirmation)
  const executeWorkflowRun = useCallback(
    async (workflowId: string, overrides?: Record<string, unknown>) => {
      setIsExecuting(true);
      setExecutionError(null);
      setExecutionSuccess(null);

      try {
        log.debug("Running workflow:", workflowId, state.workflow.name);

        const runUrl = `${getApiBase()}/unified-workflows/${workflowId}/run`;

        // Check for multi-architecture comparison
        const compareArchitectures = overrides?._compareArchitectures as string[] | undefined;
        if (compareArchitectures && compareArchitectures.length > 1) {
          // Launch one run per architecture
          log.debug("Comparing architectures:", compareArchitectures);
          for (const arch of compareArchitectures) {
            const archOverrides: Record<string, unknown> = {
              use_worktree: overrides?.use_worktree ?? true,
            };
            if (arch !== "traditional") {
              archOverrides.workflow_architecture = arch;
            }
            tracedFetch(runUrl, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ overrides: archOverrides, force_fresh_start: true }),
            })
              .then((response) => {
                log.debug(`${arch} run response:`, response.status);
              })
              .catch((error) => {
                console.error(`[WorkflowBuilder] ${arch} run error:`, error);
              });
            // Small delay between launches
            await new Promise((resolve) => setTimeout(resolve, 1000));
          }

          setTimeout(() => {
            onNavigateToActive?.();
          }, 100);

          setExecutionSuccess(
            `Launched ${compareArchitectures.length} architecture comparison runs! Check the Active tab.`,
          );
        } else {
          // Single run
          const body: Record<string, unknown> = {
            monitor_index: 0,
          };
          if (overrides && Object.keys(overrides).length > 0) {
            // Remove internal flags before sending
            const cleanOverrides = { ...overrides };
            delete cleanOverrides._compareArchitectures;
            if (Object.keys(cleanOverrides).length > 0) {
              body.overrides = cleanOverrides;
            }
          }

          tracedFetch(runUrl, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
          })
            .then((response) => {
              log.debug("Workflow run response:", response.status);
            })
            .catch((error) => {
              console.error("[WorkflowBuilder] Workflow run error:", error);
            });

          setTimeout(() => {
            onNavigateToActive?.();
          }, 100);

          setExecutionSuccess("Workflow started! Check the Active tab to monitor progress.");
        }
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
      await executeWorkflowRun(savedWorkflow.id, pendingOverrides ?? undefined);
    } finally {
      setIsSavingBeforeRun(false);
      setPendingOverrides(null);
    }
  }, [saveWorkflow, executeWorkflowRun, pendingOverrides]);

  // Handle run workflow (with optional overrides from RunOptionsDialog)
  const handleRun = useCallback(
    async (overrides?: Record<string, unknown>) => {
      if (isExecuting) return;

      const isComparison = !!overrides?._compareArchitectures;

      // Check if workflow has steps
      if (isEmpty) {
        setExecutionError(
          isComparison
            ? "Cannot compare architectures on an empty workflow. Use 'Generate & Run' first to create the workflow, then run a comparison."
            : "Cannot run an empty workflow. Add some steps first.",
        );
        return;
      }

      // Check if workflow is saved (must have an ID to run)
      // For comparisons, always force save first since the workflow must exist in the database
      const needsSave = !state.workflow.id || hasUnsavedChanges || isComparison;
      if (needsSave) {
        if (overrides) setPendingOverrides(overrides);
        setShowSaveBeforeRunDialog(true);
        return;
      }

      // Workflow is already saved, run it directly
      await executeWorkflowRun(state.workflow.id, overrides);
    },
    [state.workflow.id, isEmpty, hasUnsavedChanges, isExecuting, executeWorkflowRun],
  );

  // Handle loading a deterministic spec workflow into the builder
  const handleLoadSpecWorkflow = useCallback(
    (workflow: UnifiedWorkflow) => {
      setWorkflow(workflow);
      log.debug("Loaded spec workflow into builder:", workflow.name);
    },
    [setWorkflow],
  );

  // Handle save + run for a deterministic spec workflow.
  // NOTE: We save the workflow directly via the API instead of calling
  // saveWorkflow(), because saveWorkflow() closes over state.workflow which
  // would still hold the old/empty workflow at this point (setWorkflow dispatches
  // a reducer action that only takes effect on the next render).
  const handleSaveAndRunSpecWorkflow = useCallback(
    async (workflow: UnifiedWorkflow) => {
      try {
        const body = {
          name: workflow.name || "Untitled Workflow",
          description: workflow.description,
          category: workflow.category,
          tags: workflow.tags,
          setup_steps: workflow.setup_steps,
          verification_steps: workflow.verification_steps,
          agentic_steps: workflow.agentic_steps,
          completion_steps: workflow.completion_steps ?? [],
          max_iterations: workflow.max_iterations,
          provider: workflow.provider,
          model: workflow.model,
          stop_on_failure: workflow.stop_on_failure,
          reflection_mode: workflow.reflection_mode,
          use_worktree: workflow.use_worktree,
          multi_agent_mode: workflow.multi_agent_mode,
        };

        const response = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });

        const data = await response.json();
        if (!data.success || !data.data) {
          setExecutionError(data.error || "Failed to save spec workflow. Cannot run.");
          return;
        }

        const savedWorkflow = data.data as UnifiedWorkflow;
        // Update builder state with the saved workflow (now has a server-assigned ID)
        setWorkflow(savedWorkflow);
        await executeWorkflowRun(savedWorkflow.id);
      } catch (err) {
        const msg = err instanceof Error ? err.message : "Failed to save and run spec workflow";
        setExecutionError(msg);
      }
    },
    [setWorkflow, executeWorkflowRun],
  );

  // Handle stop execution
  const handleStop = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/stop-execution`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        setIsExecuting(false);
        log.debug("Execution stopped");
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
  const _handleOpenPromptLibrary = useCallback((phase: WorkflowPhase) => {
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
      };

      addStep(step, phase);
      setPromptPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the shell command library picker
  const _handleOpenShellCommandLibrary = useCallback((phase: WorkflowPhase) => {
    setShellCommandPickerPhase(phase);
    setShellCommandPickerOpen(true);
  }, []);

  // Handler for when a shell command is selected from the library
  const handleShellCommandSelected = useCallback(
    (command: SavedShellCommand, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: command.name || (phase === "setup" ? "Setup Command" : "Completion Command"),
        mode: "shell",
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
  const _handleOpenCheckLibrary = useCallback((phase: WorkflowPhase) => {
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
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: check.name,
        mode: "check",
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
      };

      addStep(step, phase);
      setCheckPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the test library picker
  const _handleOpenTestLibrary = useCallback((phase: WorkflowPhase) => {
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
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: test.name,
        mode: "test",
        test_type: testTypeMap[test.test_type] || "custom_command",
        test_id: test.id,
        timeout_seconds: test.timeout_seconds,
      };

      addStep(step, phase);
      setTestPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the check group library picker
  const _handleOpenCheckGroupLibrary = useCallback((phase: WorkflowPhase) => {
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
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: group.name,
        mode: "check_group",
        check_group_id: group.id,
      };

      addStep(step, phase);
      setCheckGroupPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the MCP server tool picker
  const _handleOpenMcpServerToolPicker = useCallback((phase: WorkflowPhase) => {
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
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: `${server.name}: ${tool.name}`,
        mode: "shell",
        command: `# MCP tool: ${tool.name} from ${server.name}`,
      };

      addStep(step, phase);
      setMcpPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the workflow library picker
  const handleOpenWorkflowPicker = useCallback((phase: WorkflowPhase) => {
    setWorkflowPickerPhase(phase);
    setWorkflowPickerOpen(true);
  }, []);

  // Handler for when a workflow is selected from the picker
  const handleWorkflowSelected = useCallback(
    (workflow: UnifiedWorkflow, phase: WorkflowPhase) => {
      // If a workflow step is already selected, update it instead of adding a new one
      const selected = getSelectedStep();
      if (selected && selected.type === "workflow") {
        const updatedStep = {
          ...selected,
          name: workflow.name,
          workflow_id: workflow.id,
          workflow_name: workflow.name,
        } as UnifiedStep;
        // Find the phase from the step itself
        const stepPhase = (selected as WorkflowStep).phase as WorkflowPhase;
        updateStep(updatedStep, stepPhase);
      } else {
        const step: WorkflowStep = {
          id: generateStepId(),
          type: "workflow",
          phase: phase as "setup" | "verification" | "completion",
          name: workflow.name,
          workflow_id: workflow.id,
          workflow_name: workflow.name,
        };
        addStep(step as UnifiedStep, phase);
      }
      setWorkflowPickerOpen(false);
    },
    [addStep, getSelectedStep, updateStep],
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
      setShowConstraints(false);
      log.debug("Loaded AI-generated workflow:", workflow.name);
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

  const getStepCount = getTotalStepCount;

  return (
    <div className="h-full flex">
      {/* Left Panel - Workflow List */}
      {showLibrary && (
        <div className="w-80 border-r border-border flex flex-col bg-card">
          {/* Header */}
          <div className="p-4 border-b border-border">
            {/* Title row with dropdown and actions */}
            <div className="flex items-center justify-between mb-3">
              <h2 className="text-lg font-semibold flex items-center gap-2">
                <Sparkles className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
                Workflows
              </h2>
              <div className="flex items-center gap-1">
                {/* Add/Import dropdown */}
                <DropdownMenu.Root>
                  <DropdownMenu.Trigger asChild>
                    <button
                      className="flex items-center justify-center w-7 h-7 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
                      title="Add or import workflow"
                    >
                      <Plus className="w-4 h-4" />
                    </button>
                  </DropdownMenu.Trigger>
                  <DropdownMenu.Portal>
                    <DropdownMenu.Content
                      className="min-w-[180px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
                      sideOffset={5}
                      align="end"
                    >
                      <DropdownMenu.Item
                        className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                        onSelect={handleNewWorkflow}
                      >
                        <Plus className="w-3.5 h-3.5 text-muted-foreground" />
                        <span className="flex-1">New Workflow</span>
                      </DropdownMenu.Item>
                      <DropdownMenu.Item
                        className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                        disabled={workflowsLoading}
                        onSelect={() => workflowFileInputRef.current?.click()}
                      >
                        <Upload className="w-3.5 h-3.5 text-muted-foreground" />
                        <span className="flex-1">Import from File</span>
                      </DropdownMenu.Item>
                      <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
                      <DropdownMenu.Item
                        className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                        asChild
                      >
                        <a
                          href="http://localhost:3001/build/workflows"
                          target="_blank"
                          rel="noopener noreferrer"
                          className="flex items-center gap-2 px-3 py-2 text-xs rounded-md no-underline text-inherit hover:bg-muted/50 transition-colors"
                        >
                          <ExternalLink className="w-3.5 h-3.5 text-indigo-400" />
                          <span className="flex-1">Open in Web</span>
                        </a>
                      </DropdownMenu.Item>
                    </DropdownMenu.Content>
                  </DropdownMenu.Portal>
                </DropdownMenu.Root>
                <input
                  ref={workflowFileInputRef}
                  type="file"
                  accept=".json"
                  className="hidden"
                  onChange={handleImportWorkflow}
                />
                {/* Batch delete toggle */}
                <button
                  onClick={() => setIsWorkflowSelectionMode(!isWorkflowSelectionMode)}
                  className={`flex items-center justify-center w-7 h-7 rounded-md transition-colors ${
                    isWorkflowSelectionMode
                      ? "bg-red-500/20 text-red-400"
                      : "text-muted-foreground hover:text-red-400 hover:bg-muted"
                  }`}
                  title={
                    isWorkflowSelectionMode ? "Cancel selection" : "Select workflows to delete"
                  }
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
                <PageTutorialMenu page="unified-workflow-builder" variant="compact" />
              </div>
            </div>

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
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
              <input
                type="text"
                placeholder="Search workflows..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="w-full pl-9 pr-3 py-2 bg-muted border border-border rounded-lg text-sm focus:outline-hidden focus:border-border"
              />
            </div>
            {/* Favorites filter */}
            {hasFavorites && (
              <button
                onClick={() => setShowFavoritesOnly((prev) => !prev)}
                className={`flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium transition-colors ${
                  showFavoritesOnly
                    ? "bg-amber-500/20 text-amber-400 border border-amber-500/50"
                    : "bg-white/5 text-muted-foreground border border-transparent hover:bg-white/10"
                }`}
              >
                <Star className={`w-3 h-3 ${showFavoritesOnly ? "fill-amber-400" : ""}`} />
                Favorites
              </button>
            )}
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
                  className="px-3 py-1 text-sm text-muted-foreground hover:text-foreground transition-colors"
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
                <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
              </div>
            ) : filteredWorkflows.length === 0 ? (
              <div className="text-center py-8 text-muted-foreground">
                <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
                <p className="text-sm">No workflows found</p>
              </div>
            ) : (
              <div className="space-y-1">
                {filteredWorkflows.map((workflow) => (
                  <button
                    key={workflow.id}
                    aria-label={`${isWorkflowSelectionMode ? "Select" : "Open"} workflow: ${workflow.name}`}
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
                          ? "bg-muted/80"
                          : "hover:bg-muted"
                    } ${isWorkflowSelectionMode ? "border" : ""} ${
                      isWorkflowSelectionMode && !selectedWorkflowIds.has(workflow.id)
                        ? "border-transparent"
                        : ""
                    }`}
                  >
                    {/* Checkbox in selection mode */}
                    {isWorkflowSelectionMode && (
                      <div
                        className={`shrink-0 w-5 h-5 mt-0.5 rounded border-2 flex items-center justify-center transition-colors ${
                          selectedWorkflowIds.has(workflow.id)
                            ? "bg-red-500 border-red-500"
                            : "border-muted-foreground"
                        }`}
                      >
                        {selectedWorkflowIds.has(workflow.id) && (
                          <Check className="w-3 h-3 text-white" />
                        )}
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-1.5">
                        {!isWorkflowSelectionMode && (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              toggleFavorite(workflow.id);
                            }}
                            className="shrink-0 p-0.5 rounded transition-colors hover:bg-white/10"
                            title={
                              workflow.is_favorite ? "Remove from favorites" : "Add to favorites"
                            }
                          >
                            <Star
                              className={`w-3.5 h-3.5 ${workflow.is_favorite ? "text-amber-400 fill-amber-400" : "text-muted-foreground"}`}
                            />
                          </button>
                        )}
                        <div className="font-medium text-sm truncate">{workflow.name}</div>
                      </div>
                      {workflow.description && (
                        <div className="text-xs text-muted-foreground truncate mt-0.5">
                          {workflow.description}
                        </div>
                      )}
                      <div className="flex items-center gap-2 mt-1.5">
                        <span className="text-xs text-muted-foreground">
                          {getStepCount(workflow)} steps
                        </span>
                        {workflow.category && (
                          <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
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
      )}

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-zinc-900">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-zinc-700">
          <div className="flex items-center gap-3">
            <button
              onClick={toggleLibrary}
              className={`p-1.5 rounded-md transition-colors ${
                showLibrary
                  ? "bg-zinc-700 text-zinc-200"
                  : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
              }`}
              title={showLibrary ? "Hide workflow library" : "Show workflow library"}
            >
              <PanelLeft className="w-4 h-4" />
            </button>
            <button
              data-tutorial-id="workflow-settings"
              onClick={() => {
                setShowSettings(!showSettings);
                if (!showSettings) setShowConstraints(false);
              }}
              className={`p-1.5 rounded-md transition-colors ${
                showSettings
                  ? "bg-zinc-700 text-zinc-200"
                  : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
              }`}
              title="Settings"
            >
              <Settings className="w-4 h-4" />
            </button>
            <button
              onClick={() => {
                setShowConstraints(!showConstraints);
                if (!showConstraints) setShowSettings(false);
              }}
              className={`p-1.5 rounded-md transition-colors ${
                showConstraints
                  ? "bg-zinc-700 text-zinc-200"
                  : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
              }`}
              title="Constraints"
            >
              <ShieldCheck className="w-4 h-4" />
            </button>
            <EditableWorkflowTitle
              name={state.workflow.name}
              onChange={(name) => updateWorkflow({ name })}
              hasUnsavedChanges={hasUnsavedChanges}
            />
          </div>

          <div className="flex items-center gap-2">
            <button
              data-tutorial-id="save-workflow-button"
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

            {/* Overflow menu for less-frequent actions */}
            <DropdownMenu.Root>
              <DropdownMenu.Trigger asChild>
                <button
                  className="flex items-center justify-center p-1.5 rounded-md text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800 transition-colors"
                  title="More actions"
                >
                  <MoreHorizontal className="w-4 h-4" />
                </button>
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <DropdownMenu.Content
                  className="min-w-[200px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
                  sideOffset={5}
                  align="end"
                >
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                    disabled={!state.workflow.id || isExportingWorkflow}
                    onSelect={handleExportWorkflow}
                  >
                    <Download className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">Export Workflow</span>
                  </DropdownMenu.Item>
                  <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                    disabled={isExportingSkills}
                    onSelect={handleOpenExportDialog}
                  >
                    <Download className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">Export Skills</span>
                  </DropdownMenu.Item>
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                    disabled={isImportingSkills}
                    onSelect={() => skillFileInputRef.current?.click()}
                  >
                    <Upload className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">Import Skills</span>
                  </DropdownMenu.Item>
                  <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                    onSelect={() => setShowCompositionBuilder(true)}
                  >
                    <Layers className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">Create Composition</span>
                  </DropdownMenu.Item>
                </DropdownMenu.Content>
              </DropdownMenu.Portal>
            </DropdownMenu.Root>
            <input
              ref={skillFileInputRef}
              type="file"
              accept=".json"
              className="hidden"
              onChange={handleImportFileSelected}
            />

            {isExecuting ? (
              <button
                onClick={handleStop}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-white transition-colors"
                title="Stop execution"
              >
                <Square className="w-4 h-4" />
                <span className="text-sm">Stop</span>
              </button>
            ) : (
              <div className="flex items-stretch">
                <button
                  onClick={() => handleRun()}
                  disabled={isEmpty}
                  className="flex items-center gap-2 px-3 py-1.5 rounded-l-md bg-blue-600 hover:bg-blue-500 text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  title={isEmpty ? "Add steps to run this workflow" : "Run workflow"}
                >
                  <Play className="w-4 h-4" />
                  <span className="text-sm">Run</span>
                </button>
                <button
                  onClick={() => setShowRunOptions(true)}
                  className="flex items-center px-2 py-1.5 rounded-r-md bg-blue-700 hover:bg-blue-600 text-white border-l border-blue-500/50 transition-colors"
                  title="Run with options (architecture, worktree)"
                >
                  <ChevronDown className="w-4 h-4" />
                </button>
              </div>
            )}
          </div>
        </div>

        {/* Dialogs (rendered outside header flow) */}
        <CompositionBuilderDialog
          isOpen={showCompositionBuilder}
          onClose={() => setShowCompositionBuilder(false)}
          resolveIcon={resolveSkillIcon}
        />
        <SkillExportDialog
          isOpen={showSkillExportDialog}
          onClose={() => setShowSkillExportDialog(false)}
          availableSkills={availableSkillsForExport}
          selectedIds={selectedSkillIdsForExport}
          onToggleId={(id) => {
            setSelectedSkillIdsForExport((prev) => {
              const next = new Set(prev);
              if (next.has(id)) next.delete(id);
              else next.add(id);
              return next;
            });
          }}
          onSelectAll={(ids) => setSelectedSkillIdsForExport(ids)}
          onExport={handleExportSkills}
        />
        <SkillImportDialog
          isOpen={showSkillImportDialog && !!pendingImportSkills}
          onClose={() => {
            setShowSkillImportDialog(false);
            setPendingImportSkills(null);
          }}
          skillCount={pendingImportSkills ? (pendingImportSkills as unknown[]).length : 0}
          conflictMode={importConflictMode}
          onSetConflictMode={setImportConflictMode}
          onConfirm={handleConfirmImport}
        />

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
            <span
              data-content-role="status"
              data-content-label="execution error"
              className="flex-1"
            >
              {state.error || executionError}
            </span>
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

        {/* Constraints Panel (collapsible) */}
        {showConstraints && <ConstraintsPanel onClose={() => setShowConstraints(false)} />}

        {/* Main Content with Split Layout */}
        <div className="flex-1 flex overflow-hidden">
          {/* Left: Workflow Steps or AI Generate Panel */}
          {isEmpty ? (
            <div className="flex-1 overflow-hidden">
              <AiGeneratePanel
                onCreateManually={() => handleAddStep("setup")}
                onNavigateToActiveRuns={() => onNavigateToActive?.()}
                onLoadSpecWorkflow={handleLoadSpecWorkflow}
                onSaveAndRunSpecWorkflow={handleSaveAndRunSpecWorkflow}
              />
            </div>
          ) : (
            <div
              className={`flex-1 overflow-y-auto p-4 ${selectedStep ? "border-r border-zinc-700" : ""}`}
            >
              <StageSelector />
              <div className="space-y-4 max-w-3xl mx-auto">
                {/* Setup Phase */}
                <PhaseSection
                  phase="setup"
                  steps={getActiveSteps("setup")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "setup",
                      getActiveSteps("setup"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Verification Phase */}
                <PhaseSection
                  phase="verification"
                  steps={getActiveSteps("verification")}
                  onAddStep={handleAddStep}
                  headerActions={null}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "verification",
                      getActiveSteps("verification"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Agentic Phase */}
                <PhaseSection
                  phase="agentic"
                  steps={getActiveSteps("agentic")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "agentic",
                      getActiveSteps("agentic"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                {/* Completion Phase */}
                <PhaseSection
                  phase="completion"
                  steps={getActiveSteps("completion")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "completion",
                      getActiveSteps("completion"),
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

              {/* MCP Server Tool Picker */}
              <McpServerToolPicker
                isOpen={mcpPickerOpen}
                onClose={() => setMcpPickerOpen(false)}
                onSelect={handleMcpToolSelected}
                phase={mcpPickerPhase}
              />

              {/* Workflow Library Picker */}
              <WorkflowLibraryPicker
                isOpen={workflowPickerOpen}
                onClose={() => setWorkflowPickerOpen(false)}
                onSelect={handleWorkflowSelected}
                phase={workflowPickerPhase}
                excludeWorkflowId={state.workflow.id}
              />

              {/* AI Generate Workflow Modal */}
              <AiGenerateWorkflowModal
                isOpen={aiGenerateModalOpen}
                onClose={() => setAiGenerateModalOpen(false)}
                onWorkflowGenerated={handleAiWorkflowGenerated}
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
            </div>
          )}

          {/* Add Step Dropdown (fixed overlay, renders regardless of empty state) */}
          {dropdownOpen && (
            <div
              className="fixed inset-0 z-40"
              role="button"
              tabIndex={0}
              aria-label="Close add step dropdown"
              onClick={() => setDropdownOpen(false)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") setDropdownOpen(false);
              }}
            >
              <div
                role="presentation"
                className="absolute top-1/2 left-1/2 transform -translate-x-1/2 -translate-y-1/2"
                onClick={(e) => e.stopPropagation()}
              >
                <AddStepDropdown
                  filterPhase={dropdownPhase ?? undefined}
                  onAddStep={handleStepAdded}
                  isOpen={dropdownOpen}
                  onClose={() => setDropdownOpen(false)}
                  onSkillUsed={async (skillId) => {
                    try {
                      await tracedFetch(`${getApiBase()}/skills/${skillId}/increment-usage`, {
                        method: "POST",
                      });
                    } catch {
                      // Non-critical, silently ignore
                    }
                  }}
                />
              </div>
            </div>
          )}

          {/* Right: Step Configuration Panel */}
          {selectedStep && (
            <div className="w-96 shrink-0">
              <StepConfigPanel
                onClose={() => selectStep(null)}
                onOpenWorkflowPicker={() => {
                  // Determine the phase of the currently selected step
                  const step = getSelectedStep();
                  if (step && "phase" in step) {
                    handleOpenWorkflowPicker(step.phase as WorkflowPhase);
                  }
                }}
              />
            </div>
          )}
        </div>

        {/* Run Options Dialog (rendered outside isEmpty ternary so it's always available) */}
        <RunOptionsDialog
          open={showRunOptions}
          onClose={() => setShowRunOptions(false)}
          onRun={(overrides) => {
            setShowRunOptions(false);
            handleRun(overrides);
          }}
          isLoading={isExecuting}
          workflowId={state.workflow.id || null}
        />

        {/* Save Before Run Dialog (rendered outside isEmpty ternary for comparison flow) */}
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
      <WorkflowBuilderProvider startEmpty={!editWorkflowId}>
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
