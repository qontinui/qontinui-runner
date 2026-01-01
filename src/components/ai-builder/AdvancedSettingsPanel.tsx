/**
 * AdvancedSettingsPanel.tsx
 *
 * Advanced settings including max iterations and input capture validation.
 */

import { Code, Info, MousePointer2, ToggleLeft, ToggleRight } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import CollapsiblePanel from "../CollapsiblePanel";

export function AdvancedSettingsPanel() {
  const { maxIterations, setMaxIterations, captureInputValidation, setCaptureInputValidation } =
    useAiBuilder();

  return (
    <CollapsiblePanel
      title="Advanced"
      icon={<Code className="w-4 h-4" />}
      defaultCollapsed={true}
      storageKey="ai-builder-advanced"
    >
      <div className="space-y-3">
        {/* Execution Info with hover tooltip */}
        <div className="flex items-center gap-4 flex-wrap">
          <label className="flex items-center gap-2 text-sm">
            <span className="text-muted-foreground">Max Iterations:</span>
            <input
              type="number"
              min={1}
              max={50}
              value={maxIterations}
              onChange={(e) => setMaxIterations(parseInt(e.target.value) || 10)}
              className="w-16 px-2 py-1 bg-background border border-border rounded text-sm"
            />
          </label>
          <div className="relative group">
            <Info className="w-4 h-4 text-muted-foreground hover:text-foreground cursor-help" />
            <div className="absolute left-0 bottom-full mb-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
              <p className="text-xs font-medium text-foreground mb-2">How it works</p>
              <div className="text-xs text-muted-foreground space-y-1">
                <p>
                  Each iteration spawns a new AI session. AI runs automation, analyzes results, and
                  fixes issues.
                </p>
                <p>Loop continues until all checks pass or max iterations reached.</p>
              </div>
            </div>
          </div>
        </div>

        {/* Input Capture for Coordinate Validation Toggle */}
        <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
          <div className="flex items-center gap-2">
            <MousePointer2 className="w-4 h-4 text-purple-500" />
            <div>
              <div className="flex items-center gap-1.5">
                <span className="text-sm font-medium">Capture Input for Validation</span>
                <div className="relative group">
                  <Info className="w-3.5 h-3.5 text-muted-foreground hover:text-foreground cursor-help" />
                  <div className="absolute left-0 bottom-full mb-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
                    <p className="text-xs font-medium text-foreground mb-2">
                      Debugging/Validation Feature
                    </p>
                    <div className="space-y-2 text-xs text-muted-foreground">
                      <p>
                        <strong>When Disabled:</strong> Only reported positions from the automation
                        engine are logged (where clicks <em>should</em> happen).
                      </p>
                      <p>
                        <strong>When Enabled:</strong> Captures actual mouse/keyboard events during
                        execution and compares them to reported positions.
                      </p>
                      <p className="pt-1 border-t border-border mt-2">
                        <strong>Use Case:</strong> Debug clicks missing targets due to multi-monitor
                        offsets, DPI scaling, or coordinate calculation bugs.
                      </p>
                    </div>
                  </div>
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                {captureInputValidation
                  ? "Records actual mouse/keyboard to compare with reported positions"
                  : "Disabled - only reported positions are logged"}
              </p>
            </div>
          </div>
          <button
            onClick={() => setCaptureInputValidation(!captureInputValidation)}
            className={`flex items-center transition-colors ${captureInputValidation ? "text-purple-500" : "text-muted-foreground"}`}
            title={captureInputValidation ? "Input capture enabled" : "Input capture disabled"}
          >
            {captureInputValidation ? (
              <ToggleRight className="w-8 h-8" />
            ) : (
              <ToggleLeft className="w-8 h-8" />
            )}
          </button>
        </div>

        {/* When Input Capture is enabled, show explanation */}
        {captureInputValidation && (
          <div className="space-y-2 pl-2 border-l-2 border-purple-500/30">
            <p className="text-xs text-muted-foreground">
              <strong>For Qontinui development:</strong> Captures actual mouse clicks during
              automation to detect coordinate calculation bugs (when reported click position differs
              from actual).
            </p>
            <p className="text-xs text-muted-foreground">
              Captured input will be logged to{" "}
              <code className="bg-muted px-1 rounded">.dev-logs/input_events/</code>
            </p>
          </div>
        )}
      </div>
    </CollapsiblePanel>
  );
}
