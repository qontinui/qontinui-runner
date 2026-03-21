/**
 * TutorialCard Component
 *
 * Displays a tutorial in a card format for listing in the help tab.
 */

import { Clock, BookOpen, CheckCircle2, PlayCircle, RotateCcw } from "lucide-react";
import type { Tutorial } from "../../types/tutorial";
import { cn } from "../../lib/utils";
import { getAccentColors, getStatusColors } from "@/design-system";

interface TutorialCardProps {
  tutorial: Tutorial;
  isCompleted?: boolean;
  isInProgress?: boolean;
  onStart: () => void;
  onResume?: () => void;
  onRestart?: () => void;
  /** Show with featured styling (larger, more prominent) */
  featured?: boolean;
}

/**
 * TutorialCard - Card component for tutorial listings
 */
export function TutorialCard({
  tutorial,
  isCompleted = false,
  isInProgress = false,
  onStart,
  onResume,
  onRestart,
  featured = false,
}: TutorialCardProps) {
  return (
    <div
      className={cn(
        "p-4 rounded-lg border border-border/50 bg-card/50",
        "hover:bg-card/80 hover:border-border transition-all",
        isCompleted && "border-l-4 border-l-green-500/50",
        featured && "p-5 bg-card",
      )}
    >
      <div className="flex items-start gap-4">
        {/* Icon */}
        <div
          className={cn(
            "p-3 rounded-lg shrink-0",
            isCompleted ? getStatusColors("success").bg : getAccentColors("cyan").bg,
          )}
        >
          {isCompleted ? (
            <CheckCircle2 className={cn("w-6 h-6", getStatusColors("success").icon)} />
          ) : (
            <BookOpen className={cn("w-6 h-6", getAccentColors("cyan").text)} />
          )}
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h3 className="font-semibold text-foreground">{tutorial.title}</h3>
              <p className="text-sm text-muted-foreground mt-1 line-clamp-2">
                {tutorial.description}
              </p>
            </div>

            {/* Status badge */}
            {isCompleted && (
              <span
                className={cn(
                  "shrink-0 px-2 py-1 rounded text-xs font-medium",
                  getStatusColors("success").bg,
                  getStatusColors("success").text,
                )}
              >
                Completed
              </span>
            )}
            {isInProgress && !isCompleted && (
              <span
                className={cn(
                  "shrink-0 px-2 py-1 rounded text-xs font-medium",
                  getAccentColors("amber").bg,
                  getAccentColors("amber").text,
                )}
              >
                In Progress
              </span>
            )}
          </div>

          {/* Meta info */}
          <div className="flex items-center gap-4 mt-3 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <Clock className="w-3.5 h-3.5" />
              {tutorial.duration}
            </span>
            <span
              className={cn(
                "px-2 py-0.5 rounded",
                tutorial.difficulty === "beginner" && "bg-green-500/10 text-green-500",
                tutorial.difficulty === "intermediate" && "bg-amber-500/10 text-amber-500",
                tutorial.difficulty === "advanced" && "bg-purple-500/10 text-purple-500",
              )}
            >
              {tutorial.difficulty}
            </span>
            <span>{tutorial.steps.length} steps</span>
            {tutorial.category && (
              <span className="px-2 py-0.5 bg-muted/50 rounded">{tutorial.category}</span>
            )}
          </div>

          {/* Actions */}
          <div className="flex items-center gap-2 mt-4">
            {isInProgress && onResume ? (
              <>
                <button onClick={onResume} className="btn-primary flex items-center gap-2 text-sm">
                  <PlayCircle className="w-4 h-4" />
                  Resume
                </button>
                {onRestart && (
                  <button
                    onClick={onRestart}
                    className="btn-secondary flex items-center gap-2 text-sm"
                  >
                    <RotateCcw className="w-4 h-4" />
                    Restart
                  </button>
                )}
              </>
            ) : isCompleted ? (
              <button
                onClick={onRestart || onStart}
                className="btn-secondary flex items-center gap-2 text-sm"
              >
                <RotateCcw className="w-4 h-4" />
                Review Again
              </button>
            ) : (
              <button onClick={onStart} className="btn-primary flex items-center gap-2 text-sm">
                <PlayCircle className="w-4 h-4" />
                Start Tutorial
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
