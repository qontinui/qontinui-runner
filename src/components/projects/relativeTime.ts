/**
 * "Worked on 2 days ago" — the card's recency line.
 *
 * Stefan is a non-developer (plan §1). He does not want a timestamp; he wants
 * to know whether this is the thing he was doing yesterday or something he
 * abandoned in spring. So the buckets get coarser as they get older, and the
 * string always reads as a sentence fragment.
 */

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const WEEK_MS = 7 * DAY_MS;

function plural(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? "" : "s"} ago`;
}

/**
 * Format an epoch-millisecond activity stamp as a coarse relative phrase.
 *
 * Returns `null` when there is nothing to say — no timestamp at all, or one
 * that is not a finite number. Callers render their own "never opened" copy
 * rather than being handed the word "never", because the right phrasing
 * differs between the card and the detail view.
 *
 * A stamp in the future (clock skew, or a machine that travelled across a DST
 * boundary mid-session) is clamped to "just now" rather than rendering a
 * negative age.
 */
export function formatRelativeActivity(ms: number | undefined | null, now = Date.now()): string | null {
  if (ms === undefined || ms === null || !Number.isFinite(ms)) return null;

  const age = now - ms;
  if (age < 0) return "just now";
  if (age < MINUTE_MS) return "just now";
  if (age < HOUR_MS) return plural(Math.floor(age / MINUTE_MS), "minute");
  if (age < DAY_MS) return plural(Math.floor(age / HOUR_MS), "hour");
  if (age < WEEK_MS) return plural(Math.floor(age / DAY_MS), "day");

  const weeks = Math.floor(age / WEEK_MS);
  if (weeks < 5) return plural(weeks, "week");

  const months = Math.floor(age / (30 * DAY_MS));
  if (months < 12) return plural(months, "month");

  return plural(Math.floor(age / (365 * DAY_MS)), "year");
}
