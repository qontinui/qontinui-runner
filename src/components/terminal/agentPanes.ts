import { invoke } from "@tauri-apps/api/core";

/**
 * Ask the runner which of `terminalIds` host an AI agent CLI of any provider
 * (`terminal_agent_panes`): each id maps to an agent kind (`"claude"`, `"pi"`,
 * `"codex"`, …), `"absent"`, `"none"` (no terminal process), or `"unknown"`.
 *
 * `fresh` asks for a process-table read that began after the call — pass it
 * when the answer decides a close.
 *
 * Never throws: an IPC failure or an unparseable answer is `null` (UNKNOWN for
 * every id), never an empty map that reads as "no agents anywhere".
 */
export async function probeAgentPanes(
  terminalIds: readonly string[],
  fresh = false,
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): Promise<Record<string, string> | null> {
  if (terminalIds.length === 0) return {};
  try {
    const resp = await invoker("terminal_agent_panes", { terminalIds: [...terminalIds], fresh });
    const panes = (resp as { data?: { panes?: unknown } } | null)?.data?.panes;
    if (!panes || typeof panes !== "object") return null;
    const out: Record<string, string> = {};
    for (const [id, v] of Object.entries(panes as Record<string, unknown>)) {
      if (typeof v === "string") out[id] = v;
    }
    return out;
  } catch {
    return null;
  }
}
