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
      const handler = stepOutputRegistry.get("api_request");
      expect(handler).toBeDefined();
      expect(handler?.stepType).toBe("api_request");
    });

    it("should return undefined for unregistered type", () => {
      // @ts-expect-error Testing with invalid type
      const handler = stepOutputRegistry.get("nonexistent_type");
      expect(handler).toBeUndefined();
    });

    it("should have all expected handlers registered", () => {
      const expectedTypes = [
        "api_request",
        "gui_action",
        "shell_command",
        "mcp_call",
        "screenshot",
        "workflow_ref",
        "playwright_script",
        "state_navigation",
        "check",
      ];

      for (const type of expectedTypes) {
        const handler = stepOutputRegistry.get(type as Parameters<typeof stepOutputRegistry.get>[0]);
        expect(handler).toBeDefined();
        expect(handler?.stepType).toBe(type);
      }
    });
  });

  describe("has", () => {
    it("should return true for registered types", () => {
      expect(stepOutputRegistry.has("api_request")).toBe(true);
      expect(stepOutputRegistry.has("gui_action")).toBe(true);
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
      expect(handlers.length).toBeGreaterThanOrEqual(9);

      const types = handlers.map((h) => h.stepType);
      expect(types).toContain("api_request");
      expect(types).toContain("check");
    });
  });

  describe("getRegisteredTypes", () => {
    it("should return all registered step types", () => {
      const types = stepOutputRegistry.getRegisteredTypes();
      expect(types).toContain("api_request");
      expect(types).toContain("gui_action");
      expect(types).toContain("shell_command");
      expect(types).toContain("mcp_call");
      expect(types).toContain("screenshot");
      expect(types).toContain("workflow_ref");
      expect(types).toContain("playwright_script");
      expect(types).toContain("state_navigation");
      expect(types).toContain("check");
    });
  });
});

describe("collectTestConfigs", () => {
  it("should collect configs from multiple outputs", () => {
    // Use handlers to create properly structured outputs
    const apiHandler = stepOutputRegistry.get("api_request")!;
    const shellHandler = stepOutputRegistry.get("shell_command")!;

    const apiOutput = apiHandler.parseOutput(
      { status_code: 200 },
      { method: "GET", url: "https://api.example.com/test" },
      { step_name: "API Call" },
    );

    const shellOutput = shellHandler.parseOutput(
      { exit_code: 0, stdout: "hello" },
      { command: "echo hello" },
      { step_name: "Shell" },
    );

    const configs = collectTestConfigs([apiOutput, shellOutput]);

    expect(configs.length).toBeGreaterThanOrEqual(1);
  });

  it("should return empty array for empty outputs", () => {
    const configs = collectTestConfigs([]);
    expect(configs).toEqual([]);
  });

  it("should skip outputs with no config", () => {
    // Create output via handler - without URL, toTestConfig returns null
    const apiHandler = stepOutputRegistry.get("api_request")!;
    const output = apiHandler.parseOutput(
      { status_code: 200 },
      {}, // No URL in config
      { step_name: "No URL" },
    );

    const configs = collectTestConfigs([output]);
    // Should be empty because no URL means null config
    expect(configs).toEqual([]);
  });
});

describe("summarizeOutputsForAI", () => {
  it("should generate combined summary for multiple outputs", () => {
    // Use handlers to create properly structured outputs
    const apiHandler = stepOutputRegistry.get("api_request")!;
    const checkHandler = stepOutputRegistry.get("check")!;

    const apiOutput = apiHandler.parseOutput(
      { status_code: 200, response: { data: "test" } },
      { method: "GET", url: "https://api.example.com/data" },
      { step_name: "Fetch Data" },
    );

    const checkOutput = checkHandler.parseOutput(
      { check_type: "custom", issues: [], checks_passed: 5, checks_failed: 0 },
      {},
      { step_name: "Validate" },
    );

    const summary = summarizeOutputsForAI([apiOutput, checkOutput]);

    expect(summary).toContain("Step Outputs");
    expect(summary).toContain("Fetch Data");
    expect(summary).toContain("Validate");
    // Summary uses display names, not step_type strings
    expect(summary).toContain("API Request");
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
        step_type: "api_request",
        step_name: "API",
        executed_at: new Date().toISOString(),
        duration_ms: 100,
        success: true,
        status_code: 200,
        method: "GET",
        url: "https://example.com/api",
        response: { data: "test" },
        source_config: {
          method: "GET",
          url: "https://example.com/api",
        },
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
    const hasStatusCode = fields.some((f) => f.path.includes("status_code"));
    const hasChecksPassed = fields.some((f) => f.path.includes("checks_passed"));

    expect(hasStatusCode).toBe(true);
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
        step_type: "api_request",
        step_name: "My API Call",
        executed_at: new Date().toISOString(),
        duration_ms: 100,
        success: true,
        status_code: 200,
        method: "GET",
        url: "https://example.com/api",
        response: { data: "test" },
        source_config: {
          method: "GET",
          url: "https://example.com/api",
        },
      },
    ];

    const fields = collectAssertableFields(outputs);

    // Fields should be prefixed with output id
    expect(fields.length).toBeGreaterThan(0);
    expect(fields[0].path.startsWith("test-id-123.")).toBe(true);
    expect(fields[0].name.includes("My API Call")).toBe(true);
  });
});
