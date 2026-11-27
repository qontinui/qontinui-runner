import { describe, expect, it } from "vitest";

import {
  acceptAllOps,
  dismissOp,
  shortenPlanStem,
  suggestGroups,
  type SuggestionOrg,
  type SuggestionSession,
} from "./sessionGroupSuggestions";
import { applyOpsOptimistic } from "./sessionOrgOptimistic";
import {
  SESSION_GROUP_COLOR_ROLES,
  type MemberOrg,
  type SessionGroup,
  type SessionOrg,
} from "./sessionOrgTypes";

const PLAN_A = "2026-10-13-session-manager-chrome-style-tab-organization";
const PLAN_B = "2026-10-11-runner-terminal-tmux-work-mode";

function emptyOrg(): SessionOrg {
  return {
    version: 1,
    revision: 0,
    groups: [],
    members: {},
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
}

function member(groupId: string | null): MemberOrg {
  return {
    groupId,
    order: 0,
    pinned: false,
    tags: [],
    muted: false,
    snoozedUntil: null,
    lastSeenAt: null,
  };
}

function group(id: string, colorRole: SessionGroup["colorRole"]): SessionGroup {
  return { id, title: id, colorRole, collapsed: false, order: 0, createdAt: 0 };
}

function s(sessionKey: string, keys: Partial<SuggestionSession> = {}): SuggestionSession {
  return { sessionKey, title: `title ${sessionKey}`, ...keys };
}

describe("suggestGroups", () => {
  it("proposes nothing when no session carries a key", () => {
    expect(suggestGroups([s("a"), s("b"), s("c")], emptyOrg())).toEqual([]);
  });

  it("drops a key only one session carries", () => {
    const sessions = [s("a", { planSlug: PLAN_A }), s("b"), s("c", { repo: "qontinui-runner" })];
    expect(suggestGroups(sessions, emptyOrg())).toEqual([]);
  });

  it("files each session under its strongest key only — the plan wins", () => {
    const sessions = [
      s("a", { planSlug: PLAN_A, prNumbers: [7], repo: "qontinui-runner", branch: "x" }),
      s("b", { planSlug: PLAN_A, repo: "qontinui-coord" }),
      s("c", { prNumbers: [7], repo: "qontinui-runner", branch: "x" }),
      s("d", { prNumbers: [7], repo: "qontinui-runner" }),
      s("e", { repo: "qontinui-runner", branch: "x" }),
    ];
    const out = suggestGroups(sessions, emptyOrg());
    // `a` shares PR #7 and branch x but is filed under its plan; `c` shares
    // branch x with `e` but is filed under its PR, leaving `e` alone.
    expect(out.map((g) => [g.id, g.members.map((m) => m.sessionKey)])).toEqual([
      [`plan:${PLAN_A}`, ["a", "b"]],
      ["pr:qontinui-runner#7", ["c", "d"]],
    ]);
  });

  it("does not demote a session to a weaker key when its strongest is a singleton", () => {
    const sessions = [
      s("a", { planSlug: PLAN_A, repo: "r" }),
      s("b", { repo: "r" }),
      s("c", { repo: "r" }),
    ];
    const out = suggestGroups(sessions, emptyOrg());
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("repo:r");
    expect(out[0].members.map((m) => m.sessionKey)).toEqual(["b", "c"]);
  });

  it("clusters by PR, repo-qualified, choosing the most shared PR", () => {
    const sessions = [
      s("a", { prNumbers: [3, 12], repo: "r" }),
      s("b", { prNumbers: [12], repo: "r" }),
      s("c", { prNumbers: [12], repo: "other" }),
    ];
    const out = suggestGroups(sessions, emptyOrg());
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ id: "pr:r#12", keyKind: "pr", name: "PR #12" });
    expect(out[0].members.map((m) => m.sessionKey)).toEqual(["a", "b"]);
  });

  it("excludes sessions already in a group", () => {
    const org = emptyOrg();
    org.groups = [group("g1", "brand-primary")];
    org.members = { a: member("g1"), b: member(null) };
    const sessions = [
      s("a", { planSlug: PLAN_A }),
      s("b", { planSlug: PLAN_A }),
      s("c", { planSlug: PLAN_A }),
    ];
    const out = suggestGroups(sessions, org);
    expect(out[0].members.map((m) => m.sessionKey)).toEqual(["b", "c"]);
  });

  it("excludes dismissed suggestions, by the id the dismiss op records", () => {
    const sessions = [
      s("a", { planSlug: PLAN_A }),
      s("b", { planSlug: PLAN_A }),
      s("c", { repo: "r" }),
      s("d", { repo: "r" }),
    ];
    const first = suggestGroups(sessions, emptyOrg());
    const dismissed = applyOpsOptimistic(emptyOrg(), [dismissOp(first[0])], 0);
    const after = suggestGroups(sessions, dismissed);
    expect(after.map((g) => g.id)).toEqual(["repo:r"]);
  });

  it("assigns the next unused color role and cycles like the store once all six are used", () => {
    const org = emptyOrg();
    org.groups = [group("g1", "brand-primary"), group("g2", "info")];
    const sessions = ["p1", "p2", "p3", "p4", "p5", "p6"].flatMap((plan) => [
      s(`${plan}-a`, { planSlug: plan }),
      s(`${plan}-b`, { planSlug: plan }),
    ]);
    const roles = suggestGroups(sessions, org).map((g) => g.colorRole);
    // Unused first, in role order; then ROLES[groupCount % 6] (6 → 0, 7 → 1).
    expect(roles).toEqual([
      "brand-secondary",
      "brand-success",
      "border-strong",
      "text-muted",
      SESSION_GROUP_COLOR_ROLES[0],
      SESSION_GROUP_COLOR_ROLES[1],
    ]);
  });

  it("6 ungrouped sessions across 2 plans → exactly 2 groups, and accepting creates them", () => {
    const sessions = [
      s("a1", { planSlug: PLAN_A, repo: "qontinui-runner", branch: "b1" }),
      s("a2", { planSlug: PLAN_A, repo: "qontinui-runner", branch: "b2" }),
      s("a3", { planSlug: PLAN_A, prNumbers: [101] }),
      s("b1", { planSlug: PLAN_B, repo: "qontinui-runner", branch: "b1" }),
      s("b2", { planSlug: PLAN_B, prNumbers: [101] }),
      s("b3", { planSlug: PLAN_B }),
    ];
    const out = suggestGroups(sessions, emptyOrg());
    expect(out).toHaveLength(2);
    expect(out.map((g) => g.name)).toEqual([
      "runner-terminal-tmux-work-mode",
      "session-manager-chrome-style…",
    ]);
    expect(out[1].keyValue).toBe(PLAN_A);

    const ops = acceptAllOps(out).map((op, i) =>
      op.op === "createGroup" ? { ...op, groupId: `g${i}` } : op,
    );
    const accepted = applyOpsOptimistic(emptyOrg(), ops, 0);
    expect(accepted.groups.map((g) => [g.title, g.colorRole])).toEqual([
      ["runner-terminal-tmux-work-mode", "brand-primary"],
      ["session-manager-chrome-style…", "brand-secondary"],
    ]);
    expect(
      Object.entries(accepted.members)
        .map(([k, m]) => `${k}=${m.groupId}`)
        .sort(),
    ).toEqual(["a1=g1", "a2=g1", "a3=g1", "b1=g0", "b2=g0", "b3=g0"]);
    // Accepted sessions are grouped, so nothing is proposed again.
    expect(suggestGroups(sessions, accepted)).toEqual([]);
  });

  it("is deterministic: input order and duplicates do not change the output", () => {
    const sessions = [
      s("z", { repo: "r", branch: "main" }),
      s("a", { planSlug: PLAN_A }),
      s("m", { repo: "r", branch: "main" }),
      s("b", { planSlug: PLAN_A }),
      s("q", { repo: "q" }),
      s("p", { repo: "q" }),
    ];
    const once = suggestGroups(sessions, emptyOrg());
    const shuffled = suggestGroups([...sessions].reverse().concat(sessions[0]), emptyOrg());
    expect(shuffled).toEqual(once);
    expect(once.map((g) => g.id)).toEqual([`plan:${PLAN_A}`, "branch:r:main", "repo:q"]);
  });
});

describe("shortenPlanStem", () => {
  it("strips the date prefix and keeps a short stem whole", () => {
    expect(shortenPlanStem("2026-10-11-runner-terminal-tmux-work-mode")).toBe(
      "runner-terminal-tmux-work-mode",
    );
  });

  it("cuts a long stem at a word boundary, and an unbreakable one hard", () => {
    expect(shortenPlanStem("2026-01-01-alpha-beta-gamma", 12)).toBe("alpha-beta…");
    expect(shortenPlanStem("abcdefghijklmnop", 8)).toBe("abcdefg…");
  });
});

describe("dismissOp / SuggestionOrg", () => {
  it("records the suggestion id", () => {
    const org: SuggestionOrg = { groups: [], members: {}, dismissedSuggestions: [] };
    const [g] = suggestGroups([s("a", { repo: "r" }), s("b", { repo: "r" })], org);
    expect(dismissOp(g)).toEqual({ op: "dismissSuggestion", key: "repo:r" });
  });
});
