/**
 * The restart-prep button and panel, rendered to static markup under
 * `environment: "node"` (same precedent as `SessionCountBanner.test.tsx`).
 *
 * Pinned: the button acknowledges itself and is disabled while a prepare is
 * received or in flight; the panel's headline is exactly one of its three
 * readings; every row shows its name, account, effort, page, state chip and
 * held toggle; and every control registers its UI Bridge id with its declared
 * `effect`.
 */
import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const registered: Array<{ id: string; actions: Array<{ id: string; effect?: string }> }> = [];
vi.mock("@qontinui/ui-bridge", () => ({
  useUIComponent: (opts: { id: string; actions?: Array<{ id: string; effect?: string }> }) => {
    registered.push({ id: opts.id, actions: opts.actions ?? [] });
    return {};
  },
}));

import { PrepareRestartButton, RestartPrepPanel, prepareButtonState } from "./RestartPrepPanel";
import { READY_HEADLINE, type RestartPrepSnapshot, type RestartPrepStore } from "./restartPrep";
import { changed, record, row, verdict } from "./restartPrep.testkit";

function snapshot(over: Partial<RestartPrepSnapshot> = {}): RestartPrepSnapshot {
  const rec = over.record === undefined ? record() : over.record;
  return {
    record: rec,
    view: rec ? changed(rec).view : null,
    readError: null,
    loaded: true,
    panelOpen: true,
    prepare: { phase: "idle" },
    cancel: { phase: "idle" },
    extend: { phase: "idle" },
    held: {},
    ...over,
  };
}

/** A store frozen at `snap`; the render reads it and never acts. */
function frozenStore(snap: RestartPrepSnapshot): RestartPrepStore {
  const never = () => Promise.reject(new Error("not in a render test"));
  return {
    getSnapshot: () => snap,
    subscribe: () => () => {},
    retain: () => () => {},
    refresh: async () => {},
    prepare: never,
    cancel: never,
    extend: never,
    setHeld: never,
    openPanel: () => ({ changed: false }),
    closePanel: () => {},
  };
}

const renderPanel = (snap: RestartPrepSnapshot): string =>
  renderToStaticMarkup(<RestartPrepPanel snapshot={snap} store={frozenStore(snap)} />);

describe("the header button acknowledges itself", () => {
  it("is disabled with a spinner while the prepare is in flight", () => {
    for (const rec of [null, record()]) {
      const s = prepareButtonState(snapshot({ record: rec, prepare: { phase: "in-flight" } }));
      expect(s.disabled).toBe(true);
      expect(s.spinner).toBe(true);
    }
    const html = renderToStaticMarkup(
      <PrepareRestartButton
        store={frozenStore(
          snapshot({ record: null, panelOpen: false, prepare: { phase: "in-flight" } }),
        )}
      />,
    );
    expect(html).toContain('data-prepare-phase="in-flight"');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>/);
    expect(html).toContain("Preparing…");
    expect(html).not.toContain('data-ui-bridge-id="terminal.restart-prep-panel"');
  });

  it("is enabled before a prepare and after a failure, which it names", () => {
    const idle = prepareButtonState(snapshot({ record: null }));
    expect(idle).toMatchObject({ label: "Prepare for restart", disabled: false });
    const failed = prepareButtonState(
      snapshot({ record: null, prepare: { phase: "failed", message: "coord unreachable" } }),
    );
    expect(failed).toMatchObject({ disabled: false, tone: "error", title: "coord unreachable" });
    // An unread record is UNKNOWN, never the idle "no prepare" label.
    expect(prepareButtonState(snapshot({ record: null, readError: "boom" })).label).toBe(
      "Restart prep: UNKNOWN",
    );
  });

  it("a stale failure does not hide a prepare that is running", () => {
    const ready = record({ verdict: verdict({ ready_to_restart: true }) });
    const s = prepareButtonState(
      snapshot({ record: ready, prepare: { phase: "failed", message: "old" } }),
    );
    expect(s.label).toBe("Safe to restart");
  });

  it("reads the running prepare's headline once there is one", () => {
    const ready = record({ verdict: verdict({ ready_to_restart: true }) });
    expect(prepareButtonState(snapshot({ record: ready })).label).toBe("Safe to restart");
    const waiting = record({
      verdict: verdict({
        blockers: [{ session: "s1", display_name: "n", reason: "notified: x" }],
      }),
    });
    expect(prepareButtonState(snapshot({ record: waiting })).label).toBe(
      "Restart prep: Waiting on 1",
    );
    expect(prepareButtonState(snapshot({ record: record() })).label).toBe("Restart prep: UNKNOWN");
  });
});

describe("the panel headline", () => {
  it("reads 'Safe to restart' when the verdict is ready", () => {
    const html = renderPanel(
      snapshot({ record: record({ verdict: verdict({ ready_to_restart: true }) }) }),
    );
    expect(html).toContain('data-headline-kind="ready"');
    expect(html).toContain(READY_HEADLINE);
  });

  it("reads 'Waiting on N sessions' and names each blocker", () => {
    const html = renderPanel(
      snapshot({
        record: record({
          verdict: verdict({
            blockers: [
              { session: "s1", display_name: "alpha", reason: "working: mid-turn" },
              { session: "s2", display_name: "beta", reason: "notified: told this tick" },
            ],
          }),
        }),
      }),
    );
    expect(html).toContain('data-headline-kind="waiting"');
    expect(html).toContain("Waiting on 2 sessions");
    expect(html).toContain("alpha — working: mid-turn");
    expect(html).toContain("beta — notified: told this tick");
  });

  it("reads UNKNOWN naming the input when the record or verdict is unread", () => {
    const unread = renderPanel(snapshot({ record: null, readError: "boom" }));
    expect(unread).toContain('data-headline-kind="unknown"');
    expect(unread).toContain("UNKNOWN: the restart-prep record (boom)");
    const noVerdict = renderPanel(snapshot());
    expect(noVerdict).toContain("UNKNOWN: the verdict");
    expect(noVerdict).not.toContain(READY_HEADLINE);
  });

  it("keeps the session blockers on screen under a prepare-wide UNKNOWN", () => {
    const html = renderPanel(
      snapshot({
        record: record({
          verdict: verdict({
            blockers: [
              { session: null, display_name: null, reason: "the spawn hold is not confirmed (x)" },
              { session: "s1", display_name: "alpha", reason: "working: mid-turn" },
            ],
          }),
        }),
      }),
    );
    expect(html).toContain('data-headline-kind="unknown"');
    expect(html).toContain("UNKNOWN: the spawn hold is not confirmed (x)");
    expect(html).toContain("alpha — working: mid-turn");
  });

  it("never says 'no prepare' for a record it could not read", () => {
    const html = renderPanel(snapshot({ record: null }));
    expect(html).toContain("No readable prepare record");
    expect(html).not.toContain("No restart prepare is in progress");
  });
});

describe("the panel rows and controls", () => {
  it("shows every row's identity, effort, page, state and held toggle", () => {
    registered.length = 0;
    const rec = record({
      cohort: {
        captured_at_ms: 1,
        captured_at: "",
        reason: "",
        rows: [
          row("s1", {
            held: true,
            effort: { level: "max", source: "screen" },
            pause_state: "paused",
          }),
          row("s2", { session: { ...row("s2").session, accountLabel: null, zoneIndex: 2 } }),
        ],
      },
    });
    const html = renderPanel(snapshot({ record: rec }));
    expect(html).toContain('data-ui-bridge-id="terminal.restart-prep-row-s1"');
    expect(html).toContain("name-s1");
    expect(html).toContain("gmail");
    expect(html).toContain("max · screen");
    expect(html).toContain("paused");
    expect(html).toContain("held for hand restore");
    expect(html).toMatch(/data-ui-bridge-id="terminal.restart-prep-held-s1"[^>]*data-held="true"/);
    expect(html).toMatch(/data-ui-bridge-id="terminal.restart-prep-held-s2"[^>]*data-held="false"/);
    expect(html).toContain("account unknown");
    expect(html).toContain("page-a · zone 3");
    expect(html).toContain('data-ui-bridge-id="terminal.restart-prep-cancel"');

    const effects = Object.fromEntries(
      registered.flatMap((c) => c.actions.map((a) => [`${c.id}.${a.id}`, a.effect])),
    );
    expect(effects).toMatchObject({
      "terminal.restart-prep-held-s1.toggle": "write",
      "terminal.restart-prep-held-s2.toggle": "write",
      "terminal.restart-prep-cancel.cancel": "destructive",
      "terminal.restart-prep-extend.extend": "destructive",
      "terminal.restart-prep-panel.refresh": "read",
    });
    expect(html).toContain('data-ui-bridge-id="terminal.restart-prep-extend"');
  });

  it("registers the button as destructive (it drains the device and types into sessions)", () => {
    registered.length = 0;
    renderToStaticMarkup(
      <PrepareRestartButton store={frozenStore(snapshot({ panelOpen: false }))} />,
    );
    expect(registered.find((c) => c.id === "terminal.prepare-restart-button")?.actions).toEqual([
      expect.objectContaining({ id: "prepare", effect: "destructive" }),
    ]);
  });

  it("offers Cancel only while the prepare is in its window, and says what ended it", () => {
    const cancelled = renderPanel(snapshot({ record: record({ state: "cancelled" }) }));
    expect(cancelled).not.toContain('data-ui-bridge-id="terminal.restart-prep-cancel"');
    expect(cancelled).not.toContain('data-ui-bridge-id="terminal.restart-prep-extend"');
    expect(cancelled).toContain("Prepare cancelled");
    const sameBuild = snapshot();
    sameBuild.view = { ...sameBuild.view!, same_build_hold: true };
    expect(renderPanel(sameBuild)).toContain("Release hold");
  });
});
