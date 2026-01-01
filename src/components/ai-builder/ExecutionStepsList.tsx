/**
 * ExecutionStepsList.tsx
 *
 * Displays the list of execution steps with the add step dropdown.
 * Includes ConfigSelector for GUI-based steps.
 */

import { Camera, ChevronDown, GripVertical, Play, Plus } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { ExecutionStepItem } from "./ExecutionStepItem";
import { AddStepDropdown } from "./AddStepDropdown";
import { ConfigSelector } from "../ConfigSelector";

export function ExecutionStepsList() {
  const {
    executionSteps,
    showAddDropdown,
    setShowAddDropdown,
    selectedConfigId,
    setSelectedConfigId,
    hasGuiSteps,
  } = useAiBuilder();

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Play className="w-4 h-4 text-primary" />
          <span className="font-medium">Execution Steps</span>
          <span className="text-xs text-muted-foreground">
            ({executionSteps.length} step{executionSteps.length !== 1 ? "s" : ""})
          </span>
        </div>

        {/* Add Step Dropdown */}
        <div className="relative">
          <button
            onClick={() => setShowAddDropdown(!showAddDropdown)}
            className="flex items-center gap-1 px-2 py-1 text-sm bg-primary/10 text-primary rounded hover:bg-primary/20 transition-colors"
          >
            <Plus className="w-4 h-4" />
            Add Step
            <ChevronDown
              className={`w-3 h-3 transition-transform ${showAddDropdown ? "rotate-180" : ""}`}
            />
          </button>

          {showAddDropdown && <AddStepDropdown />}
        </div>
      </div>

      {/* Steps List */}
      {executionSteps.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <GripVertical className="w-8 h-8 mx-auto mb-2 opacity-30" />
          <p className="text-sm">No steps added yet</p>
          <p className="text-xs mt-1">Click "Add Step" to build your execution sequence</p>
        </div>
      ) : (
        <div className="space-y-2">
          {executionSteps.map((step, index) => (
            <ExecutionStepItem
              key={step.id}
              step={step}
              index={index}
              totalSteps={executionSteps.length}
            />
          ))}
        </div>
      )}

      {executionSteps.length > 0 && (
        <p className="text-xs text-muted-foreground">
          <Camera className="w-3 h-3 inline mr-1" />
          Click the camera icon to toggle screenshots for each step
        </p>
      )}

      {/* Config Selector - shown when there are GUI steps */}
      {hasGuiSteps && (
        <div className="pt-2 border-t border-border">
          <div className="text-xs font-medium text-muted-foreground mb-2">Stored Configuration</div>
          <ConfigSelector
            selectedConfigId={selectedConfigId}
            onConfigSelect={setSelectedConfigId}
            hasGuiSteps={hasGuiSteps}
            compact
          />
        </div>
      )}
    </div>
  );
}
