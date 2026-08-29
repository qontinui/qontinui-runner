/**
 * `/spawn`, `/spawn-with` and `/spawn-ai` must report what they actually
 * created.
 *
 * The three handlers used to end in `ok(Array.isArray(result) ? result :
 * [])`, so a spawn that created NOTHING — the page's spawn closure
 * bailing, or resolving to `void` — still rendered `✓` in the command
 * bar's status line. That is the same false-success class as the
 * `/generate` bug: a verdict derived from "the call returned" instead of
 * from what the call produced.
 *
 * Runs under vitest's `environment: "node"` (no jsdom), so this exercises
 * the exported pure helper rather than the hook — same split as
 * `spawnTenant.test.ts`.
 */

import { describe, expect, it } from "vitest";

import { spawnVerdict } from "./useTerminalCommands";

const id = (n: number) => `tab-${n}`;

describe("spawnVerdict", () => {
  it("succeeds when every requested terminal was created", () => {
    expect(spawnVerdict([id(1), id(2)], 2)).toEqual({ ok: true, value: [id(1), id(2)] });
    expect(spawnVerdict([id(1)], 1)).toEqual({ ok: true, value: [id(1)] });
  });

  it("FAILS when nothing was created", () => {
    const verdict = spawnVerdict([], 2);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error("unreachable");
    expect(verdict.code).toBe("spawn-failed");
    expect(verdict.message).toContain("0 of 2");
  });

  it("FAILS when the spawn closure returned void (no array at all)", () => {
    // `spawnPlain` / `spawnAi` are typed `Promise<string[] | void>`; the
    // void arm is the "bailed before creating anything" case, and it was
    // the one the old `Array.isArray(...) ? ... : []` fallback laundered
    // into an empty success.
    const verdict = spawnVerdict(undefined, 1);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error("unreachable");
    expect(verdict.code).toBe("spawn-failed");
  });

  it("FAILS on a PARTIAL spawn — the grid won't match what was asked for", () => {
    const verdict = spawnVerdict([id(1)], 3);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error("unreachable");
    expect(verdict.message).toContain("1 of 3");
  });

  it("names what was being spawned so /spawn-ai's error reads correctly", () => {
    const verdict = spawnVerdict([], 2, "AI sessions");
    if (verdict.ok) throw new Error("unreachable");
    expect(verdict.message).toContain("AI sessions");
  });

  it("does not fail a spawn that over-delivers", () => {
    // Defensive: the closure is the authority on how many it made, and a
    // longer array is still every terminal the operator asked for.
    expect(spawnVerdict([id(1), id(2), id(3)], 2).ok).toBe(true);
  });
});
