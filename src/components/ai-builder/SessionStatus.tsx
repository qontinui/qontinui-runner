/**
 * SessionStatus.tsx
 *
 * Displays the current session status, iteration count, and errors fixed.
 */

import { Activity } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function SessionStatus() {
  const { sessionState } = useAiBuilder();

  if (!sessionState) {
    return null;
  }

  const getStatusColor = () => {
    switch (sessionState.status) {
      case "running":
        return "bg-green-500/20 text-green-500";
      case "complete":
        return "bg-blue-500/20 text-blue-500";
      case "stopped":
        return "bg-surface-raised/50 text-text-muted";
      default:
        return "bg-yellow-500/20 text-yellow-500";
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
          <span className="font-medium text-green-500">{sessionState.errors_fixed.length}</span>
        </div>
      </div>

      {sessionState.current_action && (
        <div className="text-xs text-muted-foreground truncate">{sessionState.current_action}</div>
      )}

      {sessionState.errors_fixed.length > 0 && (
        <div className="space-y-1 max-h-24 overflow-y-auto">
          <span className="text-xs text-muted-foreground">Recent fixes:</span>
          {sessionState.errors_fixed.slice(-3).map((fix, i) => (
            <div key={i} className="text-xs bg-green-500/10 text-green-600 p-1 rounded truncate">
              {fix.file}: {fix.description}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
