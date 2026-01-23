/**
 * Unit tests for ApiRequestHandler
 */

import { describe, it, expect } from "vitest";
import { apiRequestHandler } from "./api-request-handler";

describe("ApiRequestHandler", () => {
  describe("parseOutput", () => {
    it("should parse basic API response with status and body", () => {
      const raw = {
        status_code: 200,
        response: { data: [1, 2, 3] },
        response_headers: { "content-type": "application/json" },
      };

      const result = apiRequestHandler.parseOutput(raw, {}, { step_name: "Test API" });

      expect(result.step_type).toBe("api_request");
      expect(result.status_code).toBe(200);
      expect(result.response).toEqual({ data: [1, 2, 3] });
      expect(result.step_name).toBe("Test API");
      expect(result.id).toBeTruthy();
    });

    it("should handle alternative field names", () => {
      const raw = {
        status: 201, // alternative to status_code
        data: { created: true }, // alternative to response
      };

      const result = apiRequestHandler.parseOutput(raw);

      expect(result.status_code).toBe(201);
      expect(result.response).toEqual({ created: true });
    });

    it("should use method and URL from step config", () => {
      const raw = { status_code: 200 };
      const config = {
        method: "POST",
        url: "https://api.example.com/users",
      };

      const result = apiRequestHandler.parseOutput(raw, config);

      expect(result.method).toBe("POST");
      expect(result.url).toBe("https://api.example.com/users");
    });

    it("should calculate success based on status code", () => {
      const success = apiRequestHandler.parseOutput({ status_code: 200 });
      const clientError = apiRequestHandler.parseOutput({ status_code: 404 });
      const serverError = apiRequestHandler.parseOutput({ status_code: 500 });

      expect(success.success).toBe(true);
      expect(clientError.success).toBe(false);
      expect(serverError.success).toBe(false);
    });

    it("should include extractions from raw output", () => {
      const raw = {
        status_code: 200,
        response: { user: { id: 123, name: "John" } },
        extractions: { userId: 123 },
      };

      const result = apiRequestHandler.parseOutput(raw);

      expect(result.extractions).toBeDefined();
      expect(result.extractions?.userId).toBe(123);
    });
  });

  describe("summarizeForAI", () => {
    it("should generate markdown summary", () => {
      const output = apiRequestHandler.parseOutput(
        {
          status_code: 200,
          response: { message: "success" },
        },
        { method: "GET", url: "https://api.example.com/test" },
        { step_name: "Get Test Data" },
      );

      const summary = apiRequestHandler.summarizeForAI(output);

      expect(summary).toContain("### API Request: Get Test Data");
      expect(summary).toContain("GET");
      expect(summary).toContain("200");
      expect(summary).toContain("✅");
    });

    it("should show error status with ❌ emoji", () => {
      const output = apiRequestHandler.parseOutput(
        { status_code: 500, response: { error: "Internal error" } },
        { method: "POST", url: "https://api.example.com/fail" },
      );

      const summary = apiRequestHandler.summarizeForAI(output);

      expect(summary).toContain("❌");
      expect(summary).toContain("500");
    });

    it("should include response data preview", () => {
      const output = apiRequestHandler.parseOutput({
        status_code: 200,
        response: { items: ["a", "b", "c"] },
      });

      const summary = apiRequestHandler.summarizeForAI(output);

      expect(summary).toContain("Response Data");
      expect(summary).toContain("items");
    });
  });

  describe("toTestConfig", () => {
    it("should generate test config from output", () => {
      const output = apiRequestHandler.parseOutput(
        { status_code: 200, response: { result: "ok" } },
        {
          method: "GET",
          url: "https://api.example.com/test",
          headers: { Authorization: "Bearer token" },
        },
      );

      const config = apiRequestHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("api_request_config");
      expect(config?.config_value).toMatchObject({
        method: "GET",
        url: "https://api.example.com/test",
      });
    });

    it("should return null if no URL", () => {
      const output = apiRequestHandler.parseOutput({ status_code: 200 });

      const config = apiRequestHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should return fields for basic response", () => {
      const output = apiRequestHandler.parseOutput({
        status_code: 200,
        response: { count: 5 },
      });

      const fields = apiRequestHandler.getAssertableFields(output);

      expect(fields.length).toBeGreaterThan(0);

      const statusField = fields.find((f) => f.path === "status_code");
      expect(statusField).toBeDefined();
      expect(statusField?.type).toBe("number");
      expect(statusField?.example_value).toBe(200);
    });

    it("should include response fields", () => {
      const output = apiRequestHandler.parseOutput({
        status_code: 200,
        response: { users: [{ id: 1 }], total: 10 },
      });

      const fields = apiRequestHandler.getAssertableFields(output);

      const usersField = fields.find((f) => f.path === "response.users");
      const totalField = fields.find((f) => f.path === "response.total");

      expect(usersField).toBeDefined();
      expect(usersField?.type).toBe("array");

      expect(totalField).toBeDefined();
      expect(totalField?.type).toBe("number");
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(apiRequestHandler.stepType).toBe("api_request");
    });

    it("should have display name", () => {
      expect(apiRequestHandler.displayName).toBe("API Request");
    });

    it("should have description", () => {
      expect(apiRequestHandler.description).toBeTruthy();
    });
  });
});
