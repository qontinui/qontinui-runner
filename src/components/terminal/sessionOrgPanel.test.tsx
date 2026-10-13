/**
 * The Session Manager's Phase 3 organization surfaces, rendered: the group
 * header, the close confirmation, the card's group bar and org chips, the
 * rail, the control table, the `session-org` bridge component, the panel
 * layout and the drag payload. Plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`.
 *
 * The runner's vitest config is `environment: "node"` and `jsdom` is not in
 * the shared `node_modules`, so — as `HiddenWorkersChip` and
 * `SessionGroupSuggestionsDialog` do — markup is locked with
 * `renderToStaticMarkup`, the UI Bridge SDK is replaced by a recorder, and
 * component actions are driven directly.
 */

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { components } = vi.hoisted(() => ({
  components: [] as {
    id: string;
    actions: { id: string; effect: string; handler: (p?: unknown) => unknown }[];
  }[],
}));

vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
  useUIComponent: (opts: (typeof components)[number]) => {
    components.push(opts);
  },
  UIBridgeComponentScope: ({ children }: { children: unknown }) => children,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

import { SessionGroupHeader, groupCountLabel, needsYouBadge } from "./SessionGroupHeader";
import {
  SessionCloseDialog,
  untickedRiskyTargets,
  closeTickKey,
  type CloseTarget,
} from "./SessionCloseDialog";
import { SessionCard, snoozeLabel } from "./SessionCard";
import { SessionRail, railItemTitle } from "./SessionRail";
import { SESSION_ORG_CONTROLS, sessionOrgControlId } from "./sessionOrgControls";
import {
  SESSION_ORG_COMPONENT_ID,
  useSessionOrgBridge,
  type SessionOrgBridgeDeps,
} from "./useSessionOrgBridge";
import {
  clampPanelWidth,
  parsePanelLayout,
  PANEL_DEFAULT_WIDTH,
  PANEL_MAX_WIDTH,
  PANEL_MIN_WIDTH,
} from "./sessionPanelLayout";
import {
  decodeSessionOrgDrag,
  encodeSessionOrgDrag,
  isSessionOrgDrag,
  SESSION_ORG_DRAG_MIME,
} from "./sessionOrgDrag";
import { snoozePresets } from "./SessionOrgMenus";
import {
  buildSuggestionSessions,
  fleetFactsBySessionId,
  repoFromWorkingDir,
} from "./sessionSuggestionInputs";
import { suggestGroups } from "./sessionGroupSuggestions";
import { injectedStatus, type SessionActivity } from "./sessionStatusModel";
import type { MemberOrg, SessionGroup, SessionOrg } from "./sessionOrgTypes";
import type { UnifiedSession } from "./useSessionManager";

beforeEach(() => {
  components.length = 0;
});

const noop = () => {};

function group(over: Partial<SessionGroup> = {}): SessionGroup {
  return {
    id: "g1",
    title: "Refactor",
    colorRole: "info",
    collapsed: false,
    order: 0,
    createdAt: 0,
    ...over,
  };
}

function tabRow(id: string, activity: SessionActivity = "working"): UnifiedSession {
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
    status: injectedStatus("hidden-here", activity),
    zoneTabId: id,
    zoneIndex: null,
    remoteDeviceLabel: null,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "",
    lastMessagePreview: "",
    workSummaryHint: "",
  };
}

function header(over: Partial<Parameters<typeof SessionGroupHeader>[0]> = {}): string {
  return renderToStaticMarkup(
    createElement(SessionGroupHeader, {
      group: group(),
      shownCount: 2,
      memberCount: 2,
      needsYouCount: 0,
      renaming: false,
      onStartRename: noop,
      onRename: noop,
      onCancelRename: noop,
      onToggleCollapse: noop,
      renderMenu: () => null,
      onDragStart: noop,
      onDragEnd: noop,
      onDragOver: noop,
      onDragLeave: noop,
      onDrop: noop,
      dropIndicator: null,
      ...over,
    }),
  );
}

describe("SessionGroupHeader", () => {
  it("a COLLAPSED group with Needs-you members still shows how many need you", () => {
    const html = header({
      group: group({ collapsed: true }),
      needsYouCount: 2,
      shownCount: 3,
      memberCount: 3,
    });
    expect(html).toContain('data-group-collapsed="true"');
    expect(html).toContain('data-group-needs-you-count="2"');
    expect(html).toContain("2 need you");
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain(`data-ui-bridge-id="${sessionOrgControlId("groupCollapse", "g1")}"`);
  });

  it("shows no badge at zero, the title, the color role and the count", () => {
    const html = header();
    expect(html).not.toContain("need you");
    expect(html).toContain("Refactor");
    expect(html).toContain('data-group-color-role="info"');
    expect(html).toContain("var(--color-info)");
  });

  it("renders the inline rename input while renaming", () => {
    expect(header({ renaming: true })).toContain(
      `data-ui-bridge-id="${sessionOrgControlId("groupRenameInput", "g1")}"`,
    );
  });

  it("labels the count and the badge in words", () => {
    expect(groupCountLabel(3, 3)).toBe("3");
    expect(groupCountLabel(1, 3)).toBe("1 of 3");
    expect(needsYouBadge(0)).toBeNull();
    expect(needsYouBadge(1)).toBe("1 needs you");
  });
});

describe("SessionCloseDialog", () => {
  const target = (id: string, activity: SessionActivity, closable = true): CloseTarget => ({
    sessionId: id,
    title: `Session ${id}`,
    activity,
    activityWord: activity,
    closable,
  });
  const render = (targets: CloseTarget[]) =>
    renderToStaticMarkup(
      createElement(SessionCloseDialog, {
        heading: "Close?",
        targets,
        onClose: () => Promise.resolve([]),
        onForceClose: () => [],
        onDismiss: noop,
      }),
    );
  const confirmButton = (html: string) =>
    html.match(
      new RegExp(`<button[^>]*data-ui-bridge-id="${SESSION_ORG_CONTROLS.closeConfirm.id}"[^>]*>`),
    )?.[0] ?? "";

  it("names every session with its activity word", () => {
    const html = render([target("a", "idle-at-prompt"), target("b", "finished")]);
    expect(html).toContain("Session a");
    expect(html).toContain("idle-at-prompt");
    expect(html).toContain("Session b");
    expect(confirmButton(html)).not.toContain(' disabled=""');
    expect(html).toContain("Close 2");
  });

  it.each(["needs-you", "working", "unknown"] as const)(
    "a %s row needs a second tick before Close enables",
    (activity) => {
      const html = render([target("a", "idle-at-prompt"), target("b", activity)]);
      expect(html).toContain(`data-ui-bridge-id="${sessionOrgControlId("closeTick", "b")}"`);
      expect(html).not.toContain(`data-ui-bridge-id="${sessionOrgControlId("closeTick", "a")}"`);
      expect(confirmButton(html)).toContain(' disabled=""');
    },
  );

  it("a tick covers one session in ONE activity", () => {
    const t = target("b", "working");
    expect(untickedRiskyTargets([t], new Set([closeTickKey(t)]))).toEqual([]);
    expect(
      untickedRiskyTargets([{ ...t, activity: "needs-you" }], new Set([closeTickKey(t)])),
    ).toHaveLength(1);
  });

  it("a row with no live tab is listed, not closed, and Close is off when nothing can close", () => {
    const html = render([target("a", "idle-at-prompt", false)]);
    expect(html).toContain("nothing to close");
    expect(confirmButton(html)).toContain(' disabled=""');
  });

  it("Force close is a separate control from Close", () => {
    const html = render([target("a", "idle-at-prompt")]);
    expect(html).toContain(`data-ui-bridge-id="${SESSION_ORG_CONTROLS.forceClose.id}"`);
    expect(html).not.toContain(`data-ui-bridge-id="${SESSION_ORG_CONTROLS.forceCloseConfirm.id}"`);
  });
});

describe("SessionCard — group bar and organization chips", () => {
  const member = (over: Partial<MemberOrg> = {}): MemberOrg => ({
    groupId: "g1",
    order: 0,
    pinned: false,
    tags: [],
    muted: false,
    snoozedUntil: null,
    lastSeenAt: null,
    ...over,
  });
  const card = (over: Record<string, unknown> = {}) =>
    renderToStaticMarkup(
      createElement(SessionCard, {
        session: tabRow("t1", "needs-you"),
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

  it("the left bar is the GROUP color; status stays the dot and its word", () => {
    const html = card({ groupColor: "var(--color-info)", orgMember: member() });
    expect(html).toContain("border-left-color:var(--color-info)");
    expect(html).toContain('data-session-group-color="var(--color-info)"');
    expect(html).toContain("Needs you");
  });

  it("an ungrouped card's bar is transparent", () => {
    expect(card()).toContain("border-left-color:transparent");
  });

  it("shows pin, mute and tags from the org store", () => {
    const html = card({ orgMember: member({ pinned: true, muted: true, tags: ["a", "b", "c"] }) });
    expect(html).toContain('aria-label="Pinned"');
    expect(html).toContain('aria-label="Muted"');
    expect(html).toContain('data-session-tag="a"');
    expect(html).toContain('data-session-tag="b"');
    expect(html).not.toContain('data-session-tag="c"');
    expect(html).toContain("+1");
  });

  it("a compact (pinned) row drops the meta line but keeps the status words", () => {
    expect(card()).toContain("terminal");
    const compact = card({ compact: true });
    expect(compact).not.toContain(">terminal<");
    expect(compact).toContain("Needs you");
  });

  it("snoozeLabel names a future time and nothing once it has passed", () => {
    const now = new Date("2026-10-13T10:00:00").getTime();
    expect(snoozeLabel(null, now)).toBeNull();
    expect(snoozeLabel(now - 1, now)).toBeNull();
    expect(snoozeLabel(now + 3_600_000, now)).toMatch(/^Snoozed until /);
  });
});

describe("SessionRail", () => {
  it("one item per row: dot over the group stripe, both status words in its name", () => {
    const rows = [tabRow("a", "needs-you"), tabRow("b", "working")];
    const html = renderToStaticMarkup(
      createElement(SessionRail, {
        sessions: rows,
        groupColorOf: (s: UnifiedSession) => (s.sessionId === "a" ? "var(--color-info)" : null),
        selectedSessionId: null,
        onOpen: noop,
        onExpand: noop,
        width: 44,
      }),
    );
    expect(html.match(/data-ui-bridge-id="terminal\.session-org\.rail-item-/g)).toHaveLength(2);
    expect(html).toContain("border-left-color:var(--color-info)");
    expect(railItemTitle(rows[0])).toBe("shell a\nNeeds you · Hidden here");
  });
});

describe("the control table", () => {
  it("every control is a terminal.session-org.* id with a declared effect", () => {
    for (const [name, c] of Object.entries(SESSION_ORG_CONTROLS)) {
      expect(c.id, name).toMatch(/^terminal\.session-org\./);
      expect(["read", "write", "destructive"], name).toContain(c.effect);
    }
    const ids = Object.values(SESSION_ORG_CONTROLS).map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("exactly the closing controls are destructive", () => {
    expect(
      Object.entries(SESSION_ORG_CONTROLS)
        .filter(([, c]) => c.effect === "destructive")
        .map(([n]) => n)
        .sort(),
    ).toEqual(["closeConfirm", "forceCloseConfirm", "groupMenuCloseGroup", "sessionMenuClose"]);
  });
});

describe("the session-org bridge component", () => {
  const org: SessionOrg = {
    version: 1,
    revision: 1,
    groups: [group()],
    members: {
      "key-a": {
        groupId: "g1",
        order: 0,
        pinned: false,
        tags: [],
        muted: false,
        snoozedUntil: null,
        lastSeenAt: null,
      },
    },
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
  function mount(over: Partial<SessionOrgBridgeDeps> = {}) {
    const deps: SessionOrgBridgeDeps = {
      org: { phase: "known", org, pending: 0, lastError: null },
      rows: [tabRow("a"), tabRow("b")],
      sessionKeyOf: (s) => `key-${s.sessionId}`,
      applyOrg: vi.fn(() => Promise.resolve()),
      closeSessions: vi.fn(() => Promise.resolve([])),
      moveSessionsToPage: vi.fn(() => Promise.resolve([])),
      ...over,
    };
    function Probe() {
      useSessionOrgBridge(deps);
      return null;
    }
    renderToStaticMarkup(createElement(Probe));
    const c = components.find((x) => x.id === SESSION_ORG_COMPONENT_ID)!;
    const action = (id: string) => c.actions.find((a) => a.id === id)!;
    return { deps, c, action };
  }

  it("declares close and close-group destructive, list-groups read, the rest write", () => {
    const { c } = mount();
    const byEffect = (e: string) =>
      c.actions
        .filter((a) => a.effect === e)
        .map((a) => a.id)
        .sort();
    expect(byEffect("destructive")).toEqual(["close", "close-group"]);
    expect(byEffect("read")).toEqual(["list-groups"]);
    expect(c.actions.every((a) => ["read", "write", "destructive"].includes(a.effect))).toBe(true);
  });

  it("close-group closes exactly the group's rows, through closeSessions", async () => {
    const { deps, action } = mount();
    await action("close-group").handler({ groupId: "g1" });
    expect(deps.closeSessions).toHaveBeenCalledWith([deps.rows[0]]);
  });

  it("create-group files the named rows by their store keys", async () => {
    const { deps, action } = mount();
    await action("create-group").handler({ title: "Mine", sessionIds: "a, b" });
    expect(deps.applyOrg).toHaveBeenCalledWith([
      { op: "createGroup", title: "Mine", sessionKeys: ["key-a", "key-b"] },
    ]);
  });

  it("refuses an unknown row or group rather than guessing", () => {
    const { action } = mount();
    expect(() => action("close").handler({ sessionIds: "nope" })).toThrow(/No Live row/);
    expect(() => action("delete-group").handler({ groupId: "nope" })).toThrow(/No group/);
  });
});

describe("panel layout, drag payload, snooze presets", () => {
  it("clamps and parses the persisted layout", () => {
    expect(parsePanelLayout(null)).toEqual({ width: PANEL_DEFAULT_WIDTH, rail: false });
    expect(parsePanelLayout({ width: 10, rail: true })).toEqual({
      width: PANEL_MIN_WIDTH,
      rail: true,
    });
    expect(parsePanelLayout({ width: 9999, rail: "yes" })).toEqual({
      width: PANEL_MAX_WIDTH,
      rail: false,
    });
    expect(clampPanelWidth(Number.NaN)).toBe(PANEL_DEFAULT_WIDTH);
  });

  it("round-trips the drag payload and rejects anything else", () => {
    const p = { kind: "group" as const, groupId: "g1", tabIds: ["t1"] };
    expect(decodeSessionOrgDrag(encodeSessionOrgDrag(p))).toEqual(p);
    expect(decodeSessionOrgDrag("not json")).toBeNull();
    expect(decodeSessionOrgDrag('{"kind":"session"}')).toBeNull();
    expect(isSessionOrgDrag([SESSION_ORG_DRAG_MIME, "text/plain"])).toBe(true);
    expect(isSessionOrgDrag(["text/plain"])).toBe(false);
  });

  it("offers one hour, four hours and tomorrow 09:00", () => {
    const now = new Date("2026-10-13T10:00:00").getTime();
    const p = snoozePresets(now);
    expect(p.map((x) => x.until - now).slice(0, 2)).toEqual([3_600_000, 14_400_000]);
    expect(new Date(p[2].until).getHours()).toBe(9);
    expect(new Date(p[2].until).getDate()).toBe(14);
  });
});

describe("Suggest groups inputs", () => {
  it("6 ungrouped sessions across 2 plans → exactly 2 suggestions", () => {
    const fleet = fleetFactsBySessionId({
      sessions: ["s1", "s2", "s3", "s4", "s5", "s6", "remote"].map((id, i) => ({
        sessionId: `row-${id}`,
        deviceId: "d",
        isCallerDevice: id !== "remote",
        deviceHostname: null,
        deviceDisplayName: null,
        claudeCodeSessionId: id,
        sessionKind: null,
        intent: null,
        state: null,
        sessionStatus: null,
        workUnitSlug: i < 3 ? "2026-10-13-plan-one" : "2026-10-13-plan-two",
        repo: "qontinui-runner",
        branch: null,
        provider: null,
        correlationTopic: null,
        startedAt: null,
        lastHeartbeatAt: null,
        closedAt: null,
      })),
    });
    expect(fleet.has("remote")).toBe(false);
    const inputs = buildSuggestionSessions(
      ["s1", "s2", "s3", "s4", "s5", "s6"].map((id) => ({
        sessionKey: id,
        title: id,
        claudeSessionId: id,
        workingDir: "/w/qontinui-runner",
      })),
      fleet,
    );
    const out = suggestGroups(inputs, { groups: [], members: {}, dismissedSuggestions: [] });
    expect(out.map((s) => [s.keyKind, s.members.length])).toEqual([
      ["plan", 3],
      ["plan", 3],
    ]);
  });

  it("falls back to the working dir's repo when the fleet has no row", () => {
    expect(repoFromWorkingDir("C:\\code\\agent-worktrees\\x\\qontinui-coord\\")).toBe(
      "qontinui-coord",
    );
    expect(repoFromWorkingDir("")).toBeNull();
    const [only] = buildSuggestionSessions(
      [{ sessionKey: "k", title: "t", claudeSessionId: null, workingDir: "/w/qontinui-web" }],
      new Map(),
    );
    expect(only).toEqual({
      sessionKey: "k",
      title: "t",
      planSlug: null,
      repo: "qontinui-web",
      branch: null,
    });
  });
});
