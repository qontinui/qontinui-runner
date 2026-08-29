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
 * 29 of the 40 handlers answer `ok` after calling only closures that return
 * NOTHING. They cannot have derived that verdict; they asserted it. Deriving
 * it needs the effects themselves to return evidence — `sortZones()` saying
 * how many zones it moved, `writeToTerminal` saying it reached a PTY — which
 * is a production change and therefore a later phase.
 *
 * So this file PINS today's answers rather than endorsing them. The pinned
 * list below and the golden table in `__golden__/handlers-golden.txt` exist
 * so that when the effects do start returning evidence, the verdicts that
 * change show up as a reviewable diff instead of a silent semantic shift —
 * and so that a new handler joining the evidence-free set is a decision
 * somebody signs, not an accident.
 *
 * Cases marked "characterized, not endorsed" are ones where the `✓` is
 * PROVABLY wrong about a no-op today.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";
import { fileURLToPath } from "node:url";

import { beforeAll, describe, expect, it } from "vitest";

import { canonicalArgs, run } from "./pipeline.testkit";
import { loadRealRegistry, type RealRegistryHarness } from "./realRegistry.testkit";

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
}

async function characterize(): Promise<HandlerRow[]> {
  const rows: HandlerRow[] = [];
  for (const action of h.actions) {
    for (const bag of ["canonical", "empty"] as const) {
      const args = bag === "canonical" ? canonicalBag(action.paramSchema) : {};
      h.reset();
      let verdict: string;
      try {
        const r = await action.handler(args as never, { source: "test" });
        verdict = r.ok ? "ok" : `error:${r.code}`;
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
      });
    }
  }
  return rows;
}

// ── The pinned evidence-free set ─────────────────────────────────────

/**
 * Handlers that answer `ok` having called only closures that return nothing.
 *
 * CHARACTERIZED, NOT ENDORSED. Every entry is a `✓` the handler asserted
 * rather than derived. The list is pinned so the later phase's fix — effects
 * that return evidence, verdicts derived from it — produces a diff here.
 *
 * A new action joining this list is not automatically wrong. It IS a decision
 * that should be visible in review, which is the whole point of pinning it.
 */
const OK_WITHOUT_EVIDENCE = [
  "terminal.analyze",
  "terminal.approve-all",
  "terminal.auto-approve",
  "terminal.close",
  "terminal.doc-finder",
  "terminal.export-all",
  "terminal.focus",
  "terminal.history",
  "terminal.layout",
  "terminal.maximize",
  "terminal.metrics",
  "terminal.mute",
  "terminal.prompt",
  "terminal.resume",
  "terminal.select-by-state",
  "terminal.show-shortcuts",
  "terminal.sort-zones",
  "terminal.swap",
  "terminal.tag-clear",
  "terminal.tag-toggle",
  "terminal.toggle-auto-focus",
  "terminal.toggle-auto-restart",
  "terminal.toggle-desktop-notify",
  "terminal.toggle-file-ownership",
  "terminal.toggle-findings",
  "terminal.toggle-focus-mode",
  "terminal.toggle-sessions-sidebar",
  "terminal.toggle-sound",
  "terminal.unmute",
];

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
      .filter((r) => r.bag === "canonical" && r.verdict === "ok" && !r.evidence)
      .map((r) => r.id)
      .sort();
    expect(
      found,
      "The set of handlers that answer `ok` after calling only evidence-free " +
        "closures has changed. If a handler LEFT the set it now derives its " +
        "verdict — good, remove it from OK_WITHOUT_EVIDENCE. If one JOINED it, " +
        "that is a new unearned `✓` and needs a reason.",
    ).toEqual([...OK_WITHOUT_EVIDENCE].sort());
    // Anti-vacuity: this is a majority of the registry, not a rounding error.
    expect(found.length).toBeGreaterThan(h.actions.length / 2);
  });
});

// ── Named provable no-ops ────────────────────────────────────────────

describe("handlers — a ✓ that is provably wrong about a no-op", () => {
  /**
   * CHARACTERIZED, NOT ENDORSED.
   *
   * `/approve-all` writes `y\r` into every waiting PTY via
   * `terminalRefs.current.get(id)?.current?.writeToTerminal(...)`. With no ref
   * registered for the waiting tab the optional chain short-circuits and
   * NOTHING is written — yet the handler returns `ok({ approved: waiting.length })`,
   * counting sessions it merely INTENDED to approve. The operator sees
   * `/approve-all ✓` for the most irreversible command on the page having
   * delivered zero keystrokes.
   *
   * The count is derived from the wrong quantity (tabs in `needs-input`)
   * rather than from the writes that actually landed. Fixing it is a
   * production change; this pins the current answer.
   */
  it("`/approve-all` reports ✓ with no PTY reached", async () => {
    h.reset();
    const o = await run("/approve-all", (id) => h.byId(id));
    expect(o.actionId).toBe("terminal.approve-all");
    expect(o.verdict).toBe("ok");
    expect(h.calls).toEqual([]);
  });

  /**
   * CHARACTERIZED, NOT ENDORSED. `/tag-clear` calls `setActiveTagFilters(new
   * Set())` unconditionally and answers `✓` whether or not a filter was
   * active. Nothing in the return distinguishes "cleared three filters" from
   * "there was nothing to clear".
   */
  it("`/tag-clear` reports ✓ when no filter was active", async () => {
    h.reset();
    const o = await run("/tag-clear", (id) => h.byId(id));
    expect(o.verdict).toBe("ok");
    expect(h.callNames()).toEqual(["tags.setActiveTagFilters"]);
    expect(h.calls.every((c) => !c.evidence)).toBe(true);
  });

  /**
   * CHARACTERIZED, NOT ENDORSED. `/layout quad` when the layout is ALREADY
   * `quad` calls `setLayoutId("quad")` and answers `✓` — indistinguishable
   * from a layout that actually changed.
   */
  it("`/layout quad` reports ✓ when the layout is already quad", async () => {
    h.reset();
    const o = await run("/layout quad", (id) => h.byId(id));
    expect(o.verdict).toBe("ok");
    expect(h.calls.map((c) => [c.name, c.args])).toEqual([["zone.setLayoutId", ["quad"]]]);
    expect(h.calls.every((c) => !c.evidence)).toBe(true);
  });

  /**
   * CHARACTERIZED, NOT ENDORSED. `sortZones` and `exportAll` are `() => void`
   * on the context interface, so no `✓` from either can be derived — the
   * handler cannot learn whether a single zone moved or a single byte was
   * written.
   */
  it("`/sort-zones` and `/export-all` report ✓ from a void closure", async () => {
    for (const [input, effect] of [
      ["/sort-zones", "ctx.sortZones"],
      ["/export-all", "ctx.exportAll"],
    ] as const) {
      h.reset();
      const o = await run(input, (id) => h.byId(id));
      expect(o.verdict, input).toBe("ok");
      expect(h.callNames(), input).toEqual([effect]);
      expect(h.calls[0].evidence, input).toBe(false);
    }
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
        "# <actionId> TAB <arg bag> TAB <args> TAB <verdict> TAB <effects called> TAB <evidence?>",
        "#",
        "# `evidence=false` means every closure the handler called returned nothing,",
        "# so the verdict on that row was ASSERTED, not derived. See the module",
        "# docstring: characterized, not endorsed.",
        "",
      ].join("\n") +
      rows
        .map((r) => `${r.id}\t${r.bag}\t${r.args}\t${r.verdict}\t${r.effects}\t${r.evidence}`)
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
