/**
 * Unit tests for ShellCommandHandler
 */

import { describe, it, expect } from "vitest";
import { shellCommandHandler } from "./shell-command-handler";

describe("ShellCommandHandler", () => {
  describe("parseOutput", () => {
    it("should parse successful command output", () => {
      const raw = {
        command: "echo hello",
        exit_code: 0,
        stdout: "hello\n",
        stderr: "",
      };

      const result = shellCommandHandler.parseOutput(raw, {}, { step_name: "Echo Test" });

      expect(result.step_type).toBe("shell_command");
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

      const result = shellCommandHandler.parseOutput(raw);

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

      const result = shellCommandHandler.parseOutput(raw);

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

      const result = shellCommandHandler.parseOutput(raw);

      expect(result.working_directory).toBe("/home/user");
    });
  });

  describe("summarizeForAI", () => {
    it("should generate summary for successful command", () => {
      const output = shellCommandHandler.parseOutput({
        command: "npm test",
        exit_code: 0,
        stdout: "All tests passed",
      });

      const summary = shellCommandHandler.summarizeForAI(output);

      expect(summary).toContain("### Shell Command:");
      expect(summary).toContain("npm test");
      expect(summary).toContain("Exit Code:");
      expect(summary).toContain("0");
      expect(summary).toContain("✅");
    });

    it("should show stderr for failed commands", () => {
      const output = shellCommandHandler.parseOutput({
        command: "npm test",
        exit_code: 1,
        stderr: "Error: Test failed",
      });

      const summary = shellCommandHandler.summarizeForAI(output);

      expect(summary).toContain("❌");
      expect(summary).toContain("Standard Error");
      expect(summary).toContain("Test failed");
    });

    it("should truncate long output", () => {
      const longOutput = "x".repeat(5000);
      const output = shellCommandHandler.parseOutput({
        command: "cat bigfile",
        exit_code: 0,
        stdout: longOutput,
      });

      const summary = shellCommandHandler.summarizeForAI(output);

      expect(summary).toContain("truncated");
      expect(summary.length).toBeLessThan(longOutput.length);
    });
  });

  describe("toTestConfig", () => {
    it("should generate config from command output", () => {
      const output = shellCommandHandler.parseOutput({
        command: "npm run build",
        exit_code: 0,
        working_directory: "/project",
      });

      const config = shellCommandHandler.toTestConfig(output);

      expect(config).not.toBeNull();
      expect(config?.config_key).toBe("shell_command_config");
      expect(config?.config_value).toMatchObject({
        command: "npm run build",
        working_directory: "/project",
      });
    });

    it("should return null if no command", () => {
      const output = shellCommandHandler.parseOutput({
        exit_code: 0,
      });

      const config = shellCommandHandler.toTestConfig(output);

      expect(config).toBeNull();
    });
  });

  describe("getAssertableFields", () => {
    it("should include exit code field", () => {
      const output = shellCommandHandler.parseOutput({
        command: "test",
        exit_code: 0,
      });

      const fields = shellCommandHandler.getAssertableFields(output);

      const exitCodeField = fields.find((f) => f.path === "exit_code");
      expect(exitCodeField).toBeDefined();
      expect(exitCodeField?.type).toBe("number");
      expect(exitCodeField?.suggested_assertions).toBeDefined();
      expect(exitCodeField?.suggested_assertions?.length).toBeGreaterThan(0);
    });

    it("should include stdout content field when present", () => {
      const output = shellCommandHandler.parseOutput({
        command: "echo test",
        exit_code: 0,
        stdout: "test output",
      });

      const fields = shellCommandHandler.getAssertableFields(output);

      const stdoutField = fields.find((f) => f.path === "stdout");
      expect(stdoutField).toBeDefined();
      expect(stdoutField?.type).toBe("string");
    });

    it("should include stderr field when present", () => {
      const output = shellCommandHandler.parseOutput({
        command: "test",
        exit_code: 1,
        stderr: "error message",
      });

      const fields = shellCommandHandler.getAssertableFields(output);

      const stderrField = fields.find((f) => f.path === "stderr");
      expect(stderrField).toBeDefined();
    });
  });

  describe("singleton instance", () => {
    it("should have correct step type", () => {
      expect(shellCommandHandler.stepType).toBe("shell_command");
    });

    it("should have display name", () => {
      expect(shellCommandHandler.displayName).toBe("Shell Command");
    });
  });
});
