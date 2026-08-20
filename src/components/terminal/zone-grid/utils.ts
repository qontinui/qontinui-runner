export function formatUptime(createdAt?: number): string | undefined {
  if (!createdAt) return undefined;
  const ms = Date.now() - createdAt;
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainMin = minutes % 60;
  return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
}

export function isActionableLine(line: string): boolean {
  return /\? \(y\/n\)|\? \(yes\/no\)|Allow .+\? \(|Do you want to proceed|\? >|\(Y\/n\)|\[y\/N\]/i.test(
    line,
  );
}

export function isYesNoPrompt(lines: string[]): boolean {
  return lines.some((line) =>
    /\? \(y\/n\)|\? \(yes\/no\)|\(Y\/n\)|\[y\/N\]|Allow .+\? \(/i.test(line),
  );
}

export function computeOutputTrend(
  data: number[] | undefined,
): { trend: "up" | "down" | "stable"; rate: number; peak: number } | null {
  if (!data || data.length < 4) return null;
  const recent = data.slice(-5);
  const earlier = data.slice(-10, -5);
  if (earlier.length < 2) return null;
  const recentAvg = recent.reduce((a, b) => a + b, 0) / recent.length;
  const earlierAvg = earlier.reduce((a, b) => a + b, 0) / earlier.length;
  const peak = Math.max(...data, 1);
  const rate = recentAvg;
  if (earlierAvg === 0 && recentAvg === 0) return { trend: "stable", rate, peak };
  const ratio = earlierAvg > 0 ? recentAvg / earlierAvg : recentAvg > 0 ? 2 : 1;
  if (ratio > 1.3) return { trend: "up", rate, peak };
  if (ratio < 0.7) return { trend: "down", rate, peak };
  return { trend: "stable", rate, peak };
}

export function countMatches(lines: string[], filter: string): number {
  if (!filter || filter.length < 2) return 0;
  const lower = filter.toLowerCase();
  return lines.filter((l) => l.toLowerCase().includes(lower)).length;
}

/**
 * Whether a zone must carry the standalone session-info strip (D1 of
 * `plans/2026-08-19-session-info-dropdown-mount-gaps-remediation.md`).
 *
 * `showLabels` gates the full `ZoneLabel` chrome — rename, pin, filter,
 * output stats — on a MULTI-zone grid, because on a one-terminal page that
 * chrome is redundant with the tab bar. The session-info trigger is not
 * chrome: it is the only surface that answers "which Claude session is this,
 * and what has it opened?", and a one-terminal page is the layout most
 * operators actually run. Gating it on `showLabels` therefore made the whole
 * feature invisible for them.
 *
 * So: mount it wherever `ZoneLabel` does NOT, provided the tab actually has a
 * session to describe (a tab with no `claudeSessionId` has no session info to
 * be unknown ABOUT — the same distinction `SessionInfoDropdown` makes when it
 * returns `null`). The compact card is excluded because it replaces the whole
 * zone body with its own summary UI.
 */
export function showSoloSessionInfo(opts: {
  showLabels: boolean;
  showCompactCard: boolean;
  claudeSessionId?: string;
}): boolean {
  if (opts.showLabels || opts.showCompactCard) return false;
  return !!opts.claudeSessionId;
}
