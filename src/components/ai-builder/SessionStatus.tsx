/**
 * SessionStatus.tsx
 *
 * Displays the current session status, iteration count, and errors fixed.
 */

import { Activity } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getStatusColors, getAccentColors } from "@/design-system";

export function SessionStatus() {
  const { sessionState } = useAiBuilder();

  if (!sessionState) {
    return null;
  }

  const getStatusColor = () => {
    switch (sessionState.status) {
      case "running":
        return `${getStatusColors("running").bg} ${getStatusColors("running").text}`;
      case "complete":
        return `${getStatusColors("success").bg} ${getAccentColors("blue").text}`;
      case "stopped":
        return "bg-surface-raised/50 text-text-muted";
      default:
        return `${getStatusColors("warning").bg} ${getStatusColors("warning").text}`;
    }
  };

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Activity className="w-4 h-4 text-primary" />
          <span className="font-medium">Session Status</span>
        </div>
        <span className={`text-xs px-2 py-1 rounded ${getStatusColor()}`}>
          {sessionState.status}
        </span>
      </div>

      <div className="grid grid-cols-2 gap-2 text-sm">
        <div>
          <span className="text-muted-foreground">Iteration:</span>{" "}
          <span className="font-medium">
            {sessionState.iteration}/{sessionState.max_iterations}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground">Errors Fixed:</span>{" "}
          <span className={`font-medium ${getStatusColors("success").text}`}>
            {sessionState.errors_fixed.length}
          </span>
        </div>
      </div>

      {sessionState.current_action && (
        <div className="text-xs text-muted-foreground truncate">{sessionState.current_action}</div>
      )}

      {sessionState.errors_fixed.length > 0 && (
        <div className="space-y-1 max-h-24 overflow-y-auto">
          <span className="text-xs text-muted-foreground">Recent fixes:</span>
          {sessionState.errors_fixed.slice(-3).map((fix, i) => (
            <div
              key={i}
              className={`text-xs ${getStatusColors("success").bg} ${getStatusColors("success").text} p-1 rounded truncate`}
            >
              {fix.file}: {fix.description}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
