/**
 * TutorialTooltip Component
 *
 * A positioned tooltip that appears near a target element during contextual tutorials.
 * Includes step content, navigation buttons, and progress indicator.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { X, ChevronLeft, ChevronRight, Check, Loader2, CheckCircle2 } from "lucide-react";
import type { TooltipPosition } from "../../types/tutorial";
import { cn } from "../../lib/utils";
import { getAccentColors } from "@/design-system";

interface TutorialTooltipProps {
  /** CSS selector or data-tutorial-id to position near */
  targetSelector: string | null;

  /** Tooltip title */
  title: string;

  /** Tooltip content */
  content: string;

  /** Position relative to target */
  position?: TooltipPosition;

  /** Current step number (1-indexed) */
  currentStep?: number;

  /** Total number of steps */
  totalSteps?: number;

  /** Whether this is the first step */
  isFirstStep?: boolean;

  /** Whether this is the last step */
  isLastStep?: boolean;

  /** Whether the tooltip is visible */
  isVisible?: boolean;

  /** Whether we're waiting for an event-driven action */
  isWaiting?: boolean;

  /** Whether the timeout has been reached */
  isTimedOut?: boolean;

  /** Hint message to show after timeout */
  hintMessage?: string | null;

  /** Whether skip is allowed (after timeout) */
  canSkip?: boolean;

  /** Callback for next button */
  onNext?: () => void;

  /** Callback for previous button */
  onPrevious?: () => void;

  /** Callback for skip/close button */
  onSkip?: () => void;

  /** Callback for complete button (last step) */
  onComplete?: () => void;

  /** Optional action text */
  action?: string;

  /** Whether to show success animation (step just completed) */
  showSuccess?: boolean;
}

interface TooltipPosition2D {
  x: number;
  y: number;
  arrowPosition: "top" | "bottom" | "left" | "right";
}

const TOOLTIP_WIDTH = 320;
const TOOLTIP_OFFSET = 16;
const ARROW_SIZE = 8;

/**
 * Calculate tooltip position based on target element and preferred position
 */
function calculatePosition(
  targetRect: DOMRect,
  preferredPosition: TooltipPosition,
  tooltipHeight: number,
): TooltipPosition2D {
  const viewportWidth = window.innerWidth;
  const viewportHeight = window.innerHeight;

  let x: number;
  let y: number;
  let arrowPosition: "top" | "bottom" | "left" | "right";

  switch (preferredPosition) {
    case "top":
      x = targetRect.left + targetRect.width / 2 - TOOLTIP_WIDTH / 2;
      y = targetRect.top - tooltipHeight - TOOLTIP_OFFSET;
      arrowPosition = "bottom";
      break;

    case "bottom":
      x = targetRect.left + targetRect.width / 2 - TOOLTIP_WIDTH / 2;
      y = targetRect.bottom + TOOLTIP_OFFSET;
      arrowPosition = "top";
      break;

    case "left":
      x = targetRect.left - TOOLTIP_WIDTH - TOOLTIP_OFFSET;
      y = targetRect.top + targetRect.height / 2 - tooltipHeight / 2;
      arrowPosition = "right";
      break;

    case "right":
      x = targetRect.right + TOOLTIP_OFFSET;
      y = targetRect.top + targetRect.height / 2 - tooltipHeight / 2;
      arrowPosition = "left";
      break;

    case "center":
    default:
      x = targetRect.left + targetRect.width / 2 - TOOLTIP_WIDTH / 2;
      y = targetRect.bottom + TOOLTIP_OFFSET;
      arrowPosition = "top";
      break;
  }

  // Ensure tooltip stays within viewport
  if (x < 16) x = 16;
  if (x + TOOLTIP_WIDTH > viewportWidth - 16) x = viewportWidth - TOOLTIP_WIDTH - 16;
  if (y < 16) {
    // Flip to bottom if too close to top
    y = targetRect.bottom + TOOLTIP_OFFSET;
    arrowPosition = "top";
  }
  if (y + tooltipHeight > viewportHeight - 16) {
    // Flip to top if too close to bottom
    y = targetRect.top - tooltipHeight - TOOLTIP_OFFSET;
    arrowPosition = "bottom";
  }

  return { x, y, arrowPosition };
}

/**
 * TutorialTooltip - Contextual tutorial tooltip
 */
export function TutorialTooltip({
  targetSelector,
  title,
  content,
  position = "bottom",
  currentStep = 1,
  totalSteps = 1,
  isFirstStep = false,
  isLastStep = false,
  isVisible = true,
  isWaiting = false,
  isTimedOut = false,
  hintMessage = null,
  canSkip = false,
  onNext,
  onPrevious,
  onSkip,
  onComplete,
  action,
  showSuccess = false,
}: TutorialTooltipProps) {
  const [tooltipPosition, setTooltipPosition] = useState<TooltipPosition2D | null>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const [tooltipHeight, setTooltipHeight] = useState(200);
  const [hasPositioned, setHasPositioned] = useState(false);

  /**
   * Update tooltip position based on target element
   */
  const updatePosition = useCallback(() => {
    if (!targetSelector) {
      setTooltipPosition(null);
      return;
    }

    // Try data-tutorial-id first
    let element = document.querySelector(`[data-tutorial-id="${targetSelector}"]`) as HTMLElement;

    // Fall back to CSS selector
    if (!element) {
      element = document.querySelector(targetSelector) as HTMLElement;
    }

    if (element) {
      const targetRect = element.getBoundingClientRect();
      const calculatedPosition = calculatePosition(targetRect, position, tooltipHeight);
      setTooltipPosition(calculatedPosition);
      // Enable smooth transitions after initial position is set
      if (!hasPositioned) {
        requestAnimationFrame(() => setHasPositioned(true));
      }
    } else {
      setTooltipPosition(null);
      setHasPositioned(false);
    }
  }, [targetSelector, position, tooltipHeight, hasPositioned]);

  // Update tooltip height when content changes
  useEffect(() => {
    if (tooltipRef.current) {
      setTooltipHeight(tooltipRef.current.offsetHeight);
    }
  }, [title, content, action]);

  // Update position on mount and when dependencies change
  useEffect(() => {
    updatePosition();

    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);

    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [updatePosition]);

  // Don't render if not visible or no position
  if (!isVisible || !tooltipPosition) {
    return null;
  }

  return (
    <div
      ref={tooltipRef}
      className={cn(
        "fixed z-tutorial-tooltip",
        "bg-card border border-border rounded-xl shadow-2xl",
        "tutorial-scale-in",
        hasPositioned && "tutorial-tooltip-positioned",
      )}
      style={{
        left: tooltipPosition.x,
        top: tooltipPosition.y,
        width: TOOLTIP_WIDTH,
      }}
    >
      {/* Arrow */}
      <div
        className={cn(
          "absolute w-0 h-0",
          tooltipPosition.arrowPosition === "top" && "left-1/2 -translate-x-1/2 -top-2",
          tooltipPosition.arrowPosition === "bottom" && "left-1/2 -translate-x-1/2 -bottom-2",
          tooltipPosition.arrowPosition === "left" && "top-1/2 -translate-y-1/2 -left-2",
          tooltipPosition.arrowPosition === "right" && "top-1/2 -translate-y-1/2 -right-2",
        )}
        style={{
          borderLeft:
            tooltipPosition.arrowPosition === "top" || tooltipPosition.arrowPosition === "bottom"
              ? `${ARROW_SIZE}px solid transparent`
              : tooltipPosition.arrowPosition === "right"
                ? `${ARROW_SIZE}px solid hsl(var(--border))`
                : "none",
          borderRight:
            tooltipPosition.arrowPosition === "top" || tooltipPosition.arrowPosition === "bottom"
              ? `${ARROW_SIZE}px solid transparent`
              : tooltipPosition.arrowPosition === "left"
                ? `${ARROW_SIZE}px solid hsl(var(--border))`
                : "none",
          borderTop:
            tooltipPosition.arrowPosition === "left" || tooltipPosition.arrowPosition === "right"
              ? `${ARROW_SIZE}px solid transparent`
              : tooltipPosition.arrowPosition === "bottom"
                ? `${ARROW_SIZE}px solid hsl(var(--border))`
                : "none",
          borderBottom:
            tooltipPosition.arrowPosition === "left" || tooltipPosition.arrowPosition === "right"
              ? `${ARROW_SIZE}px solid transparent`
              : tooltipPosition.arrowPosition === "top"
                ? `${ARROW_SIZE}px solid hsl(var(--border))`
                : "none",
        }}
      />

      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border/50">
        <div className="flex items-center gap-2">
          <div
            className={cn(
              "w-6 h-6 rounded-full flex items-center justify-center text-xs font-bold",
              getAccentColors("cyan").bg,
              getAccentColors("cyan").text,
            )}
          >
            {currentStep}
          </div>
          <span className="text-xs text-muted-foreground">of {totalSteps}</span>
        </div>
        {onSkip && (
          <button
            onClick={onSkip}
            className="p-1 hover:bg-muted rounded-md transition-colors"
            aria-label="Skip tutorial"
          >
            <X className="w-4 h-4 text-muted-foreground" />
          </button>
        )}
      </div>

      {/* Content */}
      <div className="p-4 space-y-3">
        <h3 className="font-semibold text-foreground">{title}</h3>
        <p className="text-sm text-muted-foreground leading-relaxed">{content}</p>

        {action && (
          <div
            className={cn(
              "p-3 rounded-lg text-sm",
              getAccentColors("cyan").bg,
              getAccentColors("cyan").text,
            )}
          >
            <span className="font-medium">Try this:</span> {action}
          </div>
        )}

        {/* Success animation */}
        {showSuccess && (
          <div className="flex items-center justify-center py-2">
            <div className="relative">
              {/* Expanding ring */}
              <div
                className={cn(
                  "absolute inset-0 rounded-full",
                  getAccentColors("green").bg,
                  "tutorial-success-ring",
                )}
              />
              {/* Checkmark icon */}
              <CheckCircle2 className={cn("w-8 h-8 text-green-500 tutorial-success-animation")} />
            </div>
          </div>
        )}

        {/* Waiting indicator */}
        {isWaiting && !isTimedOut && !showSuccess && (
          <div
            className={cn(
              "flex items-center gap-3 p-3 rounded-lg",
              "bg-muted/30 border border-border/50",
              "tutorial-waiting-pulse",
            )}
          >
            <div className="flex items-center gap-1">
              <Loader2 className="w-4 h-4 animate-spin text-primary" />
            </div>
            <div className="flex-1">
              <span className="text-sm text-foreground">
                Waiting for you to complete the action
              </span>
              <span className="inline-flex ml-1">
                <span className="tutorial-waiting-dot-1">.</span>
                <span className="tutorial-waiting-dot-2">.</span>
                <span className="tutorial-waiting-dot-3">.</span>
              </span>
            </div>
          </div>
        )}

        {/* Hint message after timeout */}
        {isTimedOut && hintMessage && (
          <div
            className={cn(
              "p-3 rounded-lg text-sm border",
              "bg-amber-500/10 border-amber-500/30 text-amber-200",
            )}
          >
            <span className="font-medium">Hint:</span> {hintMessage}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex flex-col gap-3 p-4 border-t border-border/50">
        {/* Navigation buttons */}
        <div className="flex items-center justify-between">
          <button
            onClick={onPrevious}
            disabled={isFirstStep}
            className={cn(
              "flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium transition-all",
              "border border-border hover:bg-muted",
              "disabled:opacity-50 disabled:cursor-not-allowed",
            )}
          >
            <ChevronLeft className="w-4 h-4" />
            Back
          </button>

          <div className="flex items-center gap-2">
            {/* Skip button (shown after timeout if allowed) */}
            {canSkip && (
              <button
                onClick={onNext}
                className={cn(
                  "flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium transition-all",
                  "border border-border hover:bg-muted text-muted-foreground",
                )}
              >
                Skip
              </button>
            )}

            {isLastStep ? (
              <button
                onClick={onComplete}
                disabled={isWaiting && !canSkip}
                className={cn(
                  "flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium transition-all",
                  "bg-primary text-primary-foreground hover:bg-primary/90",
                  "disabled:opacity-50 disabled:cursor-not-allowed",
                )}
              >
                <Check className="w-4 h-4" />
                Done
              </button>
            ) : (
              <button
                onClick={onNext}
                disabled={isWaiting && !canSkip}
                className={cn(
                  "flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium transition-all",
                  "bg-primary text-primary-foreground hover:bg-primary/90",
                  "disabled:opacity-50 disabled:cursor-not-allowed",
                )}
              >
                {isWaiting ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Waiting...
                  </>
                ) : (
                  <>
                    Next
                    <ChevronRight className="w-4 h-4" />
                  </>
                )}
              </button>
            )}
          </div>
        </div>

        {/* Keyboard shortcut hints */}
        <div className="flex items-center justify-center gap-3 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <kbd className="px-1.5 py-0.5 bg-muted/50 border border-border/50 rounded text-[10px] font-mono">
              Esc
            </kbd>
            <span>to close</span>
          </span>
          {!isWaiting && (
            <span className="flex items-center gap-1">
              <kbd className="px-1.5 py-0.5 bg-muted/50 border border-border/50 rounded text-[10px] font-mono">
                &larr;
              </kbd>
              <kbd className="px-1.5 py-0.5 bg-muted/50 border border-border/50 rounded text-[10px] font-mono">
                &rarr;
              </kbd>
              <span>to navigate</span>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
