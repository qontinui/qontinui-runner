/**
 * BottomBar Component
 *
 * Bottom status bar showing iteration progress, current action, and connection status.
 */

import { Wifi } from "lucide-react";
import { Badge } from "../ui";
import type { BottomBarProps } from "./types";

export function BottomBar({ currentAction, iteration = 1, maxIterations = 1 }: BottomBarProps) {
  const iterationProgress = maxIterations > 0 ? (iteration / maxIterations) * 100 : 0;

  return (
    <div className="flex h-10 items-center border-t border-border bg-card px-4">
      <div className="flex items-center gap-2 w-[35%]">
        {/* Circular progress indicator */}
        <div className="relative h-4 w-4">
          <svg className="h-4 w-4 -rotate-90 transform">
            <circle
              cx="8"
              cy="8"
              r="6"
              stroke="currentColor"
              strokeWidth="2"
              fill="none"
              className="text-muted"
            />
            <circle
              cx="8"
              cy="8"
              r="6"
              stroke="currentColor"
              strokeWidth="2"
              fill="none"
              strokeDasharray={`${2 * Math.PI * 6}`}
              strokeDashoffset={`${2 * Math.PI * 6 * (1 - iterationProgress / 100)}`}
              className="text-blue-500 transition-all"
              strokeLinecap="round"
            />
          </svg>
        </div>
        <span className="text-sm text-foreground font-medium">
          Iteration {iteration} of {maxIterations}
        </span>
      </div>

      <div className="flex-1 text-center px-4">
        <p className="text-sm text-foreground font-medium truncate">
          {currentAction || "Waiting for action..."}
        </p>
      </div>

      <div className="flex items-center justify-end w-[20%]">
        <Badge className="bg-green-500/20 text-green-400 border border-green-500/50">
          <Wifi className="mr-1.5 h-3 w-3" />
          Connected
        </Badge>
      </div>
    </div>
  );
}
