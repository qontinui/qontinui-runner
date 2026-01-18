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
import { PHASE_INFO, STEP_TYPES, GUI_ACTION_TYPES, generateStepId } from "../../types";

// =============================================================================
// Types
// =============================================================================

type CategoryId =
  | "root"
  | "setup"
  | "verification"
  | "agentic"
  | "completion"
  | "gui_actions_setup"
  | "gui_actions_verification"
  | "api_setup"
  | "api_verification"
  | "api_completion"
  | "prompt_setup"
  | "prompt_verification"
  | "prompt_agentic"
  | "prompt_completion"
  | "shell_command_setup"
  | "shell_command_completion"
  | "tests"
  | "checks";

// =============================================================================
// Icon Mapping
// =============================================================================

const ICON_MAP: Record<string, React.ElementType> = {
  FileCode,
  Navigation,
  GitBranch,
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
    orange: "text-orange-400",
    green: "text-green-400",
    pink: "text-pink-400",
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
      case "gui_actions_setup":
      case "api_setup":
      case "prompt_setup":
      case "shell_command_setup":
        return "setup";
      case "gui_actions_verification":
      case "api_verification":
      case "prompt_verification":
        return "verification";
      case "prompt_agentic":
        return "agentic";
      case "api_completion":
      case "prompt_completion":
      case "shell_command_completion":
        return "completion";
      case "tests":
      case "checks":
        return "verification";
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
          phase: phase as "setup" | "verification",
          name: "Navigate to State",
          state_id: "",
        };
        break;

      case "workflow_ref":
        step = {
          id,
          type: "workflow_ref",
          phase: phase as "setup" | "verification",
          name: "Run Workflow",
          workflow_id: "",
        };
        break;

      case "gui_action":
        step = {
          id,
          type: "gui_action",
          phase: phase as "setup" | "verification",
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
      case "test_custom":
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
          phase: "verification",
          name: STEP_TYPES.verification.find((s) => s.type === stepType)?.label || "New Test",
          test_type: testTypeMap[stepType] || "custom_command",
          is_critical: true,
        };
        break;

      case "check_lint":
      case "check_format":
      case "check_typecheck":
      case "check_analyze":
      case "check_security":
      case "check_custom":
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
          phase: "verification",
          name: STEP_TYPES.verification.find((s) => s.type === stepType)?.label || "New Check",
          check_type: checkTypeMap[stepType] || "custom_command",
          auto_fix: checkTypeMap[stepType] === "format", // Default formatters to auto-fix
          is_blocking: true,
        };
        break;

      case "screenshot":
        step = {
          id,
          type: "screenshot",
          phase: "verification",
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

      case "shell_command":
        step = {
          id,
          type: "shell_command",
          phase: phase as "setup" | "completion",
          name: phase === "setup" ? "Setup Command" : "Completion Command",
          command: "",
          fail_on_error: true,
        };
        break;

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
            // GUI Actions get their own submenu
            if (stepType.type === "gui_action") {
              const subCategoryId =
                phase === "setup" ? "gui_actions_setup" : "gui_actions_verification";
              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId as CategoryId)}
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

            // Tests get their own submenu (only show once)
            if (stepType.type.startsWith("test_")) {
              if (stepType.type !== "test_playwright") return null;
              const testTypes = STEP_TYPES.verification.filter((s) => s.type.startsWith("test_"));
              return (
                <button
                  key="tests"
                  onClick={() => setActiveCategory("tests")}
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

            // Skip other test types (they're grouped)
            if (stepType.type.startsWith("test_")) return null;

            // Checks get their own submenu (only show once)
            if (stepType.type.startsWith("check_")) {
              if (stepType.type !== "check_lint") return null;
              const checkTypes = STEP_TYPES.verification.filter((s) => s.type.startsWith("check_"));
              return (
                <button
                  key="checks"
                  onClick={() => setActiveCategory("checks")}
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

            // Skip other check types (they're grouped)
            if (stepType.type.startsWith("check_")) return null;

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

            // Shell Commands get their own submenu (New or From Library)
            if (stepType.type === "shell_command") {
              const shellCommandSubCategories: Record<WorkflowPhase, CategoryId | null> = {
                setup: "shell_command_setup",
                verification: null, // Shell commands not in verification phase
                agentic: null, // Shell commands not in agentic phase
                completion: "shell_command_completion",
              };
              const subCategoryId = shellCommandSubCategories[phase];
              if (!subCategoryId) return null; // Skip if not applicable to this phase

              const shellCommandLabels: Record<WorkflowPhase, string> = {
                setup: "Setup Command",
                verification: "",
                agentic: "",
                completion: "Completion Command",
              };
              return (
                <button
                  key={`${stepType.type}-${phase}`}
                  onClick={() => setActiveCategory(subCategoryId)}
                  className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors group"
                >
                  <Terminal className="w-4 h-4 text-violet-400" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-zinc-200">{shellCommandLabels[phase]}</div>
                    <div className="text-xs text-zinc-500">New command or from library</div>
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

  const renderTestsMenu = () => {
    const testTypes = STEP_TYPES.verification.filter((s) => s.type.startsWith("test_"));

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

        {/* Test type items */}
        <div className="max-h-80 overflow-y-auto py-1">
          {testTypes.map((testType) => {
            const TestIcon = ICON_MAP[testType.icon] || TestTube2;
            return (
              <button
                key={testType.type}
                onClick={() => handleStepClick(testType.type, "verification")}
                className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
              >
                <TestIcon className="w-4 h-4 text-green-400" />
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-zinc-200">{testType.label}</div>
                  <div className="text-xs text-zinc-500">{testType.description}</div>
                </div>
              </button>
            );
          })}
        </div>
      </div>
    );
  };

  // =============================================================================
  // Render: Checks Submenu
  // =============================================================================

  const renderChecksMenu = () => {
    const checkTypes = STEP_TYPES.verification.filter((s) => s.type.startsWith("check_"));

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

        {/* Check type items */}
        <div className="max-h-80 overflow-y-auto py-1">
          {checkTypes.map((checkType) => {
            const CheckIcon = ICON_MAP[checkType.icon] || Shield;
            return (
              <button
                key={checkType.type}
                onClick={() => handleStepClick(checkType.type, "verification")}
                className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
              >
                <CheckIcon className="w-4 h-4 text-cyan-400" />
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-zinc-200">{checkType.label}</div>
                  <div className="text-xs text-zinc-500">{checkType.description}</div>
                </div>
              </button>
            );
          })}
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
      verification: "",
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
          <Terminal className="w-4 h-4 text-violet-400" />
          <span className="text-zinc-200">{shellCommandLabels[phase]}</span>
        </button>

        {/* Shell Command options */}
        <div className="py-1">
          {/* New Shell Command */}
          <button
            onClick={() => handleStepClick("shell_command", phase)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
          >
            <Plus className="w-4 h-4 text-violet-400" />
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
            <Library className="w-4 h-4 text-violet-400" />
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
      case "gui_actions_setup":
        return renderGuiActionsMenu("setup");
      case "gui_actions_verification":
        return renderGuiActionsMenu("verification");
      case "api_setup":
        return renderApiMenu("setup");
      case "api_verification":
        return renderApiMenu("verification");
      case "api_completion":
        return renderApiMenu("completion");
      case "tests":
        return renderTestsMenu();
      case "checks":
        return renderChecksMenu();
      case "prompt_setup":
        return renderPromptMenu("setup");
      case "prompt_verification":
        return renderPromptMenu("verification");
      case "prompt_agentic":
        return renderPromptMenu("agentic");
      case "prompt_completion":
        return renderPromptMenu("completion");
      case "shell_command_setup":
        return renderShellCommandMenu("setup");
      case "shell_command_completion":
        return renderShellCommandMenu("completion");
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
