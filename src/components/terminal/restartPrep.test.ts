/**
 * The restart-prep store and its pure readings (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 5).
 *
 * The store is driven through an injected `invoke` / `listen`, so these run
 * under `environment: "node"` with no Tauri.
 */
import { describe, expect, it } from "vitest";

import {
  createRestartPrepStore,
  READY_HEADLINE,
  RESTART_PREP_CHANGED_EVENT,
  restartPrepHeadline,
  rowStateChip,
  effortBadge,
  type Blocker,
  type PrepareRecord,
  type RestartPrepDeps,
} from "./restartPrep";
import { changed, record, row, verdict } from "./restartPrep.testkit";

const sessionBlocker = (id: string): Blocker => ({
  session: id,
  display_name: `name-${id}`,
  reason: "notified: told this tick",
});

describe("restartPrepHeadline — exactly one of three readings", () => {
  const read = (rec: PrepareRecord | null) =>
    restartPrepHeadline({ record: rec, readError: null, loaded: true });

  it("is 'Safe to restart' only when the backend's verdict is ready with no blocker", () => {
    expect(read(record({ verdict: verdict({ ready_to_restart: true }) }))).toEqual({
      kind: "ready",
      text: READY_HEADLINE,
    });
  });

  it("is 'Waiting on N sessions' naming each blocker", () => {
    const h = read(
      record({ verdict: verdict({ blockers: [sessionBlocker("a"), sessionBlocker("b")] }) }),
    );
    expect(h?.kind).toBe("waiting");
    expect(h?.text).toBe("Waiting on 2 sessions");
    expect(h?.kind === "waiting" && h.blockers.map((b) => b.session)).toEqual(["a", "b"]);
    expect(read(record({ verdict: verdict({ blockers: [sessionBlocker("a")] }) }))?.text).toBe(
      "Waiting on 1 session",
    );
  });

  it("is UNKNOWN for every unread or contradictory input, never a default", () => {
    expect(restartPrepHeadline({ record: null, readError: "boom", loaded: true })).toEqual({
      kind: "unknown",
      text: "UNKNOWN: the restart-prep record (boom)",
    });
    expect(restartPrepHeadline({ record: record(), readError: null, loaded: false })?.kind).toBe(
      "unknown",
    );
    // No verdict yet, or one about another prepare.
    expect(read(record())?.text).toMatch(/^UNKNOWN: the verdict/);
    expect(
      read(record({ verdict: verdict({ prepare_id: "old", ready_to_restart: true }) }))?.kind,
    ).toBe("unknown");
    // A prepare-wide blocker is an input the claim rests on: named, as UNKNOWN.
    const wide = read(
      record({
        verdict: verdict({
          blockers: [
            {
              session: null,
              display_name: null,
              reason: "the restart census could not be taken: x",
            },
            sessionBlocker("a"),
          ],
        }),
      }),
    );
    expect(wide).toEqual({
      kind: "unknown",
      text: "UNKNOWN: the restart census could not be taken: x",
    });
    // Self-contradicting verdicts.
    expect(
      read(
        record({ verdict: verdict({ ready_to_restart: true, blockers: [sessionBlocker("a")] }) }),
      )?.kind,
    ).toBe("unknown");
    expect(read(record({ verdict: verdict({ ready_to_restart: false }) }))?.kind).toBe("unknown");
  });

  it("headlines nothing when no prepare is in its window", () => {
    expect(read(null)).toBeNull();
    for (const state of ["restoring", "done", "cancelled"] as const) {
      expect(read(record({ state, verdict: verdict({ ready_to_restart: true }) }))).toBeNull();
    }
    expect(
      read(record({ state: "ready", verdict: verdict({ ready_to_restart: true }) }))?.kind,
    ).toBe("ready");
  });
});

describe("row chips read the backend's observation", () => {
  it("an unread row is unknown, never paused", () => {
    expect(rowStateChip(row("a", { pause_state: null }))).toEqual({
      text: "unknown: not read yet",
      tone: "unknown",
    });
    expect(rowStateChip(row("a", { pause_state: "paused" })).tone).toBe("ok");
    expect(rowStateChip(row("a", { pause_state: "limit_hit" })).text).toBe("limit hit");
  });

  it("an unread effort says so", () => {
    expect(effortBadge(row("a", { effort: { level: null, source: "unknown" } }))).toBe(
      "effort unknown",
    );
    expect(effortBadge(row("a", { effort: { level: "max", source: "screen" } }))).toBe(
      "max · screen",
    );
  });
});

/** A fake backend: records invokes, answers from `answer`, delivers events on demand. */
function fakeDeps(answer: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>) {
  const calls: Array<[string, Record<string, unknown> | undefined]> = [];
  let emit: ((payload: unknown) => void) | null = null;
  let stopped = 0;
  const deps: RestartPrepDeps = {
    invoke: (cmd, args) => {
      calls.push([cmd, args]);
      return answer(cmd, args);
    },
    listen: async (event, onPayload) => {
      expect(event).toBe(RESTART_PREP_CHANGED_EVENT);
      emit = onPayload;
      return () => {
        stopped += 1;
      };
    },
  };
  return { deps, calls, emit: (p: unknown) => emit?.(p), stopped: () => stopped };
}

describe("the store", () => {
  it("prepare twice shows ONE prepare: a click while one is in flight joins it", async () => {
    let release!: (v: unknown) => void;
    const f = fakeDeps((cmd) =>
      cmd === "restart_prep_prepare"
        ? new Promise((r) => {
            release = r;
          })
        : Promise.resolve(null),
    );
    const store = createRestartPrepStore(f.deps);
    const first = store.prepare("a");
    expect(store.getSnapshot().prepare.phase).toBe("in-flight");
    const second = store.prepare("b");
    expect(second).toBe(first);
    release(changed(record()));
    await Promise.all([first, second]);
    expect(f.calls.filter(([c]) => c === "restart_prep_prepare")).toHaveLength(1);
    expect(store.getSnapshot().prepare).toEqual({ phase: "done", message: "hold confirmed" });
    expect(store.getSnapshot().record?.prepare_id).toBe("prep-1");
  });

  it("acknowledges a prepare as received before anything is sent", () => {
    const f = fakeDeps(() => new Promise(() => {}));
    const store = createRestartPrepStore(f.deps);
    const phases: string[] = [];
    store.subscribe(() => phases.push(store.getSnapshot().prepare.phase));
    void store.prepare();
    expect(phases).toEqual(["received", "in-flight"]);
  });

  it("a refused prepare is a failed phase with the backend's words", async () => {
    const f = fakeDeps(() => Promise.reject("coord unreachable"));
    const store = createRestartPrepStore(f.deps);
    await expect(store.prepare()).rejects.toThrow("coord unreachable");
    expect(store.getSnapshot().prepare).toEqual({ phase: "failed", message: "coord unreachable" });
  });

  it("listens to restart-prep-changed instead of polling, and reads once on mount", async () => {
    const f = fakeDeps((cmd) =>
      Promise.resolve(cmd === "restart_prep_status" ? { record: null, view: null } : null),
    );
    const store = createRestartPrepStore(f.deps);
    const release = store.retain();
    const releaseAgain = store.retain();
    await Promise.resolve();
    await Promise.resolve();
    expect(f.calls.map(([c]) => c)).toEqual(["restart_prep_status"]);
    expect(store.getSnapshot().loaded).toBe(true);
    f.emit(changed(record({ verdict: verdict({ ready_to_restart: true }) })));
    expect(store.getSnapshot().record?.verdict?.ready_to_restart).toBe(true);
    // A malformed payload is ignored rather than half-applied.
    f.emit({ record: { nonsense: true } });
    expect(store.getSnapshot().record?.prepare_id).toBe("prep-1");
    release();
    expect(f.stopped()).toBe(0);
    releaseAgain();
    expect(f.stopped()).toBe(1);
  });

  it("an action reply older than an event does not overwrite it", async () => {
    let release!: (v: unknown) => void;
    const f = fakeDeps((cmd) =>
      cmd === "restart_prep_prepare"
        ? new Promise((r) => {
            release = r;
          })
        : Promise.resolve({ record: null, view: null }),
    );
    const store = createRestartPrepStore(f.deps);
    store.retain();
    await Promise.resolve();
    const p = store.prepare();
    f.emit(changed(record({ state: "ready" })));
    release(changed(record({ state: "preparing" })));
    await p;
    expect(store.getSnapshot().record?.state).toBe("ready");
  });

  it("the held toggle round-trips through the backend and reads its answer back", async () => {
    let current = record({
      cohort: { captured_at_ms: 1, captured_at: "", reason: "", rows: [row("s1"), row("s2")] },
    });
    const f = fakeDeps(async (cmd, args) => {
      if (cmd === "restart_prep_status") return { record: current, view: changed(current).view };
      if (cmd === "restart_prep_set_held") {
        current = {
          ...current,
          cohort: {
            ...current.cohort,
            rows: current.cohort.rows.map((r) =>
              r.session.claudeSessionId === args?.claudeSessionId
                ? { ...r, held: Boolean(args?.held), held_by_operator: true }
                : r,
            ),
          },
        };
        return current;
      }
      return null;
    });
    const store = createRestartPrepStore(f.deps);
    await store.refresh();
    await store.setHeld("s1", true);
    expect(f.calls.at(-1)).toEqual([
      "restart_prep_set_held",
      { prepareId: "prep-1", claudeSessionId: "s1", held: true },
    ]);
    expect(store.getSnapshot().record?.cohort.rows.map((r) => r.held)).toEqual([true, false]);
    expect(store.getSnapshot().held.s1).toEqual({ phase: "done", message: "held" });
    await store.setHeld("s1", false);
    expect(store.getSnapshot().record?.cohort.rows.map((r) => r.held)).toEqual([false, false]);
    expect(store.getSnapshot().held.s1).toEqual({ phase: "done", message: "released" });
  });

  it("a held toggle the record does not show is a failure, not a done", async () => {
    const rec = record();
    const f = fakeDeps(async (cmd) =>
      cmd === "restart_prep_status" ? { record: rec, view: changed(rec).view } : rec,
    );
    const store = createRestartPrepStore(f.deps);
    await store.refresh();
    await expect(store.setHeld("s1", true)).rejects.toThrow("does not show the change");
    expect(store.getSnapshot().held.s1?.phase).toBe("failed");
  });

  it("an unreadable status is UNKNOWN in the headline, never 'no prepare'", async () => {
    const f = fakeDeps(() => Promise.reject(new Error("lifecycle store not available")));
    const store = createRestartPrepStore(f.deps);
    await store.refresh();
    const snap = store.getSnapshot();
    expect(snap.readError).toBe("lifecycle store not available");
    expect(restartPrepHeadline(snap)?.kind).toBe("unknown");
  });
});
