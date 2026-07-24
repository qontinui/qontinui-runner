/**
 * Tests for the dynamic per-prompt slash registration
 * (`registerPromptActions`) — the pure core of
 * `usePromptLibraryCommands`, following the node-environment precedent
 * (no JSX/hook rendering; the React glue is a one-line effect).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { PromptTemplate } from "../promptLibraryApi";
import { __resetForTest, getAll, getById, getBySlash, register } from "./registry";
import type { CommandResult } from "./types";
import {
  registerPromptActions,
  type PromptLibraryCommandsContext,
} from "./usePromptLibraryCommands";

function prompt(overrides: Partial<PromptTemplate> & { name: string }): PromptTemplate {
  return {
    title: overrides.name,
    description: "",
    category: "General",
    default_action: "spawn",
    version: 1,
    parameters: [],
    body: "body",
    ...overrides,
  };
}

function makeCtx(overrides: Partial<PromptLibraryCommandsContext> = {}) {
  const calls = {
    opened: [] as Array<string | undefined>,
    spawned: [] as string[],
    inserted: [] as string[],
  };
  const ctx: PromptLibraryCommandsContext = {
    prompts: [],
    openPromptModal: (slug) => calls.opened.push(slug),
    spawnWithText: (text) => {
      calls.spawned.push(text);
    },
    insertIntoFocused: (text) => {
      calls.inserted.push(text);
      return true;
    },
    ...overrides,
  };
  return { ctx, calls };
}

beforeEach(() => {
  __resetForTest();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("registerPromptActions", () => {
  it("registers one /slug action per prompt and disposes them all", () => {
    const prompts = [prompt({ name: "ui-bridge-new-project" }), prompt({ name: "test-fix-loop" })];
    const { ctx } = makeCtx({ prompts });
    const dispose = registerPromptActions(prompts, () => ctx);

    expect(getById("prompt.ui-bridge-new-project")).toBeDefined();
    expect(getBySlash("/test-fix-loop")?.id).toBe("prompt.test-fix-loop");
    expect(getAll()).toHaveLength(2);

    dispose();
    expect(getAll()).toHaveLength(0);
    expect(getBySlash("/ui-bridge-new-project")).toBeUndefined();
  });

  it("skips a slug colliding with an existing slash and warns naming the shadower", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    register({
      id: "terminal.help",
      slash: "/help",
      label: "Help",
      description: "",
      handler: async (): Promise<CommandResult> => ({ ok: true }),
    });

    const prompts = [prompt({ name: "help" }), prompt({ name: "safe-slug" })];
    const { ctx } = makeCtx({ prompts });
    const dispose = registerPromptActions(prompts, () => ctx);

    // The collision is skipped — /help still resolves to the built-in.
    expect(getBySlash("/help")?.id).toBe("terminal.help");
    expect(getById("prompt.help")).toBeUndefined();
    // The non-colliding sibling still registered.
    expect(getById("prompt.safe-slug")).toBeDefined();
    // One warning naming the shadowing action.
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain("terminal.help");

    dispose();
    // Dispose removes only our registration, never the shadowing built-in.
    expect(getBySlash("/help")?.id).toBe("terminal.help");
  });

  it("also skips a slug colliding with an alias", () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    register({
      id: "terminal.doc-finder",
      slash: "/doc-finder",
      aliases: ["/docs"],
      label: "Docs",
      description: "",
      handler: async (): Promise<CommandResult> => ({ ok: true }),
    });
    const prompts = [prompt({ name: "docs" })];
    const { ctx } = makeCtx({ prompts });
    registerPromptActions(prompts, () => ctx);
    expect(getById("prompt.docs")).toBeUndefined();
  });

  it("a parameterless prompt runs its default action immediately (spawn)", async () => {
    const prompts = [prompt({ name: "test-fix-loop", body: "run the loop {{gone}}" })];
    const { ctx, calls } = makeCtx({ prompts });
    registerPromptActions(prompts, () => ctx);

    const result = await getBySlash("/test-fix-loop")!.handler({}, { source: "test" });
    expect(result.ok).toBe(true);
    // Stray placeholders blank out; nothing opened the modal.
    expect(calls.spawned).toEqual(["run the loop "]);
    expect(calls.opened).toEqual([]);
  });

  it("a parameterless insert-default prompt fails loudly with no focused session", async () => {
    const prompts = [prompt({ name: "ins", default_action: "insert" })];
    const { ctx, calls } = makeCtx({
      prompts,
      insertIntoFocused: () => false,
    });
    registerPromptActions(prompts, () => ctx);

    const result = await getBySlash("/ins")!.handler({}, { source: "test" });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.code).toBe("no-focused-session");
    expect(calls.spawned).toEqual([]);
  });

  it("a prompt with parameters opens the modal focused on it", async () => {
    const prompts = [
      prompt({
        name: "ui-bridge-new-project",
        parameters: [
          {
            name: "project_name",
            type: "string",
            label: "Project name",
            description: "",
            required: true,
          },
        ],
      }),
    ];
    const { ctx, calls } = makeCtx({ prompts });
    registerPromptActions(prompts, () => ctx);

    const result = await getBySlash("/ui-bridge-new-project")!.handler({}, { source: "test" });
    expect(result.ok).toBe(true);
    expect(calls.opened).toEqual(["ui-bridge-new-project"]);
    expect(calls.spawned).toEqual([]);
  });

  it("handlers read the LIVE context, not the registration-time snapshot", async () => {
    const initial = [prompt({ name: "loop" })];
    const { ctx: first } = makeCtx({ prompts: initial });
    let current = first;
    registerPromptActions(initial, () => current);

    // Simulate a refresh that updated the prompt's body in place.
    const { ctx: second, calls } = makeCtx({
      prompts: [prompt({ name: "loop", body: "fresh body" })],
    });
    current = second;

    await getBySlash("/loop")!.handler({}, { source: "test" });
    expect(calls.spawned).toEqual(["fresh body"]);
  });
});
