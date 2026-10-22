/**
 * `/prepare-restart` and `/cancel-restart` over the REAL registry
 * (`realRegistry.testkit`), on both arms of the backend's answer (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 5).
 *
 * Pinned: the prepare reaches `restart_prep_prepare` with the operator's
 * reason and reports the hold the backend read back; a refusal is a failure
 * carrying the backend's words, never a `✓`; `/prepare-restart cancel` is the
 * cancel, not a prepare with the reason "cancel"; and the panel opens.
 */
import { beforeAll, describe, expect, it } from "vitest";

import { loadRealRegistry, type RealRegistryHarness } from "./realRegistry.testkit";
import type { CommandResult } from "./types";
import { describeReport, isEffectReport } from "./verdict";

let h: RealRegistryHarness;

beforeAll(async () => {
  h = await loadRealRegistry();
}, 60_000);

const run = (id: string, args: Record<string, unknown> = {}): Promise<CommandResult> =>
  h.byId(id).handler(args, { source: "test" });

const invokes = (): Array<[string, unknown]> =>
  h.calls.filter((c) => c.name === "invoke").map((c) => [c.args[0] as string, c.args[1]]);

/** A successful verdict's status text (a failure is asserted as an object). */
const text = (r: CommandResult): string =>
  r.ok && isEffectReport(r.value) ? describeReport(r.value) : JSON.stringify(r);

describe("/prepare-restart", () => {
  it("prepares with the reason, reports the hold, and opens the panel", async () => {
    h.resetArms();
    h.reset();
    const { restartPrepStore } = await import("../restartPrep");
    restartPrepStore.closePanel();
    const r = await run("terminal.prepare-restart", { reason: "rebuild for #2054" });
    expect(r.ok).toBe(true);
    expect(text(r)).toBe("started the restart prepare — hold confirmed; 2 sessions recorded");
    expect(invokes()).toEqual([["restart_prep_prepare", { reason: "rebuild for #2054" }]]);
    expect(restartPrepStore.getSnapshot().panelOpen).toBe(true);
    expect(restartPrepStore.getSnapshot().record?.prepare_id).toBe("prep-1");
  });

  it("fails with the backend's refusal, never a ✓", async () => {
    h.resetArms();
    h.setArms({ restartPrep: "refused" });
    h.reset();
    try {
      const r = await run("terminal.prepare-restart");
      expect(r).toEqual({
        ok: false,
        code: "prepare-failed",
        message: "restart_prep cohort task failed: lifecycle store not available",
      });
    } finally {
      h.resetArms();
    }
  });

  it("`/prepare-restart cancel` cancels instead of preparing", async () => {
    h.resetArms();
    h.reset();
    const r = await run("terminal.prepare-restart", { reason: "Cancel" });
    expect(r.ok).toBe(true);
    expect(text(r)).toBe("cancelled the restart prepare — hold release: released");
    expect(invokes().map(([cmd]) => cmd)).toEqual(["restart_prep_cancel"]);
  });
});

describe("/cancel-restart", () => {
  it("reports the release on the ok arm and the refusal on the other", async () => {
    h.resetArms();
    h.reset();
    const okRun = await run("terminal.cancel-restart-prep");
    expect(text(okRun)).toBe("cancelled the restart prepare — hold release: released");
    h.setArms({ restartPrep: "refused" });
    try {
      const refused = await run("terminal.cancel-restart-prep");
      expect(refused).toEqual({
        ok: false,
        code: "cancel-failed",
        message: "no prepare is in progress — nothing to cancel",
      });
    } finally {
      h.resetArms();
    }
  });
});
