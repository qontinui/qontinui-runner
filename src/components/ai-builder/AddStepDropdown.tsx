/**
 * AddStepDropdown.tsx
 *
 * Dropdown menu for adding new execution steps.
 */

import {
  AlertTriangle,
  Camera,
  FileText,
  FolderOpen,
  MousePointer2,
  Target,
  TestTube,
  Workflow,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function AddStepDropdown() {
  const {
    states,
    images,
    workflows,
    playwrightScripts,
    savedPrompts,
    configLoaded,
    loadConfiguration,
    addStep,
    addActionStep,
    addScreenshotStep,
    setShowAddDropdown,
  } = useAiBuilder();

  return (
    <div className="absolute right-0 z-20 w-64 mt-1 bg-card border border-border rounded-md shadow-lg max-h-80 overflow-y-auto">
      {/* No config loaded message */}
      {!configLoaded && (
        <div className="px-3 py-3 border-b border-border">
          <div className="flex items-center gap-2 text-amber-500 mb-2">
            <AlertTriangle className="w-4 h-4" />
            <span className="text-sm font-medium">No Config Loaded</span>
          </div>
          <p className="text-xs text-muted-foreground mb-2">
            Qontinui automation workflows, states, and click actions are only available when a
            configuration is loaded.
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

      {/* Workflows section */}
      {workflows.length > 0 && (
        <>
          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
            <Workflow className="w-3 h-3" />
            Workflows
          </div>
          {workflows.map((workflow) => (
            <button
              key={workflow.id}
              onClick={() => addStep("workflow", workflow.name)}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <Workflow className="w-4 h-4 text-purple-500" />
              <span>{workflow.name}</span>
            </button>
          ))}
        </>
      )}

      {/* States section */}
      {states.length > 0 && (
        <>
          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
            <Target className="w-3 h-3" />
            States
          </div>
          {states.map((state) => (
            <button
              key={state.name}
              onClick={() => addStep("state", state.name)}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <Target className="w-4 h-4 text-primary" />
              <span>{state.name}</span>
              {state.images.length > 0 && (
                <span className="text-xs text-muted-foreground ml-auto">
                  {state.images.length} img
                </span>
              )}
            </button>
          ))}
        </>
      )}

      {/* Playwright Scripts section */}
      {playwrightScripts.length > 0 && (
        <>
          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
            <TestTube className="w-3 h-3" />
            Playwright Tests
          </div>
          {playwrightScripts.map((script) => (
            <button
              key={script.id}
              onClick={() =>
                addStep(
                  "playwright",
                  script.name,
                  script.id,
                  script.script_content,
                  script.target_url,
                )
              }
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <TestTube className="w-4 h-4 text-green-500" />
              <span>{script.name}</span>
              {script.target_url && (
                <span className="text-xs text-muted-foreground ml-auto truncate max-w-20">
                  {script.target_url}
                </span>
              )}
            </button>
          ))}
        </>
      )}

      {/* Prompts section */}
      {savedPrompts.length > 0 && (
        <>
          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
            <FileText className="w-3 h-3" />
            Prompts
          </div>
          {savedPrompts.map((prompt) => (
            <button
              key={prompt.id}
              onClick={() =>
                addStep(
                  "prompt",
                  prompt.name,
                  undefined,
                  undefined,
                  undefined,
                  prompt.id,
                  prompt.content,
                )
              }
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <FileText className="w-4 h-4 text-amber-500" />
              <span className="truncate">{prompt.name}</span>
              {prompt.category && (
                <span className="text-xs text-muted-foreground ml-auto">{prompt.category}</span>
              )}
            </button>
          ))}
        </>
      )}

      {/* Click Image section */}
      {images.length > 0 && (
        <>
          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
            <MousePointer2 className="w-3 h-3" />
            Click Image
          </div>
          {images.map((image) => (
            <button
              key={`${image.stateName}-${image.id}`}
              onClick={() => addActionStep("click", image.id, image.name)}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
            >
              <MousePointer2 className="w-4 h-4 text-blue-500" />
              <span className="truncate">{image.name}</span>
              <span className="text-xs text-muted-foreground ml-auto">{image.stateName}</span>
            </button>
          ))}
        </>
      )}

      {/* Capture section */}
      <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
        <Camera className="w-3 h-3" />
        Capture
      </div>
      <button
        onClick={() => addScreenshotStep()}
        className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
      >
        <Camera className="w-4 h-4 text-cyan-500" />
        <span>Screenshot</span>
        <span className="text-xs text-muted-foreground ml-auto">for AI analysis</span>
      </button>
    </div>
  );
}
