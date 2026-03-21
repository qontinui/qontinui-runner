import type {
  PlaywrightScript,
  PlaywrightResult,
  ScriptViewMode,
  ScriptExecutionState,
  DisplayMode,
} from "../../types";

export type LogLevel = "info" | "warning" | "error" | "debug" | "success";

export type {
  PlaywrightScript,
  PlaywrightResult,
  ScriptViewMode,
  ScriptExecutionState,
  DisplayMode,
};

export const DEFAULT_SCRIPT_CONTENT = `import { test, expect } from '@playwright/test';

test('example test', async ({ page }) => {
  // Navigate to the target page
  await page.goto('/');

  // Add your test assertions here
  await expect(page).toHaveTitle(/./);
});
`;

export interface FormState {
  name: string;
  description: string;
  aiInstructions: string;
  targetUrl: string;
  scriptContent: string;
  category: string;
  tags: string;
  timeoutSeconds: number;
  displayMode: DisplayMode;
  browser: "chromium" | "firefox" | "webkit";
  workflowObjective: string;
  successCriteria: string[];
  isWorkflowAutomation: boolean;
}

export type FormAction =
  | { type: "SET_NAME"; value: string }
  | { type: "SET_DESCRIPTION"; value: string }
  | { type: "SET_AI_INSTRUCTIONS"; value: string }
  | { type: "SET_TARGET_URL"; value: string }
  | { type: "SET_SCRIPT_CONTENT"; value: string }
  | { type: "SET_CATEGORY"; value: string }
  | { type: "SET_TAGS"; value: string }
  | { type: "SET_TIMEOUT_SECONDS"; value: number }
  | { type: "SET_DISPLAY_MODE"; value: DisplayMode }
  | { type: "SET_BROWSER"; value: "chromium" | "firefox" | "webkit" }
  | { type: "SET_WORKFLOW_OBJECTIVE"; value: string }
  | { type: "SET_SUCCESS_CRITERIA"; value: string[] }
  | { type: "SET_IS_WORKFLOW_AUTOMATION"; value: boolean }
  | { type: "SET_VIEW_MODE"; value: ScriptViewMode }
  | { type: "RESET" }
  | { type: "LOAD_SCRIPT"; script: PlaywrightScript }
  | { type: "LOAD_PERSISTED"; state: Partial<FormState> };

export const DEFAULT_FORM_STATE: FormState = {
  name: "",
  description: "",
  aiInstructions: "",
  targetUrl: "",
  scriptContent: DEFAULT_SCRIPT_CONTENT,
  category: "",
  tags: "",
  timeoutSeconds: 60,
  displayMode: "headless",
  browser: "chromium",
  workflowObjective: "",
  successCriteria: [],
  isWorkflowAutomation: false,
};

export function formReducer(state: FormState, action: FormAction): FormState {
  switch (action.type) {
    case "SET_NAME":
      return { ...state, name: action.value };
    case "SET_DESCRIPTION":
      return { ...state, description: action.value };
    case "SET_AI_INSTRUCTIONS":
      return { ...state, aiInstructions: action.value };
    case "SET_TARGET_URL":
      return { ...state, targetUrl: action.value };
    case "SET_SCRIPT_CONTENT":
      return { ...state, scriptContent: action.value };
    case "SET_CATEGORY":
      return { ...state, category: action.value };
    case "SET_TAGS":
      return { ...state, tags: action.value };
    case "SET_TIMEOUT_SECONDS":
      return { ...state, timeoutSeconds: action.value };
    case "SET_DISPLAY_MODE":
      return { ...state, displayMode: action.value };
    case "SET_BROWSER":
      return { ...state, browser: action.value };
    case "SET_WORKFLOW_OBJECTIVE":
      return { ...state, workflowObjective: action.value };
    case "SET_SUCCESS_CRITERIA":
      return { ...state, successCriteria: action.value };
    case "SET_IS_WORKFLOW_AUTOMATION":
      return { ...state, isWorkflowAutomation: action.value };
    case "RESET":
      return { ...DEFAULT_FORM_STATE };
    case "LOAD_SCRIPT":
      return {
        name: action.script.name || "",
        description: action.script.description || "",
        aiInstructions: action.script.ai_instructions || "",
        targetUrl: action.script.target_url || "",
        scriptContent: action.script.script_content || DEFAULT_SCRIPT_CONTENT,
        category: action.script.category || "",
        tags: action.script.tags?.join(", ") || "",
        timeoutSeconds: action.script.timeout_seconds || 60,
        displayMode: (action.script.display_mode as DisplayMode) || "headless",
        browser: action.script.browser || "chromium",
        workflowObjective: action.script.workflow_objective || "",
        successCriteria: action.script.success_criteria || [],
        isWorkflowAutomation: action.script.is_workflow_automation || false,
      };
    case "LOAD_PERSISTED":
      return { ...state, ...action.state };
    default:
      return state;
  }
}

export function formStateToPayload(form: FormState) {
  return {
    name: form.name,
    description: form.description,
    ai_instructions: form.aiInstructions || undefined,
    target_url: form.targetUrl,
    script_content: form.scriptContent,
    category: form.category,
    tags: form.tags
      .split(",")
      .map((t: string) => t.trim())
      .filter(Boolean),
    timeout_seconds: form.timeoutSeconds,
    display_mode: form.displayMode,
    browser: form.browser,
    workflow_objective: form.workflowObjective || undefined,
    success_criteria: form.successCriteria.length > 0 ? form.successCriteria : undefined,
    is_workflow_automation: form.isWorkflowAutomation,
  };
}
