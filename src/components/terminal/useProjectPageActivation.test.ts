/**
 * Tests for the pure halves of the project → terminal-page activation
 * (projects-dashboard plan §7.2 steps 1–3).
 *
 * vitest runs `environment: "node"` with no DOM, so the hook itself is not
 * renderable here; the load-bearing decisions are exported as pure functions
 * and exercised directly (same precedent as `resolveProjectPage` in
 * `useTerminalPages.test.ts` and `deriveCardStatus` in `ProjectCard.tsx`).
 *
 * The page-resolution behaviour those decisions feed — dangling-id tolerance,
 * `defaultWorkingDir` application — is already covered by
 * `useTerminalPages.test.ts`; what is new here is which id we hand it and
 * whether the result is written back to `settings.json`.
 */

import { describe, it, expect } from "vitest";

import {
  planProjectActivation,
  shouldPersistBinding,
  type ProjectActivationPlan,
} from "./useProjectPageActivation";

const NO_BINDINGS: ReadonlyMap<string, string> = new Map();

function plan(over: Partial<ProjectActivationPlan> = {}): ProjectActivationPlan {
  return { projectId: "p1", projectName: "Pizzeria", ...over };
}

describe("planProjectActivation", () => {
  it("carries the project root through as the page's default cwd", () => {
    const result = planProjectActivation(
      { id: "p1", name: "Papa's Pizzeria", path: "D:\\projects\\pizza" },
      NO_BINDINGS,
    );
    expect(result).toEqual({
      projectId: "p1",
      projectName: "Papa's Pizzeria",
      projectRoot: "D:\\projects\\pizza",
      boundPageId: undefined,
      zoneProfile: undefined,
    });
  });

  it("returns null for a deactivation", () => {
    expect(planProjectActivation(null, NO_BINDINGS)).toBeNull();
  });

  it("returns null for a hint with no id — there would be nothing to bind to", () => {
    // Minting a page for an id-less hint would leak an unreachable tab on
    // every click, because the resolved binding could never be persisted.
    expect(planProjectActivation({ name: "Pizzeria", path: "D:\\p" }, NO_BINDINGS)).toBeNull();
    expect(planProjectActivation({ id: "   ", path: "D:\\p" }, NO_BINDINGS)).toBeNull();
  });

  it("prefers the persisted terminalPageId over a session-resolved one", () => {
    const bindings = new Map([["p1", "session-page"]]);
    const result = planProjectActivation({ id: "p1", terminalPageId: "stored-page" }, bindings);
    expect(result?.boundPageId).toBe("stored-page");
  });

  it("falls back to the session binding when the hint's list is stale", () => {
    // The regression this map exists for: the Projects page's in-memory list
    // is not re-read after we persist a freshly minted binding, so a second
    // "Work on it" would otherwise mint a SECOND page for the same project.
    const bindings = new Map([["p1", "minted-page"]]);
    expect(planProjectActivation({ id: "p1" }, bindings)?.boundPageId).toBe("minted-page");
  });

  it("does not cross-contaminate: another project's session binding is not reused", () => {
    const bindings = new Map([["p2", "p2-page"]]);
    expect(planProjectActivation({ id: "p1" }, bindings)?.boundPageId).toBeUndefined();
  });

  it("treats blank name/path/profile as absent and names an unnamed project", () => {
    const result = planProjectActivation(
      { id: "p1", name: "  ", path: "  ", terminalPageId: " ", zoneProfile: " " },
      NO_BINDINGS,
    );
    expect(result).toEqual({
      projectId: "p1",
      projectName: "Project",
      projectRoot: undefined,
      boundPageId: undefined,
      zoneProfile: undefined,
    });
  });

  it("trims the fields it passes on", () => {
    const result = planProjectActivation(
      { id: " p1 ", name: " Pizzeria ", path: " D:\\p ", zoneProfile: " focus " },
      NO_BINDINGS,
    );
    expect(result).toEqual({
      projectId: "p1",
      projectName: "Pizzeria",
      projectRoot: "D:\\p",
      boundPageId: undefined,
      zoneProfile: "focus",
    });
  });
});

describe("shouldPersistBinding", () => {
  it("persists a freshly minted page id (the project had no binding)", () => {
    expect(shouldPersistBinding(plan(), "minted-1")).toBe(true);
  });

  it("writes nothing when a DANGLING id resolved back to itself", () => {
    // `resolveProjectPage` re-creates a missing page UNDER THE SAME ID rather
    // than minting a new one, so the cleared-localStorage case — the common
    // one — must not generate a settings.json write on every activation.
    expect(shouldPersistBinding(plan({ boundPageId: "page-a" }), "page-a")).toBe(false);
  });

  it("persists when the resolution moved the project to a different page", () => {
    expect(shouldPersistBinding(plan({ boundPageId: "page-a" }), "page-b")).toBe(true);
  });

  it("never persists an empty resolved id", () => {
    expect(shouldPersistBinding(plan(), "")).toBe(false);
  });
});
