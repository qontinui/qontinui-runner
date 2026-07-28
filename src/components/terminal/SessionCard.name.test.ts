/**
 * Name-precedence contract for the Session Manager card.
 *
 * The runner's vitest env is `node` — no jsdom, no `@testing-library/react`
 * (precedent: `PastSessionsView.test.ts`, `LaunchMenu.test.tsx`) — so the
 * contract lives in the two pure exports the JSX consumes. Rendering is
 * verified against a temp runner.
 */

import { describe, it, expect } from "vitest";

import { sessionCardName, sessionCardTooltip } from "./SessionCard";
import type { UnifiedSession } from "./useSessionManager";

type NameFields = Pick<
  UnifiedSession,
  "sessionId" | "registryName" | "resumeName" | "workSummaryHint" | "displayName"
>;

function fields(over: Partial<NameFields> = {}): NameFields {
  return {
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    registryName: null,
    resumeName: "fix the flaky scheduler test",
    workSummaryHint: "editing lock_engine.py",
    displayName: "PR qontinui/qontinui-coord#1151 just merged…",
    ...over,
  };
}

describe("sessionCardName", () => {
  it("prefers the live window name over every transcript-derived field", () => {
    expect(sessionCardName(fields({ registryName: "worktree prune" }))).toBe("worktree prune");
  });

  it("falls back to resumeName, then displayName, when there is no window name", () => {
    // `registryName` is null both when the session is closed and when its
    // registry row is `nameSource: "derived"` — the gate lives in
    // `registryNamesBySessionId`, so this side just has to not regress.
    expect(sessionCardName(fields())).toBe("fix the flaky scheduler test");
    expect(sessionCardName(fields({ resumeName: null }))).toBe(
      "PR qontinui/qontinui-coord#1151 just merged…",
    );
  });

  it("ignores an empty-string window name rather than blanking the card", () => {
    expect(sessionCardName(fields({ registryName: "" }))).toBe("fix the flaky scheduler test");
  });
});

describe("sessionCardTooltip", () => {
  it("always names the session id", () => {
    expect(sessionCardTooltip(fields())).toContain("Session b770ae37-1ffa-4888-a5d1-89d058307adf");
  });

  it("keeps the transcript name reachable when the window name displaced it", () => {
    // The two disagreed in 22 of 33 measured cases (2026-07-23); promoting the
    // window name without this would make `resumeName` unreachable from the UI.
    const t = sessionCardTooltip(fields({ registryName: "worktree prune" }));
    expect(t).toContain("worktree prune");
    expect(t).toContain("transcript name: fix the flaky scheduler test");
  });

  it("does not repeat the name when nothing displaced it", () => {
    const t = sessionCardTooltip(fields());
    expect(t).toContain("fix the flaky scheduler test");
    expect(t).not.toContain("transcript name:");
  });

  it("falls back through workSummaryHint to displayName for the transcript line", () => {
    expect(
      sessionCardTooltip(fields({ registryName: "worktree prune", resumeName: null })),
    ).toContain("transcript name: editing lock_engine.py");
    expect(
      sessionCardTooltip(
        fields({ registryName: "worktree prune", resumeName: null, workSummaryHint: "" }),
      ),
    ).toContain("transcript name: PR qontinui/qontinui-coord#1151 just merged…");
  });
});
