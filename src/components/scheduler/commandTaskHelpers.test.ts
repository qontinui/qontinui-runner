/**
 * Pure-helper tests for the scheduler form's `Command` task. The runner's
 * vitest config uses `environment: "node"`, so the form itself is not rendered
 * here; its wiring is checked through its source, as for the other arms.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, it, expect } from "vitest";

import {
  asCommandTask,
  buildCommandTask,
  commandFieldsError,
  commandFormFields,
  linesToArgs,
  type CommandTask,
} from "./commandTaskHelpers";

const stored: CommandTask = {
  task_type: "Command",
  program: "terraform",
  args: ["plan", "-detailed-exitcode", "-out=plan file"],
  working_directory: "/w",
  timeout_seconds: 1800,
  output: { type: "Discard" },
  detach: false,
};

describe("Command task helpers", () => {
  it("recognises only a Command task", () => {
    expect(asCommandTask(stored)).toBe(stored);
    expect(asCommandTask({ task_type: "Script", command: "x" })).toBeUndefined();
    expect(asCommandTask(undefined)).toBeUndefined();
  });

  it("round-trips a stored task through the form fields unchanged", () => {
    expect(buildCommandTask(stored, commandFormFields(stored))).toEqual(stored);
  });

  it("keeps each line as one argument, spaces and all, and drops trailing blank lines", () => {
    expect(linesToArgs("")).toEqual([]);
    expect(linesToArgs("a b\n'c'\n\n")).toEqual(["a b", "'c'"]);
    expect(linesToArgs("a\n\nb")).toEqual(["a", "", "b"]);
    expect(linesToArgs("a\r\nb")).toEqual(["a", "b"]);
  });

  it("defaults a new task to Tail 2048, the runner timeout and no detach", () => {
    const f = commandFormFields(undefined);
    expect(f).toMatchObject({
      outputMode: "Tail",
      tailBytes: "2048",
      timeoutSeconds: "",
      detach: false,
    });
    const built = buildCommandTask(undefined, { ...f, program: " sleep ", argsText: "5" });
    expect(built).toEqual({
      task_type: "Command",
      program: "sleep",
      args: ["5"],
      working_directory: null,
      timeout_seconds: null,
      output: { type: "Tail", bytes: 2048 },
      detach: false,
    });
  });

  it("keeps a stored argv the text cannot spell when the text is unchanged", () => {
    const awkward: CommandTask = { ...stored, args: ["plan", ""] };
    const fields = commandFormFields(awkward);
    expect(buildCommandTask(awkward, fields).args).toEqual(["plan", ""]);
    const multiLine: CommandTask = { ...stored, args: ["-c", "a\nb"] };
    expect(buildCommandTask(multiLine, commandFormFields(multiLine)).args).toEqual(["-c", "a\nb"]);
    // An edit takes the text's meaning.
    expect(buildCommandTask(awkward, { ...fields, argsText: "apply" }).args).toEqual(["apply"]);
  });

  it("ignores the hidden timeout and output under detach", () => {
    const f = { ...commandFormFields(stored), detach: true, tailBytes: "0", timeoutSeconds: "x" };
    expect(commandFieldsError(f)).toBeUndefined();
    const built = buildCommandTask(stored, f);
    expect(built.detach).toBe(true);
    expect(built.output).toEqual(stored.output);
    expect(built.timeout_seconds).toBe(stored.timeout_seconds);
    const fresh = buildCommandTask(undefined, {
      ...commandFormFields(undefined),
      program: "wsl",
      detach: true,
      outputMode: "Discard",
    });
    // An explicit Discard survives detach, so unticking it later persists nothing.
    expect(fresh.output).toEqual({ type: "Discard" });
    expect(fresh.timeout_seconds).toBeNull();
    const freshTail = buildCommandTask(undefined, {
      ...commandFormFields(undefined),
      program: "wsl",
      detach: true,
    });
    expect(freshTail.output).toEqual({ type: "Tail", bytes: 2048 });
  });

  it("refuses what the runner would refuse", () => {
    const ok = commandFormFields(stored);
    expect(commandFieldsError(ok)).toBeUndefined();
    expect(commandFieldsError({ ...ok, program: "  " })).toMatch(/program/);
    expect(commandFieldsError({ ...ok, outputMode: "Tail", tailBytes: "0" })).toMatch(/Tail/);
    expect(commandFieldsError({ ...ok, outputMode: "Tail", tailBytes: "4097" })).toMatch(/4096/);
    expect(commandFieldsError({ ...ok, outputMode: "Tail", tailBytes: "4096" })).toBeUndefined();
    expect(commandFieldsError({ ...ok, timeoutSeconds: "0" })).toMatch(/Timeout/);
    expect(commandFieldsError({ ...ok, timeoutSeconds: "1.5" })).toMatch(/Timeout/);
  });

  it("is what the form opens, submits and validates a Command task by", () => {
    const form = readFileSync(resolve(__dirname, "SchedulerTaskForm.tsx"), "utf8");
    expect(form).toContain('if (asCommandTask(initTask)) return "command";');
    expect(form).toContain("return buildCommandTask(initCommand, commandFields)");
    expect(form).toContain(
      'case "command":\n        return commandFieldsError(commandFields) === undefined;',
    );
    expect(form).toContain('data-testid="command-detach-note"');
  });
});
