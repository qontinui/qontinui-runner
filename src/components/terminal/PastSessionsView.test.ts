/**
 * UI-Bridge addressability contract for the "Previous Sessions" view.
 *
 * The runner's vitest config uses `environment: "node"` — no jsdom and no
 * `@testing-library/react` (precedent: `LaunchMenu.test.tsx`,
 * `StewardControl.test.tsx`, `UnzonedChip.test.ts`). So the id/scope contract is
 * locked in two layers:
 *
 *   1. the exported pure id builders (`pastSession*Id`) — asserted directly;
 *   2. the JSX that consumes them — asserted by reading this component's own
 *      source, which is what makes "every repeated action carries a stamp"
 *      checkable at all without a DOM.
 *
 * On-page behavior (a real click through
 * `POST /ui-bridge/control/element/<id>/action`) is verified against a temp
 * runner, as the plan requires.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import {
  PAST_SESSIONS_COHORT_ELEMENT,
  PAST_SESSIONS_REFRESH_ID,
  PAST_SESSIONS_RETRY_ID,
  PAST_SESSIONS_VIEW_ELEMENT,
  PAST_SESSION_CARD_ELEMENT,
  UNNAMED_PAST_SESSION,
  groupByCohort,
  pastSessionCopyId,
  pastSessionDisplayName,
  pastSessionResumeId,
  pastSessionRestoreBadge,
  pastSessionRowId,
  pastSessionTooltip,
  pastSessionsCohortToggleId,
} from "./PastSessionsView";
import { registryNamesBySessionId } from "./liveClaudeSessions";
import type { LiveClaudeSession } from "./liveClaudeSessions";
import type { PastSession } from "./usePastSessions";

const SOURCE = readFileSync(
  fileURLToPath(new URL("./PastSessionsView.tsx", import.meta.url)),
  "utf8",
);

function makeSession(claudeSessionId: string, cohortId: number, lastSeenAt: number): PastSession {
  return {
    claudeSessionId,
    resumeName: `session ${claudeSessionId}`,
    resumeCommand: `clg --resume ${claudeSessionId}`,
    account: { label: "gmail", wrapper: "clg" },
    pageId: "page-1",
    zoneIndex: 0,
    state: "closed",
    provider: "claude",
    lastSeenAt,
    openedAt: lastSeenAt - 1000,
    confirmed: true,
    transcriptExists: true,
    restorable: true,
    cohortId,
  };
}

/**
 * A live-registry row for `claudeSessionId`. Only the fields the name gate
 * reads matter here; the rest mirror the operator's ground-truth shape.
 */
function makeLiveRow(
  claudeSessionId: string,
  name: string,
  nameSource?: string,
): LiveClaudeSession {
  return {
    sessionId: claudeSessionId,
    name,
    nameSource,
    pid: 2804,
    account: { label: "gmail", wrapper: "clg" },
    workingDir: "D:/qontinui-root",
    status: "idle",
    kind: "interactive",
    startedAt: 1784712055852,
    updatedAt: 1784770342016,
    resumeCommand: `clg --resume ${claudeSessionId}`,
  };
}

describe("pastSessionDisplayName", () => {
  it("prefers the live window name over the transcript-derived resumeName", () => {
    const session = makeSession("live-1", 1, 1000);
    const names = registryNamesBySessionId([makeLiveRow("live-1", "P3 window name")]);
    expect(pastSessionDisplayName(session, names)).toBe("P3 window name");
    // Not the value the card renders today.
    expect(pastSessionDisplayName(session, names)).not.toBe(session.resumeName);
  });

  it("does NOT let a derived cwd slug preempt the existing name", () => {
    // The gate. `qontinui-root-ec` is Claude Code's own `<dir>-<2hex>`
    // auto-derivation and is strictly worse than the name already on the card,
    // so this row must lose despite being live.
    const session = makeSession("live-2", 1, 1000);
    const names = registryNamesBySessionId([makeLiveRow("live-2", "qontinui-root-ec", "derived")]);
    expect(pastSessionDisplayName(session, names)).toBe(session.resumeName);
  });

  it("lets a row with an ABSENT nameSource win", () => {
    // The absent-key majority — 12 of 17 named rows on the operator's box.
    const session = makeSession("live-3", 1, 1000);
    const row = makeLiveRow("live-3", "worktree prune");
    delete (row as { nameSource?: unknown }).nameSource;
    expect(pastSessionDisplayName(session, registryNamesBySessionId([row]))).toBe("worktree prune");
  });

  it("keeps a CLOSED session's resumeName — the live/dead asymmetry", () => {
    // The registry only ever describes running processes, so a closed session
    // is always a lookup miss. It must keep rendering exactly what it renders
    // today, never blank.
    const closed = makeSession("closed-1", 1, 1000);
    expect(closed.state).toBe("closed");
    const namesForOtherSessions = registryNamesBySessionId([
      makeLiveRow("some-other-live-session", "P3 window name"),
    ]);
    expect(pastSessionDisplayName(closed, namesForOtherSessions)).toBe(closed.resumeName);
    expect(pastSessionDisplayName(closed, new Map())).toBe(closed.resumeName);
  });

  it("falls back to the unnamed placeholder only when nothing has a name", () => {
    const nameless = { ...makeSession("nameless", 1, 1000), resumeName: "" };
    expect(pastSessionDisplayName(nameless, new Map())).toBe(UNNAMED_PAST_SESSION);
    // …and a live registry name rescues even a nameless row.
    const names = registryNamesBySessionId([makeLiveRow("nameless", "recovered")]);
    expect(pastSessionDisplayName(nameless, names)).toBe("recovered");
  });
});

describe("pastSessionTooltip", () => {
  it("always names the session id", () => {
    expect(pastSessionTooltip(makeSession("s1", 1, 1000), new Map())).toContain("s1");
  });

  it("keeps the transcript name reachable when the window name displaced it", () => {
    const session = makeSession("live-4", 1, 1000);
    const names = registryNamesBySessionId([makeLiveRow("live-4", "P3 window name")]);
    const t = pastSessionTooltip(session, names);
    expect(t).toContain("P3 window name");
    expect(t).toContain(`transcript name: ${session.resumeName}`);
  });

  it("does not repeat the name when nothing displaced it", () => {
    const closed = makeSession("closed-2", 1, 1000);
    expect(pastSessionTooltip(closed, new Map())).not.toContain("transcript name:");
  });
});

describe("past-session control ids", () => {
  it("keys copy/resume on the claude session id", () => {
    expect(pastSessionCopyId("abc-123")).toBe("terminal.past-session-copy-abc-123");
    expect(pastSessionResumeId("abc-123")).toBe("terminal.past-session-resume-abc-123");
  });

  it("keys the cohort expander on the cohort id, not on its label", () => {
    expect(pastSessionsCohortToggleId(7)).toBe("terminal.past-sessions-cohort-toggle-7");
  });

  it("namespaces the view-level actions under `terminal.`", () => {
    expect(PAST_SESSIONS_REFRESH_ID).toBe("terminal.past-sessions-refresh");
    expect(PAST_SESSIONS_RETRY_ID).toBe("terminal.past-sessions-retry");
  });

  it("mints one distinct resume id per rendered session", () => {
    const sessions = ["s1", "s2", "s3", "s4", "s5"].map((id, i) => makeSession(id, 1, 1000 + i));
    const ids = sessions.map((s) => pastSessionResumeId(s.claudeSessionId));

    expect(ids).toHaveLength(sessions.length);
    expect(new Set(ids).size).toBe(sessions.length);
    // Each id names its own session — no ordinal component anywhere.
    for (const session of sessions) {
      expect(ids).toContain(`terminal.past-session-resume-${session.claudeSessionId}`);
    }
  });

  it("keeps every card's ids stable when the newest-first sort reorders the list", () => {
    // This is the whole point of R3: `groupByCohort` sorts cohorts newest-first
    // by `lastSeenAt`, so a refresh that bumps an older cohort reorders the
    // rendered list — and any ordinal-derived id would move with it.
    const before = groupByCohort([
      makeSession("s-old", 1, 1_000),
      makeSession("s-mid", 2, 2_000),
      makeSession("s-new", 3, 3_000),
    ]);
    // A refresh where cohort 1 became the most recent.
    const after = groupByCohort([
      makeSession("s-old", 1, 9_000),
      makeSession("s-mid", 2, 2_000),
      makeSession("s-new", 3, 3_000),
    ]);

    expect(before.map((c) => c.cohortId)).toEqual([3, 2, 1]);
    expect(after.map((c) => c.cohortId)).toEqual([1, 3, 2]);

    const idsOf = (cohorts: ReturnType<typeof groupByCohort>) =>
      new Set(
        cohorts.flatMap((c) => [
          pastSessionsCohortToggleId(c.cohortId),
          ...c.sessions.flatMap((s) => [
            pastSessionCopyId(s.claudeSessionId),
            pastSessionResumeId(s.claudeSessionId),
          ]),
        ]),
      );

    expect(idsOf(after)).toEqual(idsOf(before));
  });
});

describe("past-sessions JSX stamps", () => {
  it("scopes the view, each cohort and each card with data-page-element", () => {
    expect(SOURCE).toContain(`data-page-element={PAST_SESSIONS_VIEW_ELEMENT}`);
    expect(SOURCE).toContain(`data-page-element={PAST_SESSIONS_COHORT_ELEMENT}`);
    expect(SOURCE).toContain(`data-page-element={PAST_SESSION_CARD_ELEMENT}`);

    // The read recipe uses the literal names, so pin them too.
    expect(PAST_SESSIONS_VIEW_ELEMENT).toBe("past-sessions-view");
    expect(PAST_SESSIONS_COHORT_ELEMENT).toBe("past-sessions-cohort");
    expect(PAST_SESSION_CARD_ELEMENT).toBe("past-session-card");
  });

  it("stamps a data-ui-bridge-id on every repeated action", () => {
    expect(SOURCE).toContain(`data-ui-bridge-id={pastSessionCopyId(session.claudeSessionId)}`);
    expect(SOURCE).toContain(`data-ui-bridge-id={pastSessionResumeId(session.claudeSessionId)}`);
    expect(SOURCE).toContain(`data-ui-bridge-id={PAST_SESSIONS_REFRESH_ID}`);
    expect(SOURCE).toContain(`data-ui-bridge-id={PAST_SESSIONS_RETRY_ID}`);
    // "Show N more" AND "Collapse" are one logical control → same id, twice.
    expect(
      SOURCE.split(`data-ui-bridge-id={pastSessionsCohortToggleId(cohort.cohortId)}`).length - 1,
    ).toBe(2);
  });

  it("leaves no <button> in this view without a stamped control id", () => {
    // Opening tags in this file are multi-line and close with a `>` on its own
    // line; a plain `[^>]*?>` would stop at the `>` of an `=>` arrow.
    const buttons = SOURCE.match(/<button\b[\s\S]*?\n\s*>/g) ?? [];
    const declared = (SOURCE.match(/<button\b/g) ?? []).length;

    expect(declared).toBeGreaterThan(0);
    // Guards the regex itself: a single-line `<button …>` would not match the
    // multi-line shape and could otherwise be silently skipped.
    expect(buttons).toHaveLength(declared);
    for (const button of buttons) {
      expect(button).toContain("data-ui-bridge-id=");
    }
  });
});

describe("past-session rows — restore verdict + row registration", () => {
  it("shows the in-flight verdict verbatim", () => {
    // The whole point of the backend projection: an attempted-but-unlanded
    // restore must not read as `failed`.
    expect(pastSessionRestoreBadge("pending (not yet confirmed)")).toBe(
      "pending (not yet confirmed)",
    );
  });

  it.each(["resumed", "terminal-only", "failed"])("shows the settled verdict %j", (verdict) => {
    expect(pastSessionRestoreBadge(verdict)).toBe(verdict);
  });

  it("renders nothing for the two silences", () => {
    // `undefined` = a runner build without the field (UNKNOWN — never claim
    // `not-restored`); `"not-restored"` = the backend's own "nothing to say",
    // true of nearly every row, so badging it is pure noise.
    expect(pastSessionRestoreBadge(undefined)).toBeNull();
    expect(pastSessionRestoreBadge("not-restored")).toBeNull();
    expect(pastSessionRestoreBadge("   ")).toBeNull();
  });

  it("keys the row id on the session, not on position", () => {
    // Cohorts re-sort newest-first on every refresh, so an ordinal-derived id
    // would name a different row after each poll.
    expect(pastSessionRowId("sess-a")).toBe("terminal.past-session-row-sess-a");
    expect(pastSessionRowId("sess-a")).not.toBe(pastSessionRowId("sess-b"));
  });

  it("stamps the card container so the row's TEXT reaches the snapshot", () => {
    // The scanner registers `[data-ui-bridge-id]` regardless of tag/role. A
    // card carrying only `data-page-element` is scrapeable by `read-value` but
    // absent from `/ui-bridge/control/snapshot`, so nothing the row says was
    // checkable through elements.
    expect(SOURCE).toContain("data-ui-bridge-id={pastSessionRowId(session.claudeSessionId)}");
  });

  it("renders the verdict into the row rather than only carrying it", () => {
    expect(SOURCE).toContain("pastSessionRestoreBadge(session.restoreStatus)");
    expect(SOURCE).toContain("{restoreBadge}");
  });
});
