/**
 * AdvancedSettingsPanel.tsx
 *
 * Advanced settings including input capture validation for debugging.
 */

import { Code, Info, MousePointer2, ToggleLeft, ToggleRight } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import CollapsiblePanel from "../CollapsiblePanel";
import { getAccentColors } from "@/design-system";

export function AdvancedSettingsPanel() {
  const { captureInputValidation, setCaptureInputValidation } = useAiBuilder();

  return (
    <CollapsiblePanel
      title="Advanced"
      icon={<Code className="w-4 h-4" />}
      defaultCollapsed={true}
      storageKey="ai-builder-advanced"
    >
      <div className="space-y-3">
        {/* Input Capture for Coordinate Validation Toggle */}
        <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
          <div className="flex items-center gap-2">
            <MousePointer2 className={`w-4 h-4 ${getAccentColors("purple").text}`} />
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
            className={`flex items-center transition-colors ${captureInputValidation ? getAccentColors("purple").text : "text-muted-foreground"}`}
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
          <div className={`space-y-2 pl-2 border-l-2 ${getAccentColors("purple").border}`}>
            <p className="text-xs text-muted-foreground">
              Compares reported click positions with actual mouse events to detect coordinate bugs
              (multi-monitor offsets, DPI scaling issues).
            </p>
            <p className="text-xs text-muted-foreground">
              Results appear in the <strong>Input Validation</strong> section of the Iteration
              Bundle, with the <strong>Input Validation Guide</strong> context auto-included.
            </p>
          </div>
        )}
      </div>
    </CollapsiblePanel>
  );
}
