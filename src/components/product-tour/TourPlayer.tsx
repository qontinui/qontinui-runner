/**
 * TourPlayer
 *
 * Lightweight floating player that runs a ProductTour:
 * - Highlights target elements via spotlight/pulse
 * - Auto-executes demo actions via UI Bridge
 * - Advances steps via timer, click, or after action completes
 * - Shows progress dots and skip/close controls
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { X, ChevronRight, ChevronLeft, Loader2 } from "lucide-react";
import type {
  ProductTour,
  ProductTourStep,
  TourDemoAction,
  TourPlayerState,
} from "@/types/product-tour";
import { INITIAL_PLAYER_STATE } from "@/types/product-tour";
import { getApiBase } from "@/lib/runner-api";

// =============================================================================
// Demo Action Execution
// =============================================================================

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function executeDemoAction(action: TourDemoAction): Promise<void> {
  const base = `${getApiBase()}/ui-bridge`;

  switch (action.type) {
    case "navigate":
      await fetch(`${base}/control/page/navigate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: action.target }),
      });
      break;
    case "click":
      await fetch(`${base}/control/element/${action.elementId}/action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "click" }),
      });
      break;
    case "type":
      await fetch(`${base}/control/element/${action.elementId}/action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "type", params: { text: action.value, clear: true } }),
      });
      break;
    case "select":
      await fetch(`${base}/control/element/${action.elementId}/action`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "select", params: { value: action.value } }),
      });
      break;
    case "wait":
      await sleep(action.ms);
      break;
  }
}

// =============================================================================
// Element Highlighting
// =============================================================================

function highlightElement(step: ProductTourStep): (() => void) | null {
  if (!step.targetElement) return null;

  const selector = step.targetElement.elementId
    ? `[data-element-id="${step.targetElement.elementId}"]`
    : step.targetElement.selector;
  if (!selector) return null;

  const el = document.querySelector(selector) as HTMLElement | null;
  if (!el) return null;

  el.scrollIntoView({ behavior: "smooth", block: "center" });

  const cls =
    step.targetElement.highlight === "spotlight"
      ? "tutorial-target-active"
      : step.targetElement.highlight === "pulse"
        ? "tutorial-target-pulse"
        : "tutorial-target-border"; // "glow" uses border highlight

  el.classList.add(cls);
  return () => el.classList.remove(cls);
}

// =============================================================================
// TourPlayer Component
// =============================================================================

interface TourPlayerProps {
  tour: ProductTour;
  onClose: () => void;
  onComplete?: () => void;
}

export function TourPlayer({ tour, onClose, onComplete }: TourPlayerProps) {
  const [state, setState] = useState<TourPlayerState>({
    ...INITIAL_PLAYER_STATE,
    phase: "playing",
    tour,
  });
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const cleanupHighlightRef = useRef<(() => void) | null>(null);

  const step = tour.steps[state.currentStepIndex];
  const isLast = state.currentStepIndex === tour.steps.length - 1;

  // Highlight current step's target element
  useEffect(() => {
    cleanupHighlightRef.current?.();
    cleanupHighlightRef.current = step ? highlightElement(step) : null;
    return () => {
      cleanupHighlightRef.current?.();
      cleanupHighlightRef.current = null;
    };
  }, [step]);

  const advanceStep = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    setState((s) => {
      if (s.currentStepIndex >= tour.steps.length - 1) {
        onComplete?.();
        return { ...s, phase: "done" };
      }
      return { ...s, currentStepIndex: s.currentStepIndex + 1 };
    });
  }, [tour.steps.length, onComplete]);

  // Execute demo action and handle transitions
  useEffect(() => {
    if (state.phase !== "playing" || !step) return;

    let cancelled = false;

    const run = async () => {
      // Execute demo action if present
      if (step.demoAction) {
        setState((s) => ({ ...s, isExecutingAction: true }));
        try {
          await executeDemoAction(step.demoAction);
        } catch (err) {
          console.warn("Tour demo action failed:", err);
        }
        if (cancelled) return;
        setState((s) => ({ ...s, isExecutingAction: false }));
      }

      // Handle auto/timer transitions
      if (step.transition === "auto" || step.transition === "timer") {
        const delay = step.transitionDelayMs ?? (step.transition === "auto" ? 3000 : 5000);
        timerRef.current = setTimeout(() => {
          if (!cancelled) advanceStep();
        }, delay);
      }
    };

    run();

    return () => {
      cancelled = true;
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [state.currentStepIndex, state.phase, step, advanceStep]);

  const goBack = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    setState((s) => ({
      ...s,
      currentStepIndex: Math.max(0, s.currentStepIndex - 1),
    }));
  }, []);

  const handleClose = useCallback(() => {
    cleanupHighlightRef.current?.();
    if (timerRef.current) clearTimeout(timerRef.current);
    onClose();
  }, [onClose]);

  if (state.phase === "done" || !step) {
    // Done screen
    return (
      <div className="fixed bottom-6 right-6 z-[9998] w-80 bg-card border border-border rounded-xl shadow-2xl p-5 animate-in slide-in-from-bottom-4">
        <div className="text-center space-y-3">
          <h3 className="text-lg font-semibold">Tour Complete</h3>
          <p className="text-sm text-muted-foreground">{tour.subtitle}</p>
          {tour.cta && (
            <button
              onClick={() => {
                if (tour.cta?.url) window.open(tour.cta.url, "_blank");
                handleClose();
              }}
              className="w-full px-4 py-2 rounded-lg bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90"
            >
              {tour.cta.label}
            </button>
          )}
          <button
            onClick={handleClose}
            className="text-xs text-muted-foreground hover:text-foreground"
          >
            Close
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="fixed bottom-6 right-6 z-[9998] w-96 bg-card border border-border rounded-xl shadow-2xl animate-in slide-in-from-bottom-4">
      {/* Header */}
      <div className="flex items-center justify-between px-4 pt-3 pb-1">
        <span className="text-xs text-muted-foreground font-medium">
          {tour.title}
        </span>
        <button
          onClick={handleClose}
          className="text-muted-foreground hover:text-foreground p-0.5"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {/* Content */}
      <div className="px-5 py-3 space-y-2">
        <h3 className="text-base font-semibold leading-tight">{step.headline}</h3>
        <p className="text-sm text-muted-foreground leading-relaxed">{step.body}</p>

        {state.isExecutingAction && (
          <div className="flex items-center gap-2 text-xs text-primary">
            <Loader2 className="h-3 w-3 animate-spin" />
            Demonstrating...
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between px-4 pb-3 pt-1">
        {/* Progress dots */}
        <div className="flex items-center gap-1">
          {tour.steps.map((_, i) => (
            <div
              key={i}
              className={`h-1.5 rounded-full transition-all ${
                i === state.currentStepIndex
                  ? "w-4 bg-primary"
                  : i < state.currentStepIndex
                    ? "w-1.5 bg-primary/50"
                    : "w-1.5 bg-muted-foreground/30"
              }`}
            />
          ))}
        </div>

        {/* Navigation */}
        <div className="flex items-center gap-1.5">
          {state.currentStepIndex > 0 && (
            <button
              onClick={goBack}
              className="p-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent"
            >
              <ChevronLeft className="h-4 w-4" />
            </button>
          )}
          <button
            onClick={advanceStep}
            className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-primary text-primary-foreground text-xs font-medium hover:bg-primary/90"
          >
            {isLast ? "Finish" : "Next"}
            {!isLast && <ChevronRight className="h-3 w-3" />}
          </button>
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Tour Launcher Hook
// =============================================================================

/** Simple hook to manage tour player visibility from any component. */
export function useTourPlayer() {
  const [activeTour, setActiveTour] = useState<ProductTour | null>(null);

  const launchTour = useCallback((tour: ProductTour) => {
    setActiveTour(tour);
  }, []);

  const closeTour = useCallback(() => {
    setActiveTour(null);
  }, []);

  return { activeTour, launchTour, closeTour };
}
