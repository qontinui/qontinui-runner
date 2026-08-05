/**
 * "SINCE YOU WERE LAST HERE" — the detail view's opening block (plan §5.2).
 *
 * The point of this block is that Stefan should not have to reconstruct what
 * happened from a session list. It answers three questions in three sentences:
 * did anything run, does the thing still work, and is anything waiting on him.
 *
 * Pure, so it is unit-tested (the runner's vitest config is `environment:
 * "node"`, so the JSX around it is not renderable in a test).
 */

import type { ProjectSnapshot, SessionLite } from "./types";

/** One line of the digest, with enough structure for the UI to act on it. */
export interface DigestLine {
  /**
   * Stable key for rendering AND for tests to assert on a line without
   * matching its prose.
   */
  id: "sessions" | "health" | "questions";
  text: string;
  /** `questions` carries an [Answer] affordance; the others do not. */
  actionable: boolean;
  /** Health lines colour by level; the others are neutral. */
  tone: "neutral" | "good" | "bad";
}

export interface ProjectDigest {
  /**
   * "Tuesday", "3 weeks ago", or `null` when the project has never been opened
   * — in which case the whole block is suppressed, because "since you were last
   * here" is a lie on a project you have never been to.
   */
  since: string | null;
  lines: DigestLine[];
}

const DAY_MS = 24 * 60 * 60 * 1000;
const WEEKDAYS = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
] as const;

/**
 * How the header names the last visit.
 *
 * Within the last week a weekday name is what a person actually remembers
 * ("Tuesday"), so that wins over "5 days ago". Beyond a week the weekday stops
 * being a locator and the relative phrasing is clearer.
 */
export function describeLastVisit(
  lastOpenedMs: number | undefined,
  now = Date.now(),
): string | null {
  if (lastOpenedMs === undefined || !Number.isFinite(lastOpenedMs)) return null;

  const elapsed = now - lastOpenedMs;
  // A clock skew that puts the last visit in the future is not information;
  // treat it as just now rather than rendering "in 3 hours".
  if (elapsed < 0) return "just now";
  if (elapsed < DAY_MS) return "today";
  if (elapsed < 2 * DAY_MS) return "yesterday";
  if (elapsed < 7 * DAY_MS) return WEEKDAYS[new Date(lastOpenedMs).getDay()];

  const weeks = Math.floor(elapsed / (7 * DAY_MS));
  if (weeks === 1) return "last week";
  if (weeks < 5) return `${weeks} weeks ago`;
  const months = Math.floor(elapsed / (30 * DAY_MS));
  return months <= 1 ? "a month ago" : `${months} months ago`;
}

/** Sessions that ran at or after the last visit. */
function sessionsSince(sessions: SessionLite[], sinceMs: number | undefined): SessionLite[] {
  if (sinceMs === undefined) return sessions;
  // A session with no timestamp is counted: it demonstrably exists in the
  // recent list, and dropping it would under-report work that did happen.
  return sessions.filter((s) => s.lastActivityMs === undefined || s.lastActivityMs >= sinceMs);
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? `1 ${one}` : `${n} ${many}`;
}

/**
 * Build the digest. `now` is injected so the tests are not clock-dependent.
 *
 * `lastOpenedMs` — the registry's own record of the last activation — is the
 * cut-off, NOT `snapshot.lastActivityMs`. The latter includes work that
 * happened *while* the user was here, which is precisely what "since you were
 * last here" must exclude.
 *
 * `now` defaults here rather than at the call site so a component can call this
 * during render without tripping `react-hooks/purity` — the same convention
 * `deriveCardStatus` uses.
 */
export function buildDigest(
  snapshot: ProjectSnapshot | undefined,
  lastOpenedMs: number | undefined,
  now = Date.now(),
): ProjectDigest {
  const since = describeLastVisit(lastOpenedMs, now);
  if (!snapshot) return { since, lines: [] };

  const lines: DigestLine[] = [];

  const sessions = sessionsSince(snapshot.recentSessions, lastOpenedMs);
  const filesChanged = sessions.reduce((sum, s) => sum + (s.filesTouched || 0), 0);
  if (sessions.length > 0) {
    const filesPart =
      filesChanged > 0 ? `, ${plural(filesChanged, "file changed", "files changed")}` : "";
    lines.push({
      id: "sessions",
      text: `${plural(sessions.length, "agent session ran", "agent sessions ran")}${filesPart}`,
      actionable: false,
      tone: "neutral",
    });
  }

  // Health, phrased as the user's mental model ("does my website work?") rather
  // than as a process-manager state. `unknown` is skipped entirely: a project
  // with nothing to observe should not get a reassuring line it has not earned,
  // nor a worrying one it does not deserve.
  const level = snapshot.health?.level;
  if (level === "green") {
    lines.push({
      id: "health",
      text: "It still starts up fine",
      actionable: false,
      tone: "good",
    });
  } else if (level === "red" || level === "amber") {
    lines.push({
      id: "health",
      // The snapshot's own sentence is already written for a non-developer
      // (`HealthLite.reason`), so prefer it over anything synthesized here.
      text: snapshot.health?.reason?.trim() || "Something isn't starting up",
      actionable: true,
      tone: "bad",
    });
  }

  const questionCount = snapshot.questions.length;
  if (questionCount > 0) {
    lines.push({
      id: "questions",
      text: `${plural(questionCount, "question is", "questions are")} waiting for you`,
      actionable: true,
      tone: "neutral",
    });
  }

  return { since, lines };
}

/**
 * "Tuesday, 40 min" — the RECENT WORK line's right-hand column.
 *
 * Duration comes from the session's own span where both ends are known; a
 * session still running (or missing a start) reports the day alone rather than
 * a fabricated duration.
 */
export function describeSessionWhen(
  session: SessionLite,
  startedMs?: number,
  now = Date.now(),
): string {
  const day = describeLastVisit(session.lastActivityMs, now);
  if (day === null) return "";
  if (startedMs === undefined || session.lastActivityMs === undefined) return day;

  const minutes = Math.round((session.lastActivityMs - startedMs) / 60000);
  if (minutes < 1) return `${day}, under a minute`;
  if (minutes < 60) return `${day}, ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${day}, ${hours}h` : `${day}, ${hours}h ${rest}m`;
}
