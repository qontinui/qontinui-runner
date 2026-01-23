/**
 * Unit tests for CheckHandler
 */

import { describe, it, expect } from "vitest";
import { checkHandler } from "./check-handler";

describe("CheckHandler", () => {
  describe("parseOutput", () => {
    it("should parse check output with issues", () => {
      const raw = {
        check_type: "accessibility",
        issues: [
          { severity: "error", message: "Missing alt text", location: "img#hero" },
          { severity: "warning", message: "Low contrast", location: "div.footer" },
        ],
        checks_passed: 15,
        checks_failed: 2,
      };

      const result = checkHandler.parseOutput(raw, {}, { step_name: "A11y Check" });

      expect(result.step_type).toBe("check");
      expect(result.check_type).toBe("accessibility");
      expect(result.issues).toHaveLength(2);
      expect(result.checks_passed).toBe(15);
      expect(result.checks_failed).toBe(2);
      expect(result.success).toBe(false); // has failures
    });

    it("should handle alternative field names", () => {
      const raw = {
        check_type: "validation",
        errors: [ // alternative to issues
          { severity: "error", message: "Invalid input" },
        ],
        passed: 10, // alternative to checks_passed
        failed: 1, // alternative to checks_failed
      };

      const result = checkHandler.parseOutput(raw);

      expect(result.issues).toHaveLength(1);
      expect(result.checks_passed).toBe(10);
      expect(result.checks_failed).toBe(1);
    });

    it("should parse successful check with no issues", () => {
      const raw = {
        check_type: "element_presence",
        issues: [],
        checks_passed: 5,
        checks_failed: 0,
      };

      const result = checkHandler.parseOutput(raw);

      expect(result.success).toBe(true);
      expect(result.checks_failed).toBe(0);
    });

    it("should normalize check type names", () => {
      const a11yResult = checkHandler.parseOutput({ check_type: "a11y" });
      expect(a11yResult.check_type).toBe("accessibility");

      const visualResult = checkHandler.parseOutput({ check_type: "visual-regression" });
      expect(visualResult.check_type).toBe("visual_regression");

      const elementResult = checkHandler.parseOutput({ check_type: "element" });
      expect(elementResult.check_type).toBe("element_presence");

      const textResult = checkHandler.parseOutput({ check_type: "text" });
      expect(textResult.check_type).toBe("text_match");

      const customResult = checkHandler.parseOutput({ check_type: "unknown_type" });
      expect(customResult.check_type).toBe("custom");
    });

    it("should use config check_type as fallback", () => {
      const raw = {};
      const config = { check_type: "visual" };

      const result = checkHandler.parseOutput(raw, config);

      expect(result.check_type).toBe("visual_regression");
    });

    it("should handle visual regression screenshots", () => {
      const raw = {
        check_type: "visual_regression",
        reference_screenshot: "reference_base64...",
        current_screenshot: "current_base64...",
      };

      const result = checkHandler.parseOutput(raw);

      expect(result.reference_screenshot).toBe("reference_base64...");
      expect(result.current_screenshot).toBe("current_base64...");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for passing checks", () => {
      const output = checkHandler.parseOutput({
        check_type: "accessibility",
        issues: [],
        checks_passed: 10,
        checks_failed: 0,
      });

      const summary = checkHandler.summarizeForAI(output);

      expect(summary).toContain("### Check:");
      expect(summary).toContain("accessibility");
      expect(summary).toContain("10/10");
      expect(summary).toContain("100%");
      expect(summary).toContain("✅");
      expect(summary).toContain("All Passed");
    });

    it("should show issues grouped by severity", () => {
      const output = checkHandler.parseOutput({
        check_type: "validation",
        issues: [
          { severity: "error", message: "Error 1" },
          { severity: "error", message: "Error 2" },
          { severity: "warning", message: "Warning 1" },
          { severity: "info", message: "Info 1" },
        ],
        checks_passed: 5,
        checks_failed: 4,
      });

      const summary = checkHandler.summarizeForAI(output);

      expect(summary).toContain("Errors (2)");
      expect(summary).toContain("Warnings (1)");
      expect(summary).toContain("❌");
    });

    it("should include suggestions when present", () => {
      const output = checkHandler.parseOutput({
        check_type: "accessibility",
        issues: [
          {
            severity: "error",
            message: "Missing alt text",
            suggestion: "Add alt attribute to images",
          },
        ],
        checks_failed: 1,
      });

      const summary = checkHandler.summarizeForAI(output);

      expect(summary).toContain("Suggestion:");
      expect(summary).toContain("Add alt attribute");
    });

    it("should note screenshot availability for visual regression", () => {
      const output = checkHandler.parseOutput({
        check_type: "visual_regression",
        reference_screenshot: "ref...",
        current_screenshot: "cur...",
      });

      const summary = checkHandler.summarizeForAI(output);

      expect(summary).toContain("Screenshots");
      expect(summary).toContain("Reference screenshot");
      expect(summary).toContain("Current screenshot");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config from check output", () => {
      const output = checkHandler.parseOutput({
        check_type: "accessibility",
        issues: [
          { severity: "warning", message: "Known issue" },
        ],
        checks_passed: 9,
        checks_failed: 1,
      });

      const config = checkHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("check_config");
      expect(config?.config_value).toMatchObject({
        check_type: "accessibility",
        expected_pass_count: 9,
        expected_fail_count: 1,
      });
      expect(config?.config_value).toHaveProperty("known_issues");
    });
  });

  describe("getAssertableFields", () => {
    it("should include success and pass/fail counts", () => {
      const output = checkHandler.parseOutput({
        check_type: "validation",
        checks_passed: 8,
        checks_failed: 2,
      });

      const fields = checkHandler.getAssertableFields(output);

      const successField = fields.find((f) => f.path === "success");
      expect(successField).toBeDefined();

      const passedField = fields.find((f) => f.path === "checks_passed");
      expect(passedField).toBeDefined();
      expect(passedField?.example_value).toBe(8);

      const failedField = fields.find((f) => f.path === "checks_failed");
      expect(failedField).toBeDefined();
      expect(failedField?.example_value).toBe(2);
    });

    it("should include issue counts by severity", () => {
      const output = checkHandler.parseOutput({
        check_type: "validation",
        issues: [
          { severity: "error", message: "E1" },
          { severity: "error", message: "E2" },
          { severity: "warning", message: "W1" },
        ],
      });

      const fields = checkHandler.getAssertableFields(output);

      const issueCountField = fields.find((f) => f.path === "issues.length");
      expect(issueCountField).toBeDefined();
      expect(issueCountField?.example_value).toBe(3);

      const errorCountField = fields.find((f) => f.path === "issues.error_count");
      expect(errorCountField).toBeDefined();
      expect(errorCountField?.example_value).toBe(2);

      const warningCountField = fields.find((f) => f.path === "issues.warning_count");
      expect(warningCountField).toBeDefined();
      expect(warningCountField?.example_value).toBe(1);
    });

    it("should include individual issue messages", () => {
      const output = checkHandler.parseOutput({
        check_type: "validation",
        issues: [
          { severity: "error", message: "First issue message" },
          { severity: "warning", message: "Second issue message" },
        ],
      });

      const fields = checkHandler.getAssertableFields(output);

      const issue1Field = fields.find((f) => f.path === "issues[0].message");
      expect(issue1Field).toBeDefined();
      expect(issue1Field?.example_value).toBe("First issue message");

      const issue2Field = fields.find((f) => f.path === "issues[1].message");
      expect(issue2Field).toBeDefined();
    });

    it("should include check type field", () => {
      const output = checkHandler.parseOutput({
        check_type: "accessibility",
      });

      const fields = checkHandler.getAssertableFields(output);

      const checkTypeField = fields.find((f) => f.path === "check_type");
      expect(checkTypeField).toBeDefined();
      expect(checkTypeField?.example_value).toBe("accessibility");
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(checkHandler.stepType).toBe("check");
    });

    it("should have display name", () => {
      expect(checkHandler.displayName).toBe("Check/Validation");
    });
  });
});
