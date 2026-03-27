/**
 * ContextualTutorial Component
 *
 * Renders contextual tutorials with spotlight highlighting and positioned tooltips.
 * Uses event-driven progression for interactive tutorials that guide users through the actual UI.
 */

import { useCallback, useState, useEffect, useRef } from "react";
import { X, ChevronLeft, ChevronRight, Check, Loader2, CheckCircle2 } from "lucide-react";
import { useTutorial } from "../../contexts/TutorialContext";
import { useTutorialKeyboard } from "../../hooks/useTutorialKeyboard";
import { useTutorialEvents } from "../../hooks/useTutorialEvents";
import { SpotlightOverlay } from "./SpotlightOverlay";
import { TutorialTooltip } from "./TutorialTooltip";
import { cn } from "../../lib/utils";
import { getAccentColors } from "@/design-system";
import { createLogger } from "@/lib/logger";

const logger = createLogger("ContextualTutorial");

/**
 * CenteredTooltip - Simple centered modal for steps without a target
 */
function CenteredTooltip({
  title,
  content,
  action,
  currentStep,
  totalSteps,
  isFirstStep,
  isLastStep,
  isWaiting,
  isTimedOut,
  hintMessage,
  canSkip,
  showSuccess,
  onNext,
  onPrevious,
  onSkip,
  onComplete,
}: {
  title: string;
  content: string;
  action?: string;
  currentStep: number;
  totalSteps: number;
  isFirstStep: boolean;
  isLastStep: boolean;
  isWaiting: boolean;
  isTimedOut: boolean;
  hintMessage: string | null;
  canSkip: boolean;
  showSuccess: boolean;
  onNext: () => void;
  onPrevious: () => void;
  onSkip: () => void;
  onComplete: () => void;
}) {
  return (
    <div className="fixed inset-0 z-tutorial-spotlight flex items-center justify-center bg-black/60">
      <div
        className={cn(
          "bg-card border border-border rounded-xl shadow-2xl",
          "w-[90vw] max-w-md",
          "tutorial-scale-in",
        )}
      >
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
          <button
            onClick={onSkip}
            className="p-1 hover:bg-muted rounded-md transition-colors"
            aria-label="Skip tutorial"
          >
            <X className="w-4 h-4 text-muted-foreground" />
          </button>
        </div>

        {/* Content */}
        <div className="p-4 space-y-3">
          <h3 className="font-semibold text-foreground text-lg">{title}</h3>
          <div className="text-sm text-muted-foreground leading-relaxed whitespace-pre-line">
            {content}
          </div>

          {action && (
            <div
              className={cn(
                "p-3 rounded-lg text-sm",
                getAccentColors("cyan").bg,
                getAccentColors("cyan").text,
              )}
            >
              <span className="font-medium">Action:</span> {action}
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
    </div>
  );
}

/**
 * ContextualTutorial - Renders spotlight + tooltip for contextual tutorials
 */
export function ContextualTutorial() {
  const {
    currentTutorial,
    currentStepIndex,
    isOpen,
    currentMode,
    getCurrentStep,
    isFirstStep,
    isLastStep,
    nextStep,
    previousStep,
    closeTutorial,
    completeTutorial,
  } = useTutorial();

  const currentStep = getCurrentStep();

  // Track success animation state
  const [showSuccess, setShowSuccess] = useState(false);
  const prevStepIndexRef = useRef(currentStepIndex);
  const wasWaitingRef = useRef(false);

  // Track if target element exists (check periodically in case DOM changes)
  const [targetElementExists, setTargetElementExists] = useState(false);

  // Handle hint display
  const handleShowHint = useCallback((hint: string) => {
    logger.debug("Hint triggered:", hint);
  }, []);

  // Handle skip allowed
  const handleAllowSkip = useCallback(() => {
    logger.debug("Skip now allowed");
  }, []);

  // Event-driven step progression
  const { isWaiting, isTimedOut, hintMessage, canSkip } = useTutorialEvents({
    currentStep,
    stepIndex: currentStepIndex,
    tutorialId: currentTutorial?.id ?? "",
    isActive: isOpen && !!currentTutorial,
    onAdvance: nextStep,
    onShowHint: handleShowHint,
    onAllowSkip: handleAllowSkip,
  });

  // Trigger success animation when step auto-advances from waiting state
  useEffect(() => {
    // Detect when we move to next step after being in waiting state
    if (prevStepIndexRef.current !== currentStepIndex && wasWaitingRef.current) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- animation trigger on step transition
      setShowSuccess(true);
      // Clear success animation after it plays
      const timer = setTimeout(() => {
        setShowSuccess(false);
      }, 600); // Match animation duration
      return () => clearTimeout(timer);
    }
    prevStepIndexRef.current = currentStepIndex;
  }, [currentStepIndex]);

  // Track waiting state changes
  useEffect(() => {
    wasWaitingRef.current = isWaiting;
  }, [isWaiting]);

  // Check if target element exists - update when step changes or DOM might change
  useEffect(() => {
    const targetSelector = currentStep?.targetElement?.selector;
    if (!targetSelector) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- DOM observation
      setTargetElementExists(false);
      return;
    }

    const checkElement = () => {
      const exists = !!(
        document.querySelector(`[data-tutorial-id="${targetSelector}"]`) ||
        document.querySelector(targetSelector)
      );
      setTargetElementExists(exists);
    };

    // Check immediately
    checkElement();

    // Also check after a short delay in case DOM is still updating
    const timeout1 = setTimeout(checkElement, 100);
    const timeout2 = setTimeout(checkElement, 500);

    // Set up a MutationObserver to detect when the element might appear
    const observer = new MutationObserver(() => {
      checkElement();
    });
    observer.observe(document.body, { childList: true, subtree: true });

    return () => {
      clearTimeout(timeout1);
      clearTimeout(timeout2);
      observer.disconnect();
    };
  }, [currentStep?.targetElement?.selector, currentStepIndex]);

  // Debug logging
  logger.debug("Render state:", {
    hasTutorial: !!currentTutorial,
    tutorialId: currentTutorial?.id,
    currentStepIndex,
    isOpen,
    currentMode,
    totalSteps: currentTutorial?.steps?.length,
    isWaiting,
    hasWaitConfig: !!currentStep?.wait,
  });

  // Keyboard navigation (disabled when waiting for event-driven step)
  useTutorialKeyboard({
    isActive: isOpen && !isWaiting,
    onNext: nextStep,
    onPrevious: previousStep,
    onClose: closeTutorial,
    isFirstStep: isFirstStep(),
    isLastStep: isLastStep(),
    onComplete: completeTutorial,
  });

  // Only render when tutorial is open
  if (!currentTutorial || !isOpen) {
    logger.debug("Not rendering:", {
      reason: !currentTutorial ? "no tutorial" : "not open",
    });
    return null;
  }

  if (!currentStep) {
    logger.debug("No current step found");
    return null;
  }

  logger.debug("Rendering step:", {
    stepId: currentStep.id,
    hasTarget: !!currentStep.targetElement,
    targetSelector: currentStep.targetElement?.selector,
  });

  const targetSelector = currentStep.targetElement?.selector ?? null;
  const tooltipPosition = currentStep.targetElement?.position ?? "right";
  const totalSteps = currentTutorial.steps.length;

  // For steps without a target element OR when target element isn't found, show centered modal
  // This prevents the tutorial from disappearing when the target element is missing
  const showCenteredTooltip = !targetSelector || !targetElementExists;

  return (
    <>
      {/* Spotlight overlay - only show if there's a target AND it exists */}
      {targetSelector && targetElementExists && (
        <SpotlightOverlay
          targetSelector={targetSelector}
          isVisible={true}
          padding={12}
          borderRadius={8}
          overlayOpacity={0.6}
          allowClickThrough={currentStep.targetElement?.allowInteraction ?? false}
          showPulse={true}
        />
      )}

      {/* Tooltip - positioned near target or centered modal */}
      {showCenteredTooltip ? (
        <CenteredTooltip
          title={currentStep.title}
          content={currentStep.content}
          action={currentStep.action}
          currentStep={currentStepIndex + 1}
          totalSteps={totalSteps}
          isFirstStep={isFirstStep()}
          isLastStep={isLastStep()}
          isWaiting={isWaiting}
          isTimedOut={isTimedOut}
          hintMessage={hintMessage}
          canSkip={canSkip}
          showSuccess={showSuccess}
          onNext={nextStep}
          onPrevious={previousStep}
          onSkip={closeTutorial}
          onComplete={completeTutorial}
        />
      ) : (
        <TutorialTooltip
          targetSelector={targetSelector}
          title={currentStep.title}
          content={currentStep.content}
          position={tooltipPosition}
          currentStep={currentStepIndex + 1}
          totalSteps={totalSteps}
          isFirstStep={isFirstStep()}
          isLastStep={isLastStep()}
          isVisible={true}
          isWaiting={isWaiting}
          isTimedOut={isTimedOut}
          hintMessage={hintMessage}
          canSkip={canSkip}
          showSuccess={showSuccess}
          onNext={nextStep}
          onPrevious={previousStep}
          onSkip={closeTutorial}
          onComplete={completeTutorial}
          action={currentStep.action}
        />
      )}
    </>
  );
}
