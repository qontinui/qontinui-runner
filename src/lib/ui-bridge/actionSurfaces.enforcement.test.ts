/**
 * The class gate for "an unvalidated bag reaches an effect".
 *
 * ## What this asserts, and why it is not another inspection
 *
 * PR #1301 closed that class on four surfaces and left three siblings open in
 * the same `actions: [...]` array, while its own docstring said they were
 * closed. That is the eleventh time in this loop that a fix passed review
 * because the defective shape lived somewhere the fix did not reach.
 *
 * So this suite does not check the seven surfaces the twelfth round named. It
 * ENUMERATES every action surface in `src/` from the AST and requires each one
 * to be structurally incapable of reading an unvalidated bag — either a
 * `guardedAction`, or a handler with no parameter to receive one. A surface
 * added tomorrow is enumerated tomorrow.
 *
 * ## The falsification, run as a test rather than claimed in a comment
 *
 * `ESCAPING_CLASS_COUNT = 4` was wrong by eleven classes and green for weeks;
 * `61 of 126 rules can be deleted with the suite still 33/33 green` was the
 * finding one commit later. A gate that nothing falsifies is not a gate. So
 * `"a NEW unguarded surface turns this red"` WRITES three new unguarded
 * surfaces into `src/`, in the three positions a surface can occupy, re-runs
 * the scan, and asserts each is flagged — then deletes them. If the scanner
 * ever stops seeing one of those shapes, this test fails, in this file, on
 * this run.
 *
 * ## Runtime, not only shape
 *
 * Shape says a surface CANNOT read a bag. It does not say the binder refuses
 * the right things. `guardedAction.test.ts` and
 * `terminalPaneCustomActions.test.ts` execute the wrappers against `5`, `"zz"`,
 * `[]`, an undeclared key and a non-scalar value, with the effect functions
 * spied, and assert the effect count is ZERO. Together: every surface routes
 * through the wrapper (here), and the wrapper refuses (there).
 */

import { describe, it, expect, afterAll } from "vitest";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join, resolve } from "path";
import {
  GUARD_BUILDERS,
  isFixtureFile,
  SHAPE_RULE_EXEMPT,
  sourceFiles,
  registryHandlerCallSites,
  renderInventory,
  scanTree,
  unresolvedDelegates,
  valueHandlerProperties,
  violations,
  type ActionSurface,
} from "./actionSurfaces";

/**
 * Each `scanFixture` re-walks all of `src/` on purpose — that walk IS half of
 * what is under test — so these run well past vitest's 5s default.
 */
const WALK_TIMEOUT = 120_000;

const ROOT = resolve(__dirname, "..", "..", "..");
const GOLDEN = join(ROOT, "src", "lib", "ui-bridge", "__golden__", "action-surfaces.txt");

const SURFACES = scanTree(ROOT);

/**
 * Where the falsification fixtures land: a TEMP root with its own `src/`, not
 * the repo's.
 *
 * The first version of this wrote them into the real `src/` — nearer to the
 * truth, and unusable: `globalChords.enforcement.test.ts` walks `src/` from a
 * parallel worker, and a directory that appears and vanishes mid-walk crashed
 * it with `ENOENT` on `statSync`, intermittently, in a file nobody had
 * touched. A test that mutates the tree its siblings read is a flake generator,
 * and an intermittently-red sibling is how a real red gets ignored.
 *
 * A temp root exercises exactly the same `sourceFiles()` walk over exactly the
 * same file extensions; what it does not prove is that the walk is ROOTED at
 * the repo's `src/`. That half is proven by every other test here, all of
 * which run `scanTree(ROOT)` over the real tree and find the real inventory.
 */
const FIXTURE_ROOT = join(tmpdir(), `action-surface-probe-${process.pid}`);
const FIXTURE_SRC = join(FIXTURE_ROOT, "src");

afterAll(() => {
  rmSync(FIXTURE_ROOT, { recursive: true, force: true });
});

function scanFixture(name: string, source: string): ActionSurface[] {
  rmSync(FIXTURE_SRC, { recursive: true, force: true });
  mkdirSync(FIXTURE_SRC, { recursive: true });
  writeFileSync(join(FIXTURE_SRC, name), source, "utf8");
  // Scan through the TREE walk, not by calling the parser directly: what has
  // to be proven is that the WALK reaches a new file, which is the half
  // `sourceFiles()` owns and a direct parse would fake.
  return scanTree(FIXTURE_ROOT);
}

describe("UI Bridge action surfaces — the enumeration", () => {
  it("finds surfaces at all (a scanner that finds nothing passes every other assertion)", () => {
    expect(SURFACES.length).toBeGreaterThan(50);
    expect(SURFACES.filter((s) => s.form === "guarded").length).toBeGreaterThan(10);
    expect(SURFACES.filter((s) => s.form === "literal").length).toBeGreaterThan(10);
  });

  it("covers every file that registers a UI Bridge component or element action", () => {
    // Cross-check the AST enumeration against a naive text sweep: any file
    // that spells `actions:` or `customActions:` next to a `handler` must
    // appear in the inventory. A parser that silently skips a file class —
    // exactly what `sourceFiles()`'s `/\.tsx?$/` did to `.jsx`/`.mjs` in the
    // chord scanner — shows up here rather than as a quiet zero.
    const filesWithSurfaces = new Set(SURFACES.map((s) => s.file));
    const unaccounted: string[] = [];
    for (const rel of textualCandidates()) {
      if (filesWithSurfaces.has(rel)) continue;
      // A mismatch is allowed ONLY where the file writes no `handler` in value
      // position at all — an interface, a cast, or (in this very file) a
      // fixture source string. Anything else is a surface the AST missed.
      const n = valueHandlerProperties(readFileSync(join(ROOT, rel), "utf8"), rel);
      if (n > 0) unaccounted.push(`${rel} (${n} value-position handler properties)`);
    }
    expect(unaccounted).toEqual([]);
  });

  it("NO surface can read an unvalidated argument bag", () => {
    const bad = violations(SURFACES).map(
      (v) => `${v.file}:${v.line} ${v.id ?? "(anonymous)"} — ${v.violation}`,
    );
    expect(bad).toEqual([]);
  });

  it("every delegated surface is built by a function declared inside src/", () => {
    // A factory imported from node_modules would be a surface no pass reads.
    const orphans = unresolvedDelegates(SURFACES, ROOT).map((d) => `${d.file}:${d.line} ${d.id}`);
    expect(orphans).toEqual([]);
  });

  it("every registry-handler call site binds first (the OTHER guard mechanism)", () => {
    // `registry` surfaces are exempt from the shape rule because their one
    // class of caller binds. That is a claim about call sites, so it is
    // checked at the call sites. An unclassifiable site fails closed.
    const sites = registryHandlerCallSites(ROOT);
    expect(sites.length).toBeGreaterThan(0);
    const unfunnelled = sites
      .filter((s) => s.funnel === null)
      .map((s) => `${s.file}:${s.line} ${s.text}`);
    expect(unfunnelled).toEqual([]);
  });

  it("no fixture surface is reachable from the bundle", () => {
    // The claim that lets `*.test.*` / `*.testkit.*` surfaces be counted but
    // not judged. If an app module ever imports a test module, its fixture
    // actions ARE wire surfaces and this classification is a lie.
    const offenders: string[] = [];
    for (const abs of sourceFiles(join(ROOT, "src"))) {
      const rel = abs
        .slice(ROOT.length + 1)
        .split("\\")
        .join("/");
      if (isFixtureFile(rel)) continue;
      const text = readFileSync(abs, "utf8");
      for (const m of text.matchAll(/from\s+["']([^"']+)["']/g)) {
        if (/\.(test|spec)(\b|$)|\.testkit(\b|$)/.test(m[1])) offenders.push(`${rel} → ${m[1]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("the shape rule has exactly ONE exemption, and it is the guard itself", () => {
    // An exemption list is how an enforcement mechanism dies. One entry, and
    // the enforcement test says so out loud; a second has to be argued for
    // here, in a diff, rather than added quietly to a constant.
    expect(SHAPE_RULE_EXEMPT).toHaveLength(1);
    expect(SHAPE_RULE_EXEMPT[0]).toBe("src/lib/ui-bridge/guardedAction.ts");
    // …and it is exempt because it BINDS, not because it is listed. If the
    // guard module stops calling the binder, the exemption is a hole.
    const guardSource = readFileSync(join(ROOT, SHAPE_RULE_EXEMPT[0]), "utf8");
    expect(guardSource).toContain("bindSchemaBag(");
    for (const builder of GUARD_BUILDERS) {
      expect(guardSource).toContain(`export function ${builder}`);
    }
  });
});

describe("UI Bridge action surfaces — the falsification", () => {
  it(
    "a NEW unguarded component action turns this red",
    () => {
      const found = scanFixture(
        "componentAction.tsx",
        [
          "export const probe = {",
          '  id: "probe-component",',
          '  name: "Probe",',
          "  actions: [",
          "    {",
          '      id: "probe-unguarded",',
          '      paramSchema: { count: "number" },',
          "      handler: async (params?: unknown) => {",
          "        const { count = 1 } = (params ?? {}) as { count?: number };",
          "        return count;",
          "      },",
          "    },",
          "  ],",
          "};",
        ].join("\n"),
      );
      expect(violations(found).map((v) => v.id)).toEqual(["probe-unguarded"]);
    },
    WALK_TIMEOUT,
  );

  it(
    "a NEW unguarded element custom action turns this red",
    () => {
      const found = scanFixture(
        "elementAction.ts",
        [
          "export const descriptor = {",
          '  type: "textarea",',
          "  customActions: {",
          "    probeWrite: {",
          '      id: "probe-write",',
          "      handler: async (params?: unknown) => {",
          "        const { text } = (params || {}) as { text?: string };",
          "        return text;",
          "      },",
          "    },",
          "  },",
          "};",
        ].join("\n"),
      );
      expect(violations(found).map((v) => v.id)).toEqual(["probe-write"]);
    },
    WALK_TIMEOUT,
  );

  it(
    "a NEW unguarded action built by a FACTORY, in no registration position, turns this red",
    () => {
      // The shape pass, alone. This is the position PR #1301's own fix could not
      // have covered by inspection: an action object written in a helper file
      // that no `actions: [...]` array mentions until runtime.
      const found = scanFixture(
        "factoryAction.ts",
        [
          "export function buildProbeAction(effect: (n: number) => void) {",
          "  return {",
          '    id: "probe-factory",',
          '    label: "Probe",',
          "    handler: (params?: unknown) => {",
          "      const { n = 1 } = (params ?? {}) as { n?: number };",
          "      effect(n);",
          "    },",
          "  };",
          "}",
        ].join("\n"),
      );
      expect(violations(found).map((v) => v.id)).toEqual(["probe-factory"]);
    },
    WALK_TIMEOUT,
  );

  it(
    "a handler whose arity cannot be decided fails CLOSED",
    () => {
      const found = scanFixture(
        "opaqueHandler.ts",
        [
          "const impl = async (params?: unknown) => params;",
          "export const action = {",
          '  id: "probe-opaque",',
          "  handler: impl,",
          "};",
        ].join("\n"),
      );
      expect(violations(found).map((v) => v.id)).toEqual(["probe-opaque"]);
    },
    WALK_TIMEOUT,
  );

  it(
    "the walk reaches file classes Vite bundles but a /\\.tsx?$/ walk misses",
    () => {
      // `.jsx`, `.js`, `.mjs` and `.cjs` are all bundled and were all invisible
      // to the sibling chord scanner's walk. An offender in one of them must
      // red this suite, or the enumeration is only an enumeration of `.ts`.
      for (const ext of ["jsx", "js", "mjs", "cjs"]) {
        const found = scanFixture(
          `bundled.${ext}`,
          [
            "export const descriptor = {",
            "  customActions: {",
            "    probeBundled: {",
            `      id: "probe-${ext}",`,
            "      handler: async (params) => params,",
            "    },",
            "  },",
            "};",
          ].join("\n"),
        );
        expect(violations(found).map((v) => v.id)).toEqual([`probe-${ext}`]);
      }
    },
    WALK_TIMEOUT,
  );

  it(
    "a GUARDED surface added the right way does NOT red it",
    () => {
      // The negative control. Without it, a scanner that flags everything would
      // pass every assertion above.
      const found = scanFixture(
        "guardedProbe.ts",
        [
          'import { guardedAction } from "@/lib/ui-bridge/guardedAction";',
          "export const probe = {",
          "  actions: [",
          "    guardedAction({",
          '      id: "probe-guarded",',
          '      paramSchema: { count: "number" },',
          "      run: (args) => args.count,",
          "    }),",
          "  ],",
          "};",
        ].join("\n"),
      );
      expect(violations(found)).toEqual([]);
      expect(found.map((s) => [s.form, s.id])).toContainEqual(["guarded", "probe-guarded"]);
    },
    WALK_TIMEOUT,
  );
});

describe("UI Bridge action surfaces — the inventory", () => {
  it("matches the checked-in golden", () => {
    const rendered = renderInventory(SURFACES);
    if (process.env.UPDATE_ACTION_SURFACE_GOLDEN === "1") {
      mkdirSync(join(ROOT, "src", "lib", "ui-bridge", "__golden__"), { recursive: true });
      writeFileSync(GOLDEN, rendered, "utf8");
    }
    expect(existsSync(GOLDEN)).toBe(true);
    // A diff here is not a failure to fix by re-generating without reading it:
    // it is the list of action surfaces this app exposes to the wire, and a
    // new line in it is a new thing an agent can invoke.
    expect(rendered).toBe(readFileSync(GOLDEN, "utf8").split("\r\n").join("\n"));
  });
});

/**
 * Files that TEXTUALLY look like they register an action, found without the
 * AST — the independent second opinion on the walk.
 */
function textualCandidates(): string[] {
  const out: string[] = [];
  for (const abs of sourceFiles(join(ROOT, "src"))) {
    const rel = abs
      .slice(ROOT.length + 1)
      .split("\\")
      .join("/");
    if (rel.startsWith("src/__action_surface_probe__")) continue;
    const text = readFileSync(abs, "utf8");
    if (!/\bhandler\s*:/.test(text)) continue;
    if (!/\bactions\s*:\s*\[|\bcustomActions\s*:/.test(text)) continue;
    out.push(rel);
  }
  return out;
}
