/**
 * The durable record behind a resume that is NOT the boot-restore drain — the
 * operator's "Retry resume" and the zone-profile resume (plan
 * `2026-10-07-a-resumed-gate-continuation-loses-its-reporter-…` D2).
 *
 * Both doors used to type a resume with no gate identity, so a gate
 * continuation resumed through either could never post `work_completed` for
 * its own gate. The identity lives only on the durable `TerminalSessionRecord`
 * (neither the tab state nor `ZoneSessionInfo` carries it), so both read it
 * from `terminal_session_list_open` — the drain's own source — keyed by
 * `claudeSessionId`.
 */
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse, TerminalSessionRecord } from "./types";
import type { SessionOpenArgs } from "./sessionRecordArgs";
import type { ResumeGateIdentity } from "./useTerminalInitialization";

/**
 * The records `terminal_session_list_open` returns, keyed by
 * `claudeSessionId` (first row wins, as in `fetchRestoreSet`). NOT filtered by
 * page: a retry or a profile resume names one session id, wherever its record
 * was filed.
 *
 * That command returns the RESTORABLE set (the drain's own source), not every
 * open record. A session outside it — e.g. a zone-profile resume of a gate
 * row that was not part of the restore cohort — is absent from the map and
 * resumes without a gate identity, as both doors did before this lookup.
 *
 * Never throws. An unreadable registry, or a restore set withheld by coord's
 * device drain, yields an EMPTY map — the resume then proceeds without a gate
 * identity, which is what it did before this lookup existed, and the caller
 * logs it. Failing the resume itself would turn a missing reporter into a
 * missing session.
 */
export async function fetchRecordsByClaudeSessionId(): Promise<Map<string, TerminalSessionRecord>> {
  const byId = new Map<string, TerminalSessionRecord>();
  let resp: CommandResponse | null;
  try {
    resp = await invoke<CommandResponse>("terminal_session_list_open");
  } catch (err) {
    console.warn("[resumeRecordLookup] terminal_session_list_open failed:", err);
    return byId;
  }
  const data = resp?.data as
    | { sessions?: TerminalSessionRecord[]; deferredByDrain?: unknown }
    | undefined;
  if (data?.deferredByDrain) {
    console.warn(
      "[resumeRecordLookup] restore set withheld by the coord device drain — " +
        "resuming without a gate identity",
    );
  }
  const sessions = data?.sessions;
  if (!Array.isArray(sessions)) return byId;
  for (const rec of sessions) {
    if (!rec || typeof rec.claudeSessionId !== "string") continue;
    if (!byId.has(rec.claudeSessionId)) byId.set(rec.claudeSessionId, rec);
  }
  return byId;
}

/**
 * The gate a record's continuation reports to, or `undefined` for a record
 * with none (not a continuation, or no record at all). Same shape the drain
 * builds (`gate: { gateId, gateConsumingDeviceId }`); `buildResumeCmd`
 * validates both ids before interpolating them.
 */
export function gateFromRecord(
  rec: Pick<TerminalSessionRecord, "gateId" | "gateConsumingDeviceId"> | undefined,
): ResumeGateIdentity | undefined {
  if (!rec?.gateId) return undefined;
  return { gateId: rec.gateId, gateConsumingDeviceId: rec.gateConsumingDeviceId };
}

/** The `runVerifiedResume` arguments a "Retry resume" click supplies. */
export interface RetryResumeArgs {
  tabId: string;
  claudeSessionId: string;
  configDir?: string;
  provider?: string;
  gate?: ResumeGateIdentity;
  recordOpen: SessionOpenArgs;
}

/**
 * Assemble a hand retry's `runVerifiedResume` arguments from the tab and its
 * durable record, mirroring the boot-restore drain: the record's `provider`
 * selects the adapter resume, and its gate identity rides the typed resume.
 * `record` absent (unreadable registry, no row) ⇒ neither is passed — the
 * pre-lookup behaviour. Pure; exported for unit tests.
 */
export function buildRetryResumeArgs(params: {
  tabId: string;
  claudeSessionId: string;
  configDir: string | undefined;
  record: TerminalSessionRecord | undefined;
  recordOpen: SessionOpenArgs;
}): RetryResumeArgs {
  const { tabId, claudeSessionId, configDir, record, recordOpen } = params;
  const gate = gateFromRecord(record);
  return {
    tabId,
    claudeSessionId,
    configDir,
    ...(record?.provider ? { provider: record.provider } : {}),
    ...(gate ? { gate } : {}),
    recordOpen,
  };
}
