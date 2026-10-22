/**
 * The app-wide `error` event banner's state, as a pure reducer.
 *
 * `StatusIndicator` used to hold one `showError` boolean over one `error`
 * slot: a click on X hid the card forever, only a NEW `error` event could bring
 * it back, and any new event overwrote the previous one — so one click (or one
 * passing `info`) left the working surface with no trace of a fault that was
 * still live, and an undismissed card stayed forever even after the fault
 * cleared. Neither state followed the fault (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls` F2).
 *
 * The rule this reducer encodes:
 * - every non-`info` fault is tracked on its own, keyed by `error_code`, so one
 *   fault can never erase another;
 * - a new fault shows its full banner;
 * - dismissing it collapses it to a compact RESIDUAL that names the code and
 *   re-opens the banner on click;
 * - the SAME code re-emitted while dismissed keeps the residual (and refreshes
 *   its last-seen time) — a re-report is not a new fault;
 * - an observed clear (`error-cleared` for that code) removes it;
 * - a fault whose emitter never reports clearing keeps the residual, which says
 *   when the fault was last observed rather than inventing a clear.
 *
 * `info` events are transient and live in their own slot: they auto-hide (the
 * `expired` action), a dismissed `info` leaves no residual, and an `info` never
 * touches a fault.
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

/** One tracked non-`info` fault. */
export interface TrackedFault {
  error: ErrorEventPayload;
  /** Epoch ms of the most recent observation of this fault. */
  lastSeenAt: number;
  /** True once the operator collapsed it to its residual. */
  dismissed: boolean;
}

/** The current transient `info` event, if one is up. */
export interface TransientInfo {
  error: ErrorEventPayload;
  /** Epoch ms it was received — the occurrence its auto-hide is keyed on. */
  at: number;
}

export interface ErrorBannerState {
  /** Live non-`info` faults, oldest first, at most one per `error_code`. */
  faults: TrackedFault[];
  info: TransientInfo | null;
}

export type ErrorBannerAction =
  | { type: "received"; error: ErrorEventPayload; at: number }
  | { type: "dismissed"; errorCode: string }
  | { type: "reopened"; errorCode: string }
  | { type: "cleared"; errorCode: string }
  /**
   * The transient `info` auto-hide timer fired for the occurrence received at
   * `at`. Keyed on the occurrence, not only the code, so an earlier timer can
   * never hide a newer event (the old un-cancelled `setTimeout` hid a warning
   * that arrived within 5 s of an info).
   */
  | { type: "expired"; errorCode: string; at: number };

/** What renders: full banners in flow, and residual chips in the status bar. */
export interface ErrorBannerView {
  banners: ErrorEventPayload[];
  residuals: TrackedFault[];
}

export const INITIAL_ERROR_BANNER_STATE: ErrorBannerState = { faults: [], info: null };

export function errorBannerReducer(
  state: ErrorBannerState,
  action: ErrorBannerAction,
): ErrorBannerState {
  switch (action.type) {
    case "received": {
      if (action.error.severity === "info") {
        return { ...state, info: { error: action.error, at: action.at } };
      }
      const code = action.error.error_code;
      const existing = state.faults.find((f) => f.error.error_code === code);
      if (existing) {
        // A re-report of a tracked fault: refresh what it says, keep its
        // dismissal (a re-report is not a new fault).
        return {
          ...state,
          faults: state.faults.map((f) =>
            f === existing ? { ...f, error: action.error, lastSeenAt: action.at } : f,
          ),
        };
      }
      return {
        ...state,
        faults: [...state.faults, { error: action.error, lastSeenAt: action.at, dismissed: false }],
      };
    }
    case "dismissed":
      if (state.info && state.info.error.error_code === action.errorCode) {
        return { ...state, info: null };
      }
      return setDismissed(state, action.errorCode, true);
    case "reopened":
      return setDismissed(state, action.errorCode, false);
    case "cleared": {
      const faults = state.faults.filter((f) => f.error.error_code !== action.errorCode);
      return faults.length === state.faults.length ? state : { ...state, faults };
    }
    case "expired":
      if (
        state.info === null ||
        state.info.error.error_code !== action.errorCode ||
        state.info.at !== action.at
      ) {
        return state;
      }
      return { ...state, info: null };
  }
}

function setDismissed(
  state: ErrorBannerState,
  errorCode: string,
  dismissed: boolean,
): ErrorBannerState {
  let changed = false;
  const faults = state.faults.map((f) => {
    if (f.error.error_code !== errorCode || f.dismissed === dismissed) return f;
    changed = true;
    return { ...f, dismissed };
  });
  return changed ? { ...state, faults } : state;
}

export function errorBannerView(state: ErrorBannerState): ErrorBannerView {
  const banners = state.faults.filter((f) => !f.dismissed).map((f) => f.error);
  if (state.info) banners.push(state.info.error);
  return { banners, residuals: state.faults.filter((f) => f.dismissed) };
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
