/**
 * D1 — durable command-result record.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom), so
 * per repo precedent (`MidSessionToast.test.tsx`,
 * `CommitTrafficLight.test.tsx`) we test the load-bearing logic via the
 * exported pure helper rather than rendering JSX. The helper produces the
 * exact record that CommandBar mirrors into the hidden
 * `data-page-element="command-bar-result"` node — the signal a headless
 * UI-Bridge driver reads to learn whether, and why, a command did its
 * job.
 *
 * The goal-blocker case: `/spawn-ai best` returns `fail("no-account")`
 * when no Claude account is configured. Before D1 that surfaced only in a
 * transient 3s status line; now it's a durable, observable record.
 */

import { describe, it, expect } from "vitest";

import { buildCommandResultRecord } from "./CommandBar";
import type { CommandResult } from "./commands";

const spawnAi = { id: "terminal.spawn-ai", slash: "/spawn-ai" };

describe("buildCommandResultRecord", () => {
  it("records a durable no-account failure for /spawn-ai best", () => {
    const result: CommandResult = {
      ok: false,
      code: "no-account",
      message: "no matching Claude account",
    };
    const rec = buildCommandResultRecord(spawnAi, result, "ai", 1234);

    // This is precisely what the headless driver reads back as JSON. The
    // failure is observable instead of being swallowed by the 3s status
    // line — so "click returned success but nothing spawned" now has a
    // legible reason attached.
    expect(rec).toEqual({
      ok: false,
      slash: "/spawn-ai",
      actionId: "terminal.spawn-ai",
      source: "ai",
      code: "no-account",
      message: "no matching Claude account",
      at: 1234,
    });
  });

  it("omits code/message on success and stamps the time", () => {
    const result: CommandResult = { ok: true, value: ["tab-1"] };
    const rec = buildCommandResultRecord(spawnAi, result, "slash", 42);

    expect(rec.ok).toBe(true);
    expect(rec.code).toBeUndefined();
    expect(rec.message).toBeUndefined();
    expect(rec.slash).toBe("/spawn-ai");
    expect(rec.source).toBe("slash");
    expect(rec.at).toBe(42);
  });

  it("serializes to JSON the driver can parse and discriminate on", () => {
    const fail = buildCommandResultRecord(
      spawnAi,
      { ok: false, code: "no-account" },
      "ai",
      7,
    );
    const parsed = JSON.parse(JSON.stringify(fail));
    expect(parsed.ok).toBe(false);
    expect(parsed.code).toBe("no-account");
  });
});
