/**
 * AddStepDropdown.tsx
 *
 * Hierarchical dropdown menu for adding new execution steps.
 * Shows categories that expand to reveal submenus with available items.
 */

import { useState } from "react";
import {
  AlertTriangle,
  Camera,
  ChevronRight,
  FileText,
  FolderOpen,
  MousePointer2,
  Target,
  TestTube,
  ClipboardCheck,
  Workflow,
  ArrowLeft,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

type CategoryId =
  | "workflows"
  | "gui-workflows"
  | "states"
  | "playwright"
  | "prompts"
  | "actions"
  | "capture"
  | "tests"
  | null;

interface CategoryConfig {
  id: CategoryId;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  color: string;
  requiresConfig: boolean;
  alwaysShow?: boolean;
}

const CATEGORIES: CategoryConfig[] = [
  {
    id: "workflows",
    label: "Workflows",
    icon: Workflow,
    color: "text-purple-500",
    requiresConfig: true,
  },
  {
    id: "gui-workflows",
    label: "GUI Workflows",
    icon: MousePointer2,
    color: "text-orange-500",
    requiresConfig: false,
  },
  { id: "states", label: "States", icon: Target, color: "text-primary", requiresConfig: true },
  {
    id: "playwright",
    label: "Playwright Tests",
    icon: TestTube,
    color: "text-green-500",
    requiresConfig: false,
  },
  {
    id: "prompts",
    label: "Prompts",
    icon: FileText,
    color: "text-amber-500",
    requiresConfig: false,
  },
  {
    id: "actions",
    label: "Click Actions",
    icon: MousePointer2,
    color: "text-blue-500",
    requiresConfig: true,
  },
  {
    id: "capture",
    label: "Capture",
    icon: Camera,
    color: "text-cyan-500",
    requiresConfig: false,
    alwaysShow: true,
  },
  {
    id: "tests",
    label: "Verification Tests",
    icon: ClipboardCheck,
    color: "text-emerald-500",
    requiresConfig: false,
  },
];

export function AddStepDropdown() {
  const {
    states,
    images,
    workflows,
    playwrightScripts,
    savedPrompts,
    guiWorkflows,
    verificationTests,
    configLoaded,
    loadConfiguration,
    addStep,
    addActionStep,
    addGuiWorkflowStep,
    addScreenshotStep,
    addTestStep,
    setShowAddDropdown,
  } = useAiBuilder();

  // Track which category submenu is open
  const [activeCategory, setActiveCategory] = useState<CategoryId>(null);

  // Get count for each category
  const getCategoryCount = (categoryId: CategoryId): number => {
    switch (categoryId) {
      case "workflows":
        return workflows.length;
      case "gui-workflows":
        return guiWorkflows.length;
      case "states":
        return states.length;
      case "playwright":
        return playwrightScripts.length;
      case "prompts":
        return savedPrompts.length;
      case "actions":
        return images.length;
      case "capture":
        return 1; // Always has screenshot
      case "tests":
        return verificationTests.length;
      default:
        return 0;
    }
  };

  // Check if category should be shown
  const shouldShowCategory = (category: CategoryConfig): boolean => {
    if (category.alwaysShow) return true;
    if (category.requiresConfig && !configLoaded) return false;
    return getCategoryCount(category.id) > 0;
  };

  // Render the main category menu
  const renderCategoryMenu = () => (
    <div className="py-1">
      {/* No config loaded warning */}
      {!configLoaded && (
        <div className="px-3 py-3 border-b border-border">
          <div className="flex items-center gap-2 text-amber-500 mb-2">
            <AlertTriangle className="w-4 h-4" />
            <span className="text-sm font-medium">No Config Loaded</span>
          </div>
          <p className="text-xs text-muted-foreground mb-2">
            Workflows, states, and click actions require a loaded configuration.
          </p>
          <button
            onClick={() => {
              setShowAddDropdown(false);
              loadConfiguration();
            }}
            className="w-full flex items-center justify-center gap-2 px-3 py-2 text-sm bg-primary/10 text-primary rounded hover:bg-primary/20 transition-colors"
          >
            <FolderOpen className="w-4 h-4" />
            Load Configuration
          </button>
        </div>
      )}

      {/* Category buttons */}
      {CATEGORIES.filter(shouldShowCategory).map((category) => {
        const Icon = category.icon;
        const count = getCategoryCount(category.id);

        return (
          <button
            key={category.id}
            onClick={() => setActiveCategory(category.id)}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-muted/50 transition-colors group"
          >
            <Icon className={`w-4 h-4 ${category.color}`} />
            <span className="flex-1 font-medium">{category.label}</span>
            <span className="text-xs text-muted-foreground">{count}</span>
            <ChevronRight className="w-4 h-4 text-muted-foreground group-hover:text-foreground transition-colors" />
          </button>
        );
      })}
    </div>
  );

  // Render submenu for a category
  const renderSubmenu = () => {
    const category = CATEGORIES.find((c) => c.id === activeCategory);
    if (!category) return null;

    const Icon = category.icon;

    return (
      <div className="py-1">
        {/* Back button / header */}
        <button
          onClick={() => setActiveCategory(null)}
          className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium border-b border-border hover:bg-muted/30 transition-colors"
        >
          <ArrowLeft className="w-4 h-4" />
          <Icon className={`w-4 h-4 ${category.color}`} />
          <span>{category.label}</span>
        </button>

        {/* Submenu items */}
        <div className="max-h-64 overflow-y-auto">
          {activeCategory === "workflows" &&
            workflows.map((workflow) => (
              <button
                key={workflow.id}
                onClick={() => {
                  addStep("workflow", workflow.name);
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <Workflow className="w-4 h-4 text-purple-500" />
                <span className="truncate">{workflow.name}</span>
              </button>
            ))}

          {activeCategory === "gui-workflows" &&
            guiWorkflows.map((workflow) => (
              <button
                key={workflow.id}
                onClick={() => {
                  addGuiWorkflowStep(workflow.id, workflow.name);
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <MousePointer2 className="w-4 h-4 text-orange-500" />
                <span className="truncate flex-1">{workflow.name}</span>
                <span className="text-xs text-muted-foreground">{workflow.stepCount} steps</span>
              </button>
            ))}

          {activeCategory === "states" &&
            states.map((state) => (
              <button
                key={state.name}
                onClick={() => {
                  addStep("state", state.name);
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <Target className="w-4 h-4 text-primary" />
                <span className="truncate flex-1">{state.name}</span>
                {state.images.length > 0 && (
                  <span className="text-xs text-muted-foreground">{state.images.length} img</span>
                )}
              </button>
            ))}

          {activeCategory === "playwright" &&
            playwrightScripts.map((script) => (
              <button
                key={script.id}
                onClick={() => {
                  addStep(
                    "playwright",
                    script.name,
                    script.id,
                    script.script_content,
                    script.target_url,
                  );
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <TestTube className="w-4 h-4 text-green-500" />
                <span className="truncate flex-1">{script.name}</span>
                {script.target_url && (
                  <span className="text-xs text-muted-foreground truncate max-w-24">
                    {script.target_url}
                  </span>
                )}
              </button>
            ))}

          {activeCategory === "prompts" &&
            savedPrompts.map((prompt) => (
              <button
                key={prompt.id}
                onClick={() => {
                  addStep(
                    "prompt",
                    prompt.name,
                    undefined,
                    undefined,
                    undefined,
                    prompt.id,
                    prompt.content,
                  );
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <FileText className="w-4 h-4 text-amber-500" />
                <span className="truncate flex-1">{prompt.name}</span>
                {prompt.category && (
                  <span className="text-xs text-muted-foreground">{prompt.category}</span>
                )}
              </button>
            ))}

          {activeCategory === "actions" &&
            images.map((image) => (
              <button
                key={`${image.stateName}-${image.id}`}
                onClick={() => {
                  addActionStep("click", image.id, image.name);
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <MousePointer2 className="w-4 h-4 text-blue-500" />
                <span className="truncate flex-1">{image.name}</span>
                <span className="text-xs text-muted-foreground">{image.stateName}</span>
              </button>
            ))}

          {activeCategory === "capture" && (
            <button
              onClick={() => {
                addScreenshotStep();
                setShowAddDropdown(false);
              }}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <Camera className="w-4 h-4 text-cyan-500" />
              <span className="flex-1">Screenshot</span>
              <span className="text-xs text-muted-foreground">for AI analysis</span>
            </button>
          )}

          {activeCategory === "tests" &&
            verificationTests.map((test) => (
              <button
                key={test.id}
                onClick={() => {
                  addTestStep(test.id, test.name, test.test_type, test.is_critical);
                  setShowAddDropdown(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
              >
                <ClipboardCheck className="w-4 h-4 text-emerald-500" />
                <span className="truncate flex-1">{test.name}</span>
                {test.is_critical && (
                  <span className="text-xs text-amber-500 font-medium">Critical</span>
                )}
                {test.category && (
                  <span className="text-xs text-muted-foreground">{test.category}</span>
                )}
              </button>
            ))}
        </div>
      </div>
    );
  };

  return (
    <div className="absolute right-0 z-20 w-72 mt-1 bg-card border border-border rounded-lg shadow-lg overflow-hidden">
      {activeCategory === null ? renderCategoryMenu() : renderSubmenu()}
    </div>
  );
}
