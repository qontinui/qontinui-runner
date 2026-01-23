/**
 * Unit tests for GuiActionHandler
 */

import { describe, it, expect } from "vitest";
import { guiActionHandler } from "./gui-action-handler";

describe("GuiActionHandler", () => {
  describe("parseOutput", () => {
    it("should parse click action with target", () => {
      const raw = {
        action_type: "click",
        target_label: "Submit Button",
        target_location: { x: 100, y: 200 },
        confidence: 0.95,
      };

      const result = guiActionHandler.parseOutput(raw, {}, { step_name: "Click Submit" });

      expect(result.step_type).toBe("gui_action");
      expect(result.action_type).toBe("click");
      expect(result.target_label).toBe("Submit Button");
      expect(result.target_location).toEqual({ x: 100, y: 200 });
      expect(result.confidence).toBe(0.95);
      expect(result.step_name).toBe("Click Submit");
    });

    it("should parse type action with text", () => {
      const raw = {
        action_type: "type",
        typed_text: "hello@example.com",
        target_label: "Email Input",
      };

      const result = guiActionHandler.parseOutput(raw);

      expect(result.action_type).toBe("type");
      expect(result.typed_text).toBe("hello@example.com");
    });

    it("should parse keypress action", () => {
      const raw = {
        action_type: "keypress",
        key_pressed: "Enter",
      };

      const result = guiActionHandler.parseOutput(raw);

      // Note: keypress is normalized to key_press
      expect(result.action_type).toBe("key_press");
      expect(result.key_pressed).toBe("Enter");
    });

    it("should parse scroll action", () => {
      const raw = {
        action_type: "scroll",
        scroll_delta: { x: 0, y: 100 },
      };

      const result = guiActionHandler.parseOutput(raw);

      expect(result.action_type).toBe("scroll");
      expect(result.scroll_delta).toEqual({ x: 0, y: 100 });
    });

    it("should handle bounding box from different field names", () => {
      const raw = {
        action_type: "click",
        target_bounds: { x: 10, y: 20, width: 100, height: 50 },
      };

      const result = guiActionHandler.parseOutput(raw);

      expect(result.target_bounds).toEqual({ x: 10, y: 20, width: 100, height: 50 });
    });

    it("should handle screenshots", () => {
      const raw = {
        action_type: "click",
        screenshot_before: "base64_before...",
        screenshot_after: "base64_after...",
      };

      const result = guiActionHandler.parseOutput(raw);

      expect(result.screenshot_before).toBe("base64_before...");
      expect(result.screenshot_after).toBe("base64_after...");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for click action", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        target_label: "Login Button",
        confidence: 0.92,
      });

      const summary = guiActionHandler.summarizeForAI(output);

      expect(summary).toContain("### GUI Action:");
      expect(summary).toContain("click");
      expect(summary).toContain("Login Button");
      expect(summary).toContain("92");
    });

    it("should show typed text for type actions", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "type",
        typed_text: "test input",
      });

      const summary = guiActionHandler.summarizeForAI(output);

      expect(summary).toContain("Typed Text");
      expect(summary).toContain("test input");
    });

    it("should note available screenshots", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        screenshot_before: "data...",
        screenshot_after: "data...",
      });

      const summary = guiActionHandler.summarizeForAI(output);

      expect(summary).toContain("Screenshots Available");
      expect(summary).toContain("Before action");
      expect(summary).toContain("After action");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config for click action with target_label", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        target_label: "Button",
        target_location: { x: 150, y: 250 },
        confidence: 0.9,
      });

      const config = guiActionHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("gui_action_config");
      expect(config?.config_value).toMatchObject({
        action_type: "click",
        target_label: "Button",
        target_location: { x: 150, y: 250 },
      });
    });

    it("should return null if no target_label and no target_bounds", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        // No target_label or target_bounds
      });

      const config = guiActionHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include success field", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        target_label: "Test",
      });

      const fields = guiActionHandler.getAssertableFields(output);

      const successField = fields.find((f) => f.path === "success");
      expect(successField).toBeDefined();
      expect(successField?.type).toBe("boolean");
    });

    it("should include confidence when present", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        confidence: 0.88,
      });

      const fields = guiActionHandler.getAssertableFields(output);

      const confidenceField = fields.find((f) => f.path === "confidence");
      expect(confidenceField).toBeDefined();
      expect(confidenceField?.example_value).toBe(0.88);
      expect(confidenceField?.suggested_assertions).toBeDefined();
    });

    it("should include target location when present", () => {
      const output = guiActionHandler.parseOutput({
        action_type: "click",
        target_location: { x: 100, y: 200 },
      });

      const fields = guiActionHandler.getAssertableFields(output);

      const xField = fields.find((f) => f.path === "target_location.x");
      const yField = fields.find((f) => f.path === "target_location.y");

      expect(xField).toBeDefined();
      expect(yField).toBeDefined();
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(guiActionHandler.stepType).toBe("gui_action");
    });

    it("should have display name", () => {
      expect(guiActionHandler.displayName).toBe("GUI Action");
    });
  });
});
