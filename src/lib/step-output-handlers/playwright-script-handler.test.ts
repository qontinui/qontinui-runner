/**
 * Unit tests for PlaywrightScriptHandler
 */

import { describe, it, expect } from "vitest";
import { playwrightScriptHandler } from "./playwright-script-handler";

describe("PlaywrightScriptHandler", () => {
  describe("parseOutput", () => {
    it("should parse basic Playwright script output", () => {
      const raw = {
        final_url: "https://example.com/dashboard",
        page_title: "Dashboard - Example App",
        script_content: "await page.goto('...')",
      };

      const result = playwrightScriptHandler.parseOutput(
        raw,
        {},
        { step_name: "Navigate to Dashboard" },
      );

      expect(result.step_type).toBe("playwright_script");
      expect(result.final_url).toBe("https://example.com/dashboard");
      expect(result.page_title).toBe("Dashboard - Example App");
      expect(result.script_content).toBe("await page.goto('...')");
    });

    it("should handle alternative field names", () => {
      const raw = {
        url: "https://test.com", // alternative to final_url
        title: "Test Page", // alternative to page_title
      };

      const result = playwrightScriptHandler.parseOutput(raw);

      expect(result.final_url).toBe("https://test.com");
      expect(result.page_title).toBe("Test Page");
    });

    it("should parse console logs", () => {
      const raw = {
        final_url: "https://test.com",
        console_logs: [
          { type: "log", message: "App started", timestamp: "2024-01-01T00:00:00Z" },
          { type: "error", message: "API error", timestamp: "2024-01-01T00:00:01Z" },
          { type: "warn", message: "Deprecated API", timestamp: "2024-01-01T00:00:02Z" },
        ],
      };

      const result = playwrightScriptHandler.parseOutput(raw);

      expect(result.console_logs).toHaveLength(3);
      expect(result.console_logs?.[1].type).toBe("error");
    });

    it("should parse network requests", () => {
      const raw = {
        final_url: "https://test.com",
        network_requests: [
          { url: "https://api.test.com/data", method: "GET", status: 200 },
          { url: "https://api.test.com/submit", method: "POST", status: 201 },
          { url: "https://api.test.com/fail", method: "GET", status: 500 },
        ],
      };

      const result = playwrightScriptHandler.parseOutput(raw);

      expect(result.network_requests).toHaveLength(3);
      expect(result.network_requests?.[0].status).toBe(200);
      expect(result.network_requests?.[2].status).toBe(500);
    });

    it("should parse extracted data", () => {
      const raw = {
        final_url: "https://test.com",
        extracted_data: {
          user_count: 150,
          items: ["a", "b", "c"],
          metadata: { version: "1.0" },
        },
      };

      const result = playwrightScriptHandler.parseOutput(raw);

      expect(result.extracted_data).toEqual({
        user_count: 150,
        items: ["a", "b", "c"],
        metadata: { version: "1.0" },
      });
    });

    it("should handle screenshot fields", () => {
      const raw = {
        final_url: "https://test.com",
        final_screenshot: "base64_screenshot_data...",
      };

      const result = playwrightScriptHandler.parseOutput(raw);

      expect(result.final_screenshot).toBe("base64_screenshot_data...");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary with URL and title", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://example.com/page",
        page_title: "Example Page",
      });

      const summary = playwrightScriptHandler.summarizeForAI(output);

      expect(summary).toContain("### Playwright Script:");
      expect(summary).toContain("https://example.com/page");
      expect(summary).toContain("Example Page");
      expect(summary).toContain("✅");
    });

    it("should show console errors and warnings count", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        console_logs: [
          { type: "error", message: "Error 1", timestamp: "" },
          { type: "error", message: "Error 2", timestamp: "" },
          { type: "warn", message: "Warning 1", timestamp: "" },
          { type: "log", message: "Info", timestamp: "" },
        ],
      });

      const summary = playwrightScriptHandler.summarizeForAI(output);

      expect(summary).toContain("Console Logs");
      expect(summary).toContain("2 errors");
      expect(summary).toContain("1 warnings");
    });

    it("should show failed network requests", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        network_requests: [
          { url: "https://api.test.com/ok", method: "GET", status: 200 },
          { url: "https://api.test.com/bad", method: "POST", status: 400 },
          { url: "https://api.test.com/error", method: "GET", status: 500 },
        ],
      });

      const summary = playwrightScriptHandler.summarizeForAI(output);

      expect(summary).toContain("Network Requests");
      expect(summary).toContain("2 failed");
    });

    it("should include extracted data preview", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        extracted_data: {
          title: "Test Title",
          count: 42,
        },
      });

      const summary = playwrightScriptHandler.summarizeForAI(output);

      expect(summary).toContain("Extracted Data");
      expect(summary).toContain("title");
      expect(summary).toContain("count");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config from Playwright output", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://example.com/result",
        page_title: "Result Page",
        extracted_data: { status: "success" },
      });

      const config = playwrightScriptHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("playwright_script_config");
      expect(config?.config_value).toMatchObject({
        expected_url: "https://example.com/result",
        expected_title: "Result Page",
        extracted_data: { status: "success" },
      });
    });

    it("should return null if no URL or title", () => {
      const output = playwrightScriptHandler.parseOutput({});

      const config = playwrightScriptHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include URL and title fields", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        page_title: "Test Page",
      });

      const fields = playwrightScriptHandler.getAssertableFields(output);

      const urlField = fields.find((f) => f.path === "final_url");
      expect(urlField).toBeDefined();
      expect(urlField?.example_value).toBe("https://test.com");

      const titleField = fields.find((f) => f.path === "page_title");
      expect(titleField).toBeDefined();
    });

    it("should include console error count", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        console_logs: [
          { type: "error", message: "Error", timestamp: "" },
          { type: "log", message: "Info", timestamp: "" },
        ],
      });

      const fields = playwrightScriptHandler.getAssertableFields(output);

      const errorCountField = fields.find((f) => f.path === "console_logs.error_count");
      expect(errorCountField).toBeDefined();
      expect(errorCountField?.example_value).toBe(1);
    });

    it("should include network request counts", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        network_requests: [
          { url: "https://api.com/1", method: "GET", status: 200 },
          { url: "https://api.com/2", method: "GET", status: 404 },
        ],
      });

      const fields = playwrightScriptHandler.getAssertableFields(output);

      const countField = fields.find((f) => f.path === "network_requests.length");
      expect(countField).toBeDefined();
      expect(countField?.example_value).toBe(2);

      const failedField = fields.find((f) => f.path === "network_requests.failed_count");
      expect(failedField).toBeDefined();
      expect(failedField?.example_value).toBe(1);
    });

    it("should include extracted data fields", () => {
      const output = playwrightScriptHandler.parseOutput({
        final_url: "https://test.com",
        extracted_data: {
          user_name: "John",
          is_logged_in: true,
        },
      });

      const fields = playwrightScriptHandler.getAssertableFields(output);

      const nameField = fields.find((f) => f.path === "extracted_data.user_name");
      const loggedInField = fields.find((f) => f.path === "extracted_data.is_logged_in");

      expect(nameField).toBeDefined();
      expect(nameField?.type).toBe("string");

      expect(loggedInField).toBeDefined();
      expect(loggedInField?.type).toBe("boolean");
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(playwrightScriptHandler.stepType).toBe("playwright_script");
    });

    it("should have display name", () => {
      expect(playwrightScriptHandler.displayName).toBe("Playwright Script");
    });
  });
});
