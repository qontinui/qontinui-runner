/**
 * useTutorialEvents Hook
 *
 * Manages event listeners for event-driven tutorial step progression.
 * Inspired by Node-RED's tour system with declarative wait configurations.
 *
 * Supports four event types:
 * - dom-event: Standard DOM events (click, input, change, etc.)
 * - tauri-event: Tauri backend events
 * - dom-appear: Wait for a DOM element to appear (MutationObserver)
 * - app-action: Custom application actions via notifyAction
 */

import { useEffect, useRef, useCallback, useState } from "react";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type { TutorialStep, StepWaitCondition, TourState } from "../types/tutorial";
import { createLogger } from "@/lib/logger";

const logger = createLogger("TutorialEvents");

interface UseTutorialEventsOptions {
  /** Current tutorial step */
  currentStep: TutorialStep | null;
  /** Current step index */
  stepIndex: number;
  /** Tutorial ID */
  tutorialId: string;
  /** Whether the tutorial is active */
  isActive: boolean;
  /** Callback to advance to next step */
  onAdvance: () => void;
  /** Callback when hint should be shown after timeout */
  onShowHint?: (hint: string) => void;
  /** Callback when skip should be allowed after timeout */
  onAllowSkip?: () => void;
}

interface UseTutorialEventsReturn {
  /** Whether we're currently waiting for an event */
  isWaiting: boolean;
  /** Whether the timeout has been reached */
  isTimedOut: boolean;
  /** Current hint message (if timeout reached) */
  hintMessage: string | null;
  /** Whether skip is allowed (if timeout behavior is allow-skip) */
  canSkip: boolean;
  /** Notify the tutorial system of an app action */
  notifyAction: (actionName: string, data?: unknown) => void;
}

/**
 * Resolves a selector to find a DOM element
 * Supports both data-tutorial-id attributes and CSS selectors
 */
function resolveElement(selector: string): HTMLElement | null {
  // Try data-tutorial-id first
  let element = document.querySelector(`[data-tutorial-id="${selector}"]`) as HTMLElement | null;

  // Fall back to CSS selector
  if (!element) {
    element = document.querySelector(selector) as HTMLElement | null;
  }

  return element;
}

export function useTutorialEvents({
  currentStep,
  stepIndex,
  tutorialId,
  isActive,
  onAdvance,
  onShowHint,
  onAllowSkip,
}: UseTutorialEventsOptions): UseTutorialEventsReturn {
  // State
  const [isWaiting, setIsWaiting] = useState(false);
  const [isTimedOut, setIsTimedOut] = useState(false);
  const [hintMessage, setHintMessage] = useState<string | null>(null);
  const [canSkip, setCanSkip] = useState(false);

  // Refs for cleanup
  const cleanupFnsRef = useRef<Array<() => void>>([]);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const advanceDelayRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const appActionHandlersRef = useRef<Map<string, (data?: unknown) => void>>(new Map());
  const currentWaitRef = useRef<StepWaitCondition | null>(null);

  // Tour state shared across steps
  const tourStateRef = useRef<TourState>({
    stepIndex,
    tutorialId,
    data: {},
  });

  // Update tour state when step changes
  useEffect(() => {
    tourStateRef.current.stepIndex = stepIndex;
    tourStateRef.current.tutorialId = tutorialId;
  }, [stepIndex, tutorialId]);

  /**
   * Clean up all event listeners and timers
   */
  const cleanup = useCallback(() => {
    // Clear timeouts
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    if (advanceDelayRef.current) {
      clearTimeout(advanceDelayRef.current);
      advanceDelayRef.current = null;
    }

    // Run all cleanup functions
    cleanupFnsRef.current.forEach((fn) => fn());
    cleanupFnsRef.current = [];

    // Clear app action handlers
    appActionHandlersRef.current.clear();

    // Clear current wait config
    currentWaitRef.current = null;

    // Reset state
    setIsWaiting(false);
    setIsTimedOut(false);
    setHintMessage(null);
    setCanSkip(false);
  }, []);

  /**
   * Handle successful event that matches wait condition
   */
  const handleEventMatch = useCallback(() => {
    const advanceDelay = currentWaitRef.current?.advanceDelay ?? 0;

    if (advanceDelay > 0) {
      // Show a brief "success" state before advancing
      logger.debug(`Condition met, advancing in ${advanceDelay}ms`);
      advanceDelayRef.current = setTimeout(() => {
        cleanup();
        onAdvance();
      }, advanceDelay);
    } else {
      cleanup();
      onAdvance();
    }
  }, [cleanup, onAdvance]);

  /**
   * Set up DOM event listener
   */
  const setupDOMEventListener = useCallback(
    (wait: StepWaitCondition) => {
      if (!wait.event || !wait.selector) return;

      const element = resolveElement(wait.selector);
      if (!element) {
        console.warn(`[useTutorialEvents] Element not found for selector: ${wait.selector}`);
        return;
      }

      const handler = (event: Event) => {
        // Apply filter if provided
        if (wait.filter) {
          const shouldAdvance = wait.filter(event, tourStateRef.current);
          if (!shouldAdvance) return;
        }
        handleEventMatch();
      };

      element.addEventListener(wait.event, handler);
      cleanupFnsRef.current.push(() => {
        element.removeEventListener(wait.event!, handler);
      });
    },
    [handleEventMatch],
  );

  /**
   * Set up Tauri event listener
   */
  const setupTauriEventListener = useCallback(
    async (wait: StepWaitCondition) => {
      if (!wait.tauriEvent) return;

      try {
        const unlisten: UnlistenFn = await listen(wait.tauriEvent, (event) => {
          // Apply filter if provided
          if (wait.filter) {
            const shouldAdvance = wait.filter(event.payload, tourStateRef.current);
            if (!shouldAdvance) return;
          }
          handleEventMatch();
        });

        cleanupFnsRef.current.push(unlisten);
      } catch (error) {
        console.error(
          `[useTutorialEvents] Failed to listen to Tauri event: ${wait.tauriEvent}`,
          error,
        );
      }
    },
    [handleEventMatch],
  );

  /**
   * Set up DOM appearance watcher using MutationObserver
   */
  const setupDOMAppearWatcher = useCallback(
    (wait: StepWaitCondition) => {
      if (!wait.selector) return;

      // Check if element already exists
      const existingElement = resolveElement(wait.selector);
      if (existingElement) {
        // Apply filter if provided
        if (wait.filter) {
          const shouldAdvance = wait.filter(existingElement, tourStateRef.current);
          if (shouldAdvance) {
            handleEventMatch();
            return;
          }
        } else {
          handleEventMatch();
          return;
        }
      }

      // Set up MutationObserver to watch for element appearance
      const observer = new MutationObserver(() => {
        const element = resolveElement(wait.selector!);
        if (element) {
          // Apply filter if provided
          if (wait.filter) {
            const shouldAdvance = wait.filter(element, tourStateRef.current);
            if (!shouldAdvance) return;
          }
          observer.disconnect();
          handleEventMatch();
        }
      });

      // Observe the entire document for added nodes
      observer.observe(document.body, {
        childList: true,
        subtree: true,
      });

      cleanupFnsRef.current.push(() => {
        observer.disconnect();
      });
    },
    [handleEventMatch],
  );

  /**
   * Set up app action listener
   * Listens for both:
   * 1. Direct calls via notifyAction (from useTutorialEvents return value)
   * 2. DOM events dispatched by useTutorialAware hook
   */
  const setupAppActionListener = useCallback(
    (wait: StepWaitCondition) => {
      if (!wait.actionName) return;

      const handler = (data?: unknown) => {
        // Apply filter if provided
        if (wait.filter) {
          const shouldAdvance = wait.filter(data, tourStateRef.current);
          if (!shouldAdvance) return;
        }
        handleEventMatch();
      };

      appActionHandlersRef.current.set(wait.actionName, handler);

      // Also listen for DOM events from useTutorialAware
      const domEventHandler = (event: Event) => {
        const customEvent = event as CustomEvent<{ actionName: string; data?: unknown }>;
        if (customEvent.detail.actionName === wait.actionName) {
          handler(customEvent.detail.data);
        }
      };

      window.addEventListener("tutorial-action", domEventHandler);
      cleanupFnsRef.current.push(() => {
        window.removeEventListener("tutorial-action", domEventHandler);
      });
    },
    [handleEventMatch],
  );

  /**
   * Set up timeout for the wait condition
   */
  const setupTimeout = useCallback(
    (wait: StepWaitCondition) => {
      const timeout = wait.timeout ?? 30000; // Default 30 seconds
      if (timeout <= 0) return;

      timeoutRef.current = setTimeout(() => {
        setIsTimedOut(true);

        if (wait.hint) {
          setHintMessage(wait.hint);
          onShowHint?.(wait.hint);
        }

        if (wait.onTimeout === "allow-skip") {
          setCanSkip(true);
          onAllowSkip?.();
        }
      }, timeout);
    },
    [onShowHint, onAllowSkip],
  );

  /**
   * Notify the tutorial of an app action
   */
  const notifyAction = useCallback((actionName: string, data?: unknown) => {
    const handler = appActionHandlersRef.current.get(actionName);
    if (handler) {
      handler(data);
    }
  }, []);

  // Set up event listeners when step changes
  useEffect(() => {
    // Clean up previous listeners
    cleanup();

    // Skip if not active or no step
    if (!isActive || !currentStep) return;

    const wait = currentStep.wait;
    if (!wait) {
      // No wait condition - step uses manual Next button
      return;
    }

    // Store current wait config for handleEventMatch to access
    currentWaitRef.current = wait;
    setIsWaiting(true);

    // Set up the appropriate listener based on wait type
    switch (wait.type) {
      case "dom-event":
        setupDOMEventListener(wait);
        break;
      case "tauri-event":
        setupTauriEventListener(wait);
        break;
      case "dom-appear":
        setupDOMAppearWatcher(wait);
        break;
      case "app-action":
        setupAppActionListener(wait);
        break;
    }

    // Set up timeout
    setupTimeout(wait);

    // Cleanup on unmount or step change
    return cleanup;
  }, [
    isActive,
    currentStep,
    stepIndex,
    cleanup,
    setupDOMEventListener,
    setupTauriEventListener,
    setupDOMAppearWatcher,
    setupAppActionListener,
    setupTimeout,
  ]);

  return {
    isWaiting,
    isTimedOut,
    hintMessage,
    canSkip,
    notifyAction,
  };
}

/**
 * Hook for components to notify the tutorial system of actions
 *
 * Example usage:
 * ```tsx
 * const { notifyAction } = useTutorialAware();
 *
 * function handleSave() {
 *   // ... save logic ...
 *   notifyAction('workflow-saved', { id: workflow.id });
 * }
 * ```
 */
export function useTutorialAware() {
  // This is a placeholder - the actual implementation will be connected
  // to the TutorialContext through a shared event bus or context

  const notifyAction = useCallback((actionName: string, data?: unknown) => {
    // Dispatch custom event that useTutorialEvents can listen to
    const event = new CustomEvent("tutorial-action", {
      detail: { actionName, data },
    });
    window.dispatchEvent(event);
  }, []);

  return { notifyAction };
}
