/**
 * useTutorialKeyboard
 *
 * Keyboard navigation hook for the tutorial system.
 * Handles arrow keys for navigation and Escape to close.
 */

import { useEffect, useCallback } from "react";

interface UseTutorialKeyboardOptions {
  /** Whether the tutorial is currently active */
  isActive: boolean;

  /** Callback for next step (right arrow) */
  onNext: () => void;

  /** Callback for previous step (left arrow) */
  onPrevious: () => void;

  /** Callback for closing tutorial (Escape) */
  onClose: () => void;

  /** Whether currently on first step (disables previous) */
  isFirstStep?: boolean;

  /** Whether currently on last step (changes next behavior) */
  isLastStep?: boolean;

  /** Optional callback for completing tutorial (Enter on last step) */
  onComplete?: () => void;
}

/**
 * Hook that provides keyboard navigation for tutorials
 *
 * @example
 * useTutorialKeyboard({
 *   isActive: tutorialOpen,
 *   onNext: nextStep,
 *   onPrevious: previousStep,
 *   onClose: closeTutorial,
 * });
 */
export function useTutorialKeyboard({
  isActive,
  onNext,
  onPrevious,
  onClose,
  isFirstStep = false,
  isLastStep = false,
  onComplete,
}: UseTutorialKeyboardOptions): void {
  const handleKeyDown = useCallback(
    (event: KeyboardEvent) => {
      // Only handle when tutorial is active
      if (!isActive) return;

      // Don't intercept if user is typing in an input
      const target = event.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable) {
        return;
      }

      switch (event.key) {
        case "ArrowRight":
        case "ArrowDown":
          event.preventDefault();
          onNext();
          break;

        case "ArrowLeft":
        case "ArrowUp":
          event.preventDefault();
          if (!isFirstStep) {
            onPrevious();
          }
          break;

        case "Escape":
          event.preventDefault();
          onClose();
          break;

        case "Enter":
          // Complete tutorial on Enter if on last step
          if (isLastStep && onComplete) {
            event.preventDefault();
            onComplete();
          }
          break;

        default:
          break;
      }
    },
    [isActive, onNext, onPrevious, onClose, isFirstStep, isLastStep, onComplete],
  );

  useEffect(() => {
    if (isActive) {
      document.addEventListener("keydown", handleKeyDown);
      return () => {
        document.removeEventListener("keydown", handleKeyDown);
      };
    }
  }, [isActive, handleKeyDown]);
}

export default useTutorialKeyboard;
