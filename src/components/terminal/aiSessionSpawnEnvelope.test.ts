/**
 * `create-ai-session` must not answer `success: true` for a spawn that produced
 * fewer sessions than asked for.
 *
 * It is the one launch-menu action that bypasses the registry — it takes a raw
 * `configDir` rather than an account label — so it never reached the
 * `spawnVerdict` that `callRegistry` applies to every sibling. `#1169` widened
 * that from a partial-delivery gap into a total one: a launch-spec build that
 * throws now disposes its own tab and is SKIPPED, so a launch in which EVERY
 * build failed resolves with `[]` instead of rejecting, and a headless caller
 * saw `success: true`, zero tabs, and no reason it could read.
 *
 * vitest runs `environment: "node"`, so this exercises the extracted pure
 * envelope rather than the component — the `buildCreatePlainTerminalAction`
 * precedent.
 */

import { describe, it, expect } from "vitest";
import { buildAiSessionSpawnEnvelope } from "./aiSessionSpawnEnvelope";

describe("buildAiSessionSpawnEnvelope", () => {
  it("answers the wire envelope when every requested session launched", () => {
    expect(buildAiSessionSpawnEnvelope(["tab-1", "tab-2"], 2)).toEqual({
      success: true,
      tab_ids: ["tab-1", "tab-2"],
      task_run_ids: [null, null],
    });
  });

  it("throws when NOTHING launched — the shape #1169 made reachable", () => {
    expect(() => buildAiSessionSpawnEnvelope([], 3)).toThrow(/3/);
    // …and it is a throw, not a falsy envelope: `callRegistry`'s siblings
    // reject, and the two wire ids are documented to collapse onto one handler.
    expect(() => buildAiSessionSpawnEnvelope([], 1)).toThrow();
  });

  it("throws on a PARTIAL launch too, exactly as the registry siblings do", () => {
    expect(() => buildAiSessionSpawnEnvelope(["tab-1"], 3)).toThrow(/1 of 3/);
  });

  /**
   * `handleLaunchAiSession` is typed to return `string[] | void`; a `void`
   * result is zero produced, never "assume it worked".
   */
  it("treats a void result as zero produced", () => {
    expect(() => buildAiSessionSpawnEnvelope(undefined, 1)).toThrow();
  });

  /** Over-delivery is not a failure — the verdict gates on falling short. */
  it("accepts more tabs than requested rather than failing them", () => {
    const envelope = buildAiSessionSpawnEnvelope(["tab-1", "tab-2", "tab-3"], 2);
    expect(envelope.success).toBe(true);
    expect(envelope.tab_ids).toHaveLength(3);
    expect(envelope.task_run_ids).toEqual([null, null, null]);
  });
});
