import { describe, expect, it } from "vitest";

import type { PromptParameter } from "./promptLibraryApi";
import {
  DEFAULT_MAX_MEMBERS,
  MAX_PROMPT_ARGV_COST,
  MAX_TITLE_CHARS,
  promptArgvCost,
  SERVER_MAX_MEMBERS,
  serverTrim,
  utf8ByteLength,
  expandMatrix,
  parseMatrix,
  planFanout,
  templatePlaceholders,
  type MatrixAxis,
} from "./promptMatrix";

function param(overrides: Partial<PromptParameter> & { name: string }): PromptParameter {
  return { type: "string", label: overrides.name, description: "", required: false, ...overrides };
}

function axes(input: string): MatrixAxis[] {
  const r = parseMatrix(input);
  if (!r.ok) throw new Error(`parse failed: ${JSON.stringify(r.error)}`);
  return r.axes;
}

describe("parseMatrix", () => {
  it("parses axes and trims whitespace", () => {
    expect(parseMatrix(" platform : iOS , Android ; lang:swift,kotlin")).toEqual({
      ok: true,
      axes: [
        { name: "platform", values: ["iOS", "Android"] },
        { name: "lang", values: ["swift", "kotlin"] },
      ],
    });
  });

  it("refuses an empty matrix", () => {
    expect(parseMatrix("   ")).toEqual({ ok: false, error: { kind: "empty_matrix" } });
  });

  it("refuses an empty axis segment, including a trailing ;", () => {
    expect(parseMatrix("a:1;;b:2")).toEqual({
      ok: false,
      error: { kind: "empty_axis", segment: 1 },
    });
    expect(parseMatrix("a:1;")).toEqual({ ok: false, error: { kind: "empty_axis", segment: 1 } });
  });

  it("refuses a segment with no name/value separator", () => {
    expect(parseMatrix("a:1;oops")).toEqual({
      ok: false,
      error: { kind: "missing_separator", segment: 1, text: "oops" },
    });
  });

  it("refuses an empty or non-placeholder axis name", () => {
    expect(parseMatrix(":1,2")).toEqual({
      ok: false,
      error: { kind: "empty_axis_name", segment: 0 },
    });
    expect(parseMatrix("my axis:1")).toEqual({
      ok: false,
      error: { kind: "invalid_axis_name", segment: 0, name: "my axis" },
    });
  });

  it("refuses a duplicate axis name", () => {
    expect(parseMatrix("a:1;a:2")).toEqual({
      ok: false,
      error: { kind: "duplicate_axis", name: "a" },
    });
  });

  it("refuses an empty value", () => {
    expect(parseMatrix("a:1,,2")).toEqual({
      ok: false,
      error: { kind: "empty_value", axis: "a", position: 1 },
    });
    expect(parseMatrix("a:")).toEqual({
      ok: false,
      error: { kind: "empty_value", axis: "a", position: 0 },
    });
  });
});

describe("expandMatrix", () => {
  const twoByTwo = axes("platform:iOS,Android;lang:swift,kotlin");

  it("defaults to zip", () => {
    expect(expandMatrix(twoByTwo)).toEqual({
      ok: true,
      members: [
        { platform: "iOS", lang: "swift" },
        { platform: "Android", lang: "kotlin" },
      ],
    });
  });

  it("expands the cartesian product under product, first axis slowest", () => {
    expect(expandMatrix(twoByTwo, "product")).toEqual({
      ok: true,
      members: [
        { platform: "iOS", lang: "swift" },
        { platform: "iOS", lang: "kotlin" },
        { platform: "Android", lang: "swift" },
        { platform: "Android", lang: "kotlin" },
      ],
    });
  });

  it("errors on unequal zip lengths, naming every axis, never truncating", () => {
    expect(expandMatrix(axes("a:1,2,3;b:x,y"), "zip")).toEqual({
      ok: false,
      error: {
        kind: "unequal_zip_lengths",
        axes: [
          { name: "a", length: 3 },
          { name: "b", length: 2 },
        ],
      },
    });
  });

  it("accepts unequal lengths under product", () => {
    const r = expandMatrix(axes("a:1,2,3;b:x,y"), "product");
    expect(r.ok && r.members.length).toBe(6);
  });

  it("refuses an expansion over the default ceiling", () => {
    // 5 × 5 = 25 > 24
    const r = expandMatrix(axes("a:1,2,3,4,5;b:1,2,3,4,5"), "product");
    expect(r).toEqual({
      ok: false,
      error: { kind: "too_many_members", count: 25, max: DEFAULT_MAX_MEMBERS },
    });
  });

  it("allows exactly the ceiling, and the ceiling is parameterizable", () => {
    const sixByFour = axes("a:1,2,3,4,5,6;b:1,2,3,4");
    const ok = expandMatrix(sixByFour, "product");
    expect(ok.ok && ok.members.length).toBe(24);
    expect(expandMatrix(sixByFour, "product", 10)).toEqual({
      ok: false,
      error: { kind: "too_many_members", count: 24, max: 10 },
    });
  });

  it("refuses a huge product without materialising it", () => {
    const big: MatrixAxis[] = Array.from({ length: 10 }, (_, i) => ({
      name: `a${i}`,
      values: Array.from({ length: 10 }, (_, j) => String(j)),
    }));
    const r = expandMatrix(big, "product");
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toEqual({ kind: "too_many_members", count: 1e10, max: 24 });
  });

  it("errors with no axes", () => {
    expect(expandMatrix([])).toEqual({ ok: false, error: { kind: "no_axes" } });
  });
});

describe("templatePlaceholders", () => {
  it("lists distinct names with the renderer's grammar", () => {
    expect(templatePlaceholders("{{a}} {{ b }} {{a}} {single} {{c.d-e}}")).toEqual([
      "a",
      "b",
      "c.d-e",
    ]);
  });
});

describe("planFanout", () => {
  const members = [
    { platform: "iOS", lang: "swift" },
    { platform: "Android", lang: "kotlin" },
  ];

  it("renders prompt and title per member with fixed values merged", () => {
    const rows = planFanout(
      {
        body: "Port {{feature}} to {{platform}} in {{lang}}.",
        parameters: [param({ name: "feature", required: true })],
      },
      { feature: "login" },
      members,
      "{{feature}} — {{platform}}",
    );
    expect(rows.map((r) => [r.index, r.title, r.prompt])).toEqual([
      [0, "login — iOS", "Port login to iOS in swift."],
      [1, "login — Android", "Port login to Android in kotlin."],
    ]);
    expect(rows[0].values).toEqual({ feature: "login", platform: "iOS", lang: "swift" });
    for (const r of rows) {
      expect(r.missing).toEqual([]);
      expect(r.errors).toEqual([]);
      expect(r.warnings).toEqual([]);
    }
  });

  it("lets a matrix value override a fixed value of the same name", () => {
    const rows = planFanout(
      { body: "{{platform}}", parameters: [] },
      { platform: "web" },
      members,
      "t",
    );
    expect(rows.map((r) => r.prompt)).toEqual(["iOS", "Android"]);
  });

  it("reports unfilled required params via missingRequired", () => {
    const rows = planFanout(
      { body: "{{goal}} on {{platform}}", parameters: [param({ name: "goal", required: true })] },
      { goal: "  " },
      members,
      "{{platform}}",
    );
    expect(rows.map((r) => r.missing)).toEqual([["goal"], ["goal"]]);
  });

  it("reports undeclared placeholders with no value as missing instead of letting them vanish", () => {
    const rows = planFanout(
      { body: "{{platform}} {{typo_var}} {{note}}", parameters: [param({ name: "note" })] },
      {},
      members,
      "{{platform}}",
    );
    // `note` is a declared optional param: "" is its documented rendering.
    expect(rows[0].missing).toEqual(["typo_var"]);
    expect(rows[0].prompt).toBe("iOS  ");
  });

  it("warns on every row when the body references no matrix variable", () => {
    const rows = planFanout({ body: "Do the thing.", parameters: [] }, {}, members, "{{platform}}");
    expect(rows.map((r) => r.title)).toEqual(["iOS", "Android"]);
    for (const r of rows) {
      expect(r.warnings).toEqual([
        { kind: "identical_prompts", count: 2, message: "all 2 prompts are identical" },
      ]);
    }
  });

  it("warns when an axis is referenced but every prompt still renders identical", () => {
    const rows = planFanout(
      { body: "Same {{lang}}", parameters: [] },
      {},
      [
        { platform: "iOS", lang: "x" },
        { platform: "Android", lang: "x" },
      ],
      "{{platform}}",
    );
    expect(rows[0].warnings.map((w) => w.kind)).toEqual(["identical_prompts"]);
  });

  it("does not warn identical for a single member", () => {
    const rows = planFanout({ body: "static", parameters: [] }, {}, [{ a: "1" }], "t");
    expect(rows[0].warnings).toEqual([]);
  });

  it("is a typed row error when a rendered prompt exceeds the argv bound", () => {
    const long = "x".repeat(MAX_PROMPT_ARGV_COST);
    const rows = planFanout(
      { body: "{{blob}}{{platform}}", parameters: [] },
      { blob: long },
      members,
      "{{platform}}",
    );
    // 3 platform chars + the 2 wrapping quotes.
    expect(rows[0].errors).toEqual([
      { kind: "prompt_too_long", cost: MAX_PROMPT_ARGV_COST + 5, max: MAX_PROMPT_ARGV_COST },
    ]);
    // Not truncated.
    expect(rows[0].prompt.length).toBe(MAX_PROMPT_ARGV_COST + 3);
  });

  it("measures the prompt bound in UTF-8 bytes, not UTF-16 units", () => {
    // "é" is 1 UTF-16 unit but 2 UTF-8 bytes: 13,000 of them is 13,000
    // `.length` (under the bound) yet 26,000 bytes (over it).
    const accented = "é".repeat(13_000);
    expect(accented.length).toBeLessThan(MAX_PROMPT_ARGV_COST);
    expect(utf8ByteLength(accented)).toBe(26_000);
    const rows = planFanout({ body: "{{blob}}", parameters: [] }, { blob: accented }, [{}], "t");
    expect(rows[0].errors).toEqual([
      { kind: "prompt_too_long", cost: 26_002, max: MAX_PROMPT_ARGV_COST },
    ]);
  });

  it("counts Windows quoting, so a quote-heavy prompt cannot pass the preview and fail at spawn", () => {
    // The same numbers `fanout/model.rs` pins for `prompt_argv_cost`.
    expect(promptArgvCost("abc")).toBe(5);
    expect(promptArgvCost('say "hi" \\ there')).toBe(16 + 3 + 2);
    expect(promptArgvCost("é")).toBe(4);
    // 12,288 quotes: 12 KiB of bytes, 24 KiB + 2 once escaped.
    const quotes = '"'.repeat(MAX_PROMPT_ARGV_COST / 2);
    expect(utf8ByteLength(quotes)).toBeLessThan(MAX_PROMPT_ARGV_COST);
    const rows = planFanout({ body: "{{blob}}", parameters: [] }, { blob: quotes }, [{}], "t");
    expect(rows[0].errors).toEqual([
      { kind: "prompt_too_long", cost: MAX_PROMPT_ARGV_COST + 2, max: MAX_PROMPT_ARGV_COST },
    ]);
  });

  it("accepts a prompt exactly at the bound", () => {
    const rows = planFanout(
      { body: "{{blob}}", parameters: [] },
      { blob: "x".repeat(MAX_PROMPT_ARGV_COST - 2) },
      [{}],
      "t",
    );
    expect(rows[0].errors).toEqual([]);
  });

  it("keeps the argv bound below Windows' 32 KiB command-line limit", () => {
    expect(MAX_PROMPT_ARGV_COST).toBeLessThan(32_767);
  });

  it("keeps the preview member ceiling within the server's", () => {
    expect(DEFAULT_MAX_MEMBERS).toBeLessThanOrEqual(SERVER_MAX_MEMBERS);
  });

  it("trims NEL the way the server's Rust trim does", () => {
    expect(serverTrim("\u0085 a \u0085")).toBe("a");
    const rows = planFanout({ body: "p", parameters: [] }, {}, [{}], "\u0085");
    expect(rows[0].errors).toEqual([{ kind: "empty_title" }]);
  });

  it("mirrors the server's blank-prompt, NUL, blank-title and long-title refusals", () => {
    const blank = planFanout({ body: "{{x}}", parameters: [] }, { x: "  " }, [{}], "t");
    expect(blank[0].errors).toEqual([{ kind: "empty_prompt" }]);

    const nul = planFanout({ body: "a\0b", parameters: [] }, {}, [{}], "t");
    expect(nul[0].errors).toEqual([{ kind: "prompt_contains_nul" }]);

    const noTitle = planFanout({ body: "p", parameters: [] }, {}, [{}], "  ");
    expect(noTitle[0].errors).toEqual([{ kind: "empty_title" }]);

    const longTitle = planFanout(
      { body: "p", parameters: [] },
      {},
      [{}],
      "t".repeat(MAX_TITLE_CHARS + 1),
    );
    expect(longTitle[0].errors).toEqual([
      { kind: "title_too_long", length: MAX_TITLE_CHARS + 1, max: MAX_TITLE_CHARS },
    ]);
  });

  it("warns on a title placeholder with no value", () => {
    const rows = planFanout(
      { body: "{{platform}}", parameters: [] },
      {},
      members,
      "{{platform}} {{nope}}",
    );
    expect(rows[0].warnings).toEqual([{ kind: "unknown_title_placeholder", name: "nope" }]);
    expect(rows[0].title).toBe("iOS ");
  });

  it("composes end to end: parse → expand(product) → plan", () => {
    const parsed = parseMatrix("platform:iOS,Android;lang:swift,kotlin");
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) return;
    const expanded = expandMatrix(parsed.axes, "product");
    expect(expanded.ok).toBe(true);
    if (!expanded.ok) return;
    const rows = planFanout(
      { body: "{{platform}}/{{lang}}", parameters: [] },
      {},
      expanded.members,
      "#{{platform}}-{{lang}}",
    );
    expect(rows.map((r) => r.title)).toEqual([
      "#iOS-swift",
      "#iOS-kotlin",
      "#Android-swift",
      "#Android-kotlin",
    ]);
    expect(rows.map((r) => r.index)).toEqual([0, 1, 2, 3]);
  });
});
