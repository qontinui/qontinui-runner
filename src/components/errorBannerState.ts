/**
 * The app-wide `error` event banner's state, as a pure reducer.
 *
 * `StatusIndicator` used to hold one `showError` boolean: a click on X hid the
 * card forever, and only a NEW `error` event could bring it back — so one click
 * left the working surface with no trace of a fault that was still live, and an
 * undismissed card stayed forever even after the fault cleared. Neither state
 * followed the fault (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls` F2).
 *
 * The rule this reducer encodes:
 * - a new fault shows the full banner;
 * - dismissing a non-`info` fault collapses it to a compact RESIDUAL that names
 *   the code and re-opens the banner on click;
 * - the SAME code re-emitted while dismissed keeps the residual (and refreshes
 *   its last-seen time) — a re-report is not a new fault;
 * - a DIFFERENT code shows the banner again;
 * - an observed clear (`error-cleared` for the current code) removes it;
 * - a fault whose emitter never reports clearing keeps the residual, which says
 *   when the fault was last observed rather than inventing a clear.
 *
 * `info` events stay transient: they auto-hide (the `expired` action) and a
 * dismissed `info` leaves no residual.
 */

export type ErrorSeverity = "info" | "warning" | "error" | "critical";

export interface ErrorEventPayload {
  title: string;
  message: string;
  details?: string;
  error_code: string;
  severity: ErrorSeverity;
  recoverable: boolean;
  suggested_action?: string;
}

export interface ErrorBannerState {
  /** The most recent fault, or null when none is live. */
  current: ErrorEventPayload | null;
  /** Epoch ms of the most recent observation of `current`. */
  lastSeenAt: number | null;
  /** True once the operator collapsed `current` to its residual. */
  dismissed: boolean;
}

export type ErrorBannerAction =
  | { type: "received"; error: ErrorEventPayload; at: number }
  | { type: "dismissed" }
  | { type: "reopened" }
  | { type: "cleared"; errorCode: string }
  /**
   * The transient `info` auto-hide timer fired for the occurrence received at
   * `at`. Keyed on the occurrence, not only the code, so an earlier timer can
   * never hide a newer event (the old un-cancelled `setTimeout` hid a warning
   * that arrived within 5 s of an info).
   */
  | { type: "expired"; errorCode: string; at: number };

export type ErrorBannerView = "banner" | "residual" | "none";

export const INITIAL_ERROR_BANNER_STATE: ErrorBannerState = {
  current: null,
  lastSeenAt: null,
  dismissed: false,
};

export function errorBannerReducer(
  state: ErrorBannerState,
  action: ErrorBannerAction,
): ErrorBannerState {
  switch (action.type) {
    case "received": {
      const sameFault =
        state.current !== null && state.current.error_code === action.error.error_code;
      if (sameFault && state.dismissed) {
        // A re-report of a fault the operator already collapsed: keep the
        // residual, refresh what it says.
        return { current: action.error, lastSeenAt: action.at, dismissed: true };
      }
      return { current: action.error, lastSeenAt: action.at, dismissed: false };
    }
    case "dismissed":
      if (state.current === null) return state;
      if (state.current.severity === "info") return INITIAL_ERROR_BANNER_STATE;
      return { ...state, dismissed: true };
    case "reopened":
      if (state.current === null) return state;
      return { ...state, dismissed: false };
    case "cleared":
      if (state.current === null || state.current.error_code !== action.errorCode) return state;
      return INITIAL_ERROR_BANNER_STATE;
    case "expired":
      if (
        state.current === null ||
        state.current.severity !== "info" ||
        state.current.error_code !== action.errorCode ||
        state.lastSeenAt !== action.at
      ) {
        return state;
      }
      return INITIAL_ERROR_BANNER_STATE;
  }
}

export function errorBannerView(state: ErrorBannerState): ErrorBannerView {
  if (state.current === null) return "none";
  return state.dismissed ? "residual" : "banner";
}

/** The dismiss control's accessible name — the button carries only an icon. */
export function dismissLabel(error: Pick<ErrorEventPayload, "title">): string {
  return `Dismiss: ${error.title}`;
}

/**
 * The residual's text. It names the code and when the fault was last observed;
 * it never claims the fault cleared, because only an `error-cleared` event can
 * say that.
 */
export function residualLabel(
  error: Pick<ErrorEventPayload, "error_code">,
  lastSeenAt: number,
  now: number,
): string {
  return `${error.error_code} · last seen ${formatAgo(now - lastSeenAt)}`;
}

export function formatAgo(ms: number): string {
  const secs = Math.max(0, Math.floor(ms / 1000));
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  return `${hours}h ${mins % 60}m ago`;
}
