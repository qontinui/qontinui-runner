/**
 * The status vocabulary a prompt-optimization run is written with.
 *
 * This is the READ side of a contract the write side already pins down:
 * `OptimizationOutcome::status_str`
 * (`src-tauri/src/workflow_generation/gepa_optimizer.rs`) produces exactly the
 * four values below, and `PgDb::insert_gepa_run`
 * (`src-tauri/src/database/pg/adaptive_learning.rs`) documents them as the only
 * legal contents of the `status` column.
 *
 * The distinction the gate exists to preserve is `insufficient_data` vs
 * `rejected`: a run that **could not decide** is not a run that **decided
 * against**. Collapsing them — including by rendering both in the same colour
 * under the same fallback — throws away the whole point of the held-out gate.
 * `gepaStatusStyle` therefore gives every arm its own visual treatment, and an
 * unrecognised value a deliberately *neutral* one rather than borrowing the
 * styling of a real verdict.
 */

/** Every status the writer can emit, in outcome order. */
export const GEPA_RUN_STATUSES = ["accepted", "rejected", "insufficient_data", "skipped"] as const;

export type GepaRunStatus = (typeof GEPA_RUN_STATUSES)[number];

/** How one status renders: a human label, a colour pair, and why it happened. */
export interface GepaStatusStyle {
  label: string;
  bg: string;
  text: string;
  /** Tooltip — what this verdict actually means. */
  title: string;
}

const STYLES: Record<GepaRunStatus, GepaStatusStyle> = {
  accepted: {
    label: "accepted",
    bg: "#1e3a2f",
    text: "#34d399",
    title: "The paired held-out comparison accepted the optimized prompt.",
  },
  rejected: {
    label: "rejected",
    bg: "#3b1e1e",
    text: "#f87171",
    title:
      "The paired held-out comparison decided against the optimized prompt (a regression, or no significant gain).",
  },
  insufficient_data: {
    label: "no verdict",
    bg: "#3b2f1e",
    text: "#fbbf24",
    title:
      "Too few paired held-out examples to decide. NOTHING was decided — this is not a rejection.",
  },
  skipped: {
    label: "skipped",
    bg: "#1f2937",
    text: "#9ca3af",
    title:
      "The gate never ran — optimization disabled, cooldown not elapsed, or too few training examples.",
  },
};

/**
 * Styling for a value that is not in the vocabulary above.
 *
 * Deliberately neutral: an unknown status must not be dressed as a verdict the
 * writer never issued. The raw value is shown so the mismatch is visible rather
 * than laundered into a plausible-looking badge.
 */
const UNKNOWN_STYLE: Omit<GepaStatusStyle, "label"> = {
  // Its own background — sharing `skipped`'s left the raw label as the only
  // difference. The text is light enough to clear WCAG AA at 11px; the earlier
  // #6b7280 on #1f2937 was 3.04:1, failing exactly where legibility matters
  // most.
  bg: "#111827",
  text: "#d1d5db",
  title: "Unrecognised status — not one of the four values the optimizer writes.",
};

export function isGepaRunStatus(status: string): status is GepaRunStatus {
  return (GEPA_RUN_STATUSES as readonly string[]).includes(status);
}

/** Resolve a raw `status` string to its rendering. Never throws. */
export function gepaStatusStyle(status: string): GepaStatusStyle {
  if (isGepaRunStatus(status)) {
    return STYLES[status];
  }
  return { ...UNKNOWN_STYLE, label: status || "unknown" };
}

/**
 * Whether this run's prompt was actually adopted.
 *
 * Read the **status**, not `improvement`. `improvement` is the sidecar's
 * display-only mean-of-means delta; the accept/reject decision is a paired
 * statistical verdict computed in `gepa_optimizer::evaluate_held_out`. Deriving
 * "success" from `improvement > 0` re-introduces the second decision surface
 * that the held-out gate exists to remove — and would count an
 * `insufficient_data` run, where nothing was decided at all, as a success.
 */
export function isAcceptedRun(status: string): boolean {
  return status === "accepted";
}
