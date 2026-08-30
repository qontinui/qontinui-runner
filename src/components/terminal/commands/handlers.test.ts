/**
 * The REAL handlers, invoked — with context closures stubbed, not modelled.
 *
 * Before this file, no test in the repository ran any of the 40 registered
 * handlers through the pipeline. Every handler-shaped test used a synthetic
 * stub action, so a handler could report `✓` for something that did not
 * happen and nothing would notice. `registeredActions.test.ts` reaches four
 * of them directly; this file reaches all of them, and reaches them the way
 * the operator does — through `resolve` → `matchPattern` → `chooseTier` →
 * `parse` → `handler`.
 *
 * ## What is CHARACTERIZED and what is ENDORSED
 *
 * When this file was written, 29 of the 40 handlers answered `ok` after
 * calling only closures that returned NOTHING. They could not have derived
 * that verdict; they asserted it. Deriving it needed the effects themselves
 * to return evidence — `sortZones()` saying how many zones it moved,
 * `writeToTerminal` saying it reached a PTY — which was a production change
 * and therefore a later phase.
 *
 * **That phase has landed.** The pinned list below is what remains, and the
 * golden table has grown a REPORT column carrying each handler's
 * `EffectReport` — so a verdict is now checkable against the numbers the
 * handler claims, not just against `ok` / `error`.
 *
 * Two things count as deriving a verdict, and the golden distinguishes them:
 *
 *  - the EFFECT reported (`evidence=true`) — a write envelope, a delivery
 *    count, a spawn's id list;
 *  - the handler OBSERVED the store's pre-state and compared (`report=`
 *    non-empty with `evidence=false`) — `/tag-clear` reading how many filters
 *    were active before clearing them. A React setter cannot be read back
 *    synchronously, so for a state transition the pre-state IS the honest
 *    observation available, and it is falsifiable: it reports zero.
 *
 * A row with NEITHER is an unearned `✓`, and that set is what
 * `OK_WITHOUT_EVIDENCE` pins.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";
import { fileURLToPath } from "node:url";

import { beforeAll, describe, expect, it } from "vitest";

import { canonicalArgs, run } from "./pipeline.testkit";
import { loadRealRegistry, type RealRegistryHarness } from "./realRegistry.testkit";
import { isEffectReport } from "./verdict";

const HERE = dirname(fileURLToPath(import.meta.url));
const GOLDEN_PATH = resolvePath(HERE, "__golden__", "handlers-golden.txt");
const UPDATE = process.env.TERMINAL_GOLDEN_UPDATE === "1";

let h: RealRegistryHarness;

beforeAll(async () => {
  h = await loadRealRegistry();
});

/**
 * A plausible value per `paramSchema` field name, so every handler gets one
 * run down its SUCCESS path and one down its missing-argument path. Keyed by
 * field name rather than by action, so a new action with familiar field names
 * is covered the day it registers.
 */
const FIELD_VALUE: Record<string, unknown> = {
  a: 1,
  account: "gmail",
  action: "list",
  b: 2,
  command: "ls",
  context: "hello",
  count: 1,
  goal: "ship it",
  pattern: "p",
  preset: "quad",
  state: "idle",
  tabId: "tab-a",
  tag: "alpha",
  target: "next",
  type: "architecture",
  zone: 1,
};

function canonicalBag(schema: Record<string, unknown> | undefined): Record<string, unknown> {
  const bag: Record<string, unknown> = {};
  for (const key of Object.keys(schema ?? {})) {
    if (key.startsWith("--")) continue;
    bag[key] = key in FIELD_VALUE ? FIELD_VALUE[key] : "x";
  }
  return bag;
}

interface HandlerRow {
  id: string;
  bag: "canonical" | "empty";
  args: string;
  verdict: string;
  effects: string;
  /** Did ANY closure the handler called hand back a value? */
  evidence: boolean;
  /**
   * The handler's own `EffectReport`, rendered — `"-"` when it reported none.
   *
   * This is the column that makes the table check a NUMBER rather than a
   * boolean. `terminal.approve-all` answering `ok` is not interesting; it
   * answering `approved 0 of 1` on a fixture with no mounted panes is the
   * whole point, and only this column can see it.
   */
  report: string;
}

/** Render an `EffectReport` for the golden table, or `"-"`. */
function renderReport(value: unknown): string {
  if (!isEffectReport(value)) return "-";
  const req = value.requested === undefined ? "" : `/${value.requested}`;
  return `${value.verb} ${value.affected}${req} ${value.noun}${value.kind === "state" ? " [state]" : ""}`;
}

async function characterize(): Promise<HandlerRow[]> {
  const rows: HandlerRow[] = [];
  for (const action of h.actions) {
    for (const bag of ["canonical", "empty"] as const) {
      const args = bag === "canonical" ? canonicalBag(action.paramSchema) : {};
      h.reset();
      let verdict: string;
      let report = "-";
      try {
        const r = await action.handler(args as never, { source: "test" });
        verdict = r.ok ? "ok" : `error:${r.code}`;
        if (r.ok) report = renderReport(r.value);
      } catch {
        verdict = "threw";
      }
      rows.push({
        id: action.id,
        bag,
        args: canonicalArgs(args),
        verdict,
        effects: h.calls.map((c) => c.name).join(",") || "-",
        evidence: h.calls.some((c) => c.evidence),
        report,
      });
    }
  }
  return rows;
}

// ── The pinned evidence-free set ─────────────────────────────────────

/**
 * Handlers that answer `ok` having neither read a closure's return NOR
 * reported an `EffectReport` of their own.
 *
 * This is the residue of the 29 that were pinned here before effects started
 * reporting. Every remaining entry needs a reason, and each has one recorded
 * in `PINNED_REASONS` below — a list with no reasons attached is the shape
 * that let this set grow silently in the first place.
 *
 * A new action joining this list is not automatically wrong. It IS a decision
 * that should be visible in review, which is the whole point of pinning it.
 */
const OK_WITHOUT_EVIDENCE: string[] = [];

/**
 * Why each still-pinned handler cannot report — one line per entry, and the
 * test below fails if the two lists disagree, so a handler cannot be pinned
 * without a reason or carry a reason without being pinned.
 */
const PINNED_REASONS: Record<string, string> = {};

describe("handlers — every registered handler is invocable", () => {
  it("runs all of them on both a canonical and an empty arg bag", async () => {
    const rows = await characterize();
    expect(rows).toHaveLength(h.actions.length * 2);
    const threw = rows.filter((r) => r.verdict === "threw");
    expect(threw.map((r) => `${r.id}/${r.bag}`)).toEqual([]);
  });

  it("pins the handlers that report ✓ without any evidence", async () => {
    const rows = await characterize();
    const found = rows
      .filter((r) => r.bag === "canonical" && r.verdict === "ok" && !r.evidence && r.report === "-")
      .map((r) => r.id)
      .sort();
    expect(
      found,
      "The set of handlers that answer `ok` having neither read an effect's " +
        "return nor reported an EffectReport has changed. If a handler LEFT " +
        "the set it now derives its verdict — good, remove it from " +
        "OK_WITHOUT_EVIDENCE. If one JOINED it, that is a new unearned `✓` " +
        "and needs a reason in PINNED_REASONS.",
    ).toEqual([...OK_WITHOUT_EVIDENCE].sort());
    // Every pin carries a reason, and every reason pins something. Without
    // this the list decays back into an unexplained set of ids.
    expect(Object.keys(PINNED_REASONS).sort()).toEqual([...OK_WITHOUT_EVIDENCE].sort());
  });

  it("the majority of handlers now DERIVE their verdict", async () => {
    // The anti-vacuity assertion, inverted. It used to read "more than half
    // the registry answers `ok` without evidence" — which was true, and was
    // the defect. The same measurement now has to come out the other way, so
    // a regression that quietly re-broadens the assertion cannot pass.
    const rows = await characterize();
    const succeeded = rows.filter((r) => r.bag === "canonical" && r.verdict === "ok");
    const derived = succeeded.filter((r) => r.evidence || r.report !== "-");
    expect(derived.length).toBeGreaterThan(succeeded.length / 2);
  });
});

// ── Named provable no-ops ────────────────────────────────────────────

describe("handlers — the no-ops that used to render as effects", () => {
  const reportOf = (value: unknown) => {
    if (!isEffectReport(value)) throw new Error("handler reported no EffectReport");
    return value;
  };

  /**
   * THE PRIORITY CASE, now inverted.
   *
   * `/approve-all` used to write `y\r` through
   * `terminalRefs.current.get(id)?.current?.writeToTerminal(...)`, an optional
   * chain that short-circuits silently when no `TerminalInstance` is mounted
   * — the normal state for an offscreen flow-grid zone. It then returned
   * `ok({ approved: waiting.length })`, counting sessions it merely INTENDED
   * to reach. The fixture below is exactly that situation: one tab in
   * `needs-input`, `terminalRefs` empty.
   *
   * The count now comes from DELIVERY. One session was targeted, zero
   * envelopes came back successful, and the report says both — so the status
   * line renders "approved 0 of 1 session" instead of `✓`.
   */
  it("`/approve-all` counts DELIVERIES, not intentions, with no PTY reached", async () => {
    h.reset();
    const o = await run("/approve-all", (id) => h.byId(id));
    expect(o.actionId).toBe("terminal.approve-all");
    expect(o.verdict).toBe("ok");
    // It went through the delivery path at all — the old code called nothing
    // observable whatsoever, which is why this ledger used to be empty.
    expect(h.callNames()).toEqual(["ctx.approveAll"]);
    // It targeted the ONE waiting tab, not both tabs.
    expect(h.calls[0].args[0]).toEqual(["tab-a"]);
    expect(h.calls[0].args[1]).toBe("y\r");
    const report = reportOf(o.value);
    expect(report.affected).toBe(0);
    expect(report.requested).toBe(1);
  });

  /**
   * `/tag-clear` calls `setActiveTagFilters(new Set())` unconditionally. The
   * setter is still void — a React setter cannot report — so the verdict
   * comes from the OBSERVED pre-state: how many filters were active before.
   * The fixture has none, so it reports zero and the bar renders it neutrally.
   */
  it("`/tag-clear` reports ZERO when no filter was active", async () => {
    h.reset();
    const o = await run("/tag-clear", (id) => h.byId(id));
    expect(o.verdict).toBe("ok");
    expect(h.callNames()).toEqual(["tags.setActiveTagFilters"]);
    // The closure still hands back nothing; the report is what changed.
    expect(h.calls.every((c) => !c.evidence)).toBe(true);
    expect(reportOf(o.value).affected).toBe(0);
  });

  /**
   * `/layout quad` on a grid that is already quad reports ZERO — and STILL
   * calls the setter.
   *
   * Both halves are load-bearing, and the second one is a defect this file
   * caught in the fix for the first. `setLayoutId` delegates to `applyLayout`,
   * which also clears the maximized zone, re-flows unassigned tabs into empty
   * zones and clamps the focused zone — so re-applying the current preset is a
   * real operation, and skipping it "because nothing changed" silently deletes
   * the operator's re-pack (and `ZoneLayoutPicker`'s, which routes through
   * this same handler). What was dishonest was the `✓`, never the call.
   */
  it("`/layout quad` reports ZERO but STILL re-applies, when already quad", async () => {
    h.reset();
    const o = await run("/layout quad", (id) => h.byId(id));
    expect(o.verdict).toBe("ok");
    expect(h.calls.map((c) => [c.name, c.args])).toEqual([["zone.setLayoutId", ["quad"]]]);
    expect(reportOf(o.value).affected).toBe(0);
  });

  /**
   * `sortZones` and `exportAll` were `() => void` on the context interface,
   * so no `✓` from either could be derived. Both now report: how many zone
   * assignments actually moved, and how many sessions actually reached disk
   * (with the operator dismissing the save dialog as its own outcome).
   */
  it("`/sort-zones` and `/export-all` read what their closures report", async () => {
    h.reset();
    const sorted = await run("/sort-zones", (id) => h.byId(id));
    expect(sorted.verdict).toBe("ok");
    expect(h.callNames()).toEqual(["ctx.sortZones"]);
    expect(h.calls[0].evidence).toBe(true);
    // The fixture's grid is already in state order, so nothing moved.
    expect(reportOf(sorted.value).affected).toBe(0);

    h.reset();
    const exported = await run("/export-all", (id) => h.byId(id));
    expect(exported.verdict).toBe("ok");
    expect(h.callNames()).toEqual(["ctx.exportAll"]);
    expect(h.calls[0].evidence).toBe(true);
    expect(reportOf(exported.value).affected).toBe(2);
  });

  /**
   * The idempotent pair. `/mute` twice is not a fault — it is why the action
   * exists apart from `/sound` — but the second run must not look like the
   * first. With the fixture's sound already OFF, the very first `/mute`
   * reports zero.
   */
  it("`/mute` on already-muted sound reports a no-op, not an effect", async () => {
    h.reset();
    const o = await run("/mute", (id) => h.byId(id));
    expect(o.verdict).toBe("ok");
    // It did not toggle anything — the guard was already there; what is new
    // is that the verdict says so.
    expect(h.callNames()).toEqual([]);
    const report = reportOf(o.value);
    expect(report.affected).toBe(0);
    expect(report.kind).toBe("state");
  });
});

// ── The eight recurrences, end to end through the real handlers ──────

describe("handlers — the shapes that recurred across nine rounds", () => {
  const runOne = async (input: string) => {
    h.reset();
    const o = await run(input, (id) => h.byId(id));
    return { ...o, ledger: h.calls.map((c) => [c.name, c.args] as const) };
  };

  it("`--tenant=` reads supplied-but-empty on BOTH the slash form and its alias", async () => {
    const a = await runOne("/spawn-ai 1 gmail --tenant=");
    const b = await runOne("/spawn-best 1 gmail --tenant=");
    expect(a.verdict).toBe("error:invalid-args");
    expect(b.verdict).toBe(a.verdict);
    expect(canonicalArgs(b.args)).toBe(canonicalArgs(a.args));
    // Nothing spawned. This is the P0: the alias refused while the primary
    // silently spawned under the device default.
    expect(a.ledger).toEqual([]);
    expect(b.ledger).toEqual([]);
  });

  it("a quoted prompt survives a declared flag on the same line", async () => {
    const o = await runOne('/spawn-ai 1 gmail --tenant=2299 "fix the --tenant handling"');
    expect(o.verdict).toBe("ok");
    // The whole prompt reaches the session. The regression typed `fix the`
    // into it and deleted the rest, invisibly, because the top-level tenant
    // overwrote the swallowed one on the way out.
    expect(o.ledger).toEqual([
      [
        "ctx.spawnAi",
        [1, "/cfg/gmail", "fix the --tenant handling", "2299aaaa-0000-4000-8000-000000000001"],
      ],
    ]);
  });

  it("a QUOTED flag spelling is prompt text, not syntax", async () => {
    const o = await runOne('/spawn-ai 1 gmail "--tenant"');
    expect(o.verdict).toBe("ok");
    expect(o.ledger).toEqual([["ctx.spawnAi", [1, "/cfg/gmail", "--tenant", undefined]]]);
  });

  it("`--tenant <v>` does not steal the account field", async () => {
    // The pattern's `(?<account>[\w-]+)` bound the flag NAME and
    // `(?<context>.+)` ate its value, so this answered "no matching Claude
    // account" while the byte-identical `--tenant=` spelling spawned.
    const o = await runOne("/spawn-ai 1 --tenant 2299");
    expect(o.verdict).toBe("ok");
    expect(o.args).toMatchObject({ count: 1, tenant: 2299 });
    expect(o.args).not.toHaveProperty("account", "--tenant");
    expect(o.ledger[0][1][3]).toBe("2299aaaa-0000-4000-8000-000000000001");
  });

  it("a numeric account stays SUPPLIED rather than silently becoming best", async () => {
    const o = await runOne("/spawn-ai 2 3");
    expect(o.verdict).toBe("error:no-account");
    expect(o.ledger).toEqual([]);
  });

  it("a numeric command stays SUPPLIED rather than reading as required", async () => {
    const o = await runOne("/spawn-with 2 5");
    expect(o.verdict).toBe("ok");
    expect(o.ledger).toEqual([["ctx.spawnPlain", [2, "5"]]]);
  });

  it("an empty quoted run is an EMPTY ARGUMENT, not a missing one", async () => {
    // D8: `/orchestrate ""` used to POST a conductor run with goal `"\"\""`.
    for (const input of ['/orchestrate ""', '/tag ""', '/auto-approve add ""']) {
      const o = await runOne(input);
      expect(o.verdict, input).toBe("error:invalid-args");
      expect(o.ledger, input).toEqual([]);
    }
  });

  it("a quoted goal reaches the conductor without its quote characters", async () => {
    const o = await runOne('/orchestrate "fix the thing"');
    expect(o.ledger[0][0]).toBe("invoke");
    expect(JSON.stringify(o.ledger[0][1])).toContain('"goal":"fix the thing"');
    expect(JSON.stringify(o.ledger[0][1])).not.toContain('\\"fix');
  });

  it("trailing junk on a no-argument command is refused before the handler", async () => {
    const o = await runOne("/mute please stop");
    expect(o.verdict).toBe("unbound");
    expect(o.unbound).toEqual(["please", "stop"]);
    expect(o.ledger).toEqual([]);
  });

  it("the seven iteration-8 phrasings still route to their own action", async () => {
    const expected: Array<[string, string]> = [
      ["/sort zones", "terminal.sort-zones"],
      ["/export all", "terminal.export-all"],
      ["/generate workflow", "terminal.generate-workflow"],
      ["/save workflow", "terminal.save-workflow"],
      ["/prompt library", "terminal.prompt"],
      ["/focus mode", "terminal.toggle-focus-mode"],
      ["/spawn 3 plain", "terminal.spawn"],
    ];
    for (const [input, id] of expected) {
      const o = await runOne(input);
      expect(o.actionId, input).toBe(id);
      expect(o.route, input).toBe("pattern");
      expect(o.verdict, input).not.toBe("unbound");
    }
    // …and the one that carries an argument keeps it uncorrupted.
    const spawn = await runOne("/spawn 3 plain");
    expect(spawn.args).toEqual({ count: 3 });
    expect(spawn.ledger).toEqual([["ctx.spawnPlain", [3]]]);
  });

  it("`/spawn` is bounded at both ends", async () => {
    expect((await runOne("/spawn 5000")).verdict).toBe("error:invalid-count");
    expect((await runOne("/spawn 0")).verdict).toBe("error:invalid-count");
  });
});

// ── The golden handler table ─────────────────────────────────────────

describe("handlers — golden characterization table", () => {
  it("matches the committed table", async () => {
    const rows = await characterize();
    const text =
      [
        "# terminal CommandBar handlers — golden characterization",
        "#",
        "# GENERATED. Regenerate with:",
        "#   TERMINAL_GOLDEN_UPDATE=1 npx vitest run src/components/terminal/commands/handlers.test.ts",
        "#",
        "# <actionId> TAB <arg bag> TAB <args> TAB <verdict> TAB <effects called> TAB <evidence?> TAB <report>",
        "#",
        "# `evidence=false` means every closure the handler called returned nothing.",
        "# `report=-` means the handler reported no EffectReport either. A row with",
        "# BOTH is an `ok` that was ASSERTED rather than derived — the shape this",
        "# table exists to keep visible.",
        "#",
        "# `report` reads `<verb> <affected>[/<requested>] <noun>`; `[state]` marks a",
        "# preference/mode report, where affected 1 = it moved and 0 = it was already",
        "# in that state.",
        "",
      ].join("\n") +
      rows
        .map(
          (r) =>
            `${r.id}\t${r.bag}\t${r.args}\t${r.verdict}\t${r.effects}\t${r.evidence}\t${r.report}`,
        )
        .sort()
        .join("\n") +
      "\n";

    if (UPDATE) {
      mkdirSync(dirname(GOLDEN_PATH), { recursive: true });
      writeFileSync(GOLDEN_PATH, text, "utf8");
    }
    expect(existsSync(GOLDEN_PATH)).toBe(true);
    expect(
      readFileSync(GOLDEN_PATH, "utf8").replace(/\r\n/g, "\n"),
      "Handler behaviour changed. Regenerate with TERMINAL_GOLDEN_UPDATE=1 and " +
        "review the diff — a verdict moving from error to ok, or an effect " +
        "disappearing from a row, is a semantic change.",
    ).toBe(text);
  });
});
