/**
 * Phase 7 wiring (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`):
 * the card's topic line and unseen dot, the rail's hover-card body, the
 * panel's hover-card host, and the agreement between the unseen dot and the
 * `is:unseen` filter.
 *
 * vitest runs in `node` with no jsdom, so markup is locked with
 * `renderToStaticMarkup` (the `sessionOrgPanel.test.tsx` precedent); the
 * mounted `useSessionManager` wiring needs jsdom and is verified on a temp
 * runner (Phase 8).
 */

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
  useUIComponent: () => {},
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

import { SessionCard, sessionCardTopicLine, unseenDotTitle } from "./SessionCard";
import { SessionRail } from "./SessionRail";
import {
  HOVER_CARD_GAP,
  SessionHoverCardHost,
  type Rect,
  type SessionHoverCardController,
} from "./SessionHoverCard";
import { known, unknownFact, unseenFact } from "./sessionFacts";
import { facts } from "./sessionFacts.testkit";
import { filterSessions } from "./sessionQuery";
import { injectedStatus, type SessionActivity } from "./sessionStatusModel";
import { focusedSessionKey, unseenAttr, type UnseenVerdict } from "./sessionUnseen";
import { sessionOrgControlId } from "./sessionOrgControls";
import type { UnifiedSession } from "./useSessionManager";

const noop = () => {};

function tabRow(id: string, over: Partial<UnifiedSession> = {}): UnifiedSession {
  return {
    source: "tab",
    _transcript: null,
    sessionId: id,
    accountLabel: "",
    configDir: "",
    projectPath: "/w/qontinui-runner",
    projectLabel: "qontinui-runner",
    displayName: `shell ${id}`,
    resumeName: null,
    registryName: null,
    firstMessagePreview: null,
    messageCount: 0,
    lastModified: "",
    hasPlans: false,
    status: injectedStatus("hidden-here", "working" as SessionActivity),
    zoneTabId: id,
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "",
    lastMessagePreview: "",
    workSummaryHint: "",
    ...over,
  } as UnifiedSession;
}

function card(session: UnifiedSession, over: Record<string, unknown> = {}): string {
  return renderToStaticMarkup(
    createElement(SessionCard, {
      session,
      isSelected: false,
      isChecked: false,
      selectionMode: false,
      orgMember: null,
      onResume: noop,
      onOpen: noop,
      onViewTranscript: noop,
      onCopyId: noop,
      onToggleSelect: noop,
      ...over,
    }),
  );
}

// ── Topic line ───────────────────────────────────────────────────────────────

describe("the card's topic line", () => {
  it("is the work summary, else the resume name, else the first prompt", () => {
    const base = { registryName: "live name", displayName: "d", firstMessagePreview: "fix it" };
    expect(
      sessionCardTopicLine({ ...base, workSummaryHint: "editing x.rs", resumeName: "r" })?.text,
    ).toBe("editing x.rs");
    expect(sessionCardTopicLine({ ...base, workSummaryHint: "", resumeName: "r" })?.text).toBe("r");
    expect(sessionCardTopicLine({ ...base, workSummaryHint: "", resumeName: null })?.text).toBe(
      "fix it",
    );
    expect(
      sessionCardTopicLine({
        ...base,
        workSummaryHint: "",
        resumeName: null,
        firstMessagePreview: null,
      }),
    ).toBeNull();
  });

  it("skips a candidate that repeats the headline", () => {
    // No registry name: the headline IS the resume name, so it is not repeated.
    const line = sessionCardTopicLine({
      registryName: null,
      resumeName: "Runner fixes",
      displayName: "d",
      workSummaryHint: "",
      firstMessagePreview: "please fix the runner",
    });
    expect(line?.text).toBe("please fix the runner");
  });

  it("renders as the card's second line with the full value and its source in the title", () => {
    const long = "editing lock_engine.py to retire the per-repo lock and ".repeat(3).trim();
    const html = card(tabRow("a", { workSummaryHint: long, lastMessagePreview: "OLD PREVIEW" }));
    const topic = html.match(
      /<div data-testid="session-card-topic"[^>]*title="([^"]*)"[^>]*>([^<]*)</,
    );
    expect(topic).not.toBeNull();
    expect(topic![1]).toBe(`${long}\n(topic from the work summary)`);
    expect(topic![2]).toBe(long);
    // Second line: after the name row, before the status words.
    expect(html.indexOf("session-card-topic")).toBeLessThan(html.indexOf("session-card-status"));
    // The old preview line is gone.
    expect(html).not.toContain("OLD PREVIEW");
  });

  it("is not on a compact (pinned) row", () => {
    const html = card(tabRow("a", { workSummaryHint: "editing x" }), { compact: true });
    expect(html).not.toContain("session-card-topic");
  });
});

// ── Unseen dot ───────────────────────────────────────────────────────────────

describe("the card's unseen dot", () => {
  const since = new Date(2026, 9, 14, 9, 30).getTime();

  it("is drawn only for an unseen verdict, with its UI Bridge id", () => {
    const html = card(tabRow("a"), { unseen: { state: "unseen", since } });
    expect(html).toContain('data-session-unseen="true"');
    expect(html).toContain(`data-ui-bridge-id="${sessionOrgControlId("unseenDot", "a")}"`);
    expect(html).toContain('data-testid="session-card-unseen"');
  });

  it("a seen or Unknown verdict draws no dot, and the attribute says which", () => {
    const seen = card(tabRow("a"), { unseen: { state: "seen" } });
    expect(seen).toContain('data-session-unseen="false"');
    expect(seen).not.toContain("session-card-unseen");
    const unknown = card(tabRow("a"), { unseen: { state: "unknown", reason: "store down" } });
    expect(unknown).toContain('data-session-unseen="unknown"');
    expect(unknown).not.toContain("session-card-unseen");
    expect(card(tabRow("a"))).toContain('data-session-unseen="untracked"');
  });

  it("its tooltip says what it means and how to clear it", () => {
    expect(unseenDotTitle({ state: "unseen", since })).toMatch(
      /^New activity since you last looked .*focus the session to clear$/,
    );
    expect(unseenDotTitle({ state: "seen" })).toBeNull();
    expect(unseenDotTitle(undefined)).toBeNull();
  });
});

describe("the unseen dot and is:unseen agree", () => {
  it("the filter matches exactly the rows the dot is drawn on, Unknown to the tail", () => {
    const verdicts: Record<string, UnseenVerdict> = {
      a: { state: "unseen", since: 1 },
      b: { state: "seen" },
      c: { state: "unknown", reason: "organization store unknown: corrupt" },
    };
    const rows = Object.entries(verdicts).map(([id, v]) =>
      // lastSeenAt says "never focused" — the filter must not re-derive from it.
      facts({ rowId: id, lastSeenAt: known(null), unseen: unseenFact(v) }),
    );
    const r = filterSessions("is:unseen", rows, { now: 0 });
    if (!r.ok) throw new Error("parse failed");
    expect(r.matched.map((f) => f.rowId)).toEqual(
      Object.keys(verdicts).filter((id) => unseenAttr(verdicts[id]) === "true"),
    );
    expect(r.unknown.map((u) => u.facts.rowId)).toEqual(["c"]);
    expect(r.unknown[0].reason).toBe("organization store unknown: corrupt");
  });

  it("an unknown fact never matches -is:unseen either", () => {
    const r = filterSessions("-is:unseen", [facts({ unseen: unknownFact("x") })], { now: 0 });
    if (!r.ok) throw new Error("parse failed");
    expect(r.matched).toHaveLength(0);
    expect(r.unknown).toHaveLength(1);
  });
});

describe("focusedSessionKey — focus clears the dot of the row it lands on", () => {
  const rows = [
    { sessionId: "t1", source: "transcript" as const, zoneTabId: "tab-9" },
    { sessionId: "tab-3", source: "tab" as const, zoneTabId: "tab-3" },
    { sessionId: "t2", source: "transcript" as const, zoneTabId: null },
  ];
  const keyOf = (r: (typeof rows)[number]) => `key:${r.sessionId}`;

  it("a transcript row through its live tab, a tab row through its own id", () => {
    expect(focusedSessionKey(rows, "tab-9", keyOf)).toBe("key:t1");
    expect(focusedSessionKey(rows, "tab-3", keyOf)).toBe("key:tab-3");
  });

  it("no focused tab, or one backing no row, focuses nothing", () => {
    expect(focusedSessionKey(rows, null, keyOf)).toBeNull();
    expect(focusedSessionKey(rows, "plain-shell", keyOf)).toBeNull();
  });
});

// ── Rail: the hover card is the item's body ──────────────────────────────────

describe("SessionRail with the hover card", () => {
  const bind = () => ({ onMouseEnter: noop, onMouseLeave: noop });

  it("drops the native tooltip when the hover card is bound, keeping the accessible name", () => {
    const rows = [tabRow("a")];
    const withCard = renderToStaticMarkup(
      createElement(SessionRail, {
        sessions: rows,
        groupColorOf: () => null,
        selectedSessionId: null,
        onOpen: noop,
        onExpand: noop,
        width: 44,
        hoverCard: bind,
      }),
    );
    expect(withCard).not.toMatch(/<button[^>]*rail-item-a[^>]*title=/);
    expect(withCard).toContain('aria-label="shell a: Working · Hidden here"');
    const withoutCard = renderToStaticMarkup(
      createElement(SessionRail, {
        sessions: rows,
        groupColorOf: () => null,
        selectedSessionId: null,
        onOpen: noop,
        onExpand: noop,
        width: 44,
      }),
    );
    expect(withoutCard).toMatch(/<button[^>]*rail-item-a[^>]*title=/);
  });

  it("marks an unseen session's item, in its accessible name too", () => {
    const html = renderToStaticMarkup(
      createElement(SessionRail, {
        sessions: [tabRow("a"), tabRow("b")],
        groupColorOf: () => null,
        selectedSessionId: null,
        onOpen: noop,
        onExpand: noop,
        width: 44,
        hoverCard: bind,
        unseenOf: (s: UnifiedSession): UnseenVerdict =>
          s.sessionId === "a" ? { state: "unseen", since: 1 } : { state: "seen" },
      }),
    );
    expect(html).toContain(`data-ui-bridge-id="${sessionOrgControlId("unseenDot", "a")}"`);
    expect(html).not.toContain(sessionOrgControlId("unseenDot", "b"));
    expect(html).toContain('aria-label="shell a: Working · Hidden here (new activity)"');
  });
});

// ── The panel's hover-card host ──────────────────────────────────────────────

function controller(target: SessionHoverCardController["target"]): SessionHoverCardController {
  return {
    target,
    bind: () => ({ onMouseEnter: noop, onMouseLeave: noop }),
    cardHandlers: { onPointerEnter: noop, onPointerLeave: noop },
    close: noop,
  };
}

describe("SessionHoverCardHost", () => {
  const anchor: Rect = { left: 0, top: 120, right: 340, bottom: 170 };
  const row = tabRow("a", { workSummaryHint: "editing lock_engine.py" });

  it("renders nothing until a row is hovered, or when the row has gone", () => {
    const none = renderToStaticMarkup(
      createElement(SessionHoverCardHost, {
        controller: controller(null),
        sessionOf: () => row,
        factsOf: () => null,
      }),
    );
    expect(none).toBe("");
    const gone = renderToStaticMarkup(
      createElement(SessionHoverCardHost, {
        controller: controller({ sessionId: "a", anchor }),
        sessionOf: () => undefined,
        factsOf: () => null,
      }),
    );
    expect(gone).toBe("");
  });

  it("renders the hovered row's card beside the row, never over it", () => {
    const html = renderToStaticMarkup(
      createElement(SessionHoverCardHost, {
        controller: controller({ sessionId: "a", anchor }),
        sessionOf: () => row,
        factsOf: () => facts({ repo: known("qontinui-runner"), branch: known("agent/x") }),
      }),
    );
    expect(html).toContain('data-session-id="a"');
    expect(html).toContain('data-hover-card-side="right"');
    // Placed to the right of the row: its left edge clears the row's right edge.
    const left = Number(html.match(/left:(\d+(?:\.\d+)?)px/)![1]);
    expect(left).toBe(anchor.right + HOVER_CARD_GAP);
    expect(html).toContain("editing lock_engine.py");
    expect(html).toContain("qontinui-runner");
    expect(html).toContain("agent/x");
  });

  it("the card reads Unknown for facts not built yet, never a default", () => {
    const html = renderToStaticMarkup(
      createElement(SessionHoverCardHost, {
        controller: controller({ sessionId: "a", anchor }),
        sessionOf: () => row,
        factsOf: () => null,
      }),
    );
    expect(html).toMatch(/data-hover-field="repo"[\s\S]*?data-unknown="true"/);
  });
});
