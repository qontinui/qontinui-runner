import type { Dispatch, RefObject } from "react";
import {
  TestTube,
  Plus,
  Play,
  Save,
  X,
  Loader2,
  Upload,
  Download,
  Code,
  FileText,
  Globe,
  Sparkles,
  RefreshCw,
  Info,
  ImageIcon,
  CheckSquare,
} from "lucide-react";
import { PromptSnippetSelector } from "../PromptSnippetSelector";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { FormState, FormAction, ScriptViewMode, PlaywrightScript, DisplayMode } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { LogLevel } from "./types";

interface VisualContextSettings {
  includeScreenshot: boolean;
  setIncludeScreenshot: (v: boolean) => void;
  includeTraceData: boolean;
  setIncludeTraceData: (v: boolean) => void;
  includeVideo: boolean;
  setIncludeVideo: (v: boolean) => void;
  includeVideoAfterIterations: number;
  setIncludeVideoAfterIterations: (v: number) => void;
  autoRefineMaxIterations: number;
  setAutoRefineMaxIterations: (v: number) => void;
}

interface PromptSnippetMentionState {
  isActive: boolean;
  searchQuery: string;
  position: { top: number; left: number };
  handleKeyDown: (e: React.KeyboardEvent<HTMLTextAreaElement>) => void;
  handleInput: (e: React.FormEvent<HTMLTextAreaElement>) => void;
  handleSelect: (content: string) => void;
  handleClose: () => void;
}

interface ScriptEditorFormProps {
  form: FormState;
  dispatch: Dispatch<FormAction>;
  viewMode: ScriptViewMode;
  setViewMode: (mode: ScriptViewMode) => void;
  isCreating: boolean;
  editingScript: PlaywrightScript | null;
  isGenerating: boolean;
  categories: string[];
  coverageWarnings: string[];
  isValidatingCoverage: boolean;
  descriptionTextareaRef: RefObject<HTMLTextAreaElement | null>;
  promptSnippetMention: PromptSnippetMentionState;
  insertPromptSnippetAtCursor: (content: string) => void;
  onGenerateScript: () => void;
  onCreateScript: (options?: { runTest?: boolean; autoRefine?: boolean }) => void;
  onImportScripts: () => void;
  onExportScripts: () => void;
  onClose: () => void;
  onLog: (level: LogLevel, message: string) => void;
  setCoverageWarnings: (warnings: string[]) => void;
  visualContext: VisualContextSettings;
}

export function ScriptEditorForm({
  form,
  dispatch,
  viewMode,
  setViewMode,
  isCreating,
  editingScript,
  isGenerating,
  categories,
  coverageWarnings,
  isValidatingCoverage,
  descriptionTextareaRef,
  promptSnippetMention,
  insertPromptSnippetAtCursor,
  onGenerateScript,
  onCreateScript,
  onImportScripts,
  onExportScripts,
  onClose,
  onLog,
  setCoverageWarnings,
  visualContext,
}: ScriptEditorFormProps) {
  return (
    <>
      <div className="flex items-center justify-between p-4 border-b border-border">
        <h3 className="font-medium">
          {isCreating ? "New Script" : `Editing: ${editingScript?.name}`}
        </h3>
        <div className="flex items-center gap-2">
          <button
            onClick={onImportScripts}
            className="p-2 rounded-lg hover:bg-muted transition-colors"
            title="Import"
          >
            <Upload className="w-4 h-4" />
          </button>
          <button
            onClick={onExportScripts}
            className="p-2 rounded-lg hover:bg-muted transition-colors"
            title="Export"
          >
            <Download className="w-4 h-4" />
          </button>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-4 scrollbar-dark">
        {(isCreating || editingScript) && (
          <div
            className={`card p-4 space-y-4 border-2 ${getStatusColors("success").border} min-h-min`}
          >
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold flex items-center gap-2">
                <TestTube className={`w-5 h-5 ${getStatusColors("success").text}`} />
                Create New Script
              </h3>
              <div className="flex items-center gap-2">
                <div className="flex items-center bg-card border border-border rounded-lg p-1">
                  <button
                    onClick={() => setViewMode("natural_language")}
                    className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                      viewMode === "natural_language"
                        ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                  >
                    <FileText className="w-4 h-4" />
                    Description
                  </button>
                  <button
                    onClick={() => setViewMode("code")}
                    className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                      viewMode === "code"
                        ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                  >
                    <Code className="w-4 h-4" />
                    Code
                  </button>
                </div>
                <button onClick={onClose} className="p-1 hover:bg-card rounded">
                  <X className="w-5 h-5" />
                </button>
              </div>
            </div>

            <div className="grid grid-cols-2 gap-4">
              <div>
                <label htmlFor="script-name" className="block text-sm font-medium mb-1">
                  Name *
                </label>
                <input
                  id="script-name"
                  type="text"
                  value={form.name}
                  onChange={(e) => dispatch({ type: "SET_NAME", value: e.target.value })}
                  placeholder="Login Flow Test"
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                />
              </div>
              <div>
                <label htmlFor="script-target-url" className="block text-sm font-medium mb-1">
                  Target URL
                </label>
                <div className="relative">
                  <Globe className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
                  <input
                    id="script-target-url"
                    type="text"
                    value={form.targetUrl}
                    onChange={(e) => dispatch({ type: "SET_TARGET_URL", value: e.target.value })}
                    placeholder="http://localhost:3000"
                    className="w-full pl-10 pr-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                  />
                </div>
              </div>
            </div>

            {viewMode === "natural_language" ? (
              <NaturalLanguageView
                form={form}
                dispatch={dispatch}
                isGenerating={isGenerating}
                descriptionTextareaRef={descriptionTextareaRef}
                promptSnippetMention={promptSnippetMention}
                insertPromptSnippetAtCursor={insertPromptSnippetAtCursor}
                onGenerateScript={onGenerateScript}
              />
            ) : (
              <CodeView
                form={form}
                dispatch={dispatch}
                isGenerating={isGenerating}
                coverageWarnings={coverageWarnings}
                isValidatingCoverage={isValidatingCoverage}
                onGenerateScript={onGenerateScript}
                setCoverageWarnings={setCoverageWarnings}
              />
            )}

            <div className="grid grid-cols-2 gap-4">
              <div>
                <label htmlFor="script-category" className="block text-sm font-medium mb-1">
                  Category
                </label>
                <input
                  id="script-category"
                  type="text"
                  value={form.category}
                  onChange={(e) => dispatch({ type: "SET_CATEGORY", value: e.target.value })}
                  placeholder="E2E, Smoke, Regression"
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                  list="category-suggestions"
                />
                <datalist id="category-suggestions">
                  {categories.map((cat) => (
                    <option key={cat} value={cat} />
                  ))}
                </datalist>
              </div>
              <div>
                <label htmlFor="script-tags" className="block text-sm font-medium mb-1">
                  Tags (comma-separated)
                </label>
                <input
                  id="script-tags"
                  type="text"
                  value={form.tags}
                  onChange={(e) => dispatch({ type: "SET_TAGS", value: e.target.value })}
                  placeholder="login, auth, smoke"
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                />
              </div>
            </div>

            <div className="grid grid-cols-3 gap-4">
              <div>
                <label htmlFor="script-timeout" className="block text-sm font-medium mb-1">
                  Timeout (seconds)
                </label>
                <input
                  id="script-timeout"
                  type="number"
                  value={form.timeoutSeconds}
                  onChange={(e) =>
                    dispatch({ type: "SET_TIMEOUT_SECONDS", value: parseInt(e.target.value) || 60 })
                  }
                  min={10}
                  max={600}
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                />
              </div>
              <div>
                <label htmlFor="script-browser" className="block text-sm font-medium mb-1">
                  Browser
                </label>
                <select
                  id="script-browser"
                  value={form.browser}
                  onChange={(e) =>
                    dispatch({
                      type: "SET_BROWSER",
                      value: e.target.value as "chromium" | "firefox" | "webkit",
                    })
                  }
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
                >
                  <option value="chromium">Chromium</option>
                  <option value="firefox">Firefox</option>
                  <option value="webkit">WebKit</option>
                </select>
              </div>
              <DisplayModeSelect form={form} dispatch={dispatch} onLog={onLog} />
            </div>

            <VisualContextSettingsBar settings={visualContext} />

            <div className="flex justify-between items-center gap-2 pt-4 border-t border-border">
              <button onClick={onClose} className="btn-secondary px-4 py-2">
                Cancel
              </button>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => onCreateScript()}
                  disabled={!form.name || !form.scriptContent}
                  className="btn-secondary px-4 py-2 flex items-center gap-2"
                >
                  <Save className="w-4 h-4" />
                  Save Only
                </button>
                <button
                  onClick={() => onCreateScript({ runTest: true })}
                  disabled={!form.name || !form.scriptContent}
                  className="btn-primary px-4 py-2 flex items-center gap-2"
                  title="Save and run the test once"
                >
                  <Play className="w-4 h-4" />
                  Save & Run
                </button>
                <button
                  onClick={() => onCreateScript({ autoRefine: true })}
                  disabled={!form.name || !form.scriptContent}
                  className="flex items-center gap-2 px-4 py-2 text-sm bg-gradient-to-r from-purple-500 to-pink-500 text-white rounded-lg hover:from-purple-600 hover:to-pink-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                  title="Save, run test, and refine with AI until it passes"
                >
                  <Sparkles className="w-4 h-4" />
                  Save & Auto-Refine
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </>
  );
}

function EmptyState({
  onStartCreate,
  accentColors,
}: {
  onStartCreate: () => void;
  accentColors: ReturnType<typeof getAccentColors>;
}) {
  return (
    <div className="flex-1 flex items-center justify-center">
      <div className="text-center text-muted-foreground">
        <TestTube className="w-12 h-12 mx-auto mb-3 opacity-30" />
        <p className="text-lg mb-2">No script selected</p>
        <p className="text-sm mb-4">Select a script to edit or create a new one</p>
        <button
          onClick={onStartCreate}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
          style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
        >
          <Plus className="w-4 h-4" />
          Create New Script
        </button>
      </div>
    </div>
  );
}

export { EmptyState };

function NaturalLanguageView({
  form,
  dispatch,
  isGenerating,
  descriptionTextareaRef,
  promptSnippetMention,
  insertPromptSnippetAtCursor,
  onGenerateScript,
}: {
  form: FormState;
  dispatch: Dispatch<FormAction>;
  isGenerating: boolean;
  descriptionTextareaRef: RefObject<HTMLTextAreaElement | null>;
  promptSnippetMention: PromptSnippetMentionState;
  insertPromptSnippetAtCursor: (content: string) => void;
  onGenerateScript: () => void;
}) {
  return (
    <div>
      <div className="flex items-center justify-between mb-1">
        <label htmlFor="script-description" className="block text-sm font-medium">
          Description (Natural Language)
        </label>
        <div className="flex items-center gap-2">
          <PromptSnippetSelector mode="dropdown" onSelect={insertPromptSnippetAtCursor} />
          <button
            onClick={onGenerateScript}
            disabled={isGenerating || !form.description.trim()}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-gradient-to-r from-purple-500 to-pink-500 text-white rounded-lg hover:from-purple-600 hover:to-pink-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
          >
            {isGenerating ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Generating...
              </>
            ) : (
              <>
                <Sparkles className="w-4 h-4" />
                Generate Script
              </>
            )}
          </button>
        </div>
      </div>
      <div className="relative">
        <textarea
          id="script-description"
          ref={descriptionTextareaRef}
          value={form.description}
          onChange={(e) => dispatch({ type: "SET_DESCRIPTION", value: e.target.value })}
          onKeyDown={promptSnippetMention.handleKeyDown}
          onInput={promptSnippetMention.handleInput}
          placeholder="Describe what this test should do in plain English... (Type @ to insert a prompt snippet)&#10;&#10;Example: There is a Capture Screen button on the Image Extraction page. Click it and select Capture Screen in the dialog box."
          rows={6}
          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
        />
        {promptSnippetMention.isActive && (
          <PromptSnippetSelector
            mode="popup"
            onSelect={promptSnippetMention.handleSelect}
            onClose={promptSnippetMention.handleClose}
            searchQuery={promptSnippetMention.searchQuery}
            position={promptSnippetMention.position}
          />
        )}
      </div>
      <p className="text-xs text-muted-foreground mt-1">
        Describe what you want to test, then click "Generate Script" to create the Playwright code.
      </p>

      <div className="mt-4">
        <label
          htmlFor="script-ai-instructions"
          className="block text-sm font-medium mb-1 flex items-center gap-2"
        >
          <Sparkles className={`w-4 h-4 ${getAccentColors("purple").text}`} />
          AI Instructions (Optional)
        </label>
        <textarea
          id="script-ai-instructions"
          value={form.aiInstructions}
          onChange={(e) => dispatch({ type: "SET_AI_INSTRUCTIONS", value: e.target.value })}
          placeholder="Additional instructions for the AI that modify how the description is interpreted...&#10;&#10;Example: The feature is currently broken. Stop after capturing the screen and take a screenshot. Expect failure but capture the state for debugging."
          rows={3}
          className={`w-full px-3 py-2 ${getAccentColors("purple").bg} border ${getAccentColors("purple").border} rounded-lg focus:outline-hidden focus:ring-2 focus:ring-purple-500 text-sm`}
        />
        <p className="text-xs text-muted-foreground mt-1">
          Use this to give the AI additional context without changing the test description.
        </p>
      </div>

      <details
        className={`mt-4 border ${getAccentColors("blue").border} rounded-lg p-3 ${getAccentColors("blue").bg}`}
      >
        <summary
          className={`cursor-pointer text-sm font-medium flex items-center gap-2 ${getAccentColors("blue").text}`}
        >
          <CheckSquare className="w-4 h-4" />
          Workflow Automation Settings (Optional)
        </summary>
        <div className="mt-3 space-y-3">
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="is-workflow-automation"
              checked={form.isWorkflowAutomation}
              onChange={(e) =>
                dispatch({ type: "SET_IS_WORKFLOW_AUTOMATION", value: e.target.checked })
              }
              className={`w-4 h-4 rounded ${getAccentColors("blue").border} ${getAccentColors("blue").text} focus:ring-blue-500`}
            />
            <label htmlFor="is-workflow-automation" className="text-sm">
              This is workflow automation (script success is expected, verify objective instead)
            </label>
          </div>

          <div>
            <label htmlFor="workflow-objective" className="block text-sm font-medium mb-1">
              Workflow Objective
            </label>
            <textarea
              id="workflow-objective"
              value={form.workflowObjective}
              onChange={(e) => dispatch({ type: "SET_WORKFLOW_OBJECTIVE", value: e.target.value })}
              placeholder="What should this workflow achieve?&#10;&#10;Example: Create a new state called 'test-state' and verify it appears in the state list"
              rows={2}
              className={`w-full px-3 py-2 bg-background border ${getAccentColors("blue").border} rounded-lg focus:outline-hidden focus:ring-2 focus:ring-blue-500 text-sm`}
            />
          </div>

          <div>
            <label htmlFor="success-criteria" className="block text-sm font-medium mb-1">
              Success Criteria (one per line)
            </label>
            <textarea
              id="success-criteria"
              value={form.successCriteria.join("\n")}
              onChange={(e) =>
                dispatch({
                  type: "SET_SUCCESS_CRITERIA",
                  value: e.target.value
                    .split("\n")
                    .map((s) => s.trim())
                    .filter(Boolean),
                })
              }
              placeholder="Specific things to verify after script passes:&#10;- 'test-state' appears in state list&#10;- State is visible on canvas&#10;- No error messages shown"
              rows={3}
              className={`w-full px-3 py-2 bg-background border ${getAccentColors("blue").border} rounded-lg focus:outline-hidden focus:ring-2 focus:ring-blue-500 text-sm`}
            />
          </div>

          <p className="text-xs text-muted-foreground">
            When enabled, script execution success is expected. After the script passes, AI will
            verify whether the workflow objective was actually achieved.
          </p>
        </div>
      </details>
    </div>
  );
}

function CodeView({
  form,
  dispatch,
  isGenerating,
  coverageWarnings,
  isValidatingCoverage,
  onGenerateScript,
  setCoverageWarnings,
}: {
  form: FormState;
  dispatch: Dispatch<FormAction>;
  isGenerating: boolean;
  coverageWarnings: string[];
  isValidatingCoverage: boolean;
  onGenerateScript: () => void;
  setCoverageWarnings: (warnings: string[]) => void;
}) {
  return (
    <div>
      <div className="flex items-center justify-between mb-1">
        <label htmlFor="script-content" className="block text-sm font-medium">
          Script Content (.spec.ts) *
        </label>
        {form.description.trim() && (
          <button
            onClick={onGenerateScript}
            disabled={isGenerating}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-card border border-border rounded-lg hover:bg-muted disabled:opacity-50 disabled:cursor-not-allowed transition-all"
            title="Regenerate script from description"
          >
            {isGenerating ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Regenerating...
              </>
            ) : (
              <>
                <RefreshCw className="w-4 h-4" />
                Regenerate
              </>
            )}
          </button>
        )}
      </div>
      <textarea
        id="script-content"
        value={form.scriptContent}
        onChange={(e) => dispatch({ type: "SET_SCRIPT_CONTENT", value: e.target.value })}
        rows={12}
        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50 font-mono text-sm"
        spellCheck={false}
      />

      {isValidatingCoverage && (
        <div className="mt-2 flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="w-4 h-4 animate-spin" />
          Validating script covers all requirements...
        </div>
      )}

      {coverageWarnings.length > 0 && (
        <div
          className={`mt-3 p-3 ${getAccentColors("amber").bg} border ${getAccentColors("amber").border} rounded-lg`}
        >
          <div className="flex items-start gap-2">
            <Info className={`w-4 h-4 ${getAccentColors("amber").text} mt-0.5 shrink-0`} />
            <div className="flex-1">
              <div className={`text-sm font-medium ${getAccentColors("amber").text} mb-1`}>
                Missing Requirements Detected
              </div>
              <div className={`text-sm ${getAccentColors("amber").text} mb-2`}>
                The generated code may not implement all functionality from the description:
              </div>
              <ul
                className={`text-sm ${getAccentColors("amber").text} list-disc list-inside space-y-1`}
              >
                {coverageWarnings.map((warning, i) => (
                  <li key={`warning-${i}-${warning.slice(0, 20)}`}>{warning}</li>
                ))}
              </ul>
              <button
                onClick={() => {
                  const missingItems = coverageWarnings.join(", ");
                  dispatch({
                    type: "SET_AI_INSTRUCTIONS",
                    value:
                      form.aiInstructions +
                      (form.aiInstructions ? "\n\n" : "") +
                      `IMPORTANT: Make sure to implement these missing requirements: ${missingItems}`,
                  });
                  setCoverageWarnings([]);
                  onGenerateScript();
                }}
                disabled={isGenerating}
                className={`mt-2 px-3 py-1.5 text-sm ${getAccentColors("amber").bg} hover:opacity-80 ${getAccentColors("amber").text} rounded-lg transition-colors disabled:opacity-50`}
              >
                <RefreshCw className="w-3 h-3 inline mr-1.5" />
                Regenerate with missing requirements
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function DisplayModeSelect({
  form,
  dispatch,
  onLog,
}: {
  form: FormState;
  dispatch: Dispatch<FormAction>;
  onLog: (level: LogLevel, message: string) => void;
}) {
  return (
    <div>
      <label htmlFor="script-display-mode" className="block text-sm font-medium mb-1">
        Display Mode
      </label>
      <select
        id="script-display-mode"
        value={form.displayMode}
        onChange={(e) =>
          dispatch({ type: "SET_DISPLAY_MODE", value: e.target.value as DisplayMode })
        }
        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-green-500/50"
      >
        <option value="headless">Headless (No UI)</option>
        <option value="headed">Headed (New Window)</option>
        <option value="connect_existing">Open Browser (CDP)</option>
      </select>
      {form.displayMode === "connect_existing" && (
        <div className="mt-1 space-y-1">
          <div className="flex items-center gap-1">
            <p className={`text-xs ${getAccentColors("amber").text}`}>
              Requires Chrome started with: --remote-debugging-port=9222
            </p>
            <div className="relative group">
              <Info className={`w-3.5 h-3.5 ${getAccentColors("amber").text} cursor-help`} />
              <div className="absolute right-0 bottom-full mb-2 hidden group-hover:block z-50 w-80">
                <div className="bg-popover border border-border rounded-lg shadow-lg p-3 text-xs">
                  <p className="font-medium mb-2">How to start Chrome with debugging:</p>
                  <ol className="list-decimal list-inside space-y-1 text-muted-foreground">
                    <li>Close all Chrome windows first</li>
                    <li>Click the button below to restart Chrome with debugging, or:</li>
                    <li>
                      Create a shortcut with target:{" "}
                      <code className="bg-muted px-1 rounded">
                        chrome --remote-debugging-port=9222
                      </code>
                    </li>
                  </ol>
                </div>
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={async () => {
              if (
                !confirm(
                  "This will close all Chrome windows and relaunch with debugging enabled. Continue?",
                )
              ) {
                return;
              }
              try {
                onLog("info", "Closing Chrome and relaunching with debug port...");
                const response = await tracedFetch(`${getApiBase()}/launch-debug-chrome`, {
                  method: "POST",
                });
                const result = await response.json();
                if (result.success) {
                  onLog(
                    "success",
                    "Chrome launched with debugging enabled. Navigate to your test page, then run the test.",
                  );
                } else {
                  onLog("error", `Failed to launch Chrome: ${result.error}`);
                }
              } catch (error) {
                onLog("error", `Failed to launch Chrome: ${error}`);
              }
            }}
            className={`text-xs px-2 py-1 ${getAccentColors("amber").bg} ${getAccentColors("amber").text} rounded hover:opacity-80 transition-colors`}
          >
            Restart Chrome with Debug Port
          </button>
        </div>
      )}
    </div>
  );
}

function VisualContextSettingsBar({ settings }: { settings: VisualContextSettings }) {
  return (
    <div className="flex flex-wrap items-center gap-4 text-sm text-muted-foreground py-3 border-t border-border">
      <div className="flex items-center gap-2">
        <ImageIcon className="w-4 h-4" />
        <span>Auto-Refine Visual Context:</span>
      </div>
      <label htmlFor="vc-screenshot" className="flex items-center gap-1.5 cursor-pointer">
        <input
          id="vc-screenshot"
          type="checkbox"
          checked={settings.includeScreenshot}
          onChange={(e) => settings.setIncludeScreenshot(e.target.checked)}
          className="rounded border-border"
        />
        <span>Screenshot</span>
      </label>
      <label htmlFor="vc-trace" className="flex items-center gap-1.5 cursor-pointer">
        <input
          id="vc-trace"
          type="checkbox"
          checked={settings.includeTraceData}
          onChange={(e) => settings.setIncludeTraceData(e.target.checked)}
          className="rounded border-border"
        />
        <span>Trace</span>
      </label>
      <label
        htmlFor="vc-video"
        className="flex items-center gap-1.5 cursor-pointer"
        title="Video uses significantly more tokens. Only recommended for complex failures."
      >
        <input
          id="vc-video"
          type="checkbox"
          checked={settings.includeVideo}
          onChange={(e) => settings.setIncludeVideo(e.target.checked)}
          className="rounded border-border"
        />
        <span>Video after</span>
        <input
          id="vc-video-iterations"
          type="number"
          min={1}
          max={10}
          value={settings.includeVideoAfterIterations}
          onChange={(e) =>
            settings.setIncludeVideoAfterIterations(Math.max(1, parseInt(e.target.value) || 3))
          }
          disabled={!settings.includeVideo}
          className={`w-12 px-1 py-0.5 text-center bg-background border border-border rounded ${!settings.includeVideo ? "opacity-50" : ""}`}
        />
        <span className={!settings.includeVideo ? "opacity-50" : ""}>iterations</span>
        <span title="Video uses significantly more tokens. Enables after N failed iterations.">
          <Info className="w-3.5 h-3.5 text-muted-foreground" />
        </span>
      </label>
      <div className="border-l border-border pl-4 ml-2">
        <label htmlFor="vc-max-iterations" className="flex items-center gap-1.5 cursor-pointer">
          <span>Max iterations:</span>
          <input
            id="vc-max-iterations"
            type="number"
            min={1}
            max={50}
            value={settings.autoRefineMaxIterations}
            onChange={(e) =>
              settings.setAutoRefineMaxIterations(
                Math.max(1, Math.min(50, parseInt(e.target.value) || 10)),
              )
            }
            className="w-12 px-1 py-0.5 text-center bg-background border border-border rounded"
          />
        </label>
      </div>
    </div>
  );
}
