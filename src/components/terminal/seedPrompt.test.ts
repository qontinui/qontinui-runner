/**
 * The runner's vitest env is `node` (no `localStorage`), so we mock
 * `@/lib/instance-storage` with an in-memory JSON store mirroring its
 * `getJSON`/`setJSON` contract — the same convention `lastKnownSessionIds.test.ts`
 * uses. Without it the real wrapper swallows every write (`catch {}` on a
 * missing `localStorage`) and the replay guard would appear broken when it is
 * simply unexercised.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const mem = new Map<string, unknown>();

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON<T>(key: string, fallback: T): T {
      return mem.has(key) ? (mem.get(key) as T) : fallback;
    },
    setJSON(key: string, value: unknown): void {
      mem.set(key, value);
    },
  },
}));

import { instanceStorage } from "@/lib/instance-storage";
import { isSeedConsumed, markSeedConsumed, planSeedPrompt } from "./seedPrompt";
import type { ActiveProjectHint } from "./activeProject";

const CONSUMED_KEY = "qontinui-consumed-seed-prompts";

function hint(over: Partial<ActiveProjectHint> = {}): ActiveProjectHint {
  return { id: "p1", name: "Pizzeria", path: "D:/work/pizzeria", ...over };
}

beforeEach(() => {
  mem.clear();
});

describe("planSeedPrompt", () => {
  it("plans a spawn for a fresh seeded activation", () => {
    const plan = planSeedPrompt(hint({ seedPrompt: "fix the website", seedPromptId: "s1" }));
    expect(plan).toEqual({ prompt: "fix the website", id: "s1", projectRoot: "D:/work/pizzeria" });
  });

  it("returns null for an ordinary activation", () => {
    // Open / Work-on-it carry no seed, and that is the common case — they bind
    // a terminal and stop there. Spawning an agent for them would be a
    // surprise charge on the user's quota.
    expect(planSeedPrompt(hint())).toBeNull();
    expect(planSeedPrompt(null)).toBeNull();
    expect(planSeedPrompt(hint({ seedPrompt: "   ", seedPromptId: "s1" }))).toBeNull();
  });

  it("REFUSES a seed with no id rather than guessing one", () => {
    // An un-ided seed cannot be replay-guarded. Acting on it is precisely the
    // unbounded-spawn case this module exists to prevent, so it is dropped.
    expect(planSeedPrompt(hint({ seedPrompt: "fix it" }))).toBeNull();
  });

  it("does not replay a seed that was already acted on — the load-bearing guard", () => {
    const h = hint({ seedPrompt: "fix the website", seedPromptId: "s1" });

    // First delivery: act.
    expect(planSeedPrompt(h)).not.toBeNull();
    markSeedConsumed("s1");

    // The hint is CACHED in instanceStorage and re-delivered on every webview
    // reload and cross-window `storage` event. Without this guard, one click
    // would spawn a fresh agent on each of them.
    expect(planSeedPrompt(h)).toBeNull();
    expect(planSeedPrompt(h)).toBeNull();
    expect(isSeedConsumed("s1")).toBe(true);

    // A NEW click (new id) is a new request and must still work — the guard
    // suppresses replays, not repeat fixes.
    expect(planSeedPrompt(hint({ seedPrompt: "fix again", seedPromptId: "s2" }))).not.toBeNull();
  });

  it("keeps the consumed record bounded and retains the most recent ids", () => {
    for (let i = 0; i < 60; i++) markSeedConsumed(`id-${i}`);
    const stored = instanceStorage.getJSON<string[]>(CONSUMED_KEY, []);
    expect(stored.length).toBeLessThanOrEqual(50);
    // Most recent survive — only they can be replayed, since a replay always
    // carries the LAST hint.
    expect(isSeedConsumed("id-59")).toBe(true);
    expect(isSeedConsumed("id-0")).toBe(false);
  });

  it("tolerates a corrupt consumed record instead of throwing", () => {
    instanceStorage.setJSON(CONSUMED_KEY, { nope: true });
    expect(isSeedConsumed("s1")).toBe(false);
    expect(planSeedPrompt(hint({ seedPrompt: "fix", seedPromptId: "s1" }))).not.toBeNull();
  });
});
