/**
 * Unit tests for ScreenshotHandler
 */

import { describe, it, expect } from "vitest";
import { screenshotHandler } from "./screenshot-handler";

describe("ScreenshotHandler", () => {
  describe("parseOutput", () => {
    it("should parse basic screenshot output", () => {
      const raw = {
        screenshot_base64: "iVBORw0KGgo...",
        dimensions: { width: 1920, height: 1080 },
        monitor_index: 0,
      };

      const result = screenshotHandler.parseOutput(raw, {}, { step_name: "Capture Screen" });

      expect(result.step_type).toBe("screenshot");
      expect(result.screenshot_base64).toBe("iVBORw0KGgo...");
      expect(result.dimensions).toEqual({ width: 1920, height: 1080 });
      expect(result.monitor_index).toBe(0);
      expect(result.success).toBe(true);
    });

    it("should handle width/height as separate fields", () => {
      const raw = {
        screenshot_base64: "data...",
        width: 1280,
        height: 720,
      };

      const result = screenshotHandler.parseOutput(raw);

      expect(result.dimensions).toEqual({ width: 1280, height: 720 });
    });

    it("should parse detected elements", () => {
      const raw = {
        screenshot_base64: "data...",
        detected_elements: [
          {
            label: "Login Button",
            element_type: "button",
            bounding_box: { x: 100, y: 200, width: 80, height: 30 },
            confidence: 0.95,
          },
          {
            label: "Username Input",
            element_type: "input",
            bounding_box: { x: 100, y: 150, width: 200, height: 25 },
            confidence: 0.92,
          },
        ],
      };

      const result = screenshotHandler.parseOutput(raw);

      expect(result.detected_elements).toHaveLength(2);
      expect(result.detected_elements?.[0].label).toBe("Login Button");
      expect(result.detected_elements?.[0].confidence).toBe(0.95);
    });

    it("should handle alternative field name for elements", () => {
      const raw = {
        screenshot_base64: "data...",
        elements: [
          {
            label: "Element",
            element_type: "text",
            bounding_box: { x: 0, y: 0, width: 100, height: 20 },
            confidence: 0.9,
          },
        ],
      };

      const result = screenshotHandler.parseOutput(raw);

      expect(result.detected_elements).toHaveLength(1);
    });

    it("should include analysis source", () => {
      const raw = {
        screenshot_base64: "data...",
        analysis_source: "vision",
      };

      const result = screenshotHandler.parseOutput(raw);

      expect(result.analysis_source).toBe("vision");
    });

    it("should handle annotated screenshot", () => {
      const raw = {
        screenshot_base64: "original...",
        annotated_screenshot_base64: "annotated...",
      };

      const result = screenshotHandler.parseOutput(raw);

      expect(result.screenshot_base64).toBe("original...");
      expect(result.annotated_screenshot_base64).toBe("annotated...");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary with dimensions", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        dimensions: { width: 1920, height: 1080 },
      });

      const summary = screenshotHandler.summarizeForAI(output);

      expect(summary).toContain("### Screenshot:");
      expect(summary).toContain("1920x1080");
      expect(summary).toContain("✅");
    });

    it("should list detected elements in table format", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        detected_elements: [
          {
            label: "Button 1",
            element_type: "button",
            bounding_box: { x: 10, y: 20, width: 100, height: 30 },
            confidence: 0.95,
          },
        ],
      });

      const summary = screenshotHandler.summarizeForAI(output);

      expect(summary).toContain("Detected Elements");
      expect(summary).toContain("Button 1");
      expect(summary).toContain("button");
      expect(summary).toContain("95%");
    });

    it("should indicate truncated elements list", () => {
      const elements = Array.from({ length: 20 }, (_, i) => ({
        label: `Element ${i}`,
        element_type: "text",
        bounding_box: { x: 0, y: i * 20, width: 100, height: 20 },
        confidence: 0.9,
      }));

      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        detected_elements: elements,
      });

      const summary = screenshotHandler.summarizeForAI(output);

      expect(summary).toContain("more elements");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config with dimensions and element count", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        dimensions: { width: 1920, height: 1080 },
        detected_elements: [
          {
            label: "Test",
            element_type: "text",
            bounding_box: { x: 0, y: 0, width: 50, height: 20 },
            confidence: 0.9,
          },
        ],
      });

      const config = screenshotHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("screenshot_config");
      expect(config?.config_value).toMatchObject({
        dimensions: { width: 1920, height: 1080 },
        expected_element_count: 1,
      });
    });

    it("should return null if no screenshot and no elements", () => {
      const output = screenshotHandler.parseOutput({
        dimensions: { width: 0, height: 0 },
      });

      const config = screenshotHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include dimensions fields", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        dimensions: { width: 1920, height: 1080 },
      });

      const fields = screenshotHandler.getAssertableFields(output);

      const widthField = fields.find((f) => f.path === "dimensions.width");
      const heightField = fields.find((f) => f.path === "dimensions.height");

      expect(widthField).toBeDefined();
      expect(widthField?.example_value).toBe(1920);

      expect(heightField).toBeDefined();
      expect(heightField?.example_value).toBe(1080);
    });

    it("should include element count when elements present", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        detected_elements: [
          {
            label: "Test",
            element_type: "text",
            bounding_box: { x: 0, y: 0, width: 50, height: 20 },
            confidence: 0.9,
          },
        ],
      });

      const fields = screenshotHandler.getAssertableFields(output);

      const countField = fields.find((f) => f.path === "detected_elements.length");
      expect(countField).toBeDefined();
      expect(countField?.example_value).toBe(1);
    });

    it("should include individual element fields", () => {
      const output = screenshotHandler.parseOutput({
        screenshot_base64: "data...",
        detected_elements: [
          {
            label: "Submit",
            element_type: "button",
            bounding_box: { x: 100, y: 200, width: 80, height: 30 },
            confidence: 0.95,
          },
        ],
      });

      const fields = screenshotHandler.getAssertableFields(output);

      const labelField = fields.find((f) => f.path === "detected_elements[0].label");
      const confidenceField = fields.find((f) => f.path === "detected_elements[0].confidence");

      expect(labelField).toBeDefined();
      expect(labelField?.example_value).toBe("Submit");

      expect(confidenceField).toBeDefined();
      expect(confidenceField?.example_value).toBe(0.95);
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(screenshotHandler.stepType).toBe("screenshot");
    });

    it("should have display name", () => {
      expect(screenshotHandler.displayName).toBe("Screenshot");
    });
  });
});
