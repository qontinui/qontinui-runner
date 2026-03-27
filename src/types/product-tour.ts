/**
 * Product Tour Types
 *
 * Customer-facing interactive tours that auto-demonstrate features.
 * Different from developer tutorials: tours SHOW the product working,
 * tutorials ASK the user to perform actions.
 */

export interface ProductTour {
  id: string;
  title: string;
  subtitle: string;
  targetAudience: TourAudience;
  estimatedMinutes: number;
  pages: string[];
  steps: ProductTourStep[];
  cta?: TourCTA;
}

export type TourAudience = "new-user" | "power-user" | "evaluator";

export interface TourCTA {
  label: string;
  url?: string;
  action?: string;
}

export interface ProductTourStep {
  id: string;
  headline: string;
  body: string;
  targetElement?: {
    elementId?: string;
    selector?: string;
    highlight: "spotlight" | "pulse" | "glow";
  };
  demoAction?: TourDemoAction;
  transition: "auto" | "click" | "timer";
  transitionDelayMs?: number;
}

export type TourDemoAction =
  | { type: "click"; elementId: string }
  | { type: "type"; elementId: string; value: string }
  | { type: "select"; elementId: string; value: string }
  | { type: "navigate"; target: string }
  | { type: "wait"; ms: number };

// =============================================================================
// Player State
// =============================================================================

export type TourPlayerPhase = "idle" | "playing" | "paused" | "done";

export interface TourPlayerState {
  phase: TourPlayerPhase;
  tour: ProductTour | null;
  currentStepIndex: number;
  isExecutingAction: boolean;
}

export const INITIAL_PLAYER_STATE: TourPlayerState = {
  phase: "idle",
  tour: null,
  currentStepIndex: 0,
  isExecutingAction: false,
};

// =============================================================================
// Generation State
// =============================================================================

export type TourGenerationPhase =
  | "idle"
  | "generating"
  | "previewing"
  | "saving"
  | "done"
  | "error";

export interface TourGenerationState {
  phase: TourGenerationPhase;
  tours: ProductTour[];
  error: string | null;
}

export const INITIAL_GENERATION_STATE: TourGenerationState = {
  phase: "idle",
  tours: [],
  error: null,
};

// =============================================================================
// Tour Storage
// =============================================================================

/** Key used in instanceStorage for saved product tours. */
export const PRODUCT_TOURS_STORAGE_KEY = "qontinui-product-tours";
