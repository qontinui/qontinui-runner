/**
 * Smoke test for generateConstraintToml re-exported through @qontinui/workflow-utils.
 *
 * The comprehensive test suite lives in qontinui-workflow-utils/src/constraint-toml.test.ts.
 * This file verifies that the runner can import and use the function correctly.
 */

import { describe, it, expect } from "vitest";
import { generateConstraintToml } from "@qontinui/workflow-utils";
import type { Constraint, ResourceLimits } from "@qontinui/shared-types/constraints";

describe("generateConstraintToml (runner import)", () => {
  it("generates TOML with builtins, resources, and custom constraints", () => {
    const constraints: Constraint[] = [
      {
        id: "builtin:no-secrets",
        name: "no secrets",
        description: "Built-in",
        check: { type: "grep_forbidden", pattern: "placeholder" },
        severity: "block",
        enabled: true,
      },
      {
        id: "project:no-todos",
        name: "no todos",
        description: "Remove TODO comments",
        check: { type: "grep_forbidden", pattern: "TODO" },
        severity: "warn",
        enabled: true,
      },
    ];
    const limits: ResourceLimits = { maxWallTimeSecs: 300 };

    const result = generateConstraintToml(constraints, limits);

    expect(result).toContain("[builtins]");
    expect(result).toContain("no-secrets = true");
    expect(result).toContain("[resources]");
    expect(result).toContain("max_wall_time_secs = 300");
    expect(result).toContain("[[constraint]]");
    expect(result).toContain('id = "project:no-todos"');
    expect(result).toContain('type = "grep_forbidden"');
    expect(result).toContain('pattern = "TODO"');
  });

  it("excludes AI constraints from output", () => {
    const constraints: Constraint[] = [
      {
        id: "ai:auto-rule",
        name: "AI Rule",
        description: "",
        check: { type: "grep_forbidden", pattern: "x" },
        severity: "warn",
        enabled: true,
      },
    ];
    const result = generateConstraintToml(constraints, {});
    expect(result).toBe("\n");
    expect(result).not.toContain("ai:");
  });
});
