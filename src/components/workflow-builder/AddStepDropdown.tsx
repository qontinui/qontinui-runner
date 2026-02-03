/**
 * AddStepDropdown.tsx
 *
 * Hierarchical dropdown menu for adding steps to a workflow.
 * Uses drill-down navigation with back button, similar to the legacy AI Builder.
 * Step types are grouped by their execution phase (Setup, Verification, Agentic).
 */

import React, { useState, useRef, useEffect } from "react";
import {
  Plus,
  ChevronDown,
  ChevronRight,
  ArrowLeft,
  FileCode,
  Navigation,
  GitBranch,
  Layers,
  MousePointer2,
  MousePointerClick,
  MousePointer,
  Keyboard,
  Command,
  ArrowUpDown,
  TestTube2,
  Eye,
  Code,
  Package,
  Terminal,
  Camera,
  MessageSquare,
  Globe,
  Library,
  Upload,
  Bot,
  CheckCircle2,
  AlertTriangle,
  AlignLeft,
  FileType,
  Shield,
  Plug,
} from "lucide-react";
import type { WorkflowPhase, UnifiedStep } from "../../types";
import type { GuiActionType, TestType, CheckType } from "../../types/unified-workflow";
import { STEP_TYPES, GUI_ACTION_TYPES, generateStepId } from "../../types";

// =============================================================================
// Types
// =============================================================================

type CategoryId =
  | "root"
  | "setup"
  | "verification"
  | "agentic"
  | "completion"
  // GUI actions for all non-agentic phases
  | "gui_actions_setup"
  | "gui_actions_verification"
  | "gui_actions_completion"
  // API requests for all non-agentic phases
  | "api_setup"
  | "api_verification"
  | "api_completion"
  // Prompts for all phases
  | "prompt_setup"
  | "prompt_verification"
  | "prompt_agentic"
  | "prompt_completion"
  // Shell commands for all non-agentic phases
  | "shell_command_setup"
  | "shell_command_verification"
  | "shell_command_completion"
  // Tests for all non-agentic phases
  | "tests_setup"
  | "tests_verification"
  | "tests_completion"
  // Checks for all non-agentic phases
  | "checks_setup"
  | "checks_verification"
  | "checks_completion"
  // Macros for all non-agentic phases
  | "macros_setup"
  | "macros_verification"
  | "macros_completion"
  // Workflows for all non-agentic phases
  | "workflows_setup"
  | "workflows_verification"
  | "workflows_completion"
  // States for all non-agentic phases
  | "states_setup"
  | "states_verification"
  | "states_completion"
  // MCP for all non-agentic phases
  | "mcp_setup"
  | "mcp_verification"
  | "mcp_completion";

// =============================================================================
// Icon Mapping
// =============================================================================

const ICON_MAP: Record<string, React.ElementType> = {
  FileCode,
  Navigation,
  GitBranch,
  Layers,
  MousePointer2,
  MousePointerClick,
  MousePointer,
  Keyboard,
  Command,
  ArrowUpDown,
  TestTube2,
  Eye,
  Code,
  Package,
  Terminal,
  Camera,
  MessageSquare,
  Globe,
  Bot,
  CheckCircle2,
  AlertTriangle,
  AlignLeft,
  FileType,
  Shield,
  Plug,
};

// =============================================================================
// Helper Functions
// =============================================================================

const getColorClass = (color: string): string => {
  const colorMap: Record<string, string> = {
    emerald: "text-emerald-400",
    blue: "text-blue-400",
    purple: "text-purple-400",
    pink: "text-pink-400",
    orange: "text-orange-400",
    green: "text-green-400",
    amber: "text-amber-400",
    cyan: "text-cyan-400",
    violet: "text-violet-400",
    indigo: "text-indigo-400",
  };
  return colorMap[color] || "text-zinc-400";
};

// =============================================================================
// Component
// =============================================================================

interface AddStepDropdownProps {
  /** If provided, only show steps for this phase */
  filterPhase?: WorkflowPhase;
  /** Called when a step is selected */
  onAddStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  /** Whether the dropdown is open */
  isOpen: boolean;
  /** Called to close the dropdown */
  onClose: () => void;
  /** Called to open the API library picker (phase is passed) */
  onOpenApiLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the cURL import dialog (phase is passed) */
  onOpenCurlImport?: (phase: WorkflowPhase) => void;
  /** Called to open the prompt library picker (phase is passed) */
  onOpenPromptLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the shell command library picker (phase is passed) */
  onOpenShellCommandLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the check library picker (phase is passed) */
  onOpenCheckLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the check group library picker (phase is passed) */
  onOpenCheckGroupLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the macro library picker (phase is passed) */
  onOpenMacroLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the playwright script library picker (phase is passed) */
  onOpenPlaywrightScriptLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the test library picker (phase is passed) */
  onOpenTestLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the workflow library picker (phase is passed) */
  onOpenWorkflowLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the state library picker (phase is passed) */
  onOpenStateLibrary?: (phase: WorkflowPhase) => void;
  /** Called to open the MCP server tool picker (phase is passed) */
  onOpenMcpServerToolPicker?: (phase: WorkflowPhase) => void;
}

export function AddStepDropdown({
  filterPhase,
  onAddStep,
  isOpen,
  onClose,
  onOpenApiLibrary,
  onOpenCurlImport,
  onOpenPromptLibrary,
  onOpenShellCommandLibrary,
  onOpenCheckLibrary,
  onOpenCheckGroupLibrary,
  onOpenMacroLibrary,
  onOpenPlaywrightScriptLibrary,
  onOpenTestLibrary,
  onOpenWorkflowLibrary,
  onOpenStateLibrary,
  onOpenMcpServerToolPicker,
}: AddStepDropdownProps) {
  // Navigation state - start at phase level if filterPhase provided
  const [activeCategory, setActiveCategory] = useState<CategoryId>(
    filterPhase ? filterPhase : "root",
  );

  const dropdownRef = useRef<HTMLDivElement>(null);

  // Reset navigation when dropdown closes/opens
  useEffect(() => {
    if (isOpen) {
      setActiveCategory(filterPhase ? filterPhase : "root");
    }
  }, [isOpen, filterPhase]);

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        onClose();
      }
    }

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [isOpen, onClose]);

  // Get parent category for back navigation
  const getParentCategory = (category: CategoryId): CategoryId => {
    switch (category) {
      case "setup":
      case "verification":
      case "agentic":
      case "completion":
        return "root";
      // Setup phase submenus
      case "gui_actions_setup":
      case "api_setup":
      case "prompt_setup":
      case "shell_command_setup":
      case "macros_setup":
      case "workflows_setup":
      case "states_setup":
      case "mcp_setup":
      case "tests_setup":
      case "checks_setup":
        return "setup";
      // Verification phase submenus
      case "gui_actions_verification":
      case "api_verification":
      case "prompt_verification":
      case "shell_command_verification":
      case "macros_verification":
      case "workflows_verification":
      case "states_verification":
      case "mcp_verification":
      case "tests_verification":
      case "checks_verification":
        return "verification";
      // Agentic phase submenus
      case "prompt_agentic":
        return "agentic";
      // Completion phase submenus
      case "gui_actions_completion":
      case "api_completion":
      case "prompt_completion":
      case "shell_command_completion":
      case "macros_completion":
      case "workflows_completion":
      case "states_completion":
      case "mcp_completion":
      case "tests_completion":
      case "checks_completion":
        return "completion";
      default:
        return "root";
    }
  };

  // Navigate back
  const navigateBack = () => {
    setActiveCategory(getParentCategory(activeCategory));
  };

  // Handle step creation
  const handleStepClick = (stepType: string, phase: WorkflowPhase, guiAction?: GuiActionType) => {
    const id = generateStepId();

    let step: UnifiedStep;

    switch (stepType) {
      case "script":
        step = {
          id,
          type: "script",
          phase: phase as "setup" | "completion",
          name: phase === "completion" ? "Final Script" : "New Script",
          refinement_enabled: true,
        };
        break;

      case "state":
        step = {
          id,
          type: "state",
          phase: phase as "setup" | "verification" | "completion",
          name: "Navigate to State",
          state_id: "",
        };
        break;

      case "workflow_ref":
        step = {
          id,
          type: "workflow_ref",
          phase: phase as "setup" | "verification" | "completion",
          name: "Run Workflow",
          workflow_id: "",
        };
        break;

      case "macro":
        step = {
          id,
          type: "macro",
          phase: phase as "setup" | "verification" | "completion",
          name: "Run Macro",
          macro_id: "",
        };
        break;

      case "gui_action":
        step = {
          id,
          type: "gui_action",
          phase: phase as "setup" | "verification" | "completion",
          name: guiAction
            ? GUI_ACTION_TYPES.find((g) => g.type === guiAction)?.label || "GUI Action"
            : "Click",
          action: guiAction || "click",
        };
        break;

      case "api_request":
        step = {
          id,
          type: "api_request",
          phase: phase as "setup" | "verification" | "completion",
          name: phase === "completion" ? "Final API Request" : "API Request",
          method: "GET",
          url: "",
        };
        break;

      case "test_playwright":
      case "test_vision":
      case "test_python":
      case "test_repository":
      case "test_custom": {
        const testTypeMap: Record<string, TestType> = {
          test_playwright: "playwright",
          test_vision: "qontinui_vision",
          test_python: "python",
          test_repository: "repository",
          test_custom: "custom_command",
        };
        step = {
          id,
          type: "test",
          phase: phase as "setup" | "verification" | "completion",
          name: STEP_TYPES[phase].find((s) => s.type === stepType)?.label || "New Test",
          test_type: testTypeMap[stepType] || "custom_command",
          is_critical: true,
        };
        break;
      }

      case "check_lint":
      case "check_format":
      case "check_typecheck":
      case "check_analyze":
      case "check_security":
      case "check_custom": {
        const checkTypeMap: Record<string, CheckType> = {
          check_lint: "lint",
          check_format: "format",
          check_typecheck: "typecheck",
          check_analyze: "analyze",
          check_security: "security",
          check_custom: "custom_command",
        };
        step = {
          id,
          type: "check",
          phase: phase as "setup" | "verification" | "completion",
          name: STEP_TYPES[phase].find((s) => s.type === stepType)?.label || "New Check",
          check_type: checkTypeMap[stepType] || "custom_command",
          auto_fix: checkTypeMap[stepType] === "format", // Default formatters to auto-fix
          is_blocking: true,
        };
        break;
      }

      case "screenshot":
        step = {
          id,
          type: "screenshot",
          phase: phase as "setup" | "verification" | "completion",
          name: "Screenshot",
        };
        break;

      case "prompt": {
        const promptNames: Record<WorkflowPhase, string> = {
          setup: "AI Setup Task",
          verification: "AI Verification",
          agentic: "Prompt",
          completion: "AI Completion Task",
        };
        step = {
          id,
          type: "prompt",
          phase: phase as "setup" | "verification" | "agentic" | "completion",
          name: promptNames[phase] || "Prompt",
          content: "",
          is_blocking: phase === "verification" ? true : undefined,
        };
        break;
      }

      case "shell_command": {
        const shellCommandNames: Record<WorkflowPhase, string> = {
          setup: "Setup Command",
          verification: "Verification Command",
          agentic: "Command",
          completion: "Completion Command",
        };
        step = {
          id,
          type: "shell_command",
          phase: phase as "setup" | "verification" | "completion",
          name: shellCommandNames[phase] || "Shell Command",
          command: "",
          fail_on_error: true,
        };
        break;
      }

      case "mcp_call":
        step = {
          id,
          type: "mcp_call",
          phase: phase as "setup" | "verification" | "completion",
          name: "MCP Call",
          server_id: "",
          tool_name: "",
        };
        break;

      default:
        return;
    }

    onAddStep(step, phase);
    onClose();
  };

  // =============================================================================
  // Render: Root Menu (Phase Selection)
  // =============================================================================

  const renderRootMenu = () => {
    const phases: {
      id: WorkflowPhase;
      label: string;
      color: string;
      icon: React.ElementType;
      description: string;
    }[] = [
      {
        id: "setup",
        label: "Setup Phase",
        color: "text-blue-400",
        icon: FileCode,
        description: "Prepare the environment (once)",
      },
      {
        id: "verification",
        label: "Verification Phase",
        color: "text-green-400",
        icon: TestTube2,
        description: "Check success criteria (loop)",
      },
      {
        id: "agentic",
        label: "Agentic Phase",
        color: "text-amber-400",
        icon: MessageSquare,
        description: "AI-driven execution (loop)",
      },
      {
        id: "completion",
        label: "Completion Phase",
        color: "text-purple-400",
        icon: CheckCircle2,
        description: "Final actions (once)",
      },
    ];

    return (
      <div className="py-1">
        <div className="px-3 py-2 text-xs font-medium text-zinc-500 uppercase tracking-wider border-b border-zinc-700">
          Select Phase
        </div>
        {phases.map((phase) => {
          const Icon = phase.icon;
          const stepCount = STEP_TYPES[phase.id]?.length || 0;

          return (
            <button
              key={phase.id}
              onClick={() => setActiveCategory(phase.id)}
              className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
            >
              <Icon className={`w-4 h-4 ${phase.color}`} />
              <div className="flex-1 min-w-0">
                <div className="font-medium text-zinc-200">{phase.label}</div>
                <div className="text-xs text-zinc-500">{phase.description}</div>
              </div>
              <span className="text-xs text-zinc-500">{stepCount}</span>
              <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
            </button>
          );
        })}
      </div>
    );
  };

  // =============================================================================
  // Render: Phase Steps Menu
  // =============================================================================

  const renderPhaseMenu = (phase: WorkflowPhase) => {
    const phaseConfig: Record<
      WorkflowPhase,
      { label: string; color: string; icon: React.ElementType }
    > = {
      setup: { label: "Setup Phase", color: "text-blue-400", icon: FileCode },
      verification: { label: "Verification Phase", color: "text-green-400", icon: TestTube2 },
      agentic: { label: "Agentic Phase", color: "text-amber-400", icon: MessageSquare },
      completion: { label: "Completion Phase", color: "text-purple-400", icon: CheckCircle2 },
    };

    const config = phaseConfig[phase];
    const Icon = config.icon;
    const steps = STEP_TYPES[phase] || [];

    return (
      <div className="py-1">
        {/* Header with back button */}
        {!filterPhase && (
          <button
            onClick={navigateBack}
            className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
          >
            <ArrowLeft className="w-4 h-4 text-zinc-400" />
            <Icon className={`w-4 h-4 ${config.color}`} />
            <span className="text-zinc-200">{config.label}</span>
          </button>
        )}

        {filterPhase && (
          <div className="px-3 py-2 text-xs font-medium text-zinc-500 uppercase tracking-wider border-b border-zinc-700">
            {config.label}
          </div>
        )}

        {/* Step items */}
        <div className="max-h-80 overflow-y-auto py-1">
          {steps.map((stepType) => {
            // GUI Actions get their own submenu (all non-agentic phases)
            if (stepType.type === "gui_action") {
              const guiActionSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "gui_actions_setup",
                verification: "gui_actions_verification",
                agentic: null,
                completion: "gui_actions_completion",
              };
              const subCategoryId = guiActionSubCategories[phase];
              if (!subCategoryId) return null;

              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <MousePointer2 className="w-4 h-4 text-orange-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">GUI Actions</div>
                    <div className="text-xs text-zinc-500">Click, type, scroll, hotkey</div>
                  </div>
                  <span className="text-xs text-zinc-500">{GUI_ACTION_TYPES.length}</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // API Requests get their own submenu
            if (stepType.type === "api_request") {
              const subCategoryId =
                phase === "setup"
                  ? "api_setup"
                  : phase === "verification"
                    ? "api_verification"
                    : "api_completion";
              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId as CategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Globe className="w-4 h-4 text-cyan-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">API Requests</div>
                    <div className="text-xs text-zinc-500">New, from library, import cURL</div>
                  </div>
                  <span className="text-xs text-zinc-500">3</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Tests get their own submenu (all non-agentic phases)
            if (stepType.type.startsWith("test_")) {
              if (stepType.type !== "test_playwright") return null;
              const testsSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "tests_setup",
                verification: "tests_verification",
                agentic: null,
                completion: "tests_completion",
              };
              const subCategoryId = testsSubCategories[phase];
              if (!subCategoryId) return null;

              const testTypes = STEP_TYPES[phase].filter((s) => s.type.startsWith("test_"));
              return (
                <button
                  key={`tests-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <TestTube2 className="w-4 h-4 text-green-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">Tests</div>
                    <div className="text-xs text-zinc-500">Playwright, Python, Vision, etc.</div>
                  </div>
                  <span className="text-xs text-zinc-500">{testTypes.length}</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Checks get their own submenu (all non-agentic phases)
            if (stepType.type.startsWith("check_")) {
              if (stepType.type !== "check_lint") return null;
              const checksSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "checks_setup",
                verification: "checks_verification",
                agentic: null,
                completion: "checks_completion",
              };
              const subCategoryId = checksSubCategories[phase];
              if (!subCategoryId) return null;

              const checkTypes = STEP_TYPES[phase].filter((s) => s.type.startsWith("check_"));
              return (
                <button
                  key={`checks-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Shield className="w-4 h-4 text-cyan-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">Checks</div>
                    <div className="text-xs text-zinc-500">Lint, Format, Type Check</div>
                  </div>
                  <span className="text-xs text-zinc-500">{checkTypes.length}</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Prompts get their own submenu (New or From Library)
            if (stepType.type === "prompt") {
              const promptSubCategories: Record<WorkflowPhase, CategoryId> = {
                setup: "prompt_setup",
                verification: "prompt_verification",
                agentic: "prompt_agentic",
                completion: "prompt_completion",
              };
              const subCategoryId = promptSubCategories[phase];
              const promptLabels: Record<WorkflowPhase, string> = {
                setup: "AI Setup Task",
                verification: "AI Verification",
                agentic: "AI Prompt",
                completion: "AI Completion Task",
              };
              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <MessageSquare className="w-4 h-4 text-amber-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">{promptLabels[phase]}</div>
                    <div className="text-xs text-zinc-500">New prompt or from library</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Shell Commands get their own submenu (all non-agentic phases)
            if (stepType.type === "shell_command") {
              const shellCommandSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "shell_command_setup",
                verification: "shell_command_verification",
                agentic: null,
                completion: "shell_command_completion",
              };
              const subCategoryId = shellCommandSubCategories[phase];
              if (!subCategoryId) return null;

              const shellCommandLabels: Record<WorkflowPhase, string> = {
                setup: "Setup Command",
                verification: "Verification Command",
                agentic: "",
                completion: "Completion Command",
              };
              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Terminal className="w-4 h-4 text-gray-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">{shellCommandLabels[phase]}</div>
                    <div className="text-xs text-zinc-500">New command or from library</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Macros get their own submenu (From Library or New)
            if (stepType.type === "macro") {
              const macroSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "macros_setup",
                verification: "macros_verification",
                agentic: null,
                completion: "macros_completion",
              };
              const subCategoryId = macroSubCategories[phase];
              if (!subCategoryId) return null;

              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Layers className="w-4 h-4 text-emerald-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">Macros</div>
                    <div className="text-xs text-zinc-500">Run recorded action sequences</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Workflow references get their own submenu
            if (stepType.type === "workflow_ref") {
              const workflowSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "workflows_setup",
                verification: "workflows_verification",
                agentic: null,
                completion: "workflows_completion",
              };
              const subCategoryId = workflowSubCategories[phase];
              if (!subCategoryId) return null;

              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <GitBranch className="w-4 h-4 text-blue-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">Run Workflow</div>
                    <div className="text-xs text-zinc-500">Execute another workflow as a step</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // State navigation gets its own submenu
            if (stepType.type === "state") {
              const stateSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "states_setup",
                verification: "states_verification",
                agentic: null,
                completion: "states_completion",
              };
              const subCategoryId = stateSubCategories[phase];
              if (!subCategoryId) return null;

              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Navigation className="w-4 h-4 text-purple-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">Navigate to State</div>
                    <div className="text-xs text-zinc-500">Go to a specific application state</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // MCP calls get their own submenu
            if (stepType.type === "mcp_call") {
              const mcpSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "mcp_setup",
                verification: "mcp_verification",
                agentic: null,
                completion: "mcp_completion",
              };
              const subCategoryId = mcpSubCategories[phase];
              if (!subCategoryId) return null;

              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Plug className="w-4 h-4 text-indigo-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">MCP Call</div>
                    <div className="text-xs text-zinc-500">Call a tool on an MCP server</div>
                  </div>
                  <span className="text-xs text-zinc-500">2</span>
                  <ChevronRight className="w-4 h-4 text-zinc-500 group-hover:text-zinc-300 transition-colors" />
                </button>
              );
            }

            // Regular step types - direct click to add
            const StepIcon = ICON_MAP[stepType.icon] || Plus;
            const colorClass = getColorClass(stepType.color);

            return (
              <button
                key={`${stepType.type}-${phase}`}
                onClick={() => handleStepClick(stepType.type, phase)}
                className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
              >
                <StepIcon className={`w-4 h-4 ${colorClass}`} />
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-zinc-200">{stepType.label}</div>
                  <div className="text-xs text-zinc-500">{stepType.description}</div>
                </div>
              </button>
            );
          })}
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: GUI Actions Submenu
  // =============================================================================

  const renderGuiActionsMenu = (phase: WorkflowPhase) => (
    <div className="py-1">
      {/* Header with back button */}
      <button
        onClick={navigateBack}
        className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
      >
        <ArrowLeft className="w-4 h-4 text-zinc-400" />
        <MousePointer2 className="w-4 h-4 text-orange-400" />
        <span className="text-zinc-200">GUI Actions</span>
      </button>

      {/* Action items */}
      <div className="max-h-80 overflow-y-auto py-1">
        {GUI_ACTION_TYPES.map((action) => {
          const ActionIcon = ICON_MAP[action.icon] || MousePointer2;
          return (
            <button
              key={action.type}
              onClick={() => handleStepClick("gui_action", phase, action.type)}
              className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
            >
              <ActionIcon className="w-4 h-4 text-orange-400" />
              <div className="flex-1 min-w-0">
                <div className="font-medium text-zinc-200">{action.label}</div>
                <div className="text-xs text-zinc-500">{action.description}</div>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );

  // =============================================================================
  // Render: Tests Submenu
  // =============================================================================

  const renderTestsMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <TestTube2 className="w-4 h-4 text-green-400" />
          <span className="text-zinc-200">Tests</span>
        </button>

        {/* Test options */}
        <div className="py-1">
          {/* From Test Library - select a saved test */}
          <button
            onClick={() => {
              if (onOpenTestLibrary) {
                onOpenTestLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenTestLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-green-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Test Library</div>
              <div className="text-xs text-zinc-500">Select a saved test from Test Builder</div>
            </div>
          </button>

          {/* Playwright Script Library - for backwards compatibility */}
          <button
            onClick={() => {
              if (onOpenPlaywrightScriptLibrary) {
                onOpenPlaywrightScriptLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenPlaywrightScriptLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Code className="w-4 h-4 text-green-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">Playwright Script Library</div>
              <div className="text-xs text-zinc-500">Select a saved Playwright script</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: Checks Submenu
  // =============================================================================

  const renderChecksMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <Shield className="w-4 h-4 text-cyan-400" />
          <span className="text-zinc-200">Checks</span>
        </button>

        {/* Check options */}
        <div className="py-1">
          {/* From Library - select a saved check */}
          <button
            onClick={() => {
              if (onOpenCheckLibrary) {
                onOpenCheckLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenCheckLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-cyan-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Library</div>
              <div className="text-xs text-zinc-500">Select a saved check from Check Builder</div>
            </div>
          </button>

          {/* Check Group - run all checks in a group */}
          <button
            onClick={() => {
              if (onOpenCheckGroupLibrary) {
                onOpenCheckGroupLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenCheckGroupLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Layers className="w-4 h-4 text-cyan-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">Check Group</div>
              <div className="text-xs text-zinc-500">Run all checks in a saved group</div>
            </div>
          </button>

          {/* Divider */}
          <div className="border-t border-zinc-700 my-1" />

          {/* New Custom Check - for quick inline check */}
          <button
            onClick={() => handleStepClick("check_custom", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-zinc-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Custom Check</div>
              <div className="text-xs text-zinc-500">
                Create an inline check with a custom command
              </div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: Macros Submenu
  // =============================================================================

  const renderMacrosMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <Layers className="w-4 h-4 text-emerald-400" />
          <span className="text-zinc-200">Macros</span>
        </button>

        {/* Macro options */}
        <div className="py-1">
          {/* From Library - select a saved macro */}
          <button
            onClick={() => {
              if (onOpenMacroLibrary) {
                onOpenMacroLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenMacroLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-emerald-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Library</div>
              <div className="text-xs text-zinc-500">Select a saved macro from Macro Manager</div>
            </div>
          </button>

          {/* Divider */}
          <div className="border-t border-zinc-700 my-1" />

          {/* New Macro - create inline */}
          <button
            onClick={() => handleStepClick("macro", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-zinc-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Inline Macro</div>
              <div className="text-xs text-zinc-500">Create a new macro step inline</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: Workflows Submenu
  // =============================================================================

  const renderWorkflowsMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <GitBranch className="w-4 h-4 text-blue-400" />
          <span className="text-zinc-200">Run Workflow</span>
        </button>

        {/* Workflow options */}
        <div className="py-1">
          {/* From Library - select a saved workflow */}
          <button
            onClick={() => {
              if (onOpenWorkflowLibrary) {
                onOpenWorkflowLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenWorkflowLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-blue-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Library</div>
              <div className="text-xs text-zinc-500">Select a saved workflow to run</div>
            </div>
          </button>

          {/* Divider */}
          <div className="border-t border-zinc-700 my-1" />

          {/* New inline workflow ref */}
          <button
            onClick={() => handleStepClick("workflow_ref", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-zinc-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Inline Reference</div>
              <div className="text-xs text-zinc-500">Create a workflow reference manually</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: States Submenu
  // =============================================================================

  const renderStatesMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <Navigation className="w-4 h-4 text-purple-400" />
          <span className="text-zinc-200">Navigate to State</span>
        </button>

        {/* State options */}
        <div className="py-1">
          {/* From Library - select a state */}
          <button
            onClick={() => {
              if (onOpenStateLibrary) {
                onOpenStateLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenStateLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-purple-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From State Machine</div>
              <div className="text-xs text-zinc-500">Select a state from the loaded config</div>
            </div>
          </button>

          {/* Divider */}
          <div className="border-t border-zinc-700 my-1" />

          {/* New inline state step */}
          <button
            onClick={() => handleStepClick("state", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-zinc-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Inline State</div>
              <div className="text-xs text-zinc-500">Enter a state ID manually</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: MCP Calls Submenu
  // =============================================================================

  const renderMcpMenu = (phase: WorkflowPhase) => {
    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <Plug className="w-4 h-4 text-indigo-400" />
          <span className="text-zinc-200">MCP Call</span>
        </button>

        {/* MCP options */}
        <div className="py-1">
          {/* From configured servers */}
          <button
            onClick={() => {
              if (onOpenMcpServerToolPicker) {
                onOpenMcpServerToolPicker(phase);
                onClose();
              }
            }}
            disabled={!onOpenMcpServerToolPicker}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-indigo-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Configured Servers</div>
              <div className="text-xs text-zinc-500">Select an MCP server and tool</div>
            </div>
          </button>

          {/* Divider */}
          <div className="border-t border-zinc-700 my-1" />

          {/* New inline MCP call */}
          <button
            onClick={() => handleStepClick("mcp_call", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-zinc-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Inline MCP Call</div>
              <div className="text-xs text-zinc-500">Enter server and tool details manually</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: API Requests Submenu
  // =============================================================================

  const renderApiMenu = (phase: WorkflowPhase) => (
    <div className="py-1">
      {/* Header with back button */}
      <button
        onClick={navigateBack}
        className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
      >
        <ArrowLeft className="w-4 h-4 text-zinc-400" />
        <Globe className="w-4 h-4 text-cyan-400" />
        <span className="text-zinc-200">API Requests</span>
      </button>

      {/* API options */}
      <div className="py-1">
        {/* New Request */}
        <button
          onClick={() => handleStepClick("api_request", phase)}
          className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
        >
          <Plus className="w-4 h-4 text-cyan-400" />
          <div className="flex-1 min-w-0">
            <div className="font-medium text-zinc-200">New Request</div>
            <div className="text-xs text-zinc-500">Create a blank API request</div>
          </div>
        </button>

        {/* From Library */}
        <button
          onClick={() => {
            if (onOpenApiLibrary) {
              onOpenApiLibrary(phase);
              onClose();
            }
          }}
          disabled={!onOpenApiLibrary}
          className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Library className="w-4 h-4 text-cyan-400" />
          <div className="flex-1 min-w-0">
            <div className="font-medium text-zinc-200">From Library</div>
            <div className="text-xs text-zinc-500">Use a saved API request template</div>
          </div>
        </button>

        {/* Import cURL */}
        <button
          onClick={() => {
            if (onOpenCurlImport) {
              onOpenCurlImport(phase);
              onClose();
            }
          }}
          disabled={!onOpenCurlImport}
          className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Upload className="w-4 h-4 text-cyan-400" />
          <div className="flex-1 min-w-0">
            <div className="font-medium text-zinc-200">Import cURL</div>
            <div className="text-xs text-zinc-500">Parse a cURL command</div>
          </div>
        </button>
      </div>
    </div>
  );

  // =============================================================================
  // Render: AI Prompt Submenu
  // =============================================================================

  const renderPromptMenu = (phase: WorkflowPhase) => {
    const promptLabels: Record<WorkflowPhase, string> = {
      setup: "AI Setup Task",
      verification: "AI Verification",
      agentic: "AI Prompt",
      completion: "AI Completion Task",
    };

    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <MessageSquare className="w-4 h-4 text-amber-400" />
          <span className="text-zinc-200">{promptLabels[phase]}</span>
        </button>

        {/* Prompt options */}
        <div className="py-1">
          {/* New Prompt */}
          <button
            onClick={() => handleStepClick("prompt", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-amber-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Prompt</div>
              <div className="text-xs text-zinc-500">Create a new AI prompt</div>
            </div>
          </button>

          {/* From Library */}
          <button
            onClick={() => {
              if (onOpenPromptLibrary) {
                onOpenPromptLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenPromptLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-amber-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Library</div>
              <div className="text-xs text-zinc-500">Use a saved prompt template</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: Shell Command Submenu
  // =============================================================================

  const renderShellCommandMenu = (phase: WorkflowPhase) => {
    const shellCommandLabels: Record<WorkflowPhase, string> = {
      setup: "Setup Command",
      verification: "Verification Command",
      agentic: "",
      completion: "Completion Command",
    };

    return (
      <div className="py-1">
        {/* Header with back button */}
        <button
          onClick={navigateBack}
          className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium border-b border-zinc-700 hover:bg-zinc-700/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4 text-zinc-400" />
          <Terminal className="w-4 h-4 text-gray-400" />
          <span className="text-zinc-200">{shellCommandLabels[phase]}</span>
        </button>

        {/* Shell Command options */}
        <div className="py-1">
          {/* New Shell Command */}
          <button
            onClick={() => handleStepClick("shell_command", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-gray-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">New Command</div>
              <div className="text-xs text-zinc-500">Create a new shell command</div>
            </div>
          </button>

          {/* From Library */}
          <button
            onClick={() => {
              if (onOpenShellCommandLibrary) {
                onOpenShellCommandLibrary(phase);
                onClose();
              }
            }}
            disabled={!onOpenShellCommandLibrary}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Library className="w-4 h-4 text-gray-400" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-zinc-200">From Library</div>
              <div className="text-xs text-zinc-500">Use a saved shell command</div>
            </div>
          </button>
        </div>
      </div>
    );
  };

  // =============================================================================
  // Main Render
  // =============================================================================

  const renderContent = () => {
    switch (activeCategory) {
      case "root":
        return renderRootMenu();
      case "setup":
        return renderPhaseMenu("setup");
      case "verification":
        return renderPhaseMenu("verification");
      case "agentic":
        return renderPhaseMenu("agentic");
      case "completion":
        return renderPhaseMenu("completion");
      // GUI Actions
      case "gui_actions_setup":
        return renderGuiActionsMenu("setup");
      case "gui_actions_verification":
        return renderGuiActionsMenu("verification");
      case "gui_actions_completion":
        return renderGuiActionsMenu("completion");
      // API Requests
      case "api_setup":
        return renderApiMenu("setup");
      case "api_verification":
        return renderApiMenu("verification");
      case "api_completion":
        return renderApiMenu("completion");
      // Tests
      case "tests_setup":
        return renderTestsMenu("setup");
      case "tests_verification":
        return renderTestsMenu("verification");
      case "tests_completion":
        return renderTestsMenu("completion");
      // Checks
      case "checks_setup":
        return renderChecksMenu("setup");
      case "checks_verification":
        return renderChecksMenu("verification");
      case "checks_completion":
        return renderChecksMenu("completion");
      // Macros
      case "macros_setup":
        return renderMacrosMenu("setup");
      case "macros_verification":
        return renderMacrosMenu("verification");
      case "macros_completion":
        return renderMacrosMenu("completion");
      // Prompts
      case "prompt_setup":
        return renderPromptMenu("setup");
      case "prompt_verification":
        return renderPromptMenu("verification");
      case "prompt_agentic":
        return renderPromptMenu("agentic");
      case "prompt_completion":
        return renderPromptMenu("completion");
      // Shell Commands
      case "shell_command_setup":
        return renderShellCommandMenu("setup");
      case "shell_command_verification":
        return renderShellCommandMenu("verification");
      case "shell_command_completion":
        return renderShellCommandMenu("completion");
      // Workflows
      case "workflows_setup":
        return renderWorkflowsMenu("setup");
      case "workflows_verification":
        return renderWorkflowsMenu("verification");
      case "workflows_completion":
        return renderWorkflowsMenu("completion");
      // States
      case "states_setup":
        return renderStatesMenu("setup");
      case "states_verification":
        return renderStatesMenu("verification");
      case "states_completion":
        return renderStatesMenu("completion");
      // MCP Calls
      case "mcp_setup":
        return renderMcpMenu("setup");
      case "mcp_verification":
        return renderMcpMenu("verification");
      case "mcp_completion":
        return renderMcpMenu("completion");
      default:
        return renderRootMenu();
    }
  };

  if (!isOpen) return null;

  return (
    <div
      ref={dropdownRef}
      className="absolute z-50 w-80 rounded-lg border border-zinc-700 bg-zinc-800 shadow-xl overflow-hidden"
    >
      {renderContent()}
    </div>
  );
}

// =============================================================================
// Button Component
// =============================================================================

interface AddStepButtonProps {
  onClick: () => void;
  variant?: "default" | "compact" | "phase";
  phaseLabel?: string;
}

export function AddStepButton({ onClick, variant = "default", phaseLabel }: AddStepButtonProps) {
  if (variant === "compact") {
    return (
      <button
        onClick={onClick}
        className="flex items-center gap-1 px-2 py-1 text-sm rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-300 transition-colors"
      >
        <Plus className="w-4 h-4" />
        <span>Add</span>
      </button>
    );
  }

  if (variant === "phase" && phaseLabel) {
    return (
      <button
        onClick={onClick}
        className="w-full flex items-center justify-center gap-2 p-2 rounded-md bg-zinc-700/50 hover:bg-zinc-700 text-zinc-400 hover:text-zinc-200 transition-colors text-sm"
      >
        <Plus className="w-4 h-4" />
        <span>Add {phaseLabel} Step</span>
      </button>
    );
  }

  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 px-4 py-2 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors"
    >
      <Plus className="w-4 h-4" />
      <span>Add Step</span>
      <ChevronDown className="w-4 h-4" />
    </button>
  );
}
