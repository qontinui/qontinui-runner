/**
 * How the scheduler history shows a run's `errorMessage`.
 *
 * The field is not only for errors: a detached `Command` run succeeds with
 * the note "launched (detached; exit not observed)". A message on a
 * SUCCESSFUL run is therefore shown as a neutral note, never as a red
 * "Error:" beside a Success badge.
 */
export interface HistoryMessage {
  label: "Error" | "Note";
  tone: "error" | "muted";
  text: string;
}

export function historyMessage(record: {
  success: boolean;
  errorMessage?: string | null;
}): HistoryMessage | undefined {
  if (!record.errorMessage) return undefined;
  return record.success
    ? { label: "Note", tone: "muted", text: record.errorMessage }
    : { label: "Error", tone: "error", text: record.errorMessage };
}
