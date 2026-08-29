/**
 * Tier-1 resolver + arg parser tests.
 *
 * Runs under vitest's `environment: "node"` (no jsdom). Each test
 * registers a small synthetic action set against the live registry
 * (resetting between cases via `__resetForTest`) and exercises the
 * resolver / parser as the CommandBar will see them.
 */

import { afterEach, describe, expect, it } from "vitest";

import {
  applyDeclaredFlags,
  parseArgs,
  tokenize,
  tokenizeRich,
  coerceToken,
  extractFlags,
  readTextArg,
  textArg,
} from "./parse";
import { __resetForTest, register } from "./registry";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

const action = (overrides: Partial<CommandAction> = {}): CommandAction => ({
  id: overrides.id ?? "test.action",
  slash: overrides.slash ?? "/test",
  label: overrides.label ?? "Test action",
  description: overrides.description ?? "for the resolver spec",
  handler: overrides.handler ?? (async () => ({ ok: true, value: "ran" })),
  ...overrides,
});

describe("resolve — empty input", () => {
  it("returns all actions (capped at MAX_SUGGESTIONS) in registration order when no recents", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "b", slash: "/b", label: "B" }));
    const matches = resolve("", []);
    expect(matches.map((m) => m.action.id)).toEqual(["a", "b"]);
    expect(matches.every((m) => !m.exact)).toBe(true);
    expect(matches.every((m) => !m.recent)).toBe(true);
  });

  it("places recents first, in last-used order", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "b", slash: "/b", label: "B" }));
    register(action({ id: "c", slash: "/c", label: "C" }));
    const matches = resolve("", ["c", "a"]);
    expect(matches.map((m) => m.action.id)).toEqual(["c", "a", "b"]);
    expect(matches[0].recent).toBe(true);
    expect(matches[1].recent).toBe(true);
    expect(matches[2].recent).toBe(false);
  });

  it("ignores recents that refer to unknown ids", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    const matches = resolve("", ["ghost", "a"]);
    expect(matches.map((m) => m.action.id)).toEqual(["a"]);
  });
});

describe("resolve — exact slash hit", () => {
  it("returns only the matching action when the first token matches a slash", () => {
    register(action({ id: "swap", slash: "/swap", label: "Swap zones" }));
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    const matches = resolve("/swap 1 2", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("swap");
    expect(matches[0].exact).toBe(true);
  });

  it("works without a leading slash on the input", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    const matches = resolve("spawn 3", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("spawn");
    expect(matches[0].exact).toBe(true);
  });

  it("matches via alias", () => {
    register(
      action({
        id: "spawn-ai",
        slash: "/spawn-ai",
        aliases: ["/spawn-best"],
      }),
    );
    const matches = resolve("/spawn-best 3", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("spawn-ai");
    expect(matches[0].exact).toBe(true);
  });

  it("flags recent for exact hits too", () => {
    register(action({ id: "swap", slash: "/swap" }));
    const matches = resolve("/swap", ["swap"]);
    expect(matches[0].recent).toBe(true);
  });

  // `literal` is what lets the CommandBar rank an exact hit ABOVE a Tier-2
  // regex without also handing `spawn 3 best` (English) to `/spawn`. The
  // resolver matches both spellings; only one of them is the operator
  // naming the command outright.
  it("marks a LEADING-SLASH exact hit as literal", () => {
    register(action({ id: "spawn-ai", slash: "/spawn-ai" }));
    expect(resolve("/spawn-ai 1 gmail --tenant=", [])[0].literal).toBe(true);
  });

  it("marks a slashless exact hit as NOT literal", () => {
    register(action({ id: "spawn", slash: "/spawn" }));
    expect(resolve("spawn 3 best", [])[0].literal).toBe(false);
  });

  it("marks a literal ALIAS hit as literal too", () => {
    register(action({ id: "spawn-ai", slash: "/spawn-ai", aliases: ["/spawn-best"] }));
    expect(resolve("/spawn-best 1 gmail", [])[0].literal).toBe(true);
  });

  it("never marks a fuzzy or empty-input row as literal", () => {
    register(action({ id: "layout", slash: "/layout", label: "Change layout preset" }));
    expect(resolve("", []).every((m) => !m.literal)).toBe(true);
    expect(resolve("lyt", []).every((m) => !m.literal)).toBe(true);
  });
});

describe("resolve — fuzzy match", () => {
  it("returns nothing for no-match queries", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    expect(resolve("/xyz", [])).toEqual([]);
  });

  it("prefers prefix matches over sequential fuzzy", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    register(action({ id: "swap", slash: "/swap", label: "Swap zones" }));
    const matches = resolve("/sp", []);
    expect(matches[0].action.id).toBe("spawn");
  });

  it("pulls recents above same-score peers", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn plain" }));
    register(action({ id: "spawn-ai", slash: "/spawn-ai", label: "Spawn AI" }));
    const matches = resolve("/sp", ["spawn-ai"]);
    // Both are prefix matches with the same score — recent wins.
    expect(matches[0].action.id).toBe("spawn-ai");
  });

  it("does NOT let a recent outrank a strictly better score", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn plain terminal" }));
    register(action({ id: "swap", slash: "/swap", label: "Swap two zones" }));
    // `/sw` is a PREFIX hit on /swap and only a word-boundary hit on
    // /spawn. Recency must not promote the worse match — that is what
    // made Tab complete `/spawn` for an operator typing `/sw`.
    const matches = resolve("/sw", ["spawn"]);
    expect(matches[0].action.id).toBe("swap");
  });

  it("ranks a SLASH word-boundary hit above a LABEL sequential hit", () => {
    // Live UI-Bridge regression: typing `lyt` and pressing Tab completed
    // `/analyze`. `/layout`'s slash body is a word-boundary (Tier-2) hit,
    // but "Analyze terminal output" was a Tier-3 hit on the LABEL and the
    // two tiers' score bands overlapped, so the label won. Iteration 1
    // demoted recency below score, which did not touch this half.
    register(action({ id: "analyze", slash: "/analyze", label: "Analyze terminal output" }));
    register(action({ id: "layout", slash: "/layout", label: "Switch zone layout" }));
    const matches = resolve("lyt", []);
    expect(matches[0].action.id).toBe("layout");
  });

  it("breaks a WITHIN-tier slash/label tie toward the slash", () => {
    // Re-banding the scorer made a slash hit outrank a label hit ACROSS
    // tiers. It said nothing about a tie WITHIN one: here both actions
    // take a word-boundary hit at the identical score — one on its SLASH
    // body, one only on its LABEL — and the winner used to be whichever
    // was registered first. The label-only action is registered FIRST on
    // purpose, so registration order would give the wrong answer.
    register(action({ id: "label-only", slash: "/zzz", label: "Restart something" }));
    register(action({ id: "restart", slash: "/restart", label: "Restart session in zone" }));
    const matches = resolve("rst", []);
    expect(matches[0].action.id).toBe("restart");
  });

  it("puts the slash tiebreak ABOVE recency", () => {
    // Recency is the weakest signal: which field matched is part of match
    // quality, and iteration 1 already demoted recency below quality.
    register(action({ id: "label-only", slash: "/zzz", label: "Restart something" }));
    register(action({ id: "restart", slash: "/restart", label: "Restart session in zone" }));
    const matches = resolve("rst", ["label-only"]);
    expect(matches[0].action.id).toBe("restart");
  });

  it("keeps the shipped registry's three tie queries on their slash", () => {
    // The queries that tie in the live registry, each with a decoy whose
    // LABEL scores identically. Decoys are registered first so the
    // pre-fix registration-order fallback would pick them.
    register(action({ id: "decoy-rst", slash: "/zz1", label: "Restart something" }));
    register(action({ id: "restart", slash: "/restart", label: "Restart session in zone" }));
    register(action({ id: "decoy-fnd", slash: "/zz2", label: "Find node data" }));
    register(action({ id: "findings", slash: "/findings", label: "Toggle findings panel" }));
    register(action({ id: "decoy-ntf", slash: "/zz3", label: "Notify test flags" }));
    register(
      action({ id: "notify", slash: "/desktop-notify", label: "Toggle desktop notifications" }),
    );
    expect(resolve("rst", [])[0].action.id).toBe("restart");
    expect(resolve("fnd", [])[0].action.id).toBe("findings");
    expect(resolve("ntf", [])[0].action.id).toBe("notify");
  });

  it("does NOT let the slash tiebreak beat a strictly better label score", () => {
    // The tiebreak is within-tier only — it must never promote a worse
    // match, which is exactly the failure mode the recency demotion
    // fixed. `/xlay` is a Tier-3 sequential hit on its SLASH; "Layout
    // everything" is a Tier-1 prefix hit on a LABEL. Score wins.
    register(action({ id: "seq-slash", slash: "/xlay", label: "Nothing relevant" }));
    register(action({ id: "prefix-label", slash: "/zzz", label: "Layout everything" }));
    const matches = resolve("lay", []);
    expect(matches[0].action.id).toBe("prefix-label");
  });

  it("caps to 8 suggestions", () => {
    for (let i = 0; i < 12; i++) {
      register(action({ id: `n${i}`, slash: `/n${i}`, label: `N${i}` }));
    }
    expect(resolve("/n", [])).toHaveLength(8);
  });
});

describe("parse — tokenize", () => {
  it("splits on whitespace", () => {
    expect(tokenize("a b c")).toEqual(["a", "b", "c"]);
  });

  it("treats quoted runs as one token", () => {
    expect(tokenize('3 best "fix the failing test"')).toEqual([
      "3",
      "best",
      "fix the failing test",
    ]);
  });

  it("collapses multiple spaces", () => {
    expect(tokenize("a    b\tc")).toEqual(["a", "b", "c"]);
  });

  it("returns empty for empty input", () => {
    expect(tokenize("")).toEqual([]);
    expect(tokenize("   ")).toEqual([]);
  });
});

describe("parse — coerceToken", () => {
  it("coerces clean integers to number", () => {
    expect(coerceToken("3")).toBe(3);
    expect(coerceToken("-7")).toBe(-7);
  });

  it("coerces decimals to number", () => {
    expect(coerceToken("1.5")).toBe(1.5);
  });

  it("leaves non-numeric tokens as string", () => {
    expect(coerceToken("best")).toBe("best");
    expect(coerceToken("3a")).toBe("3a");
    expect(coerceToken("")).toBe("");
  });
});

describe("parse — parseArgs", () => {
  const spawnAi = action({
    id: "spawn-ai",
    slash: "/spawn-ai",
    paramSchema: { count: "n", account: "s", context: "s" },
  });

  it("maps positional tokens to paramSchema field order", () => {
    expect(parseArgs("/spawn-ai 3 best", spawnAi)).toEqual({
      count: 3,
      account: "best",
    });
  });

  it("joins trailing tokens into the last field (free-form catch-all)", () => {
    expect(parseArgs("/spawn-ai 3 best fix the failing test", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the failing test",
    });
  });

  it("honors quoted runs as a single token", () => {
    expect(parseArgs('/spawn-ai 3 best "fix the failing test"', spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the failing test",
    });
  });

  it("returns empty when no args follow the slash", () => {
    expect(parseArgs("/spawn-ai", spawnAi)).toEqual({});
    expect(parseArgs("/spawn-ai ", spawnAi)).toEqual({});
  });

  it("returns empty for actions with no paramSchema", () => {
    const noSchema = action({ id: "approve-all", slash: "/approve-all" });
    expect(parseArgs("/approve-all ignored", noSchema)).toEqual({});
  });
});

// F3 — `/spawn-ai --tenant <slug|uuid>`. The contract that matters is that a
// declared flag NEVER disturbs positional binding, in any position, and is
// never swallowed by the free-form catch-all tail.
describe("parse — declared --flags", () => {
  const spawnAi = action({
    id: "spawn-ai",
    slash: "/spawn-ai",
    paramSchema: { count: "n", account: "s", context: "s", "--tenant": "s" },
  });

  it("extracts `--tenant value` without shifting the positional fields", () => {
    expect(parseArgs("/spawn-ai 3 best --tenant pizzeria fix the bug", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the bug",
      tenant: "pizzeria",
    });
  });

  it("extracts the `--tenant=value` form", () => {
    expect(parseArgs("/spawn-ai 3 best --tenant=pizzeria", spawnAi)).toEqual({
      count: 3,
      account: "best",
      tenant: "pizzeria",
    });
  });

  it("accepts the flag AFTER the free-form context without eating the prompt", () => {
    expect(parseArgs("/spawn-ai 3 best fix the bug --tenant pizzeria", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the bug",
      tenant: "pizzeria",
    });
  });

  it("leaves the positional shape unchanged when the flag is absent", () => {
    expect(parseArgs("/spawn-ai 3 best fix the bug", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the bug",
    });
  });

  it("keeps an UNDECLARED --flag as ordinary prompt text", () => {
    expect(parseArgs("/spawn-ai 3 best rerun with --verbose set", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "rerun with --verbose set",
    });
  });

  // A declared flag typed with NO value is SUPPLIED-AND-EMPTY, which is the
  // same state as `--tenant=` and must read as the same state. It used to
  // fall through to positional binding, so `/spawn-ai 3 best --tenant` bound
  // `context: "--tenant"`: the tenant read back ABSENT (silently spawning
  // under the device default) and the flag text was typed into the new
  // session as its prompt. Binding "" is what lets `resolveText` say
  // "tenant was supplied but empty".
  it("binds a value-less declared flag as SUPPLIED-but-empty, not as prompt text", () => {
    expect(parseArgs("/spawn-ai 3 best --tenant", spawnAi)).toEqual({
      count: 3,
      account: "best",
      tenant: "",
    });
  });

  it("does not eat a following declared flag as the value of the one before it", () => {
    const twoFlags = action({
      id: "two-flags",
      slash: "/two-flags",
      paramSchema: { count: "n", "--tenant": "s", "--zone": "n" },
    });
    expect(parseArgs("/two-flags 3 --tenant --zone 4", twoFlags)).toEqual({
      count: 3,
      tenant: "",
      zone: 4,
    });
  });
});

// The PRESET-ROUTE half. `parseArgs` runs on the slash route only;
// `applyDeclaredFlags` runs on every PRESET route's args, which is what
// stops a Tier-2 pattern with a `(?<context>.+)` tail from swallowing a
// declared flag whole. D1 of manual-test-loop iteration 7: `/spawn-ai 1
// gmail --tenant=` spawned under the device default while `/spawn-best 1
// gmail --tenant=` — same action, same args — correctly refused, split
// purely on which regex matched.
//
// The `origin` argument is D1 of iteration 8. Applying the scrub to the
// PARSED route too re-tokenized text `parseArgs` had already stripped of
// its quotes, so a `--tenant` the operator had QUOTED into their prompt
// read as a bare flag and ate the word after it.
describe("parse — applyDeclaredFlags", () => {
  const spawnAi = action({
    id: "spawn-ai",
    slash: "/spawn-ai",
    paramSchema: { count: "n", account: "s", context: "s", "--tenant": "s" },
  });
  const noFlags = action({
    id: "spawn",
    slash: "/spawn",
    paramSchema: { count: "n" },
  });

  it("extracts nothing for an action declaring no flags", () => {
    const args = { count: 3 };
    expect(applyDeclaredFlags(args, "/spawn 3 --tenant=x", noFlags, "preset")).toEqual(args);
  });

  it("still resolves QUOTING for an action declaring no flags", () => {
    // Found by re-deriving the resolution delta, not by reasoning about the
    // change: gating the quote resolution on "declares a flag" left every
    // no-flag action's Tier-2 group spelled with its quotes still in it.
    const tag = action({ id: "tag", slash: "/tag", paramSchema: { tag: "s" } });
    expect(applyDeclaredFlags({ tag: '"--tenant"' }, '/tag "--tenant"', tag, "preset")).toEqual({
      tag: "--tenant",
    });
    const orch = action({ id: "orch", slash: "/orchestrate", paramSchema: { goal: "s" } });
    expect(
      applyDeclaredFlags({ goal: '"fix the thing"' }, '/orchestrate "fix the thing"', orch, "preset"),
    ).toEqual({ goal: "fix the thing" });
    // Spacing INSIDE a quoted run is the operator's, and survives.
    expect(
      applyDeclaredFlags(
        { goal: '"fix  the   thing"' },
        '/orchestrate "fix  the   thing"',
        orch,
        "preset",
      ),
    ).toEqual({ goal: "fix  the   thing" });
  });

  it("recovers an EMPTY flag a Tier-2 catch-all swallowed into `context`", () => {
    // What `matchPattern` binds for `spawn-ai 1 gmail --tenant=`.
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", context: "--tenant=" },
        "/spawn-ai 1 gmail --tenant=",
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 1, account: "gmail", tenant: "" });
  });

  it("recovers a value-less flag the catch-all swallowed", () => {
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", context: "--tenant" },
        "/spawn-ai 1 gmail --tenant",
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 1, account: "gmail", tenant: "" });
  });

  it("recovers a dotted value and strips it from the prompt", () => {
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", context: "--tenant=a.b fix the bug" },
        "/spawn-ai 1 gmail --tenant=a.b fix the bug",
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 1, account: "gmail", context: "fix the bug", tenant: "a.b" });
  });

  it("recovers the space form and keeps the rest of the prompt in order", () => {
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", context: "fix the bug --tenant pizzeria now" },
        "/spawn-ai 1 gmail fix the bug --tenant pizzeria now",
        spawnAi,
        "preset",
      ),
    ).toEqual({
      count: 1,
      account: "gmail",
      context: "fix the bug now",
      tenant: "pizzeria",
    });
  });

  it("leaves an UNDECLARED --flag in the prompt", () => {
    const args = { count: 1, account: "gmail", context: "rerun with --verbose set" };
    expect(
      applyDeclaredFlags(args, "/spawn-ai 1 gmail rerun with --verbose set", spawnAi, "preset"),
    ).toEqual(args);
  });

  // ── D1, iteration 8 ────────────────────────────────────────────────
  // The parsed route is a NO-OP, not "idempotent". `parseArgs` extracted
  // the flags from the raw input while the quoting was still there, which
  // is strictly more than this function can recover afterwards.
  it("is a NO-OP on the parsed route, where parseArgs already extracted", () => {
    const input = "/spawn-ai 3 best --tenant pizzeria fix the bug";
    const once = parseArgs(input, spawnAi);
    expect(applyDeclaredFlags(once, input, spawnAi, "parsed")).toBe(once);
  });

  it("keeps a quoted prompt BYTE-INTACT on the parsed route WITH a top-level flag", () => {
    // Measured on-page and lost: the status line read `/spawn-ai ✓` with the
    // right tenant while the session was typed `fix the` — the scrub had
    // re-read the operator's quoted `--tenant` as a flag and eaten
    // `handling`, and the top-level tenant then overwrote the swallowed one
    // so nothing in the verdict could show it.
    const input = '/spawn-ai 1 gmail --tenant=2299 "fix the --tenant handling"';
    const once = parseArgs(input, spawnAi);
    expect(once).toEqual({
      count: 1,
      account: "gmail",
      context: "fix the --tenant handling",
      tenant: 2299,
    });
    expect(applyDeclaredFlags(once, input, spawnAi, "parsed")).toEqual(once);
  });

  it("leaves a QUOTED --flag inside a prompt alone on BOTH routes", () => {
    const input = '/spawn-ai 3 best "fix the --tenant handling"';
    const once = parseArgs(input, spawnAi);
    expect(once).toEqual({ count: 3, account: "best", context: "fix the --tenant handling" });
    expect(applyDeclaredFlags(once, input, spawnAi, "parsed")).toBe(once);
    // The Tier-2 group is a slice of the RAW input, so it still carries the
    // quote characters; the preset route resolves them and finds no flag.
    expect(
      applyDeclaredFlags(
        { count: 3, account: "best", context: '"fix the --tenant handling"' },
        input,
        spawnAi,
        "preset",
      ),
    ).toEqual(once);
  });

  it("strips ONLY the run the operator typed as a flag", () => {
    // The prompt's own `--tenant=other` is not the flag that was found, so
    // exact-run matching leaves it where it is.
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", context: '--tenant=2299 "about --tenant=other"' },
        '/spawn-ai 1 gmail --tenant=2299 "about --tenant=other"',
        spawnAi,
        "preset",
      ),
    ).toEqual({
      count: 1,
      account: "gmail",
      context: "about --tenant=other",
      tenant: 2299,
    });
  });

  it("resolves a Tier-2 group's quoting even when no flag was typed", () => {
    // Without this the quote characters were typed verbatim into the spawned
    // session, which the slash route never did for the same input.
    expect(
      applyDeclaredFlags(
        { count: 3, account: "best", context: '"fix the failing test"' },
        'spawn-ai 3 best "fix the failing test"',
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 3, account: "best", context: "fix the failing test" });
  });

  it("keeps an ALREADY-EMPTY string field supplied-but-empty", () => {
    // Dropping the key is only correct when a flag ATE the field. A field
    // that arrived empty must keep reading as supplied-and-empty, or the
    // handler gets back the "absent, so guess" arm.
    expect(
      applyDeclaredFlags(
        { count: 1, account: "", context: "hi" },
        "/spawn-ai 1 --tenant=x hi",
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 1, account: "", context: "hi", tenant: "x" });
  });

  it("keeps a Tier-3 binding the raw input never spelled as a flag", () => {
    expect(
      applyDeclaredFlags(
        { count: 1, account: "gmail", tenant: "pizzeria" },
        "spin up one gmail session for the pizzeria tenant",
        spawnAi,
        "preset",
      ),
    ).toEqual({ count: 1, account: "gmail", tenant: "pizzeria" });
  });
});

// ── D2, iteration 8 ──────────────────────────────────────────────────
// A quoted run that is ENTIRELY a declared flag is prompt text. `tokenize`
// stripped the quotes before `extractFlags` ever saw them, so a one-word
// quoted run was indistinguishable from a top-level flag: `/spawn-ai 1
// gmail "--tenant"` answered "tenant was supplied but empty" and spawned
// nothing. `"a --tenant=x b"` was already safe — the protection held only
// while the quoted run had other words in it.
describe("parse — quoting is carried, not inferred", () => {
  const spawnAi = action({
    id: "spawn-ai",
    slash: "/spawn-ai",
    paramSchema: { count: "n", account: "s", context: "s", "--tenant": "s" },
  });

  it("reads a wholly-quoted declared flag as prompt text", () => {
    const input = '/spawn-ai 1 gmail "--tenant"';
    expect(parseArgs(input, spawnAi)).toEqual({
      count: 1,
      account: "gmail",
      context: "--tenant",
    });
    expect(applyDeclaredFlags(parseArgs(input, spawnAi), input, spawnAi, "parsed")).toEqual({
      count: 1,
      account: "gmail",
      context: "--tenant",
    });
  });

  it("still reads an UNQUOTED bare declared flag as a flag", () => {
    expect(parseArgs("/spawn-ai 1 gmail --tenant", spawnAi)).toEqual({
      count: 1,
      account: "gmail",
      tenant: "",
    });
  });

  it("accepts a QUOTED value for a flag", () => {
    expect(parseArgs('/spawn-ai 1 gmail --tenant "my org" go', spawnAi)).toEqual({
      count: 1,
      account: "gmail",
      context: "go",
      tenant: "my org",
    });
  });

  it("keeps a quoted VALUE on `--name=` a flag — the quote opens AFTER the name", () => {
    expect(parseArgs('/spawn-ai 1 gmail --tenant="my org" go', spawnAi)).toEqual({
      count: 1,
      account: "gmail",
      context: "go",
      tenant: "my org",
    });
  });

  it("reports quoting on tokenizeRich and hides it on tokenize", () => {
    expect(tokenizeRich('a "b c" --d')).toEqual([
      { text: "a", quoted: false },
      { text: "b c", quoted: true },
      { text: "--d", quoted: false },
    ]);
    expect(tokenize('a "b c" --d')).toEqual(["a", "b c", "--d"]);
  });
});

describe("parse — extractFlags", () => {
  it("returns tokens untouched when the schema declares no flags", () => {
    expect(extractFlags(["3", "best", "--tenant", "x"], ["count", "account"])).toEqual({
      flags: {},
      hits: [],
      rest: ["3", "best", "--tenant", "x"],
    });
  });

  it("coerces numeric flag values like positional tokens do", () => {
    expect(extractFlags(["--zone", "4"], ["--zone"])).toEqual({
      flags: { zone: 4 },
      hits: [{ name: "zone", value: 4, consumed: ["--zone", "4"] }],
      rest: [],
    });
  });
});

describe("parse — an empty QUOTED run is an empty ARGUMENT", () => {
  /**
   * D8 (manual-test-loop iteration 10). `stripFlagRuns` tokenizes `""` to
   * zero tokens and reported `removed: 0`, so `applyDeclaredFlags` restored
   * the RAW text — two literal quote characters. `/orchestrate ""` then
   * actually POSTed `start_orchestration_run({goal: '""'})`, spending a
   * conductor run, while `/orchestrate " "` correctly refused.
   */
  const noFlags = (id: string): CommandAction => ({
    id,
    slash: `/${id}`,
    label: id,
    description: id,
    paramSchema: { goal: "s" },
    handler: async () => ({ ok: true as const }),
  });

  it('resolves `""` to the empty string, not to two quote characters', () => {
    const a = noFlags("orchestrate");
    expect(applyDeclaredFlags({ goal: '""' }, '/orchestrate ""', a, "preset")).toEqual({
      goal: "",
    });
  });

  it('reads the same as `" "` and as the unquoted empty tail', () => {
    const a = noFlags("orchestrate");
    const quoted = applyDeclaredFlags({ goal: '""' }, '/orchestrate ""', a, "preset");
    const spaced = applyDeclaredFlags({ goal: " " }, '/orchestrate " "', a, "preset");
    expect(textArg(quoted, "goal")).toBe(textArg(spaced, "goal"));
    expect(readTextArg(quoted, "goal").kind).toBe("invalid");
    expect(readTextArg(spaced, "goal").kind).toBe("invalid");
  });

  it("keeps supplied-and-empty SUPPLIED — it does not drop the key", () => {
    const a = noFlags("tag");
    const out = applyDeclaredFlags({ goal: '""' }, '/tag ""', a, "preset");
    expect("goal" in out).toBe(true);
  });

  it("still DROPS a field whose whole content was a declared flag run", () => {
    const a: CommandAction = {
      id: "spawn-ai",
      slash: "/spawn-ai",
      label: "s",
      description: "s",
      paramSchema: { count: "n", context: "s", "--tenant": "s" },
      handler: async () => ({ ok: true as const }),
    };
    const out = applyDeclaredFlags(
      { count: 1, context: "--tenant 2299" },
      "/spawn-ai 1 --tenant 2299",
      a,
      "preset",
    );
    expect("context" in out).toBe(false);
    expect(out.tenant).toBe(2299);
  });
});
