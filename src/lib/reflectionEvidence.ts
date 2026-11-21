/**
 * Readable form of a reflection fix's `effectiveness_evidence`.
 *
 * The fix evaluator (`evaluate_pending_fixes` in the runner) writes the
 * exposure counts a verdict rests on as JSON — the Rust `ExposureEvidence`
 * struct in `src-tauri/src/reflection/types.rs`, whose `summary()` this mirrors.
 * A verdict set by hand carries free text instead, which is shown unchanged,
 * and so is anything that does not carry every evaluator key.
 */

interface ExposureEvidence {
  verdict: string;
  later_runs: number;
  min_later_runs: number;
  source_finding: boolean;
  source_signature_known: boolean;
  recurrences: number;
  new_signature_peak_runs: number;
  min_later_runs_for_regression: number;
}

function parseExposureEvidence(raw: string): ExposureEvidence | null {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  const numbers = [
    "later_runs",
    "min_later_runs",
    "recurrences",
    "new_signature_peak_runs",
    "min_later_runs_for_regression",
  ];
  if (typeof v.verdict !== "string") return null;
  if (typeof v.source_finding !== "boolean" || typeof v.source_signature_known !== "boolean") {
    return null;
  }
  if (!numbers.every((key) => typeof v[key] === "number")) return null;
  return v as unknown as ExposureEvidence;
}

export function describeEffectivenessEvidence(raw: string): string {
  const e = parseExposureEvidence(raw);
  if (!e) return raw;
  const observability = !e.source_finding
    ? "the fix names no source finding, so a recurrence cannot be observed"
    : !e.source_signature_known
      ? "its source finding carries no signature, so a recurrence cannot be observed"
      : `the source finding recurred in ${e.recurrences} of them`;
  return (
    `${e.verdict} after ${e.later_runs} later run${e.later_runs === 1 ? "" : "s"} ` +
    `(needs ${e.min_later_runs}): ${observability}; the most later runs any new signature ` +
    `appeared in was ${e.new_signature_peak_runs} (regression at ${e.min_later_runs_for_regression})`
  );
}
