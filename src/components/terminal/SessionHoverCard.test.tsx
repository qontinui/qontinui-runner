/**
 * The Session Manager hover card (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7): what it says for every field, that a field no source established
 * reads `Unknown` (never a default), and that its placement never covers the
 * row it describes.
 *
 * vitest runs in `node` with no jsdom, so markup is locked with
 * `renderToStaticMarkup` (the `SessionGroupSuggestionsDialog` precedent).
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
  useUIComponent: () => {},
}));

import {
  HOVER_CARD_GAP,
  HOVER_CARD_MARGIN,
  SESSION_HOVER_CARD_ID,
  SessionHoverCard,
  buildHoverCardModel,
  placeHoverCard,
  type HoverCardField,
  type HoverCardSession,
  type Rect,
  type Size,
} from "./SessionHoverCard";
import { known, unknownFact } from "./sessionFacts";
import { HOUR, MIN, NOW, facts } from "./sessionFacts.testkit";
import type { SessionStatus } from "./sessionStatusModel";

function status(over: Partial<SessionStatus> = {}): SessionStatus {
  return {
    location: "hidden-here",
    activity: "needs-you",
    reason: "registry: waiting",
    since: NOW - 12 * MIN,
    exitCode: null,
    ...over,
  };
}

function session(over: Partial<HoverCardSession> = {}): HoverCardSession {
  return {
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    registryName: "worktree prune",
    resumeName: null,
    displayName: "PR qontinui/qontinui-coord#1151 just merged…",
    workSummaryHint: "editing lock_engine.py",
    firstMessagePreview: "please fix the scheduler",
    accountLabel: "paktis",
    lastModified: new Date(NOW - 5 * MIN).toISOString(),
    lastMessageTimestamp: new Date(NOW - 5 * MIN).toISOString(),
    status: status(),
    ...over,
  };
}

function byKey(fields: HoverCardField[], key: HoverCardField["key"]): HoverCardField {
  const f = fields.find((x) => x.key === key);
  if (!f) throw new Error(`no field ${key}`);
  return f;
}

describe("buildHoverCardModel — known facts", () => {
  const model = buildHoverCardModel(
    session(),
    facts({
      prs: known([
        { repo: "qontinui-runner", number: 12, ci: "green", authorActs: false, reason: null },
        { repo: "qontinui-runner", number: 13, ci: "landed", authorActs: null, reason: null },
        {
          repo: "qontinui-coord",
          number: 7,
          ci: "unknown",
          authorActs: null,
          reason: "coord unreachable",
        },
      ]),
    }),
    NOW,
  );

  it("titles the card with the card's headline and states the topic", () => {
    expect(model.title).toBe("worktree prune");
    expect(byKey(model.fields, "topic").value).toBe("editing lock_engine.py");
  });

  it("states repo, branch, account, group from the facts", () => {
    expect(byKey(model.fields, "repo").value).toBe("qontinui-runner");
    expect(byKey(model.fields, "branch").value).toBe("agent/feature-x");
    expect(byKey(model.fields, "account").value).toBe("paktis");
    expect(byKey(model.fields, "group").value).toBe("Coord work");
  });

  it("counts PRs opened / landed / unknown and lists each", () => {
    const prs = byKey(model.fields, "prs");
    expect(prs.value).toBe("3 opened · 1 landed · 1 unknown");
    expect(prs.detail).toContain("qontinui-runner#12 green");
    expect(prs.detail).toContain("qontinui-coord#7 CI unknown (coord unreachable)");
  });

  it("states the location word with its page, and the activity with reason and since", () => {
    expect(byKey(model.fields, "location").value).toBe('Hidden here · page "Main"');
    const activity = byKey(model.fields, "activity");
    expect(activity.value).toBe("Needs you");
    expect(activity.detail).toContain("registry: waiting");
    expect(activity.detail).toContain("(12m ago)");
  });

  it("states last activity from the facts", () => {
    expect(byKey(model.fields, "last-activity").value).toBe("10m ago");
  });

  it("marks no field unknown when every source answered", () => {
    expect(model.fields.filter((f) => f.unknown)).toEqual([]);
  });
});

describe("buildHoverCardModel — Unknown is rendered, never a default", () => {
  it("renders every facts-derived field Unknown when the facts are not loaded", () => {
    const model = buildHoverCardModel(session({ accountLabel: "" }), null, NOW);
    for (const key of ["repo", "branch", "prs", "group", "account"] as const) {
      const f = byKey(model.fields, key);
      expect(f.value, key).toBe("Unknown");
      expect(f.unknown, key).toBe(true);
    }
    expect(byKey(model.fields, "location").value).toBe("Hidden here · page Unknown");
  });

  it("carries a fact's reason as the Unknown field's title", () => {
    const model = buildHoverCardModel(
      session(),
      facts({ branch: unknownFact("branch unknown: git timed out") }),
      NOW,
    );
    const branch = byKey(model.fields, "branch");
    expect(branch.value).toBe("Unknown");
    expect(branch.title).toBe("branch unknown: git timed out");
  });

  it("an unknown PR ledger is Unknown, not 'none opened'", () => {
    const model = buildHoverCardModel(
      session(),
      facts({ prs: unknownFact("PR scan never ran for this session") }),
      NOW,
    );
    expect(byKey(model.fields, "prs").value).toBe("Unknown");
  });

  it("an ungrouped session says Ungrouped; an unknown group says Unknown", () => {
    expect(
      byKey(buildHoverCardModel(session(), facts({ group: known(null) }), NOW).fields, "group")
        .value,
    ).toBe("Ungrouped");
    expect(
      byKey(
        buildHoverCardModel(session(), facts({ group: unknownFact("store unreadable") }), NOW)
          .fields,
        "group",
      ).value,
    ).toBe("Unknown");
  });

  it("unknown location and activity read Unknown with the classifier's reason", () => {
    const model = buildHoverCardModel(
      session({
        status: status({ location: "unknown", activity: "unknown", reason: "scan degraded" }),
      }),
      facts(),
      NOW,
    );
    expect(byKey(model.fields, "location")).toMatchObject({
      value: "Unknown",
      title: "scan degraded",
    });
    expect(byKey(model.fields, "activity")).toMatchObject({
      value: "Unknown",
      title: "scan degraded",
    });
  });

  it("an activity with no stated start says 'since Unknown'", () => {
    const model = buildHoverCardModel(session({ status: status({ since: null }) }), facts(), NOW);
    expect(byKey(model.fields, "activity").detail).toBe("registry: waiting · since Unknown");
  });

  it("a session with no topic source says Unknown", () => {
    const model = buildHoverCardModel(
      session({ workSummaryHint: "", resumeName: null, firstMessagePreview: null }),
      facts(),
      NOW,
    );
    expect(byKey(model.fields, "topic").unknown).toBe(true);
  });

  it("does not name a page for a session with no tab in this window", () => {
    const model = buildHoverCardModel(
      session({ status: status({ location: "not-running", activity: "exited" }) }),
      facts({ page: known(null) }),
      NOW,
    );
    expect(byKey(model.fields, "location").value).toBe("Not running");
  });
});

// ── Placement ────────────────────────────────────────────────────────────────

const VIEWPORT: Size = { width: 1280, height: 800 };
const CARD: Size = { width: 300, height: 260 };

function cardRect(p: ReturnType<typeof placeHoverCard>): Rect {
  return { left: p.left, top: p.top, right: p.left + p.width, bottom: p.top + p.maxHeight };
}

function overlaps(a: Rect, b: Rect): boolean {
  return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
}

describe("placeHoverCard", () => {
  it("goes to the right of a row in the left-hand panel, top-aligned", () => {
    const row = { left: 0, top: 100, right: 340, bottom: 150 };
    const p = placeHoverCard(row, CARD, VIEWPORT);
    expect(p).toMatchObject({ side: "right", left: 340 + HOVER_CARD_GAP, top: 100 });
  });

  it("is pulled up to stay inside the viewport for a row near the bottom", () => {
    const row = { left: 0, top: 760, right: 340, bottom: 790 };
    const p = placeHoverCard(row, CARD, VIEWPORT);
    expect(p.side).toBe("right");
    expect(p.top + p.maxHeight).toBeLessThanOrEqual(VIEWPORT.height - HOVER_CARD_MARGIN);
  });

  it("goes left when the right side has no room", () => {
    const row = { left: 900, top: 100, right: 1270, bottom: 150 };
    expect(placeHoverCard(row, CARD, VIEWPORT).side).toBe("left");
  });

  it("goes below, then above, for a row as wide as the viewport", () => {
    expect(placeHoverCard({ left: 0, top: 50, right: 1280, bottom: 80 }, CARD, VIEWPORT).side).toBe(
      "below",
    );
    expect(
      placeHoverCard({ left: 0, top: 700, right: 1280, bottom: 730 }, CARD, VIEWPORT).side,
    ).toBe("above");
  });

  it("caps its height to the roomier side when no side fits it whole", () => {
    const p = placeHoverCard(
      { left: 0, top: 300, right: 1280, bottom: 330 },
      { width: 300, height: 2000 },
      VIEWPORT,
    );
    expect(p.side).toBe("below");
    expect(p.maxHeight).toBe(VIEWPORT.height - HOVER_CARD_MARGIN - (330 + HOVER_CARD_GAP));
  });

  it("never covers the row it describes, over a sweep of rows, cards and viewports", () => {
    // Deterministic LCG so a failure reproduces.
    let seed = 7;
    const rand = (n: number) => {
      seed = (seed * 1103515245 + 12345) % 2 ** 31;
      return Math.floor((seed / 2 ** 31) * n);
    };
    for (let i = 0; i < 5000; i++) {
      const viewport = { width: 200 + rand(1800), height: 150 + rand(1100) };
      const left = rand(viewport.width);
      const top = rand(viewport.height);
      const row = {
        left,
        top,
        right: Math.min(viewport.width, left + 1 + rand(viewport.width)),
        bottom: Math.min(viewport.height, top + 1 + rand(120)),
      };
      const card = { width: 50 + rand(500), height: 20 + rand(900) };
      const p = placeHoverCard(row, card, viewport);
      expect(overlaps(cardRect(p), row), JSON.stringify({ row, card, viewport, p })).toBe(false);
    }
  });
});

// ── Markup ───────────────────────────────────────────────────────────────────

describe("SessionHoverCard markup", () => {
  it("renders every field with its UI Bridge id, and Unknown fields marked", () => {
    const model = buildHoverCardModel(
      session(),
      facts({ branch: unknownFact("git timed out") }),
      NOW + HOUR,
    );
    const html = renderToStaticMarkup(
      <SessionHoverCard model={model} anchor={{ left: 0, top: 100, right: 340, bottom: 150 }} />,
    );
    expect(html).toContain(`data-ui-bridge-id="${SESSION_HOVER_CARD_ID}"`);
    expect(html).toContain('data-effect="read"');
    expect(html).toContain('role="tooltip"');
    expect(html).toContain('data-hover-card-side="right"');
    for (const key of [
      "topic",
      "repo",
      "branch",
      "prs",
      "location",
      "activity",
      "account",
      "last-activity",
      "group",
    ]) {
      expect(html, key).toContain(`data-hover-field="${key}"`);
    }
    expect(html).toMatch(/title="git timed out" data-unknown="true"><span[^>]*>Unknown<\/span>/);
  });
});
