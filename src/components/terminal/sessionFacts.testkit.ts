/**
 * A fully-known `SessionFacts` for the query-engine tests — override what a
 * test is about. Shared by `sessionQuery.test.ts`, `sessionQueryChips.test.ts`
 * and `sessionSearchPalette.test.ts`.
 */

import { known, type SessionFacts } from "./sessionFacts";

export const NOW = new Date(2026, 9, 13, 15, 0, 0).getTime();
export const MIN = 60_000;
export const HOUR = 60 * MIN;

/** A fully-known session; override what a test is about. */
export function facts(over: Partial<SessionFacts> = {}): SessionFacts {
  return {
    rowId: "row",
    sessionKey: "key",
    claudeSessionId: "sid",
    title: "Runner fixes",
    topic: "Task: fix the merge train",
    preview: "hello there",
    extraText: ["qontinui-runner"],
    location: known("on-screen"),
    activity: known("working"),
    exitCode: null,
    finished: known(false),
    pinned: known(false),
    muted: known(false),
    snoozedUntil: known(null),
    lastSeenAt: known(NOW - HOUR),
    group: known({ id: "g1", title: "Coord work", colorRole: "info" }),
    tags: known(["Urgent", "infra"]),
    account: known("paktis"),
    repo: known("qontinui-runner"),
    branch: known("agent/feature-x"),
    prs: known([]),
    workUnit: known("2026-10-13-session-manager"),
    device: known("spaceship"),
    page: known({ id: "p1", name: "Main" }),
    startedAt: known(NOW - 3 * HOUR),
    lastActivityAt: known(NOW - 10 * MIN),
    messageCount: known(120),
    ...over,
  };
}
