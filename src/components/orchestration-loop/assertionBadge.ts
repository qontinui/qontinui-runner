/**
 * How the orchestration panel renders one diagnose-phase assertion entry.
 *
 * An entry is either the route's verdict (`passed: true | false`) or a
 * no-verdict entry (`evaluated: false`, no `passed`) for an assertion the
 * route refused or could not evaluate. Only a verdict is green or red: a
 * no-verdict entry says nothing about the UI, so it is neutral and its title
 * carries what the route said (plan
 * 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route).
 */

export type AssertionBadgeTone = "pass" | "fail" | "noVerdict";

export interface AssertionBadge {
  tone: AssertionBadgeTone;
  /** Short label: the assertion kind and its query, else `#<n>`. */
  label: string;
  /** The glyph after the label. */
  mark: string;
  /** Hover text. */
  title: string;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function str(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** The badge for entry `index` (0-based) of a diagnose result. */
export function assertionBadge(entry: unknown, index: number): AssertionBadge {
  const e = asRecord(entry);
  // A verdict entry is the route's AssertResult (`assertion`, `query`); a
  // no-verdict entry echoes the request it sent under `assertion`.
  const request = asRecord(e.assertion);
  const kind = str(e.assertion) ?? str(request.assertion);
  const query = str(e.query) ?? str(request.query);
  const label = kind ? (query ? `${kind} ${query}` : kind) : `#${index + 1}`;

  if (e.passed === true) {
    return { tone: "pass", label, mark: " ✓", title: JSON.stringify(entry, null, 2) };
  }
  if (e.passed === false) {
    return { tone: "fail", label, mark: " ✗", title: JSON.stringify(entry, null, 2) };
  }
  const status = typeof e.status === "number" ? `HTTP ${e.status}` : "no HTTP status";
  const code = str(e.code);
  const error = str(e.error) ?? "no error message";
  return {
    tone: "noVerdict",
    label,
    mark: " ?",
    title: `No verdict (${status}${code ? ` ${code}` : ""}): ${error}`,
  };
}

/** Tailwind classes for each tone. */
export const ASSERTION_BADGE_CLASSES: Record<AssertionBadgeTone, string> = {
  pass: "bg-green-500/10 text-green-400",
  fail: "bg-red-500/10 text-red-400",
  noVerdict: "bg-muted text-muted-foreground",
};
