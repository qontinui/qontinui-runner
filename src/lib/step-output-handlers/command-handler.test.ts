/**
 * Unit tests for CommandHandler
 */

import { describe, it, expect } from "vitest";
import { commandHandler } from "./command-handler";

describe("CommandHandler", () => {
  describe("parseOutput", () => {
    it("should parse successful command output", () => {
      const raw = {
        command: "echo hello",
        exit_code: 0,
        stdout: "hello\n",
        stderr: "",
      };

      const result = commandHandler.parseOutput(raw, {}, { step_name: "Echo Test" });

      expect(result.step_type).toBe("command");
      expect(result.command).toBe("echo hello");
      expect(result.exit_code).toBe(0);
      expect(result.stdout).toBe("hello\n");
      expect(result.stderr).toBe("");
      expect(result.success).toBe(true);
    });

    it("should parse failed command with exit code", () => {
      const raw = {
        command: "exit 1",
        exit_code: 1,
        stderr: "Command failed",
      };

      const result = commandHandler.parseOutput(raw);

      expect(result.exit_code).toBe(1);
      expect(result.success).toBe(false);
      expect(result.stderr).toBe("Command failed");
    });

    it("should handle alternative field names", () => {
      const raw = {
        command: "ls -la",
        code: 0, // alternative to exit_code
        output: "file1.txt\nfile2.txt", // alternative to stdout
        error_output: "", // alternative to stderr
      };

      const result = commandHandler.parseOutput(raw);

      expect(result.command).toBe("ls -la");
      expect(result.exit_code).toBe(0);
      expect(result.stdout).toBe("file1.txt\nfile2.txt");
    });

    it("should include working directory when provided", () => {
      const raw = {
        command: "pwd",
        exit_code: 0,
        working_directory: "/home/user",
      };

      const result = commandHandler.parseOutput(raw);

      expect(result.working_directory).toBe("/home/user");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for successful command", () => {
      const output = commandHandler.parseOutput({
        command: "npm test",
        exit_code: 0,
        stdout: "All tests passed",
      });

      const summary = commandHandler.summarizeForAI(output);

      expect(summary).toContain("### Command:");
      expect(summary).toContain("npm test");
      expect(summary).toContain("Exit Code:");
      expect(summary).toContain("0");
    });

    it("should show stderr for failed commands", () => {
      const output = commandHandler.parseOutput({
        command: "npm test",
        exit_code: 1,
        stderr: "Error: Test failed",
      });

      const summary = commandHandler.summarizeForAI(output);

      expect(summary).toContain("Standard Error");
      expect(summary).toContain("Test failed");
    });

    it("should truncate long output", () => {
      const longOutput = "x".repeat(5000);
      const output = commandHandler.parseOutput({
        command: "cat bigfile",
        exit_code: 0,
        stdout: longOutput,
      });

      const summary = commandHandler.summarizeForAI(output);

      expect(summary).toContain("truncated");
      expect(summary.length).toBeLessThan(longOutput.length);
    });
  });

  describe("toTestConfig", () => {
    it("should generate config from command output", () => {
      const output = commandHandler.parseOutput({
        command: "npm run build",
        exit_code: 0,
        working_directory: "/project",
      });

      const config = commandHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("command_config");
      expect(config?.config_value).toMatchObject({
        command: "npm run build",
        working_directory: "/project",
      });
    });

    it("should return null if no command", () => {
      const output = commandHandler.parseOutput({
        exit_code: 0,
      });

      const config = commandHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include exit code field", () => {
      const output = commandHandler.parseOutput({
        command: "test",
        exit_code: 0,
      });

      const fields = commandHandler.getAssertableFields(output);

      const exitCodeField = fields.find((f) => f.path === "exit_code");
      expect(exitCodeField).toBeDefined();
      expect(exitCodeField?.type).toBe("number");
      expect(exitCodeField?.suggested_assertions).toBeDefined();
      expect(exitCodeField?.suggested_assertions?.length).toBeGreaterThan(0);
    });

    it("should include stdout content field when present", () => {
      const output = commandHandler.parseOutput({
        command: "echo test",
        exit_code: 0,
        stdout: "test output",
      });

      const fields = commandHandler.getAssertableFields(output);

      const stdoutField = fields.find((f) => f.path === "stdout");
      expect(stdoutField).toBeDefined();
      expect(stdoutField?.type).toBe("string");
    });

    it("should include stderr field when present", () => {
      const output = commandHandler.parseOutput({
        command: "test",
        exit_code: 1,
        stderr: "error message",
      });

      const fields = commandHandler.getAssertableFields(output);

      const stderrField = fields.find((f) => f.path === "stderr");
      expect(stderrField).toBeDefined();
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(commandHandler.stepType).toBe("command");
    });

    it("should have display name", () => {
      expect(commandHandler.displayName).toBe("Command");
    });
  });
});
