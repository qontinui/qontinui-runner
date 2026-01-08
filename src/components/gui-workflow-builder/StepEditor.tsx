/**
 * StepEditor Component
 *
 * Modal for editing step details.
 */

import { useState, useEffect } from "react";
import { X, AlertCircle, Check } from "lucide-react";
import type { GuiWorkflowStep, GuiActionType } from "../../types/gui-workflow";
import { validateGuiWorkflowStep } from "../../types/gui-workflow";
import { useGuiBuilder } from "./GuiBuilderContext";

interface StepEditorProps {
  step: GuiWorkflowStep | null;
  open: boolean;
  onClose: () => void;
}

export function StepEditor({ step, open, onClose }: StepEditorProps) {
  const { updateStep, images, states, configLoaded } = useGuiBuilder();

  // Local form state
  const [name, setName] = useState("");
  const [actionType, setActionType] = useState<GuiActionType>("click");
  const [targetImageIds, setTargetImageIds] = useState<string[]>([]);
  const [textInput, setTextInput] = useState("");
  const [hotkey, setHotkey] = useState("");
  const [targetStateIds, setTargetStateIds] = useState<string[]>([]);
  const [pauseAfterMs, setPauseAfterMs] = useState<string>("");

  // Initialize form when step changes
  useEffect(() => {
    if (step) {
      setName(step.name);
      setActionType(step.action_type);
      setTargetImageIds(step.target_image_ids || []);
      setTextInput(step.text_input || "");
      setHotkey(step.hotkey || "");
      setTargetStateIds(step.target_state_ids || []);
      setPauseAfterMs(step.pause_after_ms?.toString() || "");
    }
  }, [step]);

  // Handle save
  const handleSave = () => {
    if (!step) return;

    const updates: Partial<GuiWorkflowStep> = {
      name,
      action_type: actionType,
      pause_after_ms: pauseAfterMs ? parseInt(pauseAfterMs) : undefined,
    };

    // Add action-specific fields
    if (actionType === "click" || actionType === "double_click" || actionType === "right_click") {
      updates.target_image_ids = targetImageIds;
      updates.target_image_names = targetImageIds.map(
        (id) => images.find((img) => img.id === id)?.name || id,
      );
    } else if (actionType === "type") {
      updates.text_input = textInput;
    } else if (actionType === "hotkey") {
      updates.hotkey = hotkey;
    } else if (actionType === "go_to_state") {
      updates.target_state_ids = targetStateIds;
      updates.target_state_names = targetStateIds.map(
        (id) => states.find((s) => s.id === id)?.name || id,
      );
    }

    updateStep(step.id, updates);
    onClose();
  };

  // Toggle selection
  const toggleImage = (imageId: string) => {
    setTargetImageIds((prev) =>
      prev.includes(imageId) ? prev.filter((id) => id !== imageId) : [...prev, imageId],
    );
  };

  const toggleState = (stateId: string) => {
    setTargetStateIds((prev) =>
      prev.includes(stateId) ? prev.filter((id) => id !== stateId) : [...prev, stateId],
    );
  };

  // Validation
  const tempStep: GuiWorkflowStep = {
    id: step?.id || "",
    action_type: actionType,
    name,
    target_image_ids: targetImageIds,
    text_input: textInput,
    hotkey,
    target_state_ids: targetStateIds,
  };
  const validation = validateGuiWorkflowStep(tempStep);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 max-h-[90vh] overflow-hidden flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h3 className="text-lg font-semibold">Edit Step</h3>
          <button
            onClick={onClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Step Name */}
          <div>
            <label className="block text-sm font-medium mb-1">Step Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Enter step name"
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {/* Action Type */}
          <div>
            <label className="block text-sm font-medium mb-1">Action Type</label>
            <select
              value={actionType}
              onChange={(e) => setActionType(e.target.value as GuiActionType)}
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            >
              <option value="click">Click</option>
              <option value="double_click">Double Click</option>
              <option value="right_click">Right Click</option>
              <option value="type">Type</option>
              <option value="hotkey">Hotkey</option>
              <option value="go_to_state">Go to State</option>
            </select>
          </div>

          {/* Action-specific fields */}
          {(actionType === "click" ||
            actionType === "double_click" ||
            actionType === "right_click") && (
            <div>
              <label className="block text-sm font-medium mb-1">Target Images</label>
              {!configLoaded ? (
                <div className="text-sm text-muted-foreground p-3 border border-border rounded bg-muted/50 flex items-center gap-2">
                  <AlertCircle className="h-4 w-4" />
                  Load a configuration to select images
                </div>
              ) : images.length === 0 ? (
                <div className="text-sm text-muted-foreground p-3 border border-border rounded bg-muted/50">
                  No images found in loaded configuration
                </div>
              ) : (
                <div className="max-h-48 overflow-y-auto border border-border rounded p-2 space-y-1">
                  {images.map((img) => (
                    <div
                      key={img.id}
                      className="flex items-center gap-2 p-2 rounded hover:bg-muted cursor-pointer"
                      onClick={() => toggleImage(img.id)}
                    >
                      <div
                        className={`w-4 h-4 rounded border flex items-center justify-center ${
                          targetImageIds.includes(img.id)
                            ? "bg-primary border-primary text-primary-foreground"
                            : "border-border"
                        }`}
                      >
                        {targetImageIds.includes(img.id) && <Check className="h-3 w-3" />}
                      </div>
                      <span className="flex-1 truncate text-sm">{img.name}</span>
                      <span className="text-xs text-muted-foreground">{img.stateName}</span>
                    </div>
                  ))}
                </div>
              )}
              {targetImageIds.length > 0 && (
                <div className="flex flex-wrap gap-1 mt-2">
                  {targetImageIds.map((id) => {
                    const img = images.find((i) => i.id === id);
                    return (
                      <span
                        key={id}
                        className="inline-flex items-center gap-1 px-2 py-0.5 bg-muted rounded text-xs"
                      >
                        {img?.name || id}
                        <X
                          className="h-3 w-3 cursor-pointer hover:text-destructive"
                          onClick={() => toggleImage(id)}
                        />
                      </span>
                    );
                  })}
                </div>
              )}
            </div>
          )}

          {actionType === "type" && (
            <div>
              <label className="block text-sm font-medium mb-1">Text to Type</label>
              <textarea
                value={textInput}
                onChange={(e) => setTextInput(e.target.value)}
                placeholder="Enter text to type..."
                rows={3}
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-none focus:ring-2 focus:ring-primary"
              />
            </div>
          )}

          {actionType === "hotkey" && (
            <div>
              <label className="block text-sm font-medium mb-1">Hotkey Combination</label>
              <input
                type="text"
                value={hotkey}
                onChange={(e) => setHotkey(e.target.value)}
                placeholder="e.g., Ctrl+C, Alt+Tab"
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              />
              <p className="text-xs text-muted-foreground mt-1">
                Use + to combine keys (e.g., Ctrl+Shift+S)
              </p>
            </div>
          )}

          {actionType === "go_to_state" && (
            <div>
              <label className="block text-sm font-medium mb-1">Target States</label>
              {!configLoaded ? (
                <div className="text-sm text-muted-foreground p-3 border border-border rounded bg-muted/50 flex items-center gap-2">
                  <AlertCircle className="h-4 w-4" />
                  Load a configuration to select states
                </div>
              ) : states.length === 0 ? (
                <div className="text-sm text-muted-foreground p-3 border border-border rounded bg-muted/50">
                  No states found in loaded configuration
                </div>
              ) : (
                <div className="max-h-48 overflow-y-auto border border-border rounded p-2 space-y-1">
                  {states.map((state) => (
                    <div
                      key={state.id}
                      className="flex items-center gap-2 p-2 rounded hover:bg-muted cursor-pointer"
                      onClick={() => toggleState(state.id)}
                    >
                      <div
                        className={`w-4 h-4 rounded border flex items-center justify-center ${
                          targetStateIds.includes(state.id)
                            ? "bg-primary border-primary text-primary-foreground"
                            : "border-border"
                        }`}
                      >
                        {targetStateIds.includes(state.id) && <Check className="h-3 w-3" />}
                      </div>
                      <span className="flex-1 truncate text-sm">{state.name}</span>
                    </div>
                  ))}
                </div>
              )}
              {targetStateIds.length > 0 && (
                <div className="flex flex-wrap gap-1 mt-2">
                  {targetStateIds.map((id) => {
                    const state = states.find((s) => s.id === id);
                    return (
                      <span
                        key={id}
                        className="inline-flex items-center gap-1 px-2 py-0.5 bg-muted rounded text-xs"
                      >
                        {state?.name || id}
                        <X
                          className="h-3 w-3 cursor-pointer hover:text-destructive"
                          onClick={() => toggleState(id)}
                        />
                      </span>
                    );
                  })}
                </div>
              )}
            </div>
          )}

          {/* Timing */}
          <div>
            <label className="block text-sm font-medium mb-1">Pause After (ms)</label>
            <input
              type="number"
              value={pauseAfterMs}
              onChange={(e) => setPauseAfterMs(e.target.value)}
              placeholder="Optional delay after step"
              min={0}
              step={100}
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {/* Validation Errors */}
          {!validation.valid && (
            <div className="text-sm text-destructive bg-destructive/10 p-3 rounded">
              <ul className="list-disc list-inside space-y-1">
                {validation.errors.map((error, i) => (
                  <li key={i}>{error}</li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex gap-3 p-4 border-t border-border">
          <button
            onClick={onClose}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={!validation.valid}
            className="flex-1 px-4 py-2 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Save Changes
          </button>
        </div>
      </div>
    </div>
  );
}
