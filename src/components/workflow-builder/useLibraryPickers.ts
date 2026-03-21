import { useReducer, useCallback } from "react";
import type {
  WorkflowPhase,
  UnifiedStep,
  SavedPrompt,
  SavedShellCommand,
  WorkflowStep,
  UnifiedWorkflow,
} from "../../types";
import { generateStepId } from "../../types";

interface PickerState {
  promptPickerOpen: boolean;
  promptPickerPhase: WorkflowPhase;
  shellCommandPickerOpen: boolean;
  shellCommandPickerPhase: WorkflowPhase;
  checkPickerOpen: boolean;
  checkPickerPhase: WorkflowPhase;
  testPickerOpen: boolean;
  testPickerPhase: WorkflowPhase;
  checkGroupPickerOpen: boolean;
  checkGroupPickerPhase: WorkflowPhase;
  mcpPickerOpen: boolean;
  mcpPickerPhase: WorkflowPhase;
  workflowPickerOpen: boolean;
  workflowPickerPhase: WorkflowPhase;
  aiGenerateModalOpen: boolean;
  dropdownOpen: boolean;
  dropdownPhase: WorkflowPhase | null;
}

type PickerAction =
  | { type: "OPEN_PROMPT_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_PROMPT_PICKER" }
  | { type: "OPEN_SHELL_COMMAND_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_SHELL_COMMAND_PICKER" }
  | { type: "OPEN_CHECK_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_CHECK_PICKER" }
  | { type: "OPEN_TEST_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_TEST_PICKER" }
  | { type: "OPEN_CHECK_GROUP_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_CHECK_GROUP_PICKER" }
  | { type: "OPEN_MCP_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_MCP_PICKER" }
  | { type: "OPEN_WORKFLOW_PICKER"; phase: WorkflowPhase }
  | { type: "CLOSE_WORKFLOW_PICKER" }
  | { type: "OPEN_AI_GENERATE_MODAL" }
  | { type: "CLOSE_AI_GENERATE_MODAL" }
  | { type: "OPEN_DROPDOWN"; phase: WorkflowPhase }
  | { type: "CLOSE_DROPDOWN" };

const initialPickerState: PickerState = {
  promptPickerOpen: false,
  promptPickerPhase: "setup",
  shellCommandPickerOpen: false,
  shellCommandPickerPhase: "setup",
  checkPickerOpen: false,
  checkPickerPhase: "verification",
  testPickerOpen: false,
  testPickerPhase: "verification",
  checkGroupPickerOpen: false,
  checkGroupPickerPhase: "verification",
  mcpPickerOpen: false,
  mcpPickerPhase: "setup",
  workflowPickerOpen: false,
  workflowPickerPhase: "setup",
  aiGenerateModalOpen: false,
  dropdownOpen: false,
  dropdownPhase: null,
};

function pickerReducer(state: PickerState, action: PickerAction): PickerState {
  switch (action.type) {
    case "OPEN_PROMPT_PICKER":
      return { ...state, promptPickerOpen: true, promptPickerPhase: action.phase };
    case "CLOSE_PROMPT_PICKER":
      return { ...state, promptPickerOpen: false };
    case "OPEN_SHELL_COMMAND_PICKER":
      return { ...state, shellCommandPickerOpen: true, shellCommandPickerPhase: action.phase };
    case "CLOSE_SHELL_COMMAND_PICKER":
      return { ...state, shellCommandPickerOpen: false };
    case "OPEN_CHECK_PICKER":
      return { ...state, checkPickerOpen: true, checkPickerPhase: action.phase };
    case "CLOSE_CHECK_PICKER":
      return { ...state, checkPickerOpen: false };
    case "OPEN_TEST_PICKER":
      return { ...state, testPickerOpen: true, testPickerPhase: action.phase };
    case "CLOSE_TEST_PICKER":
      return { ...state, testPickerOpen: false };
    case "OPEN_CHECK_GROUP_PICKER":
      return { ...state, checkGroupPickerOpen: true, checkGroupPickerPhase: action.phase };
    case "CLOSE_CHECK_GROUP_PICKER":
      return { ...state, checkGroupPickerOpen: false };
    case "OPEN_MCP_PICKER":
      return { ...state, mcpPickerOpen: true, mcpPickerPhase: action.phase };
    case "CLOSE_MCP_PICKER":
      return { ...state, mcpPickerOpen: false };
    case "OPEN_WORKFLOW_PICKER":
      return { ...state, workflowPickerOpen: true, workflowPickerPhase: action.phase };
    case "CLOSE_WORKFLOW_PICKER":
      return { ...state, workflowPickerOpen: false };
    case "OPEN_AI_GENERATE_MODAL":
      return { ...state, aiGenerateModalOpen: true };
    case "CLOSE_AI_GENERATE_MODAL":
      return { ...state, aiGenerateModalOpen: false };
    case "OPEN_DROPDOWN":
      return { ...state, dropdownOpen: true, dropdownPhase: action.phase };
    case "CLOSE_DROPDOWN":
      return { ...state, dropdownOpen: false };
    default:
      return state;
  }
}

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

interface UseLibraryPickersArgs {
  addStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  updateStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  getSelectedStep: () => UnifiedStep | null;
}

export function useLibraryPickers({ addStep, updateStep, getSelectedStep }: UseLibraryPickersArgs) {
  const [pickerState, dispatch] = useReducer(pickerReducer, initialPickerState);

  const handleAddStep = useCallback((phase: WorkflowPhase) => {
    dispatch({ type: "OPEN_DROPDOWN", phase });
  }, []);

  const handleStepAdded = useCallback(
    (step: UnifiedStep, phase: WorkflowPhase) => {
      addStep(step, phase);
      dispatch({ type: "CLOSE_DROPDOWN" });
    },
    [addStep],
  );

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
      dispatch({ type: "CLOSE_PROMPT_PICKER" });
    },
    [addStep],
  );

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
      dispatch({ type: "CLOSE_SHELL_COMMAND_PICKER" });
    },
    [addStep],
  );

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
      dispatch({ type: "CLOSE_CHECK_PICKER" });
    },
    [addStep],
  );

  const handleTestSelected = useCallback(
    (test: SavedTest, phase: WorkflowPhase) => {
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
      dispatch({ type: "CLOSE_TEST_PICKER" });
    },
    [addStep],
  );

  const handleCheckGroupSelected = useCallback(
    (group: CheckGroup, _checksInGroup: { id: string; name: string }[], phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: group.name,
        mode: "check_group",
        check_group_id: group.id,
      };

      addStep(step, phase);
      dispatch({ type: "CLOSE_CHECK_GROUP_PICKER" });
    },
    [addStep],
  );

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
      dispatch({ type: "CLOSE_MCP_PICKER" });
    },
    [addStep],
  );

  const handleOpenWorkflowPicker = useCallback((phase: WorkflowPhase) => {
    dispatch({ type: "OPEN_WORKFLOW_PICKER", phase });
  }, []);

  const handleWorkflowSelected = useCallback(
    (workflow: UnifiedWorkflow, phase: WorkflowPhase) => {
      const selected = getSelectedStep();
      if (selected && selected.type === "workflow") {
        const updatedStep = {
          ...selected,
          name: workflow.name,
          workflow_id: workflow.id,
          workflow_name: workflow.name,
        } as UnifiedStep;
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
      dispatch({ type: "CLOSE_WORKFLOW_PICKER" });
    },
    [addStep, getSelectedStep, updateStep],
  );

  return {
    pickerState,
    pickerDispatch: dispatch,
    handleAddStep,
    handleStepAdded,
    handlePromptSelected,
    handleShellCommandSelected,
    handleCheckSelected,
    handleTestSelected,
    handleCheckGroupSelected,
    handleMcpToolSelected,
    handleOpenWorkflowPicker,
    handleWorkflowSelected,
  };
}

export type { PickerState, SavedCheck, SavedTest, CheckGroup, McpServerConfig, McpToolInfo };
