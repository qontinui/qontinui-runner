import type { ScheduledTaskType } from "../../types/scheduler";

export type RemoteAgentTask = Extract<ScheduledTaskType, { task_type: "RemoteAgent" }>;

/** The keys the scheduler form has a control for. */
export type RemoteAgentFormFields = Pick<
  RemoteAgentTask,
  | "prompt"
  | "working_directory"
  | "allowed_tools"
  | "model"
  | "mcp_connections"
  | "max_turns"
  | "timeout_seconds"
>;

/**
 * The `RemoteAgent` task the scheduler form submits: the fields the form edits,
 * laid over the task it opened with.
 *
 * Saving replaces the stored task whole, so a key the form has no control for
 * survives only if it is carried here. `effort`, `ultracode` and
 * `effort_profile` are such keys (qontinui-schemas 55eedceb), as is any other
 * key the runner's schemas pin knows and this form does not. Without the
 * overlay, editing a task's prompt would drop them with nothing to say so.
 */
export function buildRemoteAgentTask(
  opened: RemoteAgentTask | undefined,
  edited: RemoteAgentFormFields,
): RemoteAgentTask {
  return { ...opened, ...edited, task_type: "RemoteAgent" };
}
