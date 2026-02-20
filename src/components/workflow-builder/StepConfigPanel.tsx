/**
 * StepConfigPanel.tsx
 *
 * Configuration panel for the selected workflow step.
 * Shows different options based on step type.
 *
 * Core step types: command, ui_bridge, prompt
 */

import { useState } from "react";
import { X, AlertCircle, ChevronDown, ChevronRight, Plus, Trash2 } from "lucide-react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import type {
  TestType,
  PlaywrightExecutionMode,
  CheckType,
  BaseStep,
  CommandStep,
} from "../../types/unified-workflow";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { CHECK_TOOLS, CHECK_TYPE_INFO } from "../check-builder/types";

// =============================================================================
// Helper to find step phase
// =============================================================================

function findStepPhase(
  stepId: string,
  workflow: {
    setup_steps: { id: string }[];
    verification_steps: { id: string }[];
    agentic_steps: { id: string }[];
    completion_steps?: { id: string }[];
  },
): WorkflowPhase | null {
  if (workflow.setup_steps.some((s) => s.id === stepId)) return "setup";
  if (workflow.verification_steps.some((s) => s.id === stepId)) return "verification";
  if (workflow.agentic_steps.some((s) => s.id === stepId)) return "agentic";
  if (workflow.completion_steps?.some((s) => s.id === stepId)) return "completion";
  return null;
}

// =============================================================================
// Data Flow Section (Universal for all step types)
// =============================================================================

function DataFlowSection({
  step,
  onUpdate,
  allSteps,
}: {
  step: UnifiedStep;
  onUpdate: (updates: Partial<UnifiedStep>) => void;
  allSteps: { id: string; name: string }[];
}) {
  const [isOpen, setIsOpen] = useState(false);

  // Type-safe access to data flow fields (on BaseStep)
  const baseStep = step as BaseStep;
  const inputs = (baseStep as unknown as Record<string, unknown>).inputs as
    | Record<string, string>
    | undefined;
  const extract = (baseStep as unknown as Record<string, unknown>).extract as
    | Record<string, string>
    | undefined;
  const dependsOn = (baseStep as unknown as Record<string, unknown>).depends_on as
    | string[]
    | undefined;
  const required = (baseStep as unknown as Record<string, unknown>).required as boolean | undefined;

  const inputEntries = inputs ? Object.entries(inputs) : [];
  const extractEntries = extract ? Object.entries(extract) : [];
  const dependsOnList = dependsOn || [];

  const handleAddInput = () => {
    const newInputs = { ...(inputs || {}), "": "" };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleRemoveInput = (key: string) => {
    const newInputs = { ...(inputs || {}) };
    delete newInputs[key];
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputKey = (oldKey: string, newKey: string) => {
    const newInputs: Record<string, string> = {};
    for (const [k, v] of Object.entries(inputs || {})) {
      if (k === oldKey) {
        newInputs[newKey] = v;
      } else {
        newInputs[k] = v;
      }
    }
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputValue = (key: string, value: string) => {
    const newInputs = { ...(inputs || {}), [key]: value };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleAddExtract = () => {
    const newExtract = { ...(extract || {}), "": "" };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleRemoveExtract = (key: string) => {
    const newExtract = { ...(extract || {}) };
    delete newExtract[key];
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractKey = (oldKey: string, newKey: string) => {
    const newExtract: Record<string, string> = {};
    for (const [k, v] of Object.entries(extract || {})) {
      if (k === oldKey) {
        newExtract[newKey] = v;
      } else {
        newExtract[k] = v;
      }
    }
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractValue = (key: string, value: string) => {
    const newExtract = { ...(extract || {}), [key]: value };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleToggleDependency = (stepId: string) => {
    const updated = dependsOnList.includes(stepId)
      ? dependsOnList.filter((id) => id !== stepId)
      : [...dependsOnList, stepId];
    onUpdate({ depends_on: updated } as Partial<UnifiedStep>);
  };

  // Other steps that could be dependencies (exclude self)
  const otherSteps = allSteps.filter((s) => s.id !== step.id);

  return (
    <div className="mt-4 pt-4 border-t border-zinc-700">
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-2 text-xs font-medium text-zinc-500 uppercase tracking-wider hover:text-zinc-400 transition-colors"
      >
        {isOpen ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        Data Flow
      </button>

      {isOpen && (
        <div className="mt-3 space-y-4">
          {/* Inputs (from other steps) */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Inputs (from other steps)</h4>
            {inputEntries.map(([key, value], idx) => (
              <div key={idx} className="flex gap-2 mb-2">
                <input
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateInputKey(key, e.target.value)}
                  placeholder="Variable name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-none focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateInputValue(key, e.target.value)}
                  placeholder="step_id.output_key"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveInput(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddInput}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add input
            </button>
          </div>

          {/* Extract (from this step's output) */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">
              Extract (from this step's output)
            </h4>
            {extractEntries.map(([key, value], idx) => (
              <div key={idx} className="flex gap-2 mb-2">
                <input
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateExtractKey(key, e.target.value)}
                  placeholder="Output name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-none focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateExtractValue(key, e.target.value)}
                  placeholder="$.data.result"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveExtract(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddExtract}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add extraction
            </button>
          </div>

          {/* Dependencies */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Dependencies</h4>
            {otherSteps.length === 0 ? (
              <p className="text-xs text-zinc-500 italic">No other steps available</p>
            ) : (
              <div className="space-y-1 max-h-32 overflow-y-auto border border-zinc-700 rounded-md p-2 bg-zinc-800/50">
                {otherSteps.map((s) => (
                  <label
                    key={s.id}
                    className="flex items-center gap-2 p-1 rounded hover:bg-zinc-700/50 cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={dependsOnList.includes(s.id)}
                      onChange={() => handleToggleDependency(s.id)}
                      className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                    />
                    <span className="text-sm text-zinc-300 truncate">{s.name}</span>
                  </label>
                ))}
              </div>
            )}
          </div>

          {/* Required */}
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="step_required"
              checked={required !== false}
              onChange={(e) => onUpdate({ required: e.target.checked } as Partial<UnifiedStep>)}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="step_required" className="text-sm text-zinc-300">
              Required (workflow fails if this step fails)
            </label>
          </div>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Test Fields Config (used within CommandConfig when test_type or test_id is set)
// =============================================================================

function TestFieldsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Test Type</label>
        <select
          value={step.test_type}
          onChange={(e) => onUpdate({ test_type: e.target.value as TestType })}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-step-config-test-type-select"
        >
          <option value="playwright">Playwright (Browser)</option>
          <option value="qontinui_vision">Qontinui Vision</option>
          <option value="python">Python Script</option>
          <option value="repository">Repository Test</option>
          <option value="custom_command">Custom Command</option>
        </select>
      </div>

      {/* Command for custom types */}
      {(step.test_type === "custom_command" ||
        step.test_type === "python" ||
        step.test_type === "repository") && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Command</label>
          <input
            type="text"
            value={step.command || ""}
            onChange={(e) => onUpdate({ command: e.target.value })}
            placeholder={
              step.test_type === "python"
                ? "python test_script.py"
                : step.test_type === "repository"
                  ? "npm test"
                  : "command to run"
            }
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-step-config-test-command-input"
          />
        </div>
      )}

      {/* Playwright-specific options */}
      {step.test_type === "playwright" && (
        <>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Execution Mode</label>
            <select
              value={step.execution_mode || "independent"}
              onChange={(e) =>
                onUpdate({ execution_mode: e.target.value as PlaywrightExecutionMode })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-test-execution-mode-select"
            >
              <option value="independent">Independent (fresh session)</option>
              <option value="chained">Chained (continue after previous)</option>
            </select>
            <p className="text-xs text-zinc-500 mt-1">
              Independent starts a new browser session; Chained continues from previous test
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Fused Script ID (optional)
            </label>
            <input
              type="text"
              value={step.fused_script_id || ""}
              onChange={(e) => onUpdate({ fused_script_id: e.target.value || undefined })}
              placeholder="script-uuid"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-test-fused-script-id-input"
            />
            <p className="text-xs text-zinc-500 mt-1">
              If set, this test will run after the specified setup script
            </p>
          </div>
        </>
      )}
    </div>
  );
}

// =============================================================================
// Check Fields Config (used within CommandConfig when check_type is set)
// =============================================================================

function CheckFieldsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  // Filter tools by check type
  const availableTools = CHECK_TOOLS.filter((t) => t.check_type === step.check_type);
  const selectedTool = CHECK_TOOLS.find((t) => t.tool === step.tool);
  const _checkTypeInfo = CHECK_TYPE_INFO.find((t) => t.type === step.check_type);

  const isCiCd = step.check_type === "ci_cd";

  return (
    <div className="space-y-4">
      {/* Check Type */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Check Type</label>
        <select
          value={step.check_type}
          onChange={(e) => {
            const newType = e.target.value as CheckType;
            onUpdate({ check_type: newType, tool: undefined, command: undefined });
          }}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-step-config-check-type-select"
        >
          <option value="lint">Lint (code quality)</option>
          <option value="format">Format (code style)</option>
          <option value="typecheck">Type Check (static analysis)</option>
          <option value="analyze">Analyze (code metrics)</option>
          <option value="security">Security (vulnerability scan)</option>
          <option value="custom_command">Custom Command</option>
          <option value="ci_cd">CI/CD (GitHub Actions)</option>
        </select>
      </div>

      {isCiCd ? (
        <>
          {/* Repository */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Repository</label>
            <input
              type="text"
              value={step.repository || ""}
              onChange={(e) => onUpdate({ repository: e.target.value || undefined })}
              placeholder="owner/repo"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
              data-ui-id="workflow-builder-step-config-check-cicd-repo-input"
            />
            <p className="text-xs text-zinc-500 mt-1">
              GitHub repository (e.g., jspindev/qontinui-runner). Leave blank to auto-detect from
              working directory.
            </p>
          </div>

          {/* Working Directory (shown when no explicit repo) */}
          {!step.repository && (
            <div>
              <label className="block text-sm font-medium text-zinc-400 mb-1">
                Working Directory
              </label>
              <input
                type="text"
                value={step.working_directory || ""}
                onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
                placeholder="Path to git repo root"
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                data-ui-id="workflow-builder-step-config-check-cicd-workdir-input"
              />
              <p className="text-xs text-zinc-500 mt-1">
                Git repo directory to auto-detect the GitHub repository from.
              </p>
            </div>
          )}

          {/* Workflow Name */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Workflow Name (optional)
            </label>
            <input
              type="text"
              value={step.workflow_name || ""}
              onChange={(e) => onUpdate({ workflow_name: e.target.value || undefined })}
              placeholder="CI"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-cicd-workflow-input"
            />
            <p className="text-xs text-zinc-500 mt-1">GitHub Actions workflow name to filter by.</p>
          </div>

          {/* Branch */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Branch (optional)
            </label>
            <input
              type="text"
              value={step.branch || ""}
              onChange={(e) => onUpdate({ branch: e.target.value || undefined })}
              placeholder="main"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-cicd-branch-input"
            />
          </div>

          {/* Wait for completion */}
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="wait_for_completion"
              checked={step.wait_for_completion ?? false}
              onChange={(e) => onUpdate({ wait_for_completion: e.target.checked })}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-cicd-wait-checkbox"
            />
            <label htmlFor="wait_for_completion" className="text-sm text-zinc-300">
              Wait for in-progress runs
            </label>
          </div>
          <p className="text-xs text-zinc-500 ml-6 -mt-2">
            Poll until the CI run finishes instead of failing immediately when in progress.
          </p>

          {/* Timeout */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Timeout (seconds)
            </label>
            <input
              type="number"
              value={step.timeout_seconds ?? 300}
              onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 300 })}
              min={15}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-cicd-timeout-input"
            />
          </div>
        </>
      ) : (
        <>
          {/* Tool Selection */}
          {step.check_type !== "custom_command" && availableTools.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-zinc-400 mb-1">Tool</label>
              <select
                value={step.tool || ""}
                onChange={(e) => {
                  const tool = CHECK_TOOLS.find((t) => t.tool === e.target.value);
                  onUpdate({
                    tool: e.target.value || undefined,
                    command: tool?.default_command || undefined,
                  });
                }}
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                data-ui-id="workflow-builder-step-config-check-tool-select"
              >
                <option value="">Select a tool...</option>
                {availableTools.map((tool) => (
                  <option key={tool.tool} value={tool.tool}>
                    {tool.name} - {tool.description}
                  </option>
                ))}
              </select>
              {selectedTool && (
                <p className="text-xs text-zinc-500 mt-1">
                  Language: {selectedTool.language} | Auto-fix:{" "}
                  {selectedTool.supports_auto_fix ? "Yes" : "No"}
                </p>
              )}
            </div>
          )}

          {/* Command Override */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Command {step.check_type === "custom_command" ? "(required)" : "(optional override)"}
            </label>
            <input
              type="text"
              value={step.command || ""}
              onChange={(e) => onUpdate({ command: e.target.value || undefined })}
              placeholder={selectedTool?.default_command || "Enter command..."}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
              data-ui-id="workflow-builder-step-config-check-command-input"
            />
            {selectedTool?.default_command && !step.command && (
              <p className="text-xs text-zinc-500 mt-1">
                Default:{" "}
                <code className="bg-zinc-700 px-1 rounded">{selectedTool.default_command}</code>
              </p>
            )}
          </div>

          {/* Working Directory */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Working Directory (optional)
            </label>
            <input
              type="text"
              value={step.working_directory || ""}
              onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
              placeholder="Leave empty for project root"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-working-dir-input"
            />
          </div>

          {/* Auto-fix toggle (only for tools that support it) */}
          {(selectedTool?.supports_auto_fix || step.check_type === "format") && (
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="auto_fix"
                checked={step.auto_fix ?? false}
                onChange={(e) => onUpdate({ auto_fix: e.target.checked })}
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                data-ui-id="workflow-builder-step-config-check-auto-fix-checkbox"
              />
              <label htmlFor="auto_fix" className="text-sm text-zinc-300">
                Auto-fix issues (when supported)
              </label>
            </div>
          )}

          {/* Timeout */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Timeout (seconds)
            </label>
            <input
              type="number"
              value={step.timeout_seconds ?? 60}
              onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 60 })}
              min={5}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="workflow-builder-step-config-check-timeout-input"
            />
          </div>
        </>
      )}
    </div>
  );
}

// =============================================================================
// Command Step Config (unified: shell commands, checks, check groups)
// =============================================================================

function CommandConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  // If check_group_id is set, show check group config
  if (step.check_group_id) {
    return (
      <div className="space-y-4">
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Check Group ID</label>
          <input
            type="text"
            value={step.check_group_id || ""}
            onChange={(e) => onUpdate({ check_group_id: e.target.value })}
            placeholder="check-group-uuid"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-step-config-check-group-id-input"
          />
          <p className="text-xs text-zinc-500 mt-1">
            The ID of a saved check group from the Check Builder
          </p>
        </div>

        {step.check_group_id && (
          <div className="p-2 bg-cyan-500/10 border border-cyan-500/30 rounded-md">
            <p className="text-xs text-cyan-400 font-mono">{step.check_group_id}</p>
          </div>
        )}
      </div>
    );
  }

  // If check_type is set, show check-specific fields
  if (step.check_type) {
    return <CheckFieldsConfig step={step} onUpdate={onUpdate} />;
  }

  // If test_type or test_id is set, show test-specific fields
  if (step.test_type || step.test_id) {
    return <TestFieldsConfig step={step} onUpdate={onUpdate} />;
  }

  // Default: shell command config
  return (
    <div className="space-y-4">
      {/* Command */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Command</label>
        <textarea
          value={step.command || ""}
          onChange={(e) => onUpdate({ command: e.target.value })}
          placeholder="git branch backup-$(date +%Y%m%d)"
          rows={3}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
          data-ui-id="workflow-builder-step-config-shell-command-input"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Shell command to execute (bash on Unix, PowerShell on Windows)
        </p>
      </div>

      {/* Working Directory */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">
          Working Directory (optional)
        </label>
        <input
          type="text"
          value={step.working_directory || ""}
          onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
          placeholder="Leave empty for project root"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-step-config-shell-working-dir-input"
        />
      </div>

      {/* Timeout */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (seconds)</label>
        <input
          type="number"
          value={step.timeout_seconds ?? 60}
          onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 60 })}
          min={5}
          max={600}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-step-config-shell-timeout-input"
        />
      </div>

      {/* Fail on Error */}
      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="fail_on_error"
          checked={step.fail_on_error ?? true}
          onChange={(e) => onUpdate({ fail_on_error: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          data-ui-id="workflow-builder-step-config-shell-fail-on-error-checkbox"
        />
        <label htmlFor="fail_on_error" className="text-sm text-zinc-300">
          Fail workflow on non-zero exit code
        </label>
      </div>

      {/* Common Examples */}
      <div className="pt-2 border-t border-zinc-700">
        <p className="text-xs text-zinc-500 mb-2">Common examples:</p>
        <div className="flex flex-wrap gap-2">
          {[
            {
              label: "Git backup branch",
              cmd: "git branch backup-$(date +%Y%m%d) 2>/dev/null || true",
            },
            { label: "Git commit", cmd: "git add -A && git commit -m 'chore: automated update'" },
            { label: "npm install", cmd: "npm install" },
            { label: "poetry install", cmd: "poetry install" },
          ].map((example) => (
            <button
              key={example.label}
              onClick={() => onUpdate({ command: example.cmd })}
              className="px-2 py-1 text-xs bg-zinc-700 hover:bg-zinc-600 rounded text-zinc-300"
            >
              {example.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Prompt Step Config
// =============================================================================

function PromptConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "prompt" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Prompt Content</label>
        <textarea
          value={step.content || ""}
          onChange={(e) => onUpdate({ content: e.target.value })}
          placeholder="Enter the prompt for the AI agent..."
          rows={24}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 resize-y font-mono text-sm"
          data-ui-id="workflow-builder-step-config-prompt-content-input"
        />
        <p className="text-xs text-zinc-500 mt-1">
          This prompt will be sent to the AI agent during the agentic phase
        </p>
      </div>

      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">
            Provider (optional)
          </label>
          <select
            value={step.provider ?? ""}
            onChange={(e) => onUpdate({ provider: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-step-config-prompt-provider-select"
          >
            <option value="">Default</option>
            <option value="claude_cli">Claude CLI</option>
            <option value="gemini_api">Gemini API</option>
          </select>
        </div>
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Model (optional)</label>
          <select
            value={step.model ?? ""}
            onChange={(e) => onUpdate({ model: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-step-config-prompt-model-select"
          >
            <option value="">Default</option>
            <option value="claude-sonnet-4">Claude Sonnet 4</option>
            <option value="claude-opus-4">Claude Opus 4</option>
            <option value="gemini-2.5-pro">Gemini 2.5 Pro</option>
          </select>
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// UI Bridge Step Config
// =============================================================================

function UiBridgeConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "ui_bridge" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Action</label>
        <select
          value={step.action || "snapshot"}
          onChange={(e) =>
            onUpdate({
              action: e.target.value as "navigate" | "execute" | "assert" | "snapshot",
            })
          }
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-emerald-500/50"
          data-ui-id="workflow-builder-step-config-ui-bridge-action-select"
        >
          <option value="navigate">Navigate</option>
          <option value="execute">Execute Instruction</option>
          <option value="assert">Assert Condition</option>
          <option value="snapshot">Take Snapshot</option>
        </select>
      </div>

      {/* URL for navigate action */}
      {step.action === "navigate" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">URL</label>
          <input
            type="url"
            value={step.url || ""}
            onChange={(e) => onUpdate({ url: e.target.value })}
            placeholder="https://example.com"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500/50"
            data-ui-id="workflow-builder-step-config-ui-bridge-url-input"
          />
          <p className="text-xs text-zinc-500 mt-1">The URL to navigate to</p>
        </div>
      )}

      {/* Instruction for execute action */}
      {step.action === "execute" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Instruction</label>
          <textarea
            value={step.instruction || ""}
            onChange={(e) => onUpdate({ instruction: e.target.value })}
            placeholder="Click the submit button, fill in the form..."
            rows={4}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500/50 resize-y"
            data-ui-id="workflow-builder-step-config-ui-bridge-instruction-input"
          />
          <p className="text-xs text-zinc-500 mt-1">
            Natural language instruction for the UI Bridge to execute
          </p>
        </div>
      )}

      {/* Target, assert_type, expected for assert action */}
      {step.action === "assert" && (
        <>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Target Element</label>
            <input
              type="text"
              value={step.target || ""}
              onChange={(e) => onUpdate({ target: e.target.value })}
              placeholder='[data-ui-id="submit-btn"], .header-title, etc.'
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500/50 font-mono text-sm"
              data-ui-id="workflow-builder-step-config-ui-bridge-target-input"
            />
            <p className="text-xs text-zinc-500 mt-1">
              CSS selector or data-ui-id of the target element
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Assert Type</label>
            <select
              value={step.assert_type || "exists"}
              onChange={(e) =>
                onUpdate({
                  assert_type: e.target.value as
                    | "exists"
                    | "text_equals"
                    | "contains"
                    | "visible"
                    | "enabled",
                })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-emerald-500/50"
              data-ui-id="workflow-builder-step-config-ui-bridge-assert-type-select"
            >
              <option value="exists">Exists</option>
              <option value="text_equals">Text Equals</option>
              <option value="contains">Contains</option>
              <option value="visible">Visible</option>
              <option value="enabled">Enabled</option>
            </select>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Expected Value</label>
            <input
              type="text"
              value={step.expected || ""}
              onChange={(e) => onUpdate({ expected: e.target.value })}
              placeholder="Expected text or value"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500/50"
              data-ui-id="workflow-builder-step-config-ui-bridge-expected-input"
            />
            <p className="text-xs text-zinc-500 mt-1">
              Expected value for text_equals and contains assertions
            </p>
          </div>
        </>
      )}

      {/* Timeout */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (ms)</label>
        <input
          type="number"
          value={step.timeout_ms ?? 5000}
          onChange={(e) => onUpdate({ timeout_ms: parseInt(e.target.value) || 5000 })}
          min={1000}
          max={60000}
          step={1000}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-emerald-500/50"
          data-ui-id="workflow-builder-step-config-ui-bridge-timeout-input"
        />
      </div>
    </div>
  );
}

// =============================================================================
// Main StepConfigPanel Component
// =============================================================================

interface StepConfigPanelProps {
  onClose?: () => void;
}

export function StepConfigPanel({ onClose }: StepConfigPanelProps) {
  const { state, getSelectedStep, updateStep } = useWorkflowBuilder();
  const selectedStep = getSelectedStep();

  if (!selectedStep) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Select a step to configure</p>
        </div>
      </div>
    );
  }

  const phase = findStepPhase(selectedStep.id, state.workflow);

  if (!phase) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Step not found in workflow</p>
        </div>
      </div>
    );
  }

  const handleUpdate = (updates: Partial<UnifiedStep>) => {
    updateStep({ ...selectedStep, ...updates } as UnifiedStep, phase);
  };

  // Get step type label
  const stepTypeLabel = (() => {
    switch (selectedStep.type) {
      case "command": {
        const cmd = selectedStep as CommandStep;
        return cmd.test_type || cmd.test_id ? "Test" : "Command";
      }
      case "prompt":
        return "AI Prompt";
      case "ui_bridge":
        return "UI Bridge";
      default:
        return "Step";
    }
  })();

  // Collect all steps for dependency selection in data flow
  const allSteps = [
    ...state.workflow.setup_steps,
    ...state.workflow.verification_steps,
    ...state.workflow.agentic_steps,
    ...(state.workflow.completion_steps || []),
  ];

  return (
    <div className="h-full flex flex-col bg-zinc-850 border-l border-zinc-700">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-zinc-700">
        <div>
          <h3 className="text-sm font-medium text-zinc-200">Configure Step</h3>
          <p className="text-xs text-zinc-500">{stepTypeLabel}</p>
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="p-1 hover:bg-zinc-700 rounded transition-colors text-zinc-400 hover:text-zinc-200"
            data-ui-id="workflow-builder-step-config-close-btn"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4">
        {/* Common fields */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-zinc-400 mb-1">Step Name</label>
          <input
            type="text"
            value={selectedStep.name}
            onChange={(e) => handleUpdate({ name: e.target.value })}
            placeholder="Step name"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="workflow-builder-step-config-name-input"
          />
        </div>

        {/* Type-specific config */}
        {selectedStep.type === "command" && (
          <CommandConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "prompt" && (
          <PromptConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {/* TestConfig removed — test mode is now inside CommandConfig */}
        {selectedStep.type === "ui_bridge" && (
          <UiBridgeConfig step={selectedStep} onUpdate={handleUpdate} />
        )}

        {/* Console Error Handling -- shown for all verification phase steps */}
        {phase === "verification" && (
          <div className="mt-4 pt-4 border-t border-zinc-700">
            <h4 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-3">
              Console Errors
            </h4>
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="fail_on_console_errors"
                checked={(selectedStep as BaseStep).fail_on_console_errors ?? false}
                onChange={(e) =>
                  handleUpdate({ fail_on_console_errors: e.target.checked } as Partial<UnifiedStep>)
                }
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                data-ui-id="workflow-builder-step-config-fail-on-console-errors-checkbox"
              />
              <label htmlFor="fail_on_console_errors" className="text-sm text-zinc-300">
                Fail on console errors
              </label>
            </div>
            <p className="text-xs text-zinc-500 mt-1 ml-6">
              Step will fail if browser console errors are detected during execution, even if the
              step itself passes.
            </p>
          </div>
        )}

        {/* Data Flow section (universal for all step types) */}
        <DataFlowSection step={selectedStep} onUpdate={handleUpdate} allSteps={allSteps} />
      </div>
    </div>
  );
}
