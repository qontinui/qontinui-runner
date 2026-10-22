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
      blockers: [],
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
    expect(wide?.kind).toBe("unknown");
    expect(wide?.text).toBe("UNKNOWN: the restart census could not be taken: x");
    // ...and still carries every blocker, so the sessions stay listed.
    expect(wide?.kind === "unknown" && wide.blockers.map((b) => b.session)).toEqual([null, "a"]);
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
  it("colours a restore by its verdict", () => {
    const at = { at_ms: 1, at: "", effort_applied: null, effort: null };
    expect(rowStateChip(row("a", { restore: { ...at, verdict: "resumed" } })).tone).toBe("ok");
    expect(rowStateChip(row("a", { restore: { ...at, verdict: "failed(spawn)" } })).tone).toBe(
      "unknown",
    );
    expect(
      rowStateChip(row("a", { restore: { ...at, verdict: "skipped(resource_guard:x)" } })).tone,
    ).toBe("wait");
  });

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

  it("acknowledges a prepare in the same turn as the click, before anything answers", () => {
    const f = fakeDeps(() => new Promise(() => {}));
    const store = createRestartPrepStore(f.deps);
    const phases: string[] = [];
    store.subscribe(() => phases.push(store.getSnapshot().prepare.phase));
    void store.prepare();
    expect(phases).toEqual(["in-flight"]);
  });

  it("extend and cancel each join their own in-flight call and report the hold", async () => {
    const resolvers: Record<string, (v: unknown) => void> = {};
    const f = fakeDeps(
      (cmd) =>
        new Promise((r) => {
          resolvers[cmd] = r;
        }),
    );
    const store = createRestartPrepStore(f.deps);
    const e1 = store.extend();
    expect(store.extend()).toBe(e1);
    const c1 = store.cancel();
    expect(store.getSnapshot().extend.phase).toBe("in-flight");
    expect(store.getSnapshot().cancel.phase).toBe("in-flight");
    resolvers.restart_prep_extend(changed(record({ hold_until: "later" })));
    await e1;
    expect(store.getSnapshot().extend).toEqual({ phase: "done", message: "hold confirmed" });
    resolvers.restart_prep_cancel("not a reply");
    await expect(c1).rejects.toThrow("unrecognized shape");
    expect(store.getSnapshot().cancel.phase).toBe("failed");
    expect(f.calls.map(([c]) => c)).toEqual(["restart_prep_extend", "restart_prep_cancel"]);
  });

  it("a refused prepare is a failed phase with the backend's words", async () => {
    const f = fakeDeps(() => Promise.reject("coord unreachable"));
    const store = createRestartPrepStore(f.deps);
    await expect(store.prepare()).rejects.toThrow("coord unreachable");
    expect(store.getSnapshot().prepare).toEqual({ phase: "failed", message: "coord unreachable" });
  });

  it("listens to restart-prep-changed instead of polling", async () => {
    const f = fakeDeps((cmd) => Promise.resolve(cmd === "restart_prep_current" ? null : null));
    const store = createRestartPrepStore(f.deps);
    const release = store.retain();
    const releaseAgain = store.retain();
    await Promise.resolve();
    await Promise.resolve();
    // One read on mount, one more once the listener is registered — and no
    // more after that: the events carry every change.
    expect(f.calls.map(([c]) => c)).toEqual(["restart_prep_current", "restart_prep_current"]);
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
        : Promise.resolve(null),
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
      if (cmd === "restart_prep_current") return changed(current);
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

  it("a held-toggle reply never overwrites an event that landed while it was out", async () => {
    const rec = record();
    let answer!: (v: unknown) => void;
    const f = fakeDeps((cmd) =>
      cmd === "restart_prep_current"
        ? Promise.resolve(changed(rec))
        : new Promise((r) => {
            answer = r;
          }),
    );
    const store = createRestartPrepStore(f.deps);
    store.retain();
    await store.refresh();
    const toggled = store.setHeld("s1", true);
    // The notice tick queued behind the toggle runs and announces first.
    const newer = record({
      state: "ready",
      cohort: { ...rec.cohort, rows: [row("s1", { held: true, pause_state: "paused" })] },
    });
    f.emit(changed(newer));
    answer({ ...rec, cohort: { ...rec.cohort, rows: [row("s1", { held: true })] } });
    await toggled;
    expect(store.getSnapshot().record?.state).toBe("ready");
    expect(store.getSnapshot().record?.cohort.rows[0].pause_state).toBe("paused");
    expect(store.getSnapshot().held.s1?.phase).toBe("done");
  });

  it("a read that started before an applied action reply does not land over it", async () => {
    let answerRead!: (v: unknown) => void;
    const f = fakeDeps((cmd) =>
      cmd === "restart_prep_current"
        ? new Promise((r) => {
            answerRead = r;
          })
        : Promise.resolve(changed(record({ state: "ready" }))),
    );
    const store = createRestartPrepStore(f.deps);
    const read = store.refresh();
    await store.prepare();
    expect(store.getSnapshot().record?.state).toBe("ready");
    answerRead(changed(record({ state: "preparing" })));
    await read;
    expect(store.getSnapshot().record?.state).toBe("ready");
  });

  it("a failed read that raced a good event does not turn it UNKNOWN", async () => {
    let fail!: (e: unknown) => void;
    const f = fakeDeps(
      () =>
        new Promise((_, reject) => {
          fail = reject;
        }),
    );
    const store = createRestartPrepStore(f.deps);
    store.retain();
    await Promise.resolve();
    f.emit(changed(record()));
    fail(new Error("timeout"));
    await Promise.resolve();
    await Promise.resolve();
    expect(store.getSnapshot().readError).toBeNull();
    expect(store.getSnapshot().record?.prepare_id).toBe("prep-1");
  });

  it("a held toggle the record does not show is a failure, not a done", async () => {
    const rec = record();
    const f = fakeDeps(async (cmd) => (cmd === "restart_prep_current" ? changed(rec) : rec));
    const store = createRestartPrepStore(f.deps);
    await store.refresh();
    await expect(store.setHeld("s1", true)).rejects.toThrow("does not show the change");
    expect(store.getSnapshot().held.s1?.phase).toBe("failed");
  });

  it("reads again once the listener is registered, so a change in the gap is not lost", async () => {
    let registered!: () => void;
    let state: "preparing" | "ready" = "preparing";
    const reads: string[] = [];
    const store = createRestartPrepStore({
      invoke: async (cmd) => {
        expect(cmd).toBe("restart_prep_current");
        const rec = record({ state });
        reads.push(state);
        return changed(rec);
      },
      listen: () =>
        new Promise((resolve) => {
          registered = () => resolve(() => {});
        }),
    });
    store.retain();
    await new Promise((r) => setTimeout(r, 0));
    expect(store.getSnapshot().record?.state).toBe("preparing");
    // The backend moved on before the listener was up: nobody heard it.
    state = "ready";
    registered();
    await new Promise((r) => setTimeout(r, 0));
    expect(reads).toEqual(["preparing", "ready"]);
    expect(store.getSnapshot().record?.state).toBe("ready");
  });

  it("dismisses only a waiting restore, and reads the record back as done", async () => {
    const restoring = record({ state: "restoring" });
    const f = fakeDeps(async (cmd, args) => {
      if (cmd === "restart_prep_current") return changed(restoring);
      expect(cmd).toBe("restart_prep_dismiss");
      expect(args).toEqual({ prepareId: "prep-1" });
      return { ...restoring, state: "done" };
    });
    const store = createRestartPrepStore(f.deps);
    await store.refresh();
    await store.dismiss();
    expect(store.getSnapshot().record?.state).toBe("done");
    expect(store.getSnapshot().dismiss).toEqual({ phase: "done", message: "dismissed" });
    await expect(store.dismiss()).rejects.toThrow("nothing to dismiss");
    expect(f.calls.filter(([c]) => c === "restart_prep_dismiss")).toHaveLength(1);
  });

  it("a listener that cannot be set up is reported, not silent", async () => {
    const store = createRestartPrepStore({
      invoke: () => Promise.resolve(null),
      listen: () => Promise.reject(new Error("no event plugin")),
    });
    store.retain();
    await Promise.resolve();
    await Promise.resolve();
    expect(store.getSnapshot().eventsError).toBe("no event plugin");
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
