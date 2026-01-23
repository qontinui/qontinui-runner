/**
 * Unit tests for McpCallHandler
 */

import { describe, it, expect } from "vitest";
import { mcpCallHandler } from "./mcp-call-handler";

describe("McpCallHandler", () => {
  describe("parseOutput", () => {
    it("should parse successful MCP call", () => {
      const raw = {
        server_id: "weather-server",
        tool_name: "get_weather",
        arguments: { city: "London" },
        result: { temp: 15, condition: "cloudy" },
      };

      const result = mcpCallHandler.parseOutput(raw, {}, { step_name: "Get Weather" });

      expect(result.step_type).toBe("mcp_call");
      expect(result.server_id).toBe("weather-server");
      expect(result.tool_name).toBe("get_weather");
      expect(result.arguments).toEqual({ city: "London" });
      expect(result.result).toEqual({ temp: 15, condition: "cloudy" });
      expect(result.success).toBe(true);
    });

    it("should handle alternative field names", () => {
      const raw = {
        server: "db-server", // alternative to server_id
        tool: "query", // alternative to tool_name
        args: { sql: "SELECT *" }, // alternative to arguments
        response: { rows: [] }, // alternative to result
      };

      const result = mcpCallHandler.parseOutput(raw);

      expect(result.server_id).toBe("db-server");
      expect(result.tool_name).toBe("query");
      expect(result.arguments).toEqual({ sql: "SELECT *" });
      expect(result.result).toEqual({ rows: [] });
    });

    it("should handle MCP error via is_error flag", () => {
      const raw = {
        server_id: "test-server",
        tool_name: "failing_tool",
        is_error: true,
        result: "Tool execution failed: timeout",
      };

      const result = mcpCallHandler.parseOutput(raw);

      expect(result.is_error).toBe(true);
      expect(result.success).toBe(false);
      expect(result.error).toBe("Tool execution failed: timeout");
    });

    it("should default to unknown for missing server_id and tool_name", () => {
      const raw = {
        result: "some result",
      };

      const result = mcpCallHandler.parseOutput(raw);

      expect(result.server_id).toBe("unknown");
      expect(result.tool_name).toBe("unknown");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for MCP call", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "api-server",
        tool_name: "fetch_data",
        arguments: { id: 123 },
        result: { data: "test" },
      });

      const summary = mcpCallHandler.summarizeForAI(output);

      expect(summary).toContain("### MCP Tool Call:");
      expect(summary).toContain("api-server");
      expect(summary).toContain("fetch_data");
      expect(summary).toContain("✅");
    });

    it("should show error for failed calls", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "test",
        tool_name: "fail",
        is_error: true,
        result: "Connection refused",
      });

      const summary = mcpCallHandler.summarizeForAI(output);

      expect(summary).toContain("❌");
      expect(summary).toContain("Error");
    });

    it("should include arguments preview", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "test",
        tool_name: "call",
        arguments: { key: "value", count: 5 },
      });

      const summary = mcpCallHandler.summarizeForAI(output);

      expect(summary).toContain("Arguments");
      expect(summary).toContain("key");
    });
  });

  describe("toTestConfig", () => {
    it("should generate config from MCP call", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "my-server",
        tool_name: "my_tool",
        arguments: { param: "value" },
        result: { success: true },
      });

      const config = mcpCallHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("mcp_call_config");
      expect(config?.config_value).toMatchObject({
        server_id: "my-server",
        tool_name: "my_tool",
        arguments: { param: "value" },
      });
    });

    it("should still return config with default unknown values", () => {
      // When server_id and tool_name are not provided, they default to "unknown"
      // toTestConfig checks if both exist, which they will (as "unknown")
      const output = mcpCallHandler.parseOutput({});

      const config = mcpCallHandler.toTestConfig(output);

      // With defaults of "unknown", it still generates config
      expect(config).not.toBeNull();
      expect(config?.config_value).toMatchObject({
        server_id: "unknown",
        tool_name: "unknown",
      });
    });
  });

  describe("getAssertableFields", () => {
    it("should include success and is_error fields", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "test",
        tool_name: "test_tool",
      });

      const fields = mcpCallHandler.getAssertableFields(output);

      const successField = fields.find((f) => f.path === "success");
      expect(successField).toBeDefined();

      const isErrorField = fields.find((f) => f.path === "is_error");
      expect(isErrorField).toBeDefined();
    });

    it("should include result fields when present", () => {
      const output = mcpCallHandler.parseOutput({
        server_id: "test",
        tool_name: "test",
        result: {
          status: "ok",
          count: 10,
        },
      });

      const fields = mcpCallHandler.getAssertableFields(output);

      const statusField = fields.find((f) => f.path === "result.status");
      const countField = fields.find((f) => f.path === "result.count");

      expect(statusField).toBeDefined();
      expect(countField).toBeDefined();
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(mcpCallHandler.stepType).toBe("mcp_call");
    });

    it("should have display name", () => {
      expect(mcpCallHandler.displayName).toBe("MCP Tool Call");
    });
  });
});
