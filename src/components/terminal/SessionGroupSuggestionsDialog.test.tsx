/**
 * The "Suggest groups" preview: what each row shows, and that every component
 * action hands `onApply` the exact batch the pure module built.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library (and `jsdom` is not installed in the shared `node_modules`), so — as
 * `HiddenWorkersChip` does — markup is locked with `renderToStaticMarkup`.
 * The UI Bridge SDK is replaced by a recorder, so the component actions and
 * their declared effects are driven directly; the buttons call the same
 * module functions (`acceptOps`, `dismissOp`, `acceptAllOps`).
 */

import type { ReactNode } from "react";
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
  UIBridgeComponentScope: ({ children }: { children: ReactNode }) => children,
}));

import {
  SESSION_GROUP_SUGGESTIONS_COMPONENT_ID,
  SESSION_GROUP_SUGGESTIONS_IDS,
  SessionGroupSuggestionsDialog,
  sessionGroupSuggestionAcceptId,
  sessionGroupSuggestionDismissId,
} from "./SessionGroupSuggestionsDialog";
import { suggestGroups, type GroupSuggestion } from "./sessionGroupSuggestions";
import type { SessionOrgOp } from "./sessionOrgTypes";

beforeEach(() => {
  components.length = 0;
});

const SUGGESTIONS: GroupSuggestion[] = suggestGroups(
  [
    { sessionKey: "a", title: "Alpha session", planSlug: "2026-10-13-alpha-plan" },
    { sessionKey: "b", title: "Beta session", planSlug: "2026-10-13-alpha-plan" },
    { sessionKey: "c", title: "Gamma session", repo: "qontinui-runner" },
    { sessionKey: "d", title: "Delta session", repo: "qontinui-runner" },
  ],
  { groups: [], members: {}, dismissedSuggestions: [] },
);

function render(suggestions: readonly GroupSuggestion[], onApply = vi.fn(), open = true) {
  const html = renderToStaticMarkup(
    <SessionGroupSuggestionsDialog
      open={open}
      suggestions={suggestions}
      onApply={onApply}
      onClose={() => {}}
    />,
  );
  return { html, onApply };
}

/** The markup of one suggestion row, from its opening `<li` to the next. */
function rows(html: string): string[] {
  return html.split("<li data-suggestion-id=").slice(1);
}

function lastActions() {
  const c = components.filter((x) => x.id === SESSION_GROUP_SUGGESTIONS_COMPONENT_ID).at(-1);
  if (!c) throw new Error("component not registered");
  return Object.fromEntries(c.actions.map((a) => [a.id, a]));
}

describe("SessionGroupSuggestionsDialog", () => {
  it("renders nothing and registers nothing while closed", () => {
    expect(render(SUGGESTIONS, vi.fn(), false).html).toBe("");
    expect(components).toEqual([]);
  });

  it("shows each suggestion's name, color role, key and member titles", () => {
    const { html } = render(SUGGESTIONS);
    expect(html).toContain(`data-ui-bridge-id="${SESSION_GROUP_SUGGESTIONS_IDS.root}"`);
    expect(html).toContain('data-suggestion-count="2"');
    const [plan, repo] = rows(html);
    expect(plan).toMatch(/^"plan:2026-10-13-alpha-plan"/);
    expect(plan).toContain('data-color-role="brand-primary"');
    expect(plan).toContain("var(--color-brand-primary)");
    expect(plan).toContain(">alpha-plan<");
    expect(plan).toContain("by plan: 2026-10-13-alpha-plan");
    expect(plan).toContain("Alpha session");
    expect(plan).toContain("Beta session");
    expect(repo).toMatch(/^"repo:qontinui-runner"/);
    expect(repo).toContain('data-color-role="brand-secondary"');
    expect(repo).toContain("Gamma session");
  });

  it("gives every button its stable UI Bridge id", () => {
    const { html } = render(SUGGESTIONS);
    for (const id of [
      SESSION_GROUP_SUGGESTIONS_IDS.acceptAll,
      SESSION_GROUP_SUGGESTIONS_IDS.close,
      ...SUGGESTIONS.flatMap((s) => [
        sessionGroupSuggestionAcceptId(s.id),
        sessionGroupSuggestionDismissId(s.id),
      ]),
    ]) {
      expect(html).toContain(`data-ui-bridge-id="${id}"`);
    }
  });

  it("declares an effect on every component action and acts by suggestion id", () => {
    const { onApply } = render(SUGGESTIONS);
    const actions = lastActions();
    expect(Object.fromEntries(Object.values(actions).map((a) => [a.id, a.effect]))).toEqual({
      "list-suggestions": "read",
      "accept-suggestion": "write",
      "dismiss-suggestion": "write",
      "accept-all": "write",
    });
    actions["accept-suggestion"].handler({ id: "repo:qontinui-runner" });
    actions["dismiss-suggestion"].handler({ id: "plan:2026-10-13-alpha-plan" });
    actions["accept-all"].handler();
    expect(onApply.mock.calls.map((c) => c[0] as SessionOrgOp[])).toEqual([
      SUGGESTIONS[1].acceptOps,
      [{ op: "dismissSuggestion", key: "plan:2026-10-13-alpha-plan" }],
      [...SUGGESTIONS[0].acceptOps, ...SUGGESTIONS[1].acceptOps],
    ]);
    expect((actions["list-suggestions"].handler() as { id: string }[]).map((s) => s.id)).toEqual([
      "plan:2026-10-13-alpha-plan",
      "repo:qontinui-runner",
    ]);
  });

  it("refuses an unknown suggestion id and an undeclared key without applying", () => {
    const { onApply } = render(SUGGESTIONS);
    const actions = lastActions();
    expect(() => actions["dismiss-suggestion"].handler({ id: "repo:nope" })).toThrow(
      /No suggestion "repo:nope"/,
    );
    expect(() =>
      actions["accept-suggestion"].handler({ id: "repo:qontinui-runner", extra: 1 }),
    ).toThrow();
    expect(onApply).not.toHaveBeenCalled();
  });

  it("says why the list is empty, and Accept all is disabled and inert", () => {
    const { html, onApply } = render([]);
    expect(html).toContain("No two ungrouped sessions share a plan, pull request, branch or repo.");
    expect(html).toMatch(
      /<button[^>]*data-ui-bridge-id="terminal\.session-group-suggestions-accept-all"[^>]*disabled=""/,
    );
    lastActions()["accept-all"].handler();
    expect(onApply).not.toHaveBeenCalled();
  });
});
