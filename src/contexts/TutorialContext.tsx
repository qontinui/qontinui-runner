/**
 * TutorialContext
 *
 * Provides tutorial state management for the qontinui-runner application.
 * Follows the same patterns as ExecutionContext for consistency.
 */

import {
  createContext,
  useContext,
  ReactNode,
  useCallback,
  useEffect,
  useState,
  useRef,
} from "react";
import { createLogger } from "@/lib/logger";
import type {
  Tutorial,
  TutorialStep,
  TutorialMode,
  ValidationState,
  PersistedTutorialState,
} from "../types/tutorial";
import { instanceStorage } from "@/lib/instance-storage";

const log = createLogger("TutorialContext");

// Storage key for instanceStorage persistence
const STORAGE_KEY = "qontinui-runner-tutorial-state";

// Default persisted state
const DEFAULT_PERSISTED_STATE: PersistedTutorialState = {
  completedTutorials: [],
  inProgressTutorials: [],
  dontShowAgain: false,
  progressRecords: {},
};

// Store context in window to survive HMR
declare global {
  interface Window {
    __TUTORIAL_CONTEXT__?: React.Context<TutorialContextValue | null>;
  }
}

interface TutorialContextValue {
  // Core state
  currentTutorial: Tutorial | null;
  currentStepIndex: number;
  isOpen: boolean;
  currentMode: TutorialMode | null;

  // Progress tracking (persisted)
  completedTutorials: string[];
  inProgressTutorials: string[];
  dontShowAgain: boolean;

  // Contextual mode state
  targetElement: HTMLElement | null;

  // Validation state
  validationState: ValidationState | null;

  // Actions
  openTutorial: (tutorial: Tutorial, mode?: TutorialMode) => void;
  closeTutorial: () => void;
  nextStep: () => Promise<void>;
  previousStep: () => void;
  goToStep: (index: number) => void;
  completeTutorial: () => void;
  skipTutorial: () => void;

  // Computed helpers
  getCurrentStep: () => TutorialStep | null;
  isFirstStep: () => boolean;
  isLastStep: () => boolean;
  getCompletionPercentage: () => number;
  isTutorialCompleted: (tutorialId: string) => boolean;
  isTutorialInProgress: (tutorialId: string) => boolean;

  // Contextual helpers
  highlightElement: (selector: string) => boolean;
  clearHighlight: () => void;

  // Persistence controls
  setDontShowAgain: (value: boolean) => void;
  resetProgress: () => void;

  // Target registration for components
  registerTarget: (id: string, element: HTMLElement) => void;
  unregisterTarget: (id: string) => void;
  getTarget: (id: string) => HTMLElement | null;
}

// Create context once and store in window to survive HMR reloads
const TutorialContext: React.Context<TutorialContextValue | null> =
  window.__TUTORIAL_CONTEXT__ ||
  (window.__TUTORIAL_CONTEXT__ = createContext<TutorialContextValue | null>(null));

interface TutorialProviderProps {
  children: ReactNode;
  /** Callback to navigate to a page when tutorial starts */
  onNavigate?: (page: string) => void;
}

/**
 * Load persisted state from instanceStorage
 */
function loadPersistedState(): PersistedTutorialState {
  try {
    const stored = instanceStorage.getJSON<Partial<PersistedTutorialState> | null>(
      STORAGE_KEY,
      null,
    );
    if (stored) {
      return {
        ...DEFAULT_PERSISTED_STATE,
        ...stored,
      };
    }
  } catch (error) {
    console.warn("[TutorialContext] Failed to load persisted state:", error);
  }
  return DEFAULT_PERSISTED_STATE;
}

/**
 * Save state to instanceStorage
 */
function savePersistedState(state: PersistedTutorialState): void {
  try {
    instanceStorage.setJSON(STORAGE_KEY, state);
  } catch (error) {
    console.warn("[TutorialContext] Failed to save persisted state:", error);
  }
}

/**
 * TutorialProvider - Manages tutorial state and provides context
 */
export function TutorialProvider({ children, onNavigate }: TutorialProviderProps) {
  // Load initial state from instanceStorage
  const [persistedState, setPersistedState] = useState<PersistedTutorialState>(() =>
    loadPersistedState(),
  );

  // Core tutorial state
  const [currentTutorial, setCurrentTutorial] = useState<Tutorial | null>(null);
  const [currentStepIndex, setCurrentStepIndex] = useState(0);
  const [isOpen, setIsOpen] = useState(false);
  const [currentMode, setCurrentMode] = useState<TutorialMode | null>(null);

  // Contextual mode state
  const [targetElement, setTargetElement] = useState<HTMLElement | null>(null);

  // Validation state
  const [validationState, setValidationState] = useState<ValidationState | null>(null);

  // Target element registry
  const targetRegistry = useRef<Map<string, HTMLElement>>(new Map());

  // Persist state changes to instanceStorage
  useEffect(() => {
    savePersistedState(persistedState);
  }, [persistedState]);

  /**
   * Register a target element for contextual highlighting
   */
  const registerTarget = useCallback((id: string, element: HTMLElement) => {
    targetRegistry.current.set(id, element);
  }, []);

  /**
   * Unregister a target element
   */
  const unregisterTarget = useCallback((id: string) => {
    targetRegistry.current.delete(id);
  }, []);

  /**
   * Get a registered target element by ID
   */
  const getTarget = useCallback((id: string): HTMLElement | null => {
    return targetRegistry.current.get(id) || null;
  }, []);

  // ========== HIGHLIGHT FUNCTIONS (defined first to avoid circular deps) ==========

  /**
   * Clear element highlight
   */
  const clearHighlight = useCallback(() => {
    // Note: We don't use targetElement in deps to avoid circular dependency
    // Clear all highlighted elements from the DOM
    document.querySelectorAll(".tutorial-target-active").forEach((el) => {
      el.classList.remove(
        "tutorial-target-active",
        "tutorial-target-border",
        "tutorial-target-pulse",
      );
    });
    setTargetElement(null);
  }, []);

  /**
   * Highlight element by selector
   */
  const highlightElement = useCallback(
    (selector: string): boolean => {
      // Try data-tutorial-id first
      let element = document.querySelector(`[data-tutorial-id="${selector}"]`) as HTMLElement;

      // Fall back to CSS selector
      if (!element) {
        element = document.querySelector(selector) as HTMLElement;
      }

      if (element) {
        // Remove previous highlight
        clearHighlight();

        // Add highlight class
        element.classList.add("tutorial-target-active");
        setTargetElement(element);

        // Scroll into view if needed
        element.scrollIntoView({
          behavior: "smooth",
          block: "center",
          inline: "center",
        });

        return true;
      }

      console.warn("[TutorialContext] Target element not found:", selector);
      return false;
    },
    [clearHighlight],
  );

  // ========== TUTORIAL ACTIONS ==========

  /**
   * Complete tutorial
   */
  const completeTutorial = useCallback(() => {
    if (!currentTutorial) return;

    log.debug("Completing tutorial:", currentTutorial.id);

    // Update persisted state
    setPersistedState((prev) => {
      const newCompleted = prev.completedTutorials.includes(currentTutorial.id)
        ? prev.completedTutorials
        : [...prev.completedTutorials, currentTutorial.id];

      const newInProgress = prev.inProgressTutorials.filter((id) => id !== currentTutorial.id);

      // Update progress record with completion time
      const updatedProgress = {
        ...prev.progressRecords[currentTutorial.id],
        completedAt: Date.now(),
        stepProgress: [
          ...(prev.progressRecords[currentTutorial.id]?.stepProgress || []),
          {
            stepId: currentTutorial.steps[currentStepIndex].id,
            completed: true,
            timestamp: Date.now(),
          },
        ],
      };

      return {
        ...prev,
        completedTutorials: newCompleted,
        inProgressTutorials: newInProgress,
        progressRecords: {
          ...prev.progressRecords,
          [currentTutorial.id]: updatedProgress,
        },
      };
    });

    // Close the tutorial
    clearHighlight();
    setCurrentTutorial(null);
    setCurrentStepIndex(0);
    setIsOpen(false);
    setCurrentMode(null);
    setValidationState(null);
  }, [currentTutorial, currentStepIndex, clearHighlight]);

  /**
   * Open a tutorial
   */
  const openTutorial = useCallback(
    (tutorial: Tutorial, mode?: TutorialMode) => {
      log.debug("Opening tutorial:", tutorial.id, "mode:", mode || tutorial.mode);

      // Navigate to focus page if specified
      if (tutorial.focusPage && onNavigate) {
        log.debug("Navigating to focus page:", tutorial.focusPage);
        onNavigate(tutorial.focusPage);
      }

      setCurrentTutorial(tutorial);
      setCurrentMode(mode || tutorial.mode);
      setIsOpen(true);
      setValidationState(null);

      // Check for existing progress
      const existingProgress = persistedState.progressRecords[tutorial.id];
      if (existingProgress) {
        setCurrentStepIndex(existingProgress.currentStepIndex);
      } else {
        setCurrentStepIndex(0);
        // Mark as in-progress
        setPersistedState((prev) => ({
          ...prev,
          inProgressTutorials: prev.inProgressTutorials.includes(tutorial.id)
            ? prev.inProgressTutorials
            : [...prev.inProgressTutorials, tutorial.id],
          progressRecords: {
            ...prev.progressRecords,
            [tutorial.id]: {
              tutorialId: tutorial.id,
              currentStepIndex: 0,
              stepProgress: [],
              startedAt: Date.now(),
            },
          },
        }));
      }

      // Highlight target element if step has a target
      const initialStep = tutorial.steps[existingProgress?.currentStepIndex || 0];
      if (initialStep?.targetElement) {
        highlightElement(initialStep.targetElement.selector);
      }
    },
    [persistedState.progressRecords, highlightElement, onNavigate],
  );

  /**
   * Close tutorial without completing
   */
  const closeTutorial = useCallback(() => {
    log.debug("Closing tutorial");

    // Save current progress
    if (currentTutorial) {
      setPersistedState((prev) => ({
        ...prev,
        progressRecords: {
          ...prev.progressRecords,
          [currentTutorial.id]: {
            ...prev.progressRecords[currentTutorial.id],
            currentStepIndex,
          },
        },
      }));
    }

    clearHighlight();
    setCurrentTutorial(null);
    setCurrentStepIndex(0);
    setIsOpen(false);
    setCurrentMode(null);
    setValidationState(null);
  }, [currentTutorial, currentStepIndex, clearHighlight]);

  /**
   * Skip tutorial (close without saving progress)
   */
  const skipTutorial = useCallback(() => {
    log.debug("Skipping tutorial");

    clearHighlight();
    setCurrentTutorial(null);
    setCurrentStepIndex(0);
    setIsOpen(false);
    setCurrentMode(null);
    setValidationState(null);
  }, [clearHighlight]);

  /**
   * Move to next step
   */
  const nextStep = useCallback(async () => {
    log.debug("nextStep called:", {
      hasTutorial: !!currentTutorial,
      currentStepIndex,
      totalSteps: currentTutorial?.steps?.length,
    });

    if (!currentTutorial) {
      log.debug("nextStep: No tutorial, returning");
      return;
    }

    const currentStep = currentTutorial.steps[currentStepIndex];

    // Run validation if required
    if (currentStep?.validation && !currentStep.validation.optional) {
      setValidationState({
        isValidating: true,
        isValid: false,
        feedback: "",
        feedbackType: "failure",
      });

      try {
        const isValid = await currentStep.validation.validate();
        if (!isValid) {
          setValidationState({
            isValidating: false,
            isValid: false,
            feedback: currentStep.validation.feedback.failure,
            feedbackType: "failure",
          });
          return;
        }
        setValidationState({
          isValidating: false,
          isValid: true,
          feedback: currentStep.validation.feedback.success,
          feedbackType: "success",
        });
      } catch (error) {
        console.error("[TutorialContext] Validation error:", error);
        setValidationState({
          isValidating: false,
          isValid: false,
          feedback: currentStep.validation.feedback.failure,
          feedbackType: "failure",
        });
        return;
      }
    }

    // Clear validation state after short delay
    setTimeout(() => setValidationState(null), 1500);

    // Check if this is the last step
    if (currentStepIndex >= currentTutorial.steps.length - 1) {
      log.debug("On last step, completing tutorial");
      completeTutorial();
      return;
    }

    // Move to next step
    const nextIndex = currentStepIndex + 1;
    log.debug("Moving to next step:", {
      from: currentStepIndex,
      to: nextIndex,
      nextStepId: currentTutorial.steps[nextIndex]?.id,
    });
    setCurrentStepIndex(nextIndex);

    // Update progress
    setPersistedState((prev) => ({
      ...prev,
      progressRecords: {
        ...prev.progressRecords,
        [currentTutorial.id]: {
          ...prev.progressRecords[currentTutorial.id],
          currentStepIndex: nextIndex,
          stepProgress: [
            ...(prev.progressRecords[currentTutorial.id]?.stepProgress || []),
            {
              stepId: currentStep.id,
              completed: true,
              timestamp: Date.now(),
            },
          ],
        },
      },
    }));

    // Highlight next target element if step has a target
    const nextStepData = currentTutorial.steps[nextIndex];
    if (nextStepData?.targetElement) {
      highlightElement(nextStepData.targetElement.selector);
    } else {
      clearHighlight();
    }
  }, [currentTutorial, currentStepIndex, completeTutorial, highlightElement, clearHighlight]);

  /**
   * Move to previous step
   */
  const previousStep = useCallback(() => {
    log.debug("previousStep called:", {
      hasTutorial: !!currentTutorial,
      currentStepIndex,
      canGoBack: currentStepIndex > 0,
    });

    if (!currentTutorial || currentStepIndex <= 0) {
      log.debug("previousStep: Cannot go back");
      return;
    }

    const prevIndex = currentStepIndex - 1;
    log.debug("Moving to previous step:", {
      from: currentStepIndex,
      to: prevIndex,
      prevStepId: currentTutorial.steps[prevIndex]?.id,
    });
    setCurrentStepIndex(prevIndex);
    setValidationState(null);

    // Update progress
    setPersistedState((prev) => ({
      ...prev,
      progressRecords: {
        ...prev.progressRecords,
        [currentTutorial.id]: {
          ...prev.progressRecords[currentTutorial.id],
          currentStepIndex: prevIndex,
        },
      },
    }));

    // Highlight previous target element if step has a target
    const prevStep = currentTutorial.steps[prevIndex];
    if (prevStep?.targetElement) {
      highlightElement(prevStep.targetElement.selector);
    } else {
      clearHighlight();
    }
  }, [currentTutorial, currentStepIndex, highlightElement, clearHighlight]);

  /**
   * Go to specific step
   */
  const goToStep = useCallback(
    (index: number) => {
      if (!currentTutorial || index < 0 || index >= currentTutorial.steps.length) return;

      setCurrentStepIndex(index);
      setValidationState(null);

      // Update progress
      setPersistedState((prev) => ({
        ...prev,
        progressRecords: {
          ...prev.progressRecords,
          [currentTutorial.id]: {
            ...prev.progressRecords[currentTutorial.id],
            currentStepIndex: index,
          },
        },
      }));

      // Highlight target element if step has a target
      const step = currentTutorial.steps[index];
      if (step?.targetElement) {
        highlightElement(step.targetElement.selector);
      } else {
        clearHighlight();
      }
    },
    [currentTutorial, highlightElement, clearHighlight],
  );

  // ========== COMPUTED HELPERS ==========

  /**
   * Get current step
   */
  const getCurrentStep = useCallback((): TutorialStep | null => {
    if (!currentTutorial || currentStepIndex < 0) return null;
    return currentTutorial.steps[currentStepIndex] || null;
  }, [currentTutorial, currentStepIndex]);

  /**
   * Check if on first step
   */
  const isFirstStep = useCallback((): boolean => {
    return currentStepIndex === 0;
  }, [currentStepIndex]);

  /**
   * Check if on last step
   */
  const isLastStep = useCallback((): boolean => {
    if (!currentTutorial) return false;
    return currentStepIndex >= currentTutorial.steps.length - 1;
  }, [currentTutorial, currentStepIndex]);

  /**
   * Get completion percentage
   */
  const getCompletionPercentage = useCallback((): number => {
    if (!currentTutorial || currentTutorial.steps.length === 0) return 0;
    return Math.round((currentStepIndex / currentTutorial.steps.length) * 100);
  }, [currentTutorial, currentStepIndex]);

  /**
   * Check if tutorial is completed
   */
  const isTutorialCompleted = useCallback(
    (tutorialId: string): boolean => {
      return persistedState.completedTutorials.includes(tutorialId);
    },
    [persistedState.completedTutorials],
  );

  /**
   * Check if tutorial is in progress
   */
  const isTutorialInProgress = useCallback(
    (tutorialId: string): boolean => {
      return persistedState.inProgressTutorials.includes(tutorialId);
    },
    [persistedState.inProgressTutorials],
  );

  /**
   * Set don't show again preference
   */
  const setDontShowAgain = useCallback((value: boolean) => {
    setPersistedState((prev) => ({
      ...prev,
      dontShowAgain: value,
    }));
  }, []);

  /**
   * Reset all progress
   */
  const resetProgress = useCallback(() => {
    setPersistedState(DEFAULT_PERSISTED_STATE);
    setCurrentTutorial(null);
    setCurrentStepIndex(0);
    setIsOpen(false);
    setCurrentMode(null);
    setValidationState(null);
    clearHighlight();
  }, [clearHighlight]);

  const value: TutorialContextValue = {
    // Core state
    currentTutorial,
    currentStepIndex,
    isOpen,
    currentMode,

    // Persisted state
    completedTutorials: persistedState.completedTutorials,
    inProgressTutorials: persistedState.inProgressTutorials,
    dontShowAgain: persistedState.dontShowAgain,

    // Contextual state
    targetElement,

    // Validation
    validationState,

    // Actions
    openTutorial,
    closeTutorial,
    nextStep,
    previousStep,
    goToStep,
    completeTutorial,
    skipTutorial,

    // Computed helpers
    getCurrentStep,
    isFirstStep,
    isLastStep,
    getCompletionPercentage,
    isTutorialCompleted,
    isTutorialInProgress,

    // Contextual helpers
    highlightElement,
    clearHighlight,

    // Persistence controls
    setDontShowAgain,
    resetProgress,

    // Target registration
    registerTarget,
    unregisterTarget,
    getTarget,
  };

  return <TutorialContext.Provider value={value}>{children}</TutorialContext.Provider>;
}

/**
 * Hook to access tutorial context
 */
export function useTutorial() {
  const context = useContext(TutorialContext);
  if (!context) {
    throw new Error("useTutorial must be used within TutorialProvider");
  }
  return context;
}

/**
 * Hook to get current active tutorial and step
 */
export function useActiveTutorial() {
  const { currentTutorial, getCurrentStep, currentStepIndex, isOpen, currentMode } = useTutorial();

  return {
    tutorial: currentTutorial,
    step: getCurrentStep(),
    stepIndex: currentStepIndex,
    isActive: isOpen,
    mode: currentMode,
  };
}

/**
 * Hook to get tutorial progress info
 */
export function useTutorialProgress() {
  const { completedTutorials, inProgressTutorials, isTutorialCompleted, isTutorialInProgress } =
    useTutorial();

  return {
    completed: completedTutorials,
    inProgress: inProgressTutorials,
    isCompleted: isTutorialCompleted,
    isInProgress: isTutorialInProgress,
    completedCount: completedTutorials.length,
    inProgressCount: inProgressTutorials.length,
  };
}
