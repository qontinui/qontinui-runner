import React, { useState, useCallback, useEffect, useRef } from "react";
import {
  Plus,
  Sparkles,
  Info,
  Trash2,
  ChevronDown,
  ChevronUp,
  AlertCircle,
  Pencil,
} from "lucide-react";
import type {
  LogSourceSelection,
  HealthCheckUrl,
  ModelOverrideConfig,
  ModelOverrides,
} from "../../types";
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
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PipelineConfigPanel } from "./PipelineConfigPanel";
import { PromptTemplateEditor } from "./PromptTemplateEditor";
import { ContextManagement } from "./ContextManagement";
import { ConstraintOverridesEditor } from "./ConstraintOverridesEditor";
import { instanceStorage } from "@/lib/instance-storage";

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
                <span className="text-xs text-zinc-400 w-28 shrink-0 truncate" title={phase.label}>
                  {phase.label}
                </span>
                <select
                  value={provider}
                  onChange={(e) => {
                    updatePhaseOverride(phase.key, "provider", e.target.value);
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

      {isExpanded && (
        <div className="p-3 pt-0 space-y-3 border-t border-zinc-700">
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

interface SettingsPanelProps {
  nameInputRef?: React.RefObject<HTMLInputElement | null>;
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

  function renderBooleanSetting(def: BooleanSettingDef) {
    const displayValue = getBooleanDisplayValue(def, (workflow as never)[def.key]);

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
          {booleans.map(renderSetting)}
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

    return <React.Fragment key={section.id}>{section.settings.map(renderSetting)}</React.Fragment>;
  }

  return (
    <div className="p-4 border-t border-zinc-700 space-y-4">
      {visibleSections.map(renderSection)}
    </div>
  );
}

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

export { SettingsPanel, EditableWorkflowTitle };
export type { SettingsPanelProps };
