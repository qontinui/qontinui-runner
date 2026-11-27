/**
 * The scheduler form's `Command` task (qontinui-schemas
 * `ScheduledTaskType::Command`): a program exec'd as an argv, with no shell
 * and no AI session.
 *
 * The published @qontinui/shared-types `ScheduledTaskType` predates the
 * variant, so it is described structurally here, as `Script` is in the form.
 */

/** What happens to the child's output: nothing kept, or the stderr tail. */
export type CommandOutput = { type: "Discard" } | { type: "Tail"; bytes: number };

export interface CommandTask {
  task_type: "Command";
  program: string;
  args?: string[];
  working_directory?: string | null;
  timeout_seconds?: number | null;
  output?: CommandOutput;
  detach?: boolean;
}

/** The runner's cap on a `Tail` (`CommandOutput::MAX_TAIL_BYTES`). */
export const COMMAND_MAX_TAIL_BYTES = 4096;
/** The `bytes` of the default `Tail` (`CommandOutput::DEFAULT_TAIL_BYTES`). */
export const COMMAND_DEFAULT_TAIL_BYTES = 2048;
/** The runner's timeout when `timeout_seconds` is unset. */
export const COMMAND_DEFAULT_TIMEOUT_SECONDS = 3600;

export const asCommandTask = (t: unknown): CommandTask | undefined =>
  (t as { task_type?: string } | null | undefined)?.task_type === "Command"
    ? (t as CommandTask)
    : undefined;

/** The form's text for an argv: one argument per line. */
export const argsToLines = (args: string[] | undefined): string => (args ?? []).join("\n");

/**
 * The argv the form's one-argument-per-line text means. Each line is one
 * argument exactly as typed (no quoting or splitting); trailing empty lines
 * are not arguments. An empty argument is written as an empty line between
 * others.
 */
export function linesToArgs(text: string): string[] {
  if (text === "") return [];
  const lines = text.split(/\r?\n/);
  while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/** The edited fields the form has a control for. */
export interface CommandFormFields {
  program: string;
  argsText: string;
  workingDirectory: string;
  timeoutSeconds: string;
  outputMode: "Discard" | "Tail";
  tailBytes: string;
  detach: boolean;
}

/** Why the form's `Command` fields cannot be saved, or `undefined` if they can. */
export function commandFieldsError(f: CommandFormFields): string | undefined {
  if (f.program.trim() === "") return "A program is required.";
  if (f.timeoutSeconds.trim() !== "") {
    const t = Number(f.timeoutSeconds);
    if (!Number.isInteger(t) || t < 1) return "Timeout must be a whole number of seconds.";
  }
  if (f.outputMode === "Tail") {
    const b = Number(f.tailBytes);
    if (!Number.isInteger(b) || b < 1 || b > COMMAND_MAX_TAIL_BYTES) {
      return `Tail bytes must be between 1 and ${COMMAND_MAX_TAIL_BYTES}.`;
    }
  }
  return undefined;
}

/**
 * The `Command` task the form submits: the edited fields laid over the task
 * it opened with, so a key this form has no control for survives a save.
 */
export function buildCommandTask(
  opened: CommandTask | undefined,
  f: CommandFormFields,
): CommandTask {
  const timeout = f.timeoutSeconds.trim() === "" ? null : Number(f.timeoutSeconds);
  return {
    ...opened,
    task_type: "Command",
    program: f.program.trim(),
    args: linesToArgs(f.argsText),
    working_directory: f.workingDirectory.trim() || null,
    timeout_seconds: timeout,
    output:
      f.outputMode === "Discard"
        ? { type: "Discard" }
        : { type: "Tail", bytes: Number(f.tailBytes) },
    detach: f.detach,
  };
}

/** The form's initial fields for `opened` (or a new task). */
export function commandFormFields(opened: CommandTask | undefined): CommandFormFields {
  const output = opened?.output ?? { type: "Tail", bytes: COMMAND_DEFAULT_TAIL_BYTES };
  return {
    program: opened?.program ?? "",
    argsText: argsToLines(opened?.args),
    workingDirectory: opened?.working_directory ?? "",
    timeoutSeconds:
      opened?.timeout_seconds !== undefined && opened?.timeout_seconds !== null
        ? String(opened.timeout_seconds)
        : "",
    outputMode: output.type,
    tailBytes: String(output.type === "Tail" ? output.bytes : COMMAND_DEFAULT_TAIL_BYTES),
    detach: opened?.detach ?? false,
  };
}
