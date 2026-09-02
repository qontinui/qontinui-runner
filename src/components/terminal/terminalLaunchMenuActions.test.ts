/**
 * The four `terminal-launch-menu` actions, driven with BOTH effects spied.
 *
 * Iteration 12 of the manual-test loop recorded that there was no test file
 * for these handlers at all, and called that "the defect behind the defect".
 * Three of the four then answered the same malformed input — a bare `5` —
 * three different ways, and one of them spent money doing it:
 *
 *     create-ai-session(5)   → throws, wire [], 0 created
 *     create-plain(5)        → success: true, wire ['terminal_create'], live 0 → 1
 *     create-best-account(5) → success: true, wire ['terminal_create',
 *                              'build_ai_launch_command', 'terminal_write'],
 *                              the write being `claude --session-id … --config-dir …`
 *
 * Every assertion below is on the EFFECT COUNT, because a refusal that throws
 * after spawning is the failure, not the fix.
 */

import { describe, it, expect, vi } from "vitest";
import { buildTerminalLaunchMenuActions } from "./terminalLaunchMenuActions";

function harness() {
  const callRegistry = vi.fn(async () => ["tab-1"] as unknown as never);
  const launchAiSession = vi.fn(async () => ["tab-1"]);
  const actions = buildTerminalLaunchMenuActions({
    callRegistry: callRegistry as never,
    launchAiSession,
  });
  const byId = Object.fromEntries(actions.map((a) => [a.id, a]));
  /** Every effect either handler could reach, in one place. */
  const wire = () => [
    ...callRegistry.mock.calls.map((c) => (c as unknown[])[0]),
    ...launchAiSession.mock.calls.map(() => "launchAiSession"),
  ];
  return { callRegistry, launchAiSession, byId, wire, ids: actions.map((a) => a.id) };
}

/** The four ids this menu has always published. Renaming one breaks agents. */
const IDS = ["create-plain", "create-ai-session", "create-best-account", "create-with-command"];

/**
 * Bags no launch action can accept.
 *
 * `5` first, because it is the one the twelfth round measured three different
 * answers to.
 */
const MALFORMED: Array<[string, unknown]> = [
  ["a number", 5],
  ["a string", "zz"],
  ["an empty list", []],
  ["a populated list", ["a"]],
  ["true", true],
  ["an undeclared key", { zzz: "x" }],
  ["count as an object", { count: {} }],
  ["count as a list", { count: [] }],
  ["a valid count plus an undeclared key", { count: 1, zzz: "x" }],
];

describe("terminal launch menu", () => {
  it("publishes exactly the four wire ids, each with a paramSchema", () => {
    const { ids, byId } = harness();
    expect(ids).toEqual(IDS);
    for (const id of IDS) expect(Object.keys(byId[id].paramSchema ?? {}).length).toBeGreaterThan(0);
  });

  for (const id of IDS) {
    it(`${id} refuses every malformed bag with a COMPLETELY EMPTY effect wire`, async () => {
      for (const [label, bag] of MALFORMED) {
        const { byId, wire } = harness();
        await expect(
          Promise.resolve().then(() => byId[id].handler(bag)),
          `${id} / ${label}`,
        ).rejects.toThrow();
        expect(wire(), `${id} / ${label} reached an effect`).toEqual([]);
      }
    });
  }

  it("create-plain: `5` throws with an empty wire — the same answer as its sibling", async () => {
    const { byId, wire, callRegistry } = harness();
    await expect(Promise.resolve().then(() => byId["create-plain"].handler(5))).rejects.toThrow(
      "create-plain: arguments must be an object (got number)",
    );
    expect(wire()).toEqual([]);
    // …and still spawns for a real bag.
    await byId["create-plain"].handler({ count: 2 });
    expect(callRegistry).toHaveBeenCalledWith("terminal.spawn", { count: 2 });
  });

  it("create-best-account: `5` costs nothing (no spawn, no launch command, no write)", async () => {
    const { byId, wire } = harness();
    await expect(
      Promise.resolve().then(() => byId["create-best-account"].handler(5)),
    ).rejects.toThrow("create-best-account: arguments must be an object (got number)");
    expect(wire()).toEqual([]);
  });

  it("create-with-command: an undeclared key is refused, not dropped", async () => {
    const { byId, wire } = harness();
    await expect(
      Promise.resolve().then(() =>
        byId["create-with-command"].handler({ command: "echo pwn", zzz: "x" }),
      ),
    ).rejects.toThrow('create-with-command: takes no argument named "zzz"');
    expect(wire()).toEqual([]);
  });

  it("create-ai-session: a non-scalar context never reaches the spawn", async () => {
    const { byId, launchAiSession } = harness();
    await expect(
      Promise.resolve().then(() =>
        byId["create-ai-session"].handler({ configDir: "/c", context: {} }),
      ),
    ).rejects.toThrow('create-ai-session: "context" must be text or a number (got an object)');
    expect(launchAiSession).not.toHaveBeenCalled();
  });

  it("keeps every operator-facing sentence automation already matches", async () => {
    const { byId } = harness();
    await expect(
      Promise.resolve().then(() => byId["create-plain"].handler({ count: 0 })),
    ).rejects.toThrow("create-plain requires { count: number } where count >= 1");
    await expect(
      Promise.resolve().then(() => byId["create-ai-session"].handler({})),
    ).rejects.toThrow(
      "create-ai-session requires { count?: number, configDir: string, context?: string }",
    );
    await expect(
      Promise.resolve().then(() => byId["create-with-command"].handler({})),
    ).rejects.toThrow("create-with-command requires { count?: number, command: string }");
    await expect(
      Promise.resolve().then(() => byId["create-best-account"].handler({ count: 0 })),
    ).rejects.toThrow("create-best-account: count must be a positive number");
  });

  it("create-best-account rethrows a registry no-account as the historical wording", async () => {
    const callRegistry = vi.fn(async () => {
      throw new Error("no-account");
    });
    const [, , best] = buildTerminalLaunchMenuActions({
      callRegistry: callRegistry as never,
      launchAiSession: async () => [],
    });
    await expect(Promise.resolve().then(() => best.handler({ count: 1 }))).rejects.toThrow(
      "No AI accounts available",
    );
  });

  it("reads a numeric-looking command back as text", async () => {
    // Binding coerces `"5"` to the number 5; `textArg` restores it. Skipping
    // that is how `/spawn-with 2 5` once reported "command is required" for a
    // command that was supplied.
    const { byId, callRegistry } = harness();
    await byId["create-with-command"].handler({ count: 1, command: "5" });
    expect(callRegistry).toHaveBeenCalledWith("terminal.spawn-with", { count: 1, command: "5" });
  });
});
