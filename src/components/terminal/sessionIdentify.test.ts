/**
 * Phase 7's pure modules — the topic line, the unseen-activity dot and the
 * mute filter (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`).
 */

import { describe, expect, it } from "vitest";

import { audibleNeedsInput, mutedTabIds, tabSessionKey } from "./sessionMute";
import { known, unknownFact } from "./sessionFacts";
import type { MemberOrg, SessionOrg } from "./sessionOrgTypes";
import { sessionTopic, sessionTopicTitle } from "./sessionTopic";
import { observeActivity, unseenVerdict, type ActivityObservation } from "./sessionUnseen";
import type { SessionOrgView } from "./useSessionOrg";
import { createActivityObserver, lastSeenFact } from "./useSessionUnseen";

// ── Topic ────────────────────────────────────────────────────────────────────

describe("sessionTopic", () => {
  const all = {
    workSummaryHint: "editing lock_engine.py",
    resumeName: "fix the flaky scheduler test",
    firstMessagePreview: "please\n  look at the scheduler",
  };

  it("prefers the work summary, then the resume name, then the first prompt", () => {
    expect(sessionTopic(all)).toEqual({ text: "editing lock_engine.py", source: "work-summary" });
    expect(sessionTopic({ ...all, workSummaryHint: "" })?.source).toBe("resume-name");
    expect(sessionTopic({ ...all, workSummaryHint: " ", resumeName: null })).toEqual({
      text: "please look at the scheduler",
      source: "first-prompt",
    });
  });

  it("skips a candidate that repeats the headline", () => {
    expect(sessionTopic(all, "Editing  lock_engine.py")?.source).toBe("resume-name");
  });

  it("is null when nothing carries a topic", () => {
    expect(
      sessionTopic({ workSummaryHint: "", resumeName: null, firstMessagePreview: undefined }),
    ).toBeNull();
  });

  it("the title carries the full value and its source", () => {
    expect(sessionTopicTitle({ text: "x y", source: "first-prompt" })).toBe(
      "x y\n(topic from the first prompt)",
    );
  });
});

// ── Unseen ───────────────────────────────────────────────────────────────────

describe("observeActivity", () => {
  it("records first observation without a change", () => {
    const next = observeActivity(new Map(), [{ key: "a", activity: "working" }], 100);
    expect(next.get("a")).toEqual({
      activity: "working",
      firstObservedAt: 100,
      activityChangedAt: null,
    });
  });

  it("stamps a change of the known activity word", () => {
    const one = observeActivity(new Map(), [{ key: "a", activity: "working" }], 100);
    const two = observeActivity(one, [{ key: "a", activity: "needs-you" }], 200);
    expect(two.get("a")?.activityChangedAt).toBe(200);
  });

  it("returns the same map when nothing changed (no re-render)", () => {
    const one = observeActivity(new Map(), [{ key: "a", activity: "working" }], 100);
    expect(observeActivity(one, [{ key: "a", activity: "working" }], 200)).toBe(one);
  });

  it("an unknown reading neither sets nor moves the change stamp", () => {
    const one = observeActivity(new Map(), [{ key: "a", activity: "working" }], 100);
    const two = observeActivity(one, [{ key: "a", activity: "unknown" }], 200);
    expect(two).toBe(one);
    const three = observeActivity(two, [{ key: "a", activity: "working" }], 300);
    expect(three.get("a")?.activityChangedAt).toBeNull();
  });

  it("the first known reading after an unknown start is not a change", () => {
    const one = observeActivity(new Map(), [{ key: "a", activity: "unknown" }], 100);
    const two = observeActivity(one, [{ key: "a", activity: "idle-at-prompt" }], 200);
    expect(two.get("a")).toMatchObject({ activity: "idle-at-prompt", activityChangedAt: null });
  });

  it("drops sessions no longer listed", () => {
    const one = observeActivity(new Map(), [{ key: "a", activity: "working" }], 100);
    expect(observeActivity(one, [], 200).size).toBe(0);
  });
});

describe("createActivityObserver", () => {
  it("notifies its subscribers only when a listing changed something", () => {
    const observer = createActivityObserver();
    let calls = 0;
    observer.subscribe(() => calls++);
    observer.observe([{ key: "a", activity: "working" }], 100);
    observer.observe([{ key: "a", activity: "working" }], 200);
    expect(calls).toBe(1);
    observer.observe([{ key: "a", activity: "needs-you" }], 300);
    expect(calls).toBe(2);
    expect(observer.get().get("a")?.activityChangedAt).toBe(300);
  });
});

describe("lastSeenFact", () => {
  it("is unknown until the store is known, then the member's stamp or null", () => {
    expect(lastSeenFact({ phase: "loading" }, "a").known).toBe(false);
    expect(lastSeenFact(orgView({ a: member({ lastSeenAt: 5 }) }), "a")).toEqual(known(5));
    expect(lastSeenFact(orgView({}), "a")).toEqual(known(null));
  });
});

describe("unseenVerdict", () => {
  const obs = (over: Partial<ActivityObservation> = {}): ActivityObservation => ({
    activity: "working",
    firstObservedAt: 1_000,
    activityChangedAt: null,
    ...over,
  });

  it("output after the last focus is unseen", () => {
    expect(
      unseenVerdict({
        lastSeenAt: known(5_000),
        lastActivityAt: 6_000,
        observation: obs(),
        focused: false,
      }),
    ).toEqual({ state: "unseen", since: 6_000 });
  });

  it("an activity change after the last focus is unseen", () => {
    expect(
      unseenVerdict({
        lastSeenAt: known(5_000),
        lastActivityAt: 4_000,
        observation: obs({ activityChangedAt: 7_000 }),
        focused: false,
      }),
    ).toEqual({ state: "unseen", since: 7_000 });
  });

  it("nothing since the last focus is seen", () => {
    expect(
      unseenVerdict({
        lastSeenAt: known(5_000),
        lastActivityAt: 4_000,
        observation: obs({ activityChangedAt: 3_000 }),
        focused: false,
      }).state,
    ).toBe("seen");
  });

  it("the focused session is never unseen", () => {
    expect(
      unseenVerdict({
        lastSeenAt: known(5_000),
        lastActivityAt: 9_000,
        observation: obs(),
        focused: true,
      }).state,
    ).toBe("seen");
  });

  it("a never-focused session is measured from when this window first listed it", () => {
    const base = { lastSeenAt: known<number | null>(null), focused: false };
    expect(unseenVerdict({ ...base, lastActivityAt: 900, observation: obs() }).state).toBe("seen");
    expect(unseenVerdict({ ...base, lastActivityAt: 1_500, observation: obs() }).state).toBe(
      "unseen",
    );
  });

  it("an unreadable store is unknown, never seen", () => {
    expect(
      unseenVerdict({
        lastSeenAt: unknownFact("organization store not read yet"),
        lastActivityAt: 9_000,
        observation: obs(),
        focused: false,
      }),
    ).toEqual({ state: "unknown", reason: "organization store not read yet" });
  });
});

// ── Mute ─────────────────────────────────────────────────────────────────────

function member(over: Partial<MemberOrg> = {}): MemberOrg {
  return {
    groupId: null,
    order: 0,
    pinned: false,
    tags: [],
    muted: false,
    snoozedUntil: null,
    lastSeenAt: null,
    ...over,
  };
}

function orgView(members: Record<string, MemberOrg>): SessionOrgView {
  const org: SessionOrg = {
    version: 1,
    revision: 1,
    groups: [],
    members,
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
  return { phase: "known", org, pending: 0, lastError: null };
}

describe("mute", () => {
  const ctx = { pageId: "p1", windowLabel: "main" };
  const claudeTab = { id: "t1", title: "shell", claudeSessionId: "sid-1" };
  const plainTab = { id: "t2", title: "cwd title", spawnName: "build", workingDir: "/r" };

  it("keys a tab by its Claude session id, else its pane key (spawn name over title)", () => {
    expect(tabSessionKey(claudeTab, ctx)).toBe("sid-1");
    expect(tabSessionKey(plainTab, ctx)).toBe("pane:p1\u001fbuild\u001f/r");
  });

  it("collects the muted tabs only", () => {
    const view = orgView({
      "sid-1": member({ muted: true }),
      [tabSessionKey(plainTab, ctx)]: member({ muted: false }),
    });
    expect([...mutedTabIds([claudeTab, plainTab], view, ctx)]).toEqual(["t1"]);
  });

  it("an unknown or loading store mutes nothing", () => {
    expect(mutedTabIds([claudeTab], { phase: "loading" }, ctx).size).toBe(0);
    expect(
      mutedTabIds([claudeTab], { phase: "unknown", reason: "x", preservedAt: null }, ctx).size,
    ).toBe(0);
  });

  it("drops only the muted tabs from the sound / notification list", () => {
    expect(audibleNeedsInput(["t1", "t2", "t3"], new Set(["t2"]))).toEqual(["t1", "t3"]);
    expect(audibleNeedsInput(["t2"], new Set(["t2"]))).toEqual([]);
  });
});
