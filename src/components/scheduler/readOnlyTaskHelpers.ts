/**
 * The `task_type`s the scheduler form has an editor for. `Script` is not one:
 * the form shows it read-only through its own arm.
 */
const FORM_HANDLED_TASK_TYPES = new Set(["Workflow", "Prompt", "AutoFix", "RemoteAgent", "Script"]);

/**
 * The `task_type` of a stored task the scheduler form has no editor for
 * (e.g. `Watcher`, `BackgroundCapture`), or `undefined` when the form handles
 * it or there is no task.
 *
 * The form shows such a task read-only and round-trips it unchanged, so its
 * name, schedule and conditions stay editable. Without this it opened the task
 * as an empty Workflow editor, and Save stayed disabled.
 */
export function readOnlyTaskType(task: unknown): string | undefined {
  const taskType = (task as { task_type?: unknown } | null | undefined)?.task_type;
  if (typeof taskType !== "string" || FORM_HANDLED_TASK_TYPES.has(taskType)) return undefined;
  return taskType;
}
