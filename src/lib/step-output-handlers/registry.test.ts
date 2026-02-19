/**
 * Unit tests for StepOutputHandlerRegistry and utility functions
 */

import { describe, it, expect } from "vitest";
// Import from index to trigger handler registration
import {
  stepOutputRegistry,
  collectTestConfigs,
  summarizeOutputsForAI,
  collectAssertableFields,
} from "./index";
import type { StepOutput } from "../../types/step-output";

describe("StepOutputHandlerRegistry", () => {
  describe("get", () => {
    it("should return handler for registered type", () => {
      const handler = stepOutputRegistry.get("command");
      expect(handler).toBeDefined();
      expect(handler?.stepType).toBe("command");
    });

    it("should return handler for shell_command (backward compat)", () => {
      const handler = stepOutputRegistry.get("shell_command");
      expect(handler).toBeDefined();
      expect(handler?.stepType).toBe("shell_command");
    });

    it("should return undefined for unregistered type", () => {
      // @ts-expect-error Testing with invalid type
      const handler = stepOutputRegistry.get("nonexistent_type");
      expect(handler).toBeUndefined();
    });

    it("should have all expected handlers registered", () => {
      const expectedTypes = ["command", "shell_command", "check"];

      for (const type of expectedTypes) {
        const handler = stepOutputRegistry.get(
          type as Parameters<typeof stepOutputRegistry.get>[0],
        );
        expect(handler).toBeDefined();
      }
    });
  });

  describe("has", () => {
    it("should return true for registered types", () => {
      expect(stepOutputRegistry.has("command")).toBe(true);
      expect(stepOutputRegistry.has("shell_command")).toBe(true);
      expect(stepOutputRegistry.has("check")).toBe(true);
    });

    it("should return false for unregistered types", () => {
      // @ts-expect-error Testing with invalid type
      expect(stepOutputRegistry.has("unknown")).toBe(false);
    });
  });

  describe("getAll", () => {
    it("should return all registered handlers", () => {
      const handlers = stepOutputRegistry.getAll();
      // command, shell_command (alias), check
      expect(handlers.length).toBeGreaterThanOrEqual(3);

      const types = handlers.map((h) => h.stepType);
      expect(types).toContain("command");
      expect(types).toContain("check");
    });
  });

  describe("getRegisteredTypes", () => {
    it("should return all registered step types", () => {
      const types = stepOutputRegistry.getRegisteredTypes();
      expect(types).toContain("command");
      expect(types).toContain("shell_command");
      expect(types).toContain("check");
    });
  });
});

describe("collectTestConfigs", () => {
  it("should collect configs from multiple outputs", () => {
    const commandHandler = stepOutputRegistry.get("command")!;

    const commandOutput = commandHandler.parseOutput(
      { exit_code: 0, stdout: "hello" },
      { command: "echo hello" },
      { step_name: "Command" },
    );

    const configs = collectTestConfigs([commandOutput]);

    expect(configs.length).toBeGreaterThanOrEqual(1);
  });

  it("should return empty array for empty outputs", () => {
    const configs = collectTestConfigs([]);
    expect(configs).toEqual([]);
  });

  it("should skip outputs with no config", () => {
    const commandHandler = stepOutputRegistry.get("command")!;
    const output = commandHandler.parseOutput(
      { exit_code: 0 },
      {}, // No command in config
      { step_name: "No Command" },
    );

    const configs = collectTestConfigs([output]);
    // Should be empty because no command means null config
    expect(configs).toEqual([]);
  });
});

describe("summarizeOutputsForAI", () => {
  it("should generate combined summary for multiple outputs", () => {
    const commandHandler = stepOutputRegistry.get("command")!;
    const checkHandler = stepOutputRegistry.get("check")!;

    const commandOutput = commandHandler.parseOutput(
      { exit_code: 0, stdout: "ok" },
      { command: "echo ok" },
      { step_name: "Run Command" },
    );

    const checkOutput = checkHandler.parseOutput(
      { check_type: "custom", issues: [], checks_passed: 5, checks_failed: 0 },
      {},
      { step_name: "Validate" },
    );

    const summary = summarizeOutputsForAI([commandOutput, checkOutput]);

    expect(summary).toContain("Step Outputs");
    expect(summary).toContain("Run Command");
    expect(summary).toContain("Validate");
    expect(summary).toContain("Command");
    expect(summary).toContain("Check");
  });

  it("should return empty string for empty outputs", () => {
    const summary = summarizeOutputsForAI([]);
    expect(summary).toBe("");
  });

  it("should handle unknown step types gracefully", () => {
    const outputs = [
      {
        id: "1",
        // @ts-expect-error Testing with invalid step type
        step_type: "unknown_type",
        step_name: "Unknown Step",
        executed_at: new Date().toISOString(),
        duration_ms: 100,
        success: true,
      },
    ] as StepOutput[];

    const summary = summarizeOutputsForAI(outputs);

    // Should not throw, should produce some output
    expect(typeof summary).toBe("string");
  });
});

describe("collectAssertableFields", () => {
  it("should collect fields from multiple outputs", () => {
    const outputs: StepOutput[] = [
      {
        id: "1",
        step_type: "command",
        step_name: "Command",
        executed_at: new Date().toISOString(),
        duration_ms: 100,
        success: true,
        command: "echo test",
        exit_code: 0,
        stdout: "test",
        stderr: "",
      },
      {
        id: "2",
        step_type: "check",
        step_name: "Check",
        executed_at: new Date().toISOString(),
        duration_ms: 50,
        success: true,
        check_type: "custom",
        issues: [],
        checks_passed: 3,
        checks_failed: 0,
      },
    ];

    const fields = collectAssertableFields(outputs);

    expect(fields.length).toBeGreaterThan(0);

    // Should have fields from both outputs - fields are prefixed with output id
    const hasExitCode = fields.some((f) => f.path.includes("exit_code"));
    const hasChecksPassed = fields.some((f) => f.path.includes("checks_passed"));

    expect(hasExitCode).toBe(true);
    expect(hasChecksPassed).toBe(true);
  });

  it("should return empty array for empty outputs", () => {
    const fields = collectAssertableFields([]);
    expect(fields).toEqual([]);
  });

  it("should prefix fields with output id and step name", () => {
    const outputs: StepOutput[] = [
      {
        id: "test-id-123",
        step_type: "command",
        step_name: "My Command",
        executed_at: new Date().toISOString(),
        duration_ms: 100,
        success: true,
        command: "echo test",
        exit_code: 0,
        stdout: "test",
        stderr: "",
      },
    ];

    const fields = collectAssertableFields(outputs);

    // Fields should be prefixed with output id
    expect(fields.length).toBeGreaterThan(0);
    expect(fields[0].path.startsWith("test-id-123.")).toBe(true);
    expect(fields[0].name.includes("My Command")).toBe(true);
  });
});
