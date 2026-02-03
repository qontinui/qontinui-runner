/**
 * Unit tests for StateNavigationHandler
 */

import { describe, it, expect } from "vitest";
import { stateNavigationHandler } from "./state-navigation-handler";

describe("StateNavigationHandler", () => {
  describe("parseOutput", () => {
    it("should parse successful navigation", () => {
      const raw = {
        target_state: "logged_in",
        reached: true,
        final_state: "logged_in",
        path_taken: [
          { from_state: "start", to_state: "login_page", transition_name: "navigate" },
          { from_state: "login_page", to_state: "logged_in", transition_name: "submit_login" },
        ],
      };

      const result = stateNavigationHandler.parseOutput(raw, {}, { step_name: "Login Navigation" });

      expect(result.step_type).toBe("state_navigation");
      expect(result.target_state).toBe("logged_in");
      expect(result.reached).toBe(true);
      expect(result.final_state).toBe("logged_in");
      expect(result.path_taken).toHaveLength(2);
    });

    it("should handle alternative field names", () => {
      const raw = {
        target_state: "checkout",
        reached: true,
        current_state: "checkout", // alternative to final_state
        path: [
          // alternative to path_taken
          { from_state: "cart", to_state: "checkout", transition_name: "proceed" },
        ],
      };

      const result = stateNavigationHandler.parseOutput(raw);

      expect(result.final_state).toBe("checkout");
      expect(result.path_taken).toHaveLength(1);
    });

    it("should infer reached from final_state matching target", () => {
      const raw = {
        target_state: "dashboard",
        final_state: "dashboard",
        // reached not provided
      };

      const result = stateNavigationHandler.parseOutput(raw);

      expect(result.reached).toBe(true);
    });

    it("should handle failed navigation", () => {
      const raw = {
        target_state: "admin_panel",
        reached: false,
        final_state: "access_denied",
        path_taken: [
          { from_state: "home", to_state: "access_denied", transition_name: "navigate_admin" },
        ],
      };

      const result = stateNavigationHandler.parseOutput(raw);

      expect(result.reached).toBe(false);
      expect(result.final_state).toBe("access_denied");
      expect(result.success).toBe(false);
    });

    it("should use target_state from config as fallback", () => {
      const raw = { reached: true };
      const config = { target_state: "from_config" };

      const result = stateNavigationHandler.parseOutput(raw, config);

      expect(result.target_state).toBe("from_config");
    });

    it("should include action_taken in path steps", () => {
      const raw = {
        target_state: "target",
        reached: true,
        path_taken: [
          {
            from_state: "start",
            to_state: "target",
            transition_name: "go",
            action_taken: "click button",
          },
        ],
      };

      const result = stateNavigationHandler.parseOutput(raw);

      expect(result.path_taken?.[0].action_taken).toBe("click button");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for successful navigation", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "checkout",
        reached: true,
        final_state: "checkout",
        path_taken: [
          { from_state: "cart", to_state: "checkout", transition_name: "proceed_to_checkout" },
        ],
      });

      const summary = stateNavigationHandler.summarizeForAI(output);

      expect(summary).toContain("### State Navigation:");
      expect(summary).toContain("checkout");
      expect(summary).toContain("✅");
      expect(summary).toContain("Yes");
    });

    it("should show path taken with transitions", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "end",
        reached: true,
        path_taken: [
          { from_state: "start", to_state: "middle", transition_name: "step1" },
          { from_state: "middle", to_state: "end", transition_name: "step2" },
        ],
      });

      const summary = stateNavigationHandler.summarizeForAI(output);

      expect(summary).toContain("Path Taken");
      expect(summary).toContain("2 transitions");
      expect(summary).toContain("start → middle");
      expect(summary).toContain("step1");
    });

    it("should note when already at target", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "home",
        reached: true,
        path_taken: [],
      });

      const summary = stateNavigationHandler.summarizeForAI(output);

      expect(summary).toContain("Already at target");
    });

    it("should show failed navigation with ❌", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "restricted",
        reached: false,
        final_state: "access_denied",
      });

      const summary = stateNavigationHandler.summarizeForAI(output);

      expect(summary).toContain("❌");
      expect(summary).toContain("No");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config for navigation", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "dashboard",
        reached: true,
        final_state: "dashboard",
        path_taken: [
          { from_state: "login", to_state: "dashboard", transition_name: "after_login" },
        ],
      });

      const config = stateNavigationHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("state_navigation_config");
      expect(config?.config_value).toMatchObject({
        target_state: "dashboard",
        expected_reached: true,
        expected_path_length: 1,
        path_states: ["dashboard"],
      });
    });

    it("should return null if no target_state", () => {
      const output = stateNavigationHandler.parseOutput({
        reached: true,
      });

      const config = stateNavigationHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include reached field", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "target",
        reached: true,
      });

      const fields = stateNavigationHandler.getAssertableFields(output);

      const reachedField = fields.find((f) => f.path === "reached");
      expect(reachedField).toBeDefined();
      expect(reachedField?.type).toBe("boolean");
      expect(reachedField?.example_value).toBe(true);
    });

    it("should include target and final state fields", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "target",
        final_state: "target",
        reached: true,
      });

      const fields = stateNavigationHandler.getAssertableFields(output);

      const targetField = fields.find((f) => f.path === "target_state");
      expect(targetField).toBeDefined();

      const finalField = fields.find((f) => f.path === "final_state");
      expect(finalField).toBeDefined();
    });

    it("should include path length when path exists", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "target",
        reached: true,
        path_taken: [
          { from_state: "a", to_state: "b", transition_name: "ab" },
          { from_state: "b", to_state: "c", transition_name: "bc" },
        ],
      });

      const fields = stateNavigationHandler.getAssertableFields(output);

      const pathLengthField = fields.find((f) => f.path === "path_taken.length");
      expect(pathLengthField).toBeDefined();
      expect(pathLengthField?.example_value).toBe(2);
    });

    it("should include individual transition fields", () => {
      const output = stateNavigationHandler.parseOutput({
        target_state: "target",
        reached: true,
        path_taken: [{ from_state: "start", to_state: "target", transition_name: "direct" }],
      });

      const fields = stateNavigationHandler.getAssertableFields(output);

      const transitionField = fields.find((f) => f.path === "path_taken[0].transition_name");
      expect(transitionField).toBeDefined();
      expect(transitionField?.example_value).toBe("direct");
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(stateNavigationHandler.stepType).toBe("state_navigation");
    });

    it("should have display name", () => {
      expect(stateNavigationHandler.displayName).toBe("State Navigation");
    });
  });
});
