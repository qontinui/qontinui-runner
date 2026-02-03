/**
 * Unit tests for WorkflowRefHandler
 */

import { describe, it, expect } from "vitest";
import { workflowRefHandler } from "./workflow-ref-handler";

describe("WorkflowRefHandler", () => {
  describe("parseOutput", () => {
    it("should parse basic workflow reference output", () => {
      const raw = {
        workflow_id: "wf_123",
        workflow_name: "Login Flow",
        final_state: "logged_in",
      };

      const result = workflowRefHandler.parseOutput(raw, {}, { step_name: "Run Login" });

      expect(result.step_type).toBe("workflow_ref");
      expect(result.workflow_id).toBe("wf_123");
      expect(result.workflow_name).toBe("Login Flow");
      expect(result.final_state).toBe("logged_in");
      expect(result.step_name).toBe("Run Login");
    });

    it("should parse nested outputs", () => {
      const raw = {
        workflow_id: "wf_456",
        workflow_name: "Checkout",
        nested_outputs: [
          { step_type: "gui_action", step_name: "Click Add", success: true },
          { step_type: "api_request", step_name: "Submit Order", success: true },
        ],
      };

      const result = workflowRefHandler.parseOutput(raw);

      expect(result.nested_outputs).toHaveLength(2);
      expect(result.nested_outputs?.[0].step_name).toBe("Click Add");
      expect(result.nested_outputs?.[1].step_name).toBe("Submit Order");
    });

    it("should handle alternative field name for nested outputs", () => {
      const raw = {
        workflow_id: "wf_789",
        outputs: [{ step_type: "check", step_name: "Verify", success: true }],
      };

      const result = workflowRefHandler.parseOutput(raw);

      expect(result.nested_outputs).toHaveLength(1);
    });

    it("should parse output variables", () => {
      const raw = {
        workflow_id: "wf_test",
        output_variables: {
          user_id: "12345",
          auth_token: "abc123",
        },
      };

      const result = workflowRefHandler.parseOutput(raw);

      expect(result.output_variables).toEqual({
        user_id: "12345",
        auth_token: "abc123",
      });
    });

    it("should use config values as fallback", () => {
      const raw = {};
      const config = {
        workflow_id: "config_wf",
        workflow_name: "From Config",
      };

      const result = workflowRefHandler.parseOutput(raw, config);

      expect(result.workflow_id).toBe("config_wf");
      expect(result.workflow_name).toBe("From Config");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for workflow execution", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        workflow_name: "Test Workflow",
        final_state: "completed",
      });

      const summary = workflowRefHandler.summarizeForAI(output);

      expect(summary).toContain("### Workflow Reference:");
      expect(summary).toContain("wf_test");
      expect(summary).toContain("Test Workflow");
      expect(summary).toContain("completed");
      expect(summary).toContain("✅");
    });

    it("should show nested steps summary", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        workflow_name: "Multi-Step",
        nested_outputs: [
          { step_type: "gui_action", step_name: "Step 1", success: true },
          { step_type: "gui_action", step_name: "Step 2", success: true },
          { step_type: "api_request", step_name: "Step 3", success: false },
        ],
      });

      const summary = workflowRefHandler.summarizeForAI(output);

      expect(summary).toContain("Nested Steps:** 3");
      expect(summary).toContain("gui_action");
      expect(summary).toContain("api_request");
      expect(summary).toContain("Step Sequence");
    });

    it("should show output variables", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        output_variables: {
          result: "success",
          count: 10,
        },
      });

      const summary = workflowRefHandler.summarizeForAI(output);

      expect(summary).toContain("Output Variables");
      expect(summary).toContain("result");
      expect(summary).toContain("count");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config for workflow reference", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_config",
        workflow_name: "Config Test",
        final_state: "done",
        nested_outputs: [{ step_type: "check", step_name: "Check", success: true }],
      });

      const config = workflowRefHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("workflow_ref_config");
      expect(config?.config_value).toMatchObject({
        workflow_id: "wf_config",
        workflow_name: "Config Test",
        expected_final_state: "done",
        expected_step_count: 1,
      });
    });

    it("should return null if no workflow_id", () => {
      const output = workflowRefHandler.parseOutput({});

      const config = workflowRefHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include success and final_state fields", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        final_state: "completed",
      });

      const fields = workflowRefHandler.getAssertableFields(output);

      const successField = fields.find((f) => f.path === "success");
      expect(successField).toBeDefined();

      const stateField = fields.find((f) => f.path === "final_state");
      expect(stateField).toBeDefined();
      expect(stateField?.example_value).toBe("completed");
    });

    it("should include nested output counts", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        nested_outputs: [
          { step_type: "check", step_name: "Check 1", success: true },
          { step_type: "check", step_name: "Check 2", success: true },
          { step_type: "check", step_name: "Check 3", success: false },
        ],
      });

      const fields = workflowRefHandler.getAssertableFields(output);

      const countField = fields.find((f) => f.path === "nested_outputs.length");
      expect(countField).toBeDefined();
      expect(countField?.example_value).toBe(3);

      const successCountField = fields.find((f) => f.path === "nested_outputs.success_count");
      expect(successCountField).toBeDefined();
      expect(successCountField?.example_value).toBe(2);
    });

    it("should include output variable fields", () => {
      const output = workflowRefHandler.parseOutput({
        workflow_id: "wf_test",
        output_variables: {
          user_name: "John",
          is_admin: true,
          score: 100,
        },
      });

      const fields = workflowRefHandler.getAssertableFields(output);

      const nameField = fields.find((f) => f.path === "output_variables.user_name");
      const adminField = fields.find((f) => f.path === "output_variables.is_admin");
      const scoreField = fields.find((f) => f.path === "output_variables.score");

      expect(nameField).toBeDefined();
      expect(nameField?.type).toBe("string");

      expect(adminField).toBeDefined();
      expect(adminField?.type).toBe("boolean");

      expect(scoreField).toBeDefined();
      expect(scoreField?.type).toBe("number");
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(workflowRefHandler.stepType).toBe("workflow_ref");
    });

    it("should have display name", () => {
      expect(workflowRefHandler.displayName).toBe("Workflow Reference");
    });
  });
});
