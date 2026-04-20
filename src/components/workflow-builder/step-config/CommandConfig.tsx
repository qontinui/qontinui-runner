import type { UnifiedStep } from "../../../types/unified-workflow";
import type { TestType, PlaywrightExecutionMode, CheckType } from "../../../types/unified-workflow";
import { CHECK_TOOLS, CHECK_TYPE_INFO } from "./check-constants";

type CommandStepType = UnifiedStep & { type: "command" };
type StepUpdater = (updates: Partial<CommandStepType>) => void;

function TestFieldsConfig({ step, onUpdate }: { step: CommandStepType; onUpdate: StepUpdater }) {
  return (
    <div className="space-y-4">
      <div>
        <label htmlFor="test-type-select" className="block text-sm font-medium text-zinc-400 mb-1">
          Test Type
        </label>
        <select
          id="test-type-select"
          value={step.testType ?? ""}
          onChange={(e) => onUpdate({ testType: e.target.value as TestType })}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="playwright">Playwright (Browser)</option>
          <option value="qontinui_vision">Qontinui Vision</option>
          <option value="python">Python Script</option>
          <option value="repository">Repository Test</option>
          <option value="custom_command">Custom Command</option>
        </select>
      </div>

      {(step.testType === "custom_command" ||
        step.testType === "python" ||
        step.testType === "repository") && (
        <div>
          <label
            htmlFor="test-command-input"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            Command
          </label>
          <input
            id="test-command-input"
            type="text"
            value={step.command || ""}
            onChange={(e) => onUpdate({ command: e.target.value })}
            placeholder={
              step.testType === "python"
                ? "python test_script.py"
                : step.testType === "repository"
                  ? "npm test"
                  : "command to run"
            }
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
        </div>
      )}

      {step.testType === "playwright" && (
        <>
          <div>
            <label
              htmlFor="execution-mode-select"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Execution Mode
            </label>
            <select
              id="execution-mode-select"
              value={step.executionMode || "independent"}
              onChange={(e) =>
                onUpdate({ executionMode: e.target.value as PlaywrightExecutionMode })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            >
              <option value="independent">Independent (fresh session)</option>
              <option value="chained">Chained (continue after previous)</option>
            </select>
            <p className="text-xs text-zinc-500 mt-1">
              Independent starts a new browser session; Chained continues from previous test
            </p>
          </div>

          <div>
            <label
              htmlFor="fused-script-id-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Fused Script ID (optional)
            </label>
            <input
              id="fused-script-id-input"
              type="text"
              value={step.fusedScriptId || ""}
              onChange={(e) => onUpdate({ fusedScriptId: e.target.value || undefined })}
              placeholder="script-uuid"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
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

function CheckFieldsConfig({ step, onUpdate }: { step: CommandStepType; onUpdate: StepUpdater }) {
  const availableTools = CHECK_TOOLS.filter((t) => t.check_type === step.checkType);
  const selectedTool = CHECK_TOOLS.find((t) => t.tool === step.tool);
  const _checkTypeInfo = CHECK_TYPE_INFO.find((t) => t.type === step.checkType);

  const isCiCd = step.checkType === "ci_cd";

  return (
    <div className="space-y-4">
      <div>
        <label htmlFor="check-type-select" className="block text-sm font-medium text-zinc-400 mb-1">
          Check Type
        </label>
        <select
          id="check-type-select"
          value={step.checkType ?? ""}
          onChange={(e) => {
            const newType = e.target.value as CheckType;
            onUpdate({ checkType: newType, tool: undefined, command: undefined });
          }}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
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
          <div>
            <label
              htmlFor="cicd-repository-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Repository
            </label>
            <input
              id="cicd-repository-input"
              type="text"
              value={step.repository || ""}
              onChange={(e) => onUpdate({ repository: e.target.value || undefined })}
              placeholder="owner/repo"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
            />
            <p className="text-xs text-zinc-500 mt-1">
              GitHub repository (e.g., jspindev/qontinui-runner). Leave blank to auto-detect from
              working directory.
            </p>
          </div>

          {!step.repository && (
            <div>
              <label
                htmlFor="cicd-working-dir-input"
                className="block text-sm font-medium text-zinc-400 mb-1"
              >
                Working Directory
              </label>
              <input
                id="cicd-working-dir-input"
                type="text"
                value={step.workingDirectory || ""}
                onChange={(e) => onUpdate({ workingDirectory: e.target.value || undefined })}
                placeholder="Path to git repo root"
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
              />
              <p className="text-xs text-zinc-500 mt-1">
                Git repo directory to auto-detect the GitHub repository from.
              </p>
            </div>
          )}

          <div>
            <label
              htmlFor="cicd-workflow-name-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Workflow Name (optional)
            </label>
            <input
              id="cicd-workflow-name-input"
              type="text"
              value={step.workflowName || ""}
              onChange={(e) => onUpdate({ workflowName: e.target.value || undefined })}
              placeholder="CI"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
            <p className="text-xs text-zinc-500 mt-1">GitHub Actions workflow name to filter by.</p>
          </div>

          <div>
            <label
              htmlFor="cicd-branch-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Branch (optional)
            </label>
            <input
              id="cicd-branch-input"
              type="text"
              value={step.branch || ""}
              onChange={(e) => onUpdate({ branch: e.target.value || undefined })}
              placeholder="main"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>

          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="wait_for_completion"
              checked={step.waitForCompletion ?? false}
              onChange={(e) => onUpdate({ waitForCompletion: e.target.checked })}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="wait_for_completion" className="text-sm text-zinc-300">
              Wait for in-progress runs
            </label>
          </div>
          <p className="text-xs text-zinc-500 ml-6 -mt-2">
            Poll until the CI run finishes instead of failing immediately when in progress.
          </p>

          <div>
            <label
              htmlFor="cicd-timeout-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Timeout (seconds)
            </label>
            <input
              id="cicd-timeout-input"
              type="number"
              value={step.timeoutSeconds ?? 300}
              onChange={(e) => onUpdate({ timeoutSeconds: parseInt(e.target.value) || 300 })}
              min={15}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>
        </>
      ) : (
        <>
          {step.checkType !== "custom_command" && availableTools.length > 0 && (
            <div>
              <label
                htmlFor="check-tool-select"
                className="block text-sm font-medium text-zinc-400 mb-1"
              >
                Tool
              </label>
              <select
                id="check-tool-select"
                value={step.tool || ""}
                onChange={(e) => {
                  const tool = CHECK_TOOLS.find((t) => t.tool === e.target.value);
                  onUpdate({
                    tool: e.target.value || undefined,
                    command: tool?.default_command || undefined,
                  });
                }}
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
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

          <div>
            <label
              htmlFor="check-command-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Command {step.checkType === "custom_command" ? "(required)" : "(optional override)"}
            </label>
            <input
              id="check-command-input"
              type="text"
              value={step.command || ""}
              onChange={(e) => onUpdate({ command: e.target.value || undefined })}
              placeholder={selectedTool?.default_command || "Enter command..."}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
            />
            {selectedTool?.default_command && !step.command && (
              <p className="text-xs text-zinc-500 mt-1">
                Default:{" "}
                <code className="bg-zinc-700 px-1 rounded">{selectedTool.default_command}</code>
              </p>
            )}
          </div>

          <div>
            <label
              htmlFor="check-working-dir-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Working Directory (optional)
            </label>
            <input
              id="check-working-dir-input"
              type="text"
              value={step.workingDirectory || ""}
              onChange={(e) => onUpdate({ workingDirectory: e.target.value || undefined })}
              placeholder="Leave empty for project root"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>

          {(selectedTool?.supports_auto_fix || step.checkType === "format") && (
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="auto_fix"
                checked={step.autoFix ?? false}
                onChange={(e) => onUpdate({ autoFix: e.target.checked })}
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
              />
              <label htmlFor="auto_fix" className="text-sm text-zinc-300">
                Auto-fix issues (when supported)
              </label>
            </div>
          )}

          <div>
            <label
              htmlFor="check-timeout-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Timeout (seconds)
            </label>
            <input
              id="check-timeout-input"
              type="number"
              value={step.timeoutSeconds ?? 60}
              onChange={(e) => onUpdate({ timeoutSeconds: parseInt(e.target.value) || 60 })}
              min={5}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>
        </>
      )}
    </div>
  );
}

export function CommandConfig({
  step,
  onUpdate,
}: {
  step: CommandStepType;
  onUpdate: StepUpdater;
}) {
  const effectiveMode = step.mode;

  const handleModeChange = (newMode: string) => {
    const cleared: Partial<CommandStepType> = {
      mode: newMode as "shell" | "check" | "check_group" | "test",
    };
    if (newMode !== "check") {
      cleared.check_type = undefined;
    }
    if (newMode !== "check_group") {
      cleared.check_group_id = undefined;
    }
    if (newMode !== "test") {
      cleared.test_type = undefined;
      cleared.test_id = undefined;
      cleared.code = undefined;
    }
    onUpdate(cleared);
  };

  const modeSelector = (
    <div className="mb-4">
      <label htmlFor="command-mode-select" className="block text-sm font-medium text-zinc-400 mb-1">
        Command Mode
      </label>
      <select
        id="command-mode-select"
        value={effectiveMode ?? ""}
        onChange={(e) => handleModeChange(e.target.value)}
        className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
      >
        <option value="shell">Shell Command</option>
        <option value="check">Check (lint, typecheck, etc.)</option>
        <option value="check_group">Check Group</option>
        <option value="test">Test</option>
      </select>
    </div>
  );

  if (effectiveMode === "check_group") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <div>
          <label
            htmlFor="check-group-id-input"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            Check Group ID
          </label>
          <input
            id="check-group-id-input"
            type="text"
            value={step.checkGroupId || ""}
            onChange={(e) => onUpdate({ checkGroupId: e.target.value })}
            placeholder="check-group-uuid"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">
            The ID of a saved check group from the Check Builder
          </p>
        </div>

        {step.checkGroupId && (
          <div className="p-2 bg-cyan-500/10 border border-cyan-500/30 rounded-md">
            <p className="text-xs text-cyan-400 font-mono">{step.checkGroupId}</p>
          </div>
        )}
      </div>
    );
  }

  if (effectiveMode === "check") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <CheckFieldsConfig step={step} onUpdate={onUpdate} />
      </div>
    );
  }

  if (effectiveMode === "test") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <TestFieldsConfig step={step} onUpdate={onUpdate} />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {modeSelector}
      <div>
        <label
          htmlFor="shell-command-textarea"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Command
        </label>
        <textarea
          id="shell-command-textarea"
          value={step.command || ""}
          onChange={(e) => onUpdate({ command: e.target.value })}
          placeholder="git branch backup-$(date +%Y%m%d)"
          rows={3}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Shell command to execute (bash on Unix, PowerShell on Windows)
        </p>
      </div>

      <div>
        <label
          htmlFor="shell-working-dir-input"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Working Directory (optional)
        </label>
        <input
          id="shell-working-dir-input"
          type="text"
          value={step.workingDirectory || ""}
          onChange={(e) => onUpdate({ workingDirectory: e.target.value || undefined })}
          placeholder="Leave empty for project root"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      <div>
        <label
          htmlFor="shell-timeout-input"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Timeout (seconds)
        </label>
        <input
          id="shell-timeout-input"
          type="number"
          value={step.timeoutSeconds ?? 60}
          onChange={(e) => onUpdate({ timeoutSeconds: parseInt(e.target.value) || 60 })}
          min={5}
          max={600}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="fail_on_error"
          checked={step.failOnError ?? true}
          onChange={(e) => onUpdate({ failOnError: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
        />
        <label htmlFor="fail_on_error" className="text-sm text-zinc-300">
          Fail workflow on non-zero exit code
        </label>
      </div>

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
