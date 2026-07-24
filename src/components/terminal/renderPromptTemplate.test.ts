import { describe, expect, it } from "vitest";

import type { PromptParameter } from "./promptLibraryApi";
import {
  initialParamValues,
  isParameterVisible,
  missingRequired,
  renderPromptTemplate,
} from "./renderPromptTemplate";

function param(overrides: Partial<PromptParameter> & { name: string }): PromptParameter {
  return {
    type: "string",
    label: overrides.name,
    description: "",
    required: false,
    ...overrides,
  };
}

describe("renderPromptTemplate", () => {
  it("substitutes {{name}} placeholders with collected values", () => {
    const body = "Create a new {{language}} project called `{{project_name}}`.\n\nGoal: {{goal}}";
    const out = renderPromptTemplate(body, {
      language: "TypeScript",
      project_name: "my-dashboard",
      goal: "a metrics page",
    });
    expect(out).toBe(
      "Create a new TypeScript project called `my-dashboard`.\n\nGoal: a metrics page",
    );
  });

  it("substitutes unfilled placeholders to the empty string", () => {
    expect(renderPromptTemplate("A: {{a}} B: {{b}}", { a: "x" })).toBe("A: x B: ");
    expect(renderPromptTemplate("N: {{nope}}", {})).toBe("N: ");
  });

  it("tolerates whitespace inside the braces and repeats", () => {
    expect(renderPromptTemplate("{{ a }} + {{a}}", { a: "1" })).toBe("1 + 1");
  });

  it("stringifies numbers and booleans", () => {
    expect(renderPromptTemplate("{{n}}/{{b}}", { n: 3, b: false })).toBe("3/false");
  });
});

describe("missingRequired", () => {
  const params: PromptParameter[] = [
    param({ name: "project_name", required: true }),
    param({ name: "goal", required: true }),
    param({ name: "notes", required: false }),
  ];

  it("names required-but-empty params", () => {
    expect(missingRequired(params, { project_name: "x" })).toEqual(["goal"]);
    expect(missingRequired(params, {})).toEqual(["project_name", "goal"]);
  });

  it("treats whitespace-only strings as unfilled", () => {
    expect(missingRequired(params, { project_name: "  ", goal: "g" })).toEqual(["project_name"]);
  });

  it("returns empty when everything required is filled (optional may stay empty)", () => {
    expect(missingRequired(params, { project_name: "p", goal: "g" })).toEqual([]);
  });

  it("counts boolean false as filled", () => {
    const boolParams = [param({ name: "flag", type: "boolean", required: true })];
    expect(missingRequired(boolParams, { flag: false })).toEqual([]);
    expect(missingRequired(boolParams, {})).toEqual(["flag"]);
  });

  it("skips params hidden by an unmet depends_on", () => {
    const depParams = [
      param({ name: "mode", required: true }),
      param({
        name: "detail",
        required: true,
        depends_on: { param: "mode", value: "advanced" },
      }),
    ];
    expect(missingRequired(depParams, { mode: "simple" })).toEqual([]);
    expect(missingRequired(depParams, { mode: "advanced" })).toEqual(["detail"]);
  });
});

describe("isParameterVisible", () => {
  it("is visible without depends_on and matches by string equality", () => {
    expect(isParameterVisible(param({ name: "a" }), {})).toBe(true);
    const dep = param({ name: "b", depends_on: { param: "a", value: "x" } });
    expect(isParameterVisible(dep, { a: "x" })).toBe(true);
    expect(isParameterVisible(dep, { a: "y" })).toBe(false);
    expect(isParameterVisible(dep, {})).toBe(false);
  });
});

describe("initialParamValues", () => {
  it("seeds defaults per type and booleans to false", () => {
    const params = [
      param({ name: "lang", type: "select", default: "TypeScript" }),
      param({ name: "count", type: "number", default: 2 }),
      param({ name: "flag", type: "boolean" }),
      param({ name: "free" }),
    ];
    expect(initialParamValues(params)).toEqual({
      lang: "TypeScript",
      count: 2,
      flag: false,
    });
  });
});
