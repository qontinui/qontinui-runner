/**
 * The account a `claude --resume <id>` runs under, for every resume path that
 * is NOT the boot restore — the zone-profile resume and the transcript / Past
 * Sessions resume.
 *
 * The defect (2026-10-08, boot restore; same shape here): a session whose
 * recorded `configDir` is null/absent was resumed as a bare
 * `claude --resume <id>`, so it ran under whatever `CLAUDE_CONFIG_DIR` the
 * pane's PTY carried — the spawn-time account picker's — while its transcript
 * lived under the default `~/.claude`. Result: "No conversation found".
 *
 * The boot restore was fixed by having the backend resolve the account from
 * where the transcript actually is (`terminal_session_list_open`'s
 * `resumeConfigDir`). Every other resume path asks the SAME backend resolver
 * through `terminal_resolve_resume_account`, then funnels the answer through
 * the boot restore's own `resolveRestoreConfigDir` (resolved dir first, raw
 * `configDir` only as a fallback, both shell-sanitized) and builds the typed
 * line with its `buildResumeCmd`. One resolver, one sanitizer, one builder.
 */

import { invoke } from "@tauri-apps/api/core";

import type { CommandResponse } from "./types";
import type { ZoneSessionInfo } from "./zoneProfileStorage";
import { buildResumeCmd, resolveRestoreConfigDir } from "./useTerminalInitialization";

/** What a resume path knows about the session it is about to resume. */
export interface ResumeAccountQuery {
  claudeSessionId: string;
  /** The account the caller has on file, if any (may be stale or absent). */
  configDir?: string | null;
  /** The session's project dir, if known; the backend fills it from the record. */
  workingDir?: string | null;
}

/** Injectable `invoke`, for tests. */
export type InvokeFn = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;

/**
 * Resolve the account a resume of this session must run under — the dir its
 * transcript is found under, else the record's own, else the ambient default
 * — sanitized for splicing into a typed command. `undefined` only when no
 * safe dir can be named at all (or the backend could not be asked AND the
 * caller had no usable dir of its own; logged).
 */
export async function resolveResumeAccount(
  q: ResumeAccountQuery,
  invokeFn: InvokeFn = invoke,
): Promise<string | undefined> {
  let resolved: string | undefined;
  try {
    const resp = (await invokeFn("terminal_resolve_resume_account", {
      claudeSessionId: q.claudeSessionId,
      configDir: q.configDir || null,
      workingDir: q.workingDir || null,
    })) as CommandResponse | null;
    const data = resp?.data as { resumeConfigDir?: unknown } | undefined;
    if (typeof data?.resumeConfigDir === "string") resolved = data.resumeConfigDir;
  } catch (err) {
    console.warn(
      `[resumeAccount] terminal_resolve_resume_account failed for ${q.claudeSessionId} — ` +
        "falling back to the recorded account (UNKNOWN whether it holds the transcript):",
      err,
    );
  }
  return resolveRestoreConfigDir({
    resumeConfigDir: resolved,
    configDir: q.configDir || undefined,
  });
}

/**
 * The account a parked zone-profile session resumes under: its resolved
 * `resumeConfigDir`, falling back to the profile's saved `claudeConfigDir`
 * only when no resolved dir is usable. Pure.
 */
export function profileSessionAccount(s: ZoneSessionInfo): string | undefined {
  return resolveRestoreConfigDir({
    resumeConfigDir: s.resumeConfigDir,
    configDir: s.claudeConfigDir,
  });
}

/**
 * The line typed into a zone-profile session's pane. Built by the boot
 * restore's `buildResumeCmd` (autonomous permission mode, resume-size picker
 * policy, PowerShell vs POSIX env syntax) with the RESOLVED account, so a
 * profile session saved without an account still names one. Pure.
 */
export function profileResumeCommand(s: ZoneSessionInfo): string {
  return buildResumeCmd(s.claudeSessionId, profileSessionAccount(s));
}

/**
 * Resolve every profile session's account up front (one backend call each, in
 * parallel), stamping it as `resumeConfigDir` on a copy. Never throws: a
 * failed resolution leaves the saved `claudeConfigDir` to stand.
 */
export async function withResolvedResumeAccounts(
  sessions: ZoneSessionInfo[],
  resolve: (q: ResumeAccountQuery) => Promise<string | undefined> = resolveResumeAccount,
): Promise<ZoneSessionInfo[]> {
  return Promise.all(
    sessions.map(async (s) => ({
      ...s,
      resumeConfigDir: await resolve({
        claudeSessionId: s.claudeSessionId,
        configDir: s.claudeConfigDir,
      }),
    })),
  );
}

/** One step of applying a profile's sessions to tabs. */
export interface ProfileTabStep {
  session: ZoneSessionInfo;
  /** An existing tab to reuse for this session's zone. */
  reuseTabId?: string;
  /**
   * When no tab is reused: the account to PIN the freshly spawned pane's PTY
   * to (`terminal_create` `claudeConfigDir`), so the pane runs under the
   * transcript's account rather than the spawn-time picker's.
   */
  pin?: string;
}

/**
 * Pair each profile session with a tab: existing tabs first, in order (the
 * pre-existing assignment rule), then one freshly spawned pane per remaining
 * session, pinned to that session's resolved account. A reused tab's PTY
 * already exists and cannot be re-pinned; the account then rides the typed
 * `CLAUDE_CONFIG_DIR=` prefix alone (`profileResumeCommand`). Pure.
 */
export function planProfileTabs(
  sessions: ZoneSessionInfo[],
  liveTabIds: string[],
): ProfileTabStep[] {
  return sessions.map((session, i) =>
    i < liveTabIds.length
      ? { session, reuseTabId: liveTabIds[i] }
      : { session, pin: profileSessionAccount(session) },
  );
}
