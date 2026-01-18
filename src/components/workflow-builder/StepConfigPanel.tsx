/**
 * StepConfigPanel.tsx
 *
 * Configuration panel for the selected workflow step.
 * Shows different options based on step type.
 */

import React, { useState } from "react";
import {
  X,
  AlertCircle,
  Globe,
  ExternalLink,
  Wifi,
  Search,
  Play,
  CheckCircle,
  List,
  FileSearch,
} from "lucide-react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import type {
  GuiActionType,
  TestType,
  PlaywrightExecutionMode,
  ApiRequestStep,
  McpCallStep,
  CheckType,
} from "../../types/unified-workflow";
import { GUI_ACTION_TYPES, STEP_TYPES } from "../../types";
import { CHECK_TOOLS, CHECK_TYPE_INFO } from "../check-builder/types";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { ApiRequestStepEditor } from "./ApiRequestStepEditor";
import { McpCallStepEditor } from "./McpCallStepEditor";

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
// Script Step Config
// =============================================================================

function ScriptConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "script" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Target URL</label>
        <input
          type="url"
          value={step.target_url || ""}
          onChange={(e) => onUpdate({ target_url: e.target.value || undefined })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          The URL to navigate to before running the script
        </p>
      </div>

      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="refinement_enabled"
          checked={step.refinement_enabled ?? true}
          onChange={(e) => onUpdate({ refinement_enabled: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
        />
        <label htmlFor="refinement_enabled" className="text-sm text-zinc-300">
          Enable refinement loop
        </label>
      </div>
      <p className="text-xs text-zinc-500 -mt-2">
        AI will refine the script until it succeeds or max attempts reached
      </p>

      <div className="p-3 bg-zinc-800/50 rounded-md border border-zinc-700">
        <p className="text-sm text-zinc-400">
          Use the dedicated Script Builder to create or edit the Playwright script.
        </p>
      </div>
    </div>
  );
}

// =============================================================================
// State Step Config
// =============================================================================

function StateConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "state" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">State Name</label>
        <input
          type="text"
          value={step.state_name || ""}
          onChange={(e) => onUpdate({ state_name: e.target.value || undefined })}
          placeholder="Enter state name"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">The state to navigate to</p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">State ID</label>
        <input
          type="text"
          value={step.state_id || ""}
          onChange={(e) => onUpdate({ state_id: e.target.value })}
          placeholder="state-uuid"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (seconds)</label>
        <input
          type="number"
          value={step.timeout_seconds ?? 30}
          onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 30 })}
          min={5}
          max={300}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      {step.phase === "setup" && (
        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="run_subsequent"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="run_subsequent" className="text-sm text-zinc-300">
            Run on subsequent iterations
          </label>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Workflow Ref Step Config
// =============================================================================

function WorkflowRefConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "workflow_ref" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Workflow Name</label>
        <input
          type="text"
          value={step.workflow_name || ""}
          onChange={(e) => onUpdate({ workflow_name: e.target.value || undefined })}
          placeholder="Enter workflow name"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Workflow ID</label>
        <input
          type="text"
          value={step.workflow_id || ""}
          onChange={(e) => onUpdate({ workflow_id: e.target.value })}
          placeholder="workflow-uuid"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      {step.phase === "setup" && (
        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="run_subsequent_wf"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="run_subsequent_wf" className="text-sm text-zinc-300">
            Run on subsequent iterations
          </label>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// GUI Action Step Config
// =============================================================================

function GuiActionConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "gui_action" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  const actionInfo = GUI_ACTION_TYPES.find((a) => a.type === step.action);

  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Action Type</label>
        <select
          value={step.action}
          onChange={(e) => onUpdate({ action: e.target.value as GuiActionType })}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        >
          {GUI_ACTION_TYPES.map((action) => (
            <option key={action.type} value={action.type}>
              {action.label}
            </option>
          ))}
        </select>
      </div>

      {/* Click-based actions need target images */}
      {(step.action === "click" ||
        step.action === "double_click" ||
        step.action === "right_click") && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Target Image Names</label>
          <input
            type="text"
            value={step.target_image_names?.join(", ") || ""}
            onChange={(e) =>
              onUpdate({
                target_image_names: e.target.value
                  .split(",")
                  .map((s) => s.trim())
                  .filter(Boolean),
              })
            }
            placeholder="button.png, icon.png"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">
            Comma-separated list of image names from your library
          </p>
        </div>
      )}

      {/* Type action needs text input */}
      {step.action === "type" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Text to Type</label>
          <input
            type="text"
            value={step.text_input || ""}
            onChange={(e) => onUpdate({ text_input: e.target.value })}
            placeholder="Enter text..."
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
        </div>
      )}

      {/* Hotkey action needs key combo */}
      {step.action === "hotkey" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Hotkey Combination</label>
          <input
            type="text"
            value={step.hotkey || ""}
            onChange={(e) => onUpdate({ hotkey: e.target.value })}
            placeholder="ctrl+s, alt+tab, etc."
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">Use + to combine keys: ctrl+shift+s, alt+f4</p>
        </div>
      )}

      {/* Scroll action needs direction */}
      {step.action === "scroll" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Direction</label>
          <select
            value={step.scroll_direction || "down"}
            onChange={(e) => onUpdate({ scroll_direction: e.target.value as "up" | "down" })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          >
            <option value="up">Up</option>
            <option value="down">Down</option>
          </select>
        </div>
      )}

      {step.phase === "setup" && (
        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="run_subsequent_gui"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="run_subsequent_gui" className="text-sm text-zinc-300">
            Run on subsequent iterations
          </label>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Test Step Config
// =============================================================================

function TestConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "test" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  const testTypes = STEP_TYPES.verification.filter((s) => s.type.startsWith("test_"));

  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Test Type</label>
        <select
          value={step.test_type}
          onChange={(e) => onUpdate({ test_type: e.target.value as TestType })}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
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
            />
            <p className="text-xs text-zinc-500 mt-1">
              If set, this test will run after the specified setup script
            </p>
          </div>
        </>
      )}

      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="is_critical"
          checked={step.is_critical ?? true}
          onChange={(e) => onUpdate({ is_critical: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
        />
        <label htmlFor="is_critical" className="text-sm text-zinc-300">
          Critical test (fails workflow on failure)
        </label>
      </div>
    </div>
  );
}

// =============================================================================
// Check Step Config
// =============================================================================

function CheckConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "check" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  // Get tools for the current check type
  const checkTypeMap: Record<CheckType, string> = {
    lint: "lint",
    format: "format",
    typecheck: "typecheck",
    analyze: "analyze",
    security: "security",
    custom_command: "custom_command",
  };

  // Filter tools by check type
  const availableTools = CHECK_TOOLS.filter((t) => t.check_type === step.check_type);
  const selectedTool = CHECK_TOOLS.find((t) => t.tool === step.tool);
  const checkTypeInfo = CHECK_TYPE_INFO.find((t) => t.type === step.check_type);

  return (
    <div className="space-y-4">
      {/* Check Type */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Check Type</label>
        <select
          value={step.check_type}
          onChange={(e) => {
            const newType = e.target.value as CheckType;
            // Reset tool when type changes
            onUpdate({ check_type: newType, tool: undefined, command: undefined });
          }}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="lint">Lint (code quality)</option>
          <option value="format">Format (code style)</option>
          <option value="typecheck">Type Check (static analysis)</option>
          <option value="analyze">Analyze (code metrics)</option>
          <option value="security">Security (vulnerability scan)</option>
          <option value="custom_command">Custom Command</option>
        </select>
      </div>

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
          />
          <label htmlFor="auto_fix" className="text-sm text-zinc-300">
            Auto-fix issues (when supported)
          </label>
        </div>
      )}

      {/* Blocking toggle */}
      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="is_blocking"
          checked={step.is_blocking ?? true}
          onChange={(e) => onUpdate({ is_blocking: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
        />
        <label htmlFor="is_blocking" className="text-sm text-zinc-300">
          Blocking (fails workflow on check failure)
        </label>
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
        />
      </div>
    </div>
  );
}

// =============================================================================
// Shell Command Step Config
// =============================================================================

function ShellCommandConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "shell_command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
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
        />
        <label htmlFor="fail_on_error" className="text-sm text-zinc-300">
          Fail workflow on non-zero exit code
        </label>
      </div>

      {/* Run on subsequent iterations (setup phase only) */}
      {step.phase === "setup" && (
        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="run_on_subsequent"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="run_on_subsequent" className="text-sm text-zinc-300">
            Run on subsequent iterations (not just first run)
          </label>
        </div>
      )}

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
// Screenshot Step Config
// =============================================================================

function ScreenshotConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "screenshot" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Monitor</label>
        <select
          value={step.monitor ?? "primary"}
          onChange={(e) => {
            const val = e.target.value;
            // Handle numeric values for monitor index
            if (val === "primary" || val === "all" || val === "left" || val === "right") {
              onUpdate({ monitor: val });
            } else {
              onUpdate({ monitor: parseInt(val) || 0 });
            }
          }}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="primary">Primary Monitor</option>
          <option value="all">All Monitors</option>
          <option value="left">Left Monitor</option>
          <option value="right">Right Monitor</option>
          <option value="0">Monitor 0</option>
          <option value="1">Monitor 1</option>
          <option value="2">Monitor 2</option>
        </select>
        <p className="text-xs text-zinc-500 mt-1">Select which monitor to capture</p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Delay (ms)</label>
        <input
          type="number"
          value={step.delay_ms ?? 0}
          onChange={(e) => onUpdate({ delay_ms: parseInt(e.target.value) || 0 })}
          min={0}
          max={10000}
          step={100}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">Wait before capturing (useful for animations)</p>
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
// API Request Step Config
// =============================================================================

function ApiRequestConfig({
  step,
  onUpdate,
  onOpenEditor,
}: {
  step: UnifiedStep & { type: "api_request" };
  onUpdate: (updates: Partial<typeof step>) => void;
  onOpenEditor: () => void;
}) {
  return (
    <div className="space-y-4">
      {/* Summary display */}
      <div className="p-3 bg-zinc-800 border border-zinc-700 rounded-md space-y-2">
        <div className="flex items-center gap-2">
          <span className="px-2 py-0.5 text-xs font-medium rounded bg-cyan-500/20 text-cyan-400">
            {step.method}
          </span>
          <span className="text-sm text-zinc-300 truncate flex-1 font-mono">
            {step.url || "No URL configured"}
          </span>
        </div>
        {step.headers && Object.keys(step.headers).length > 0 && (
          <p className="text-xs text-zinc-500">{Object.keys(step.headers).length} header(s)</p>
        )}
        {step.extractions && step.extractions.length > 0 && (
          <p className="text-xs text-zinc-500">{step.extractions.length} variable extraction(s)</p>
        )}
        {step.assertions && step.assertions.length > 0 && (
          <p className="text-xs text-zinc-500">{step.assertions.length} assertion(s)</p>
        )}
      </div>

      {/* Open full editor button */}
      <button
        onClick={onOpenEditor}
        className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-cyan-600 hover:bg-cyan-500 text-white rounded-md font-medium transition-colors"
      >
        <Globe className="w-4 h-4" />
        <span>Open Request Editor</span>
        <ExternalLink className="w-3 h-3" />
      </button>

      <p className="text-xs text-zinc-500">
        Configure method, URL, headers, body, variable extractions, and assertions in the full
        editor.
      </p>

      {step.phase === "setup" && (
        <div className="flex items-center gap-2 pt-2 border-t border-zinc-700">
          <input
            type="checkbox"
            id="run_subsequent_api"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="run_subsequent_api" className="text-sm text-zinc-300">
            Run on subsequent iterations
          </label>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// MCP Call Step Config
// =============================================================================

function McpCallConfig({
  step,
  onUpdate,
  onOpenEditor,
}: {
  step: UnifiedStep & { type: "mcp_call" };
  onUpdate: (updates: Partial<typeof step>) => void;
  onOpenEditor: () => void;
}) {
  return (
    <div className="space-y-4">
      {/* Summary display */}
      <div className="p-3 bg-zinc-800 border border-zinc-700 rounded-md space-y-2">
        <div className="flex items-center gap-2">
          <span className="px-2 py-0.5 text-xs font-medium rounded bg-purple-500/20 text-purple-400">
            MCP
          </span>
          <span className="text-sm text-zinc-300 truncate flex-1 font-mono">
            {step.server_name || step.server_id || "No server configured"}
          </span>
        </div>
        {step.tool_name && (
          <p className="text-xs text-zinc-400">
            Tool: <span className="text-zinc-300 font-mono">{step.tool_name}</span>
          </p>
        )}
        {step.extractions && step.extractions.length > 0 && (
          <p className="text-xs text-zinc-500">{step.extractions.length} variable extraction(s)</p>
        )}
        {step.assertions && step.assertions.length > 0 && (
          <p className="text-xs text-zinc-500">{step.assertions.length} assertion(s)</p>
        )}
      </div>

      {/* Open full editor button */}
      <button
        onClick={onOpenEditor}
        className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-purple-600 hover:bg-purple-500 text-white rounded-md font-medium transition-colors"
      >
        <Wifi className="w-4 h-4" />
        <span>Open MCP Call Editor</span>
        <ExternalLink className="w-3 h-3" />
      </button>

      <p className="text-xs text-zinc-500">
        Configure server, tool, arguments, variable extractions, and assertions in the full editor.
      </p>

      {step.phase === "verification" && (
        <div className="flex items-center gap-2 pt-2 border-t border-zinc-700">
          <input
            type="checkbox"
            id="run_subsequent_mcp"
            checked={step.run_on_subsequent_iterations ?? false}
            onChange={(e) => onUpdate({ run_on_subsequent_iterations: e.target.checked })}
            className="rounded bg-zinc-700 border-zinc-600 text-purple-500 focus:ring-purple-500/50"
          />
          <label htmlFor="run_subsequent_mcp" className="text-sm text-zinc-300">
            Run on subsequent iterations
          </label>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// AWAS Discover Step Config
// =============================================================================

function AwasDiscoverConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "awas_discover" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="p-3 bg-teal-900/20 border border-teal-700/50 rounded-md">
        <div className="flex items-center gap-2 text-teal-400 text-sm font-medium mb-1">
          <Search className="w-4 h-4" />
          AWAS Manifest Discovery
        </div>
        <p className="text-xs text-zinc-400">
          Discovers the AWAS manifest from a website's /.well-known/ai-actions.json
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Target URL *</label>
        <input
          type="url"
          value={step.url || ""}
          onChange={(e) => onUpdate({ url: e.target.value })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          The base URL of the website to discover AWAS manifest from
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (seconds)</label>
        <input
          type="number"
          value={step.timeout_seconds ?? 30}
          onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 30 })}
          min={5}
          max={120}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
      </div>
    </div>
  );
}

// =============================================================================
// AWAS Execute Step Config
// =============================================================================

function AwasExecuteConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "awas_execute" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="p-3 bg-teal-900/20 border border-teal-700/50 rounded-md">
        <div className="flex items-center gap-2 text-teal-400 text-sm font-medium mb-1">
          <Play className="w-4 h-4" />
          AWAS Action Execution
        </div>
        <p className="text-xs text-zinc-400">Executes a specific action from an AWAS manifest</p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Target URL *</label>
        <input
          type="url"
          value={step.url || ""}
          onChange={(e) => onUpdate({ url: e.target.value })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Action ID *</label>
        <input
          type="text"
          value={step.action_id || ""}
          onChange={(e) => onUpdate({ action_id: e.target.value })}
          placeholder="list_items, create_record, etc."
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          The action ID from the AWAS manifest to execute
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Parameters (JSON)</label>
        <textarea
          value={step.params ? JSON.stringify(step.params, null, 2) : "{}"}
          onChange={(e) => {
            try {
              const parsed = JSON.parse(e.target.value);
              onUpdate({ params: parsed });
            } catch {
              // Invalid JSON, don't update
            }
          }}
          placeholder='{"param1": "value1"}'
          rows={4}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50 font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Parameters to pass to the action (path, query, body, header)
        </p>
      </div>
    </div>
  );
}

// =============================================================================
// AWAS Check Support Step Config
// =============================================================================

function AwasCheckSupportConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "awas_check_support" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="p-3 bg-teal-900/20 border border-teal-700/50 rounded-md">
        <div className="flex items-center gap-2 text-teal-400 text-sm font-medium mb-1">
          <CheckCircle className="w-4 h-4" />
          AWAS Support Check
        </div>
        <p className="text-xs text-zinc-400">
          Checks if a website supports AWAS (has a valid manifest)
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Target URL *</label>
        <input
          type="url"
          value={step.url || ""}
          onChange={(e) => onUpdate({ url: e.target.value })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Returns true if the website has an AWAS manifest
        </p>
      </div>
    </div>
  );
}

// =============================================================================
// AWAS List Actions Step Config
// =============================================================================

function AwasListActionsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "awas_list_actions" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="p-3 bg-teal-900/20 border border-teal-700/50 rounded-md">
        <div className="flex items-center gap-2 text-teal-400 text-sm font-medium mb-1">
          <List className="w-4 h-4" />
          AWAS List Actions
        </div>
        <p className="text-xs text-zinc-400">Lists all available actions from an AWAS manifest</p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">
          Target URL (optional)
        </label>
        <input
          type="url"
          value={step.url || ""}
          onChange={(e) => onUpdate({ url: e.target.value })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          If provided, fetches manifest first. Otherwise uses cached manifest.
        </p>
      </div>
    </div>
  );
}

// =============================================================================
// AWAS Extract Elements Step Config
// =============================================================================

function AwasExtractElementsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "awas_extract_elements" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="p-3 bg-teal-900/20 border border-teal-700/50 rounded-md">
        <div className="flex items-center gap-2 text-teal-400 text-sm font-medium mb-1">
          <FileSearch className="w-4 h-4" />
          AWAS Element Extraction
        </div>
        <p className="text-xs text-zinc-400">
          Extracts AWAS-annotated elements (data-awas-*) from HTML
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">HTML Content *</label>
        <textarea
          value={step.html || ""}
          onChange={(e) => onUpdate({ html: e.target.value })}
          placeholder="<html>...</html>"
          rows={6}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50 font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          HTML content to scan for data-awas-* attributes
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Base URL (optional)</label>
        <input
          type="url"
          value={step.base_url || ""}
          onChange={(e) => onUpdate({ base_url: e.target.value })}
          placeholder="https://example.com"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-teal-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Used to resolve relative URLs in extracted elements
        </p>
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
  const [apiRequestEditorOpen, setApiRequestEditorOpen] = useState(false);
  const [mcpCallEditorOpen, setMcpCallEditorOpen] = useState(false);
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
      case "script":
        return "Playwright Script";
      case "state":
        return "Navigate to State";
      case "workflow_ref":
        return "Run Workflow";
      case "gui_action":
        return "GUI Action";
      case "test":
        return "Test";
      case "check":
        return "Check";
      case "shell_command":
        return "Shell Command";
      case "screenshot":
        return "Screenshot";
      case "prompt":
        return "AI Prompt";
      case "api_request":
        return "API Request";
      case "awas_discover":
        return "AWAS Discover";
      case "awas_execute":
        return "AWAS Execute";
      case "awas_check_support":
        return "AWAS Check Support";
      case "awas_list_actions":
        return "AWAS List Actions";
      case "awas_extract_elements":
        return "AWAS Extract Elements";
      default:
        return "Step";
    }
  })();

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
          />
        </div>

        {/* Type-specific config */}
        {selectedStep.type === "script" && (
          <ScriptConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "state" && (
          <StateConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "workflow_ref" && (
          <WorkflowRefConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "gui_action" && (
          <GuiActionConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "test" && <TestConfig step={selectedStep} onUpdate={handleUpdate} />}
        {selectedStep.type === "check" && (
          <CheckConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "shell_command" && (
          <ShellCommandConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "screenshot" && (
          <ScreenshotConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "prompt" && (
          <PromptConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "api_request" && (
          <ApiRequestConfig
            step={selectedStep}
            onUpdate={handleUpdate}
            onOpenEditor={() => setApiRequestEditorOpen(true)}
          />
        )}
        {selectedStep.type === "mcp_call" && (
          <McpCallConfig
            step={selectedStep}
            onUpdate={handleUpdate}
            onOpenEditor={() => setMcpCallEditorOpen(true)}
          />
        )}
        {selectedStep.type === "awas_discover" && (
          <AwasDiscoverConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "awas_execute" && (
          <AwasExecuteConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "awas_check_support" && (
          <AwasCheckSupportConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "awas_list_actions" && (
          <AwasListActionsConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "awas_extract_elements" && (
          <AwasExtractElementsConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
      </div>

      {/* API Request Editor Modal */}
      {selectedStep.type === "api_request" && (
        <ApiRequestStepEditor
          step={selectedStep as ApiRequestStep}
          open={apiRequestEditorOpen}
          onClose={() => setApiRequestEditorOpen(false)}
          onSave={(updates) => {
            handleUpdate(updates);
            setApiRequestEditorOpen(false);
          }}
        />
      )}

      {/* MCP Call Editor Modal */}
      {selectedStep.type === "mcp_call" && (
        <McpCallStepEditor
          step={selectedStep as McpCallStep}
          open={mcpCallEditorOpen}
          onClose={() => setMcpCallEditorOpen(false)}
          onSave={(updates) => {
            handleUpdate(updates);
            setMcpCallEditorOpen(false);
          }}
        />
      )}
    </div>
  );
}
