/**
 * THE operator-initiated resume path: open a new tab for a session and type its
 * `--resume`, verified — shared by every one-click Resume (Previous Sessions
 * cards, the transcript panel, the session manager) and by the "Since restart"
 * bulk resume, so single and bulk resume cannot drift apart (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phase 4).
 *
 * It is the boot restore's own pair — `createTerminal` + `runVerifiedResume`
 * — so the typed command is the shared `buildResumeCmd` (provider-shaped,
 * `CLAUDE_CONFIG_DIR` only for a known non-default account), the handshake is
 * verified with one retry, a failed resume parks the tab in `resumeFailed`
 * (the `ResumeFailedBanner` Retry), and the registry OPEN record is written
 * only once the handshake verified. Each call takes its own spawn through the
 * resource guard (`createTerminal`'s `spawnSource`), so a bulk caller has no
 * bypass of it.
 */

import { invoke } from "@tauri-apps/api/core";
import type { ResourceGuardSource } from "@/lib/resourceGuard";
import { describeThrown } from "@/lib/utils";
import { loadDefaultConfigHome, resolveAccountDir } from "./defaultConfigHome";
import { rememberSessionId } from "./lastKnownSessionIds";
import { fetchLiveClaudeSessionIds } from "./liveClaudeSessions";
import {
  buildSessionOpenArgs,
  noteRecordedZone,
  recordedZoneLedgerFor,
  UNZONED_INDEX,
  type OpenRecordTab,
  type SessionOpenArgs,
} from "./sessionRecordArgs";
import { runVerifiedResume } from "./useTerminalInitialization";
import type { PastSession } from "./usePastSessions";
import type { TerminalRefsMap } from "./writeWhenReady";

/** What to resume. */
export interface ResumeTarget {
  claudeSessionId: string;
  /** The new tab's name — the display-name rule, never the bare `"claude"` label. */
  displayName: string;
  /** The directory the session was launched in (Claude Code scopes sessions by it). */
  workingDir: string;
  /**
   * The account's config dir; `undefined` = the default home. Pass the default
   * home's EXPLICIT path when it is known: the verified resume records it (so
   * the session no longer reads "account unknown"), and the typed command
   * drops it (`runVerifiedResume`). Callers pass an account they KNOW — an
   * unknown account is refused before this point; a dir that cannot be typed
   * safely is refused here (`needs-account`, see `resolveAccountDir`).
   */
  configDir: string | undefined;
  /** Provider owning the session (`"claude"` when absent). */
  provider?: string;
}

/** Why a resume did not come back. */
export type ResumeFailure =
  | "invalid-session-id"
  | "needs-account"
  | "already-running"
  | "not-created"
  | "not-verified";

export type ResumeAttempt =
  | { kind: "verified"; tabId: string }
  | { kind: "failed"; failure: ResumeFailure; tabId: string | null };

/** Operator-facing sentence for a {@link ResumeFailure}. */
export function describeResumeFailure(failure: ResumeFailure): string {
  switch (failure) {
    case "invalid-session-id":
      return "the session id is not shell-safe";
    case "needs-account":
      return "account unknown — its account directory cannot be typed safely (not shell-safe, or possibly the unreadable default account)";
    case "already-running":
      return "already running in another window — a second resume would fork it";
    case "not-created":
      return "no terminal was created (the resource guard refused or the spawn failed)";
    case "not-verified":
      return "the resume did not verify — see the tab's Retry";
  }
}

const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;

/**
 * The operator-facing notice for a one-click resume that did not come back, or
 * `null` when it did. `not-verified` names the tab's own Retry; every other
 * failure left no tab to look at, so without this it would be silent.
 */
export function resumeFailureNotice(attempt: ResumeAttempt, displayName: string): string | null {
  if (attempt.kind === "verified") return null;
  return `Could not resume "${displayName}": ${describeResumeFailure(attempt.failure)}`;
}

export interface ResumeInNewTabDeps {
  createTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
    spawnSource?: ResourceGuardSource,
  ) => Promise<string | null>;
  updateTab: (
    id: string,
    updates: Partial<{
      claudeSessionId?: string;
      claudeConfigDir?: string;
      claudeRecordConfigDir?: string;
      isReconnecting?: boolean;
      resumeFailed?: boolean;
    }>,
  ) => void;
  terminalRefs: TerminalRefsMap;
  /** Page the new tab lands on — recorded in the durable registry on verify. */
  pageId: string;
  /** Injectable for tests: the live-session ids, or null when unreadable. */
  liveSessionIds?: () => Promise<ReadonlySet<string> | null>;
  /** Injectable for tests: the verified type-and-check. */
  verify?: typeof runVerifiedResume;
  /** Injectable for tests: the default Claude home (see `defaultConfigHome.ts`). */
  defaultConfigHome?: () => Promise<string | null>;
}

/**
 * The account split for a resume: `recordDir` for the registry record only,
 * `typedDir` for the typed command, the tab and the last-known id. `null` when
 * the dir cannot be resumed under (`needs-account`). No dir at all is the
 * default home with no explicit path known.
 */
async function resumeAccountDirs(
  configDir: string | undefined,
  defaultConfigHome: (() => Promise<string | null>) | undefined,
): Promise<{ recordDir: string | undefined; typedDir: string | undefined } | null> {
  if (!configDir?.trim()) return { recordDir: undefined, typedDir: undefined };
  const home = await (defaultConfigHome ?? loadDefaultConfigHome)();
  const dir = resolveAccountDir(configDir, home);
  return dir.kind === "resolved" ? { recordDir: dir.recordDir, typedDir: dir.typedDir } : null;
}

/**
 * Open a tab for `target` and type its verified `--resume`.
 *
 * A session some live process already hosts is refused (`already-running`):
 * typing `--resume` would fork its transcript. An UNREADABLE live registry
 * does not refuse — unlike the unattended boot restore, which fails closed,
 * this is an operator's explicit click — and the verification still guards the
 * typed command.
 */
export async function resumeInNewTab(
  deps: ResumeInNewTabDeps,
  target: ResumeTarget,
  spawnSource?: ResourceGuardSource,
): Promise<ResumeAttempt> {
  const { claudeSessionId } = target;
  if (!SESSION_ID_RE.test(claudeSessionId)) {
    return { kind: "failed", failure: "invalid-session-id", tabId: null };
  }
  // Every account passes the same rule the boot restore uses
  // (`resolveAccountDir`). A rejected dir is NOT dropped (that would resume
  // under the default account, failing as "No conversation found") — the
  // resume is refused.
  const dirs = await resumeAccountDirs(target.configDir, deps.defaultConfigHome);
  if (dirs === null) {
    return { kind: "failed", failure: "needs-account", tabId: null };
  }
  const { recordDir, typedDir } = dirs;
  const live = await (deps.liveSessionIds ?? fetchLiveClaudeSessionIds)();
  if (live?.has(claudeSessionId)) {
    return { kind: "failed", failure: "already-running", tabId: null };
  }

  const tabId = await deps.createTerminal(
    target.displayName,
    target.workingDir,
    undefined,
    spawnSource,
  );
  if (!tabId) return { kind: "failed", failure: "not-created", tabId: null };

  // The tab and the last-known id carry the TYPED form: the default home's
  // explicit path goes to the registry record only (and to the tab's
  // never-typed `claudeRecordConfigDir`, which a Retry records under).
  deps.updateTab(tabId, {
    claudeSessionId,
    claudeConfigDir: typedDir,
    claudeRecordConfigDir: recordDir,
    isReconnecting: true,
  });
  // Durable per-tab id, so a close→reopen of this tab stays resumable.
  rememberSessionId(tabId, claudeSessionId, typedDir);
  // The tab has no zone yet: note the honest UNZONED record so the page's
  // re-resolution backstop corrects it once auto-fill places the tab.
  noteRecordedZone(recordedZoneLedgerFor(deps.pageId), claudeSessionId, UNZONED_INDEX);

  const outcome = await (deps.verify ?? runVerifiedResume)({
    terminalRefs: deps.terminalRefs,
    tabId,
    claudeSessionId,
    configDir: typedDir,
    provider: target.provider,
    updateTab: deps.updateTab,
    defaultConfigHome: deps.defaultConfigHome,
    // Written only on a VERIFIED handshake. `authoritative`: `--resume` names
    // the exact id, so the binding is known, not inferred.
    recordOpen: {
      claudeSessionId,
      configDir: recordDir,
      workingDir: target.workingDir,
      pageId: deps.pageId,
      zoneIndex: UNZONED_INDEX,
      title: target.displayName,
      terminalId: tabId,
      origin: "authoritative",
      provider: target.provider,
    },
    // A fresh plain shell: no claude can be in it before the first type.
    verifyOptions: { skipFirstProbe: true },
  });
  return outcome === "verified"
    ? { kind: "verified", tabId }
    : { kind: "failed", failure: "not-verified", tabId };
}

export interface ResumeProfileSessionDeps {
  terminalRefs: TerminalRefsMap;
  updateTab: ResumeInNewTabDeps["updateTab"];
  /**
   * Surfaces an operator-facing notice — the page's notification, the same
   * one a Previous Sessions resume reports through (`pastSessionResumeNotice`).
   */
  notify: (message: string) => void;
  /** Injectable for tests: the verified type-and-check. */
  verify?: typeof runVerifiedResume;
  /** Injectable for tests: the default Claude home. */
  defaultConfigHome?: () => Promise<string | null>;
  /** Injectable for tests: the Tauri `invoke`. */
  invokeFn?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}

/**
 * Resume a zone-profile session into the tab its zone already holds — the
 * same account rule and the same verified, typed choke point
 * (`runVerifiedResume`) as every other resume. `recordOpen` is the open
 * record for the verified branch; its `configDir` is replaced by the explicit
 * path, while the tab carries the typed form, so a profile saved from it never
 * stores the default home as a dir to type. The caller has already gated the
 * id (shell-safe, not live elsewhere).
 *
 * Like the boot drain, the session is marked restore-pending before the
 * resume is typed, so a failed verification parks the tab in its Retry with
 * the open record protected from the liveness poll's close; the verified
 * branch of `runVerifiedResume` clears the marker. An account that cannot be
 * resumed under is not typed: the session is held as awaiting an account (so
 * the poll keeps it open on the roster) and the refusal is surfaced.
 */
export async function resumeProfileSession(
  deps: ResumeProfileSessionDeps,
  args: {
    tabId: string;
    claudeSessionId: string;
    configDir: string | undefined;
    recordOpen: SessionOpenArgs;
  },
): Promise<ResumeAttempt> {
  const { tabId, claudeSessionId } = args;
  const call = deps.invokeFn ?? ((cmd, cmdArgs) => invoke(cmd, cmdArgs));
  const dirs = await resumeAccountDirs(args.configDir, deps.defaultConfigHome);
  if (dirs === null) {
    const refused: ResumeAttempt = { kind: "failed", failure: "needs-account", tabId: null };
    await call("terminal_session_mark_awaiting_account", { claudeSessionId }).catch(
      (err: unknown) =>
        console.warn(
          `[TerminalSession] could not hold needs-account session ${claudeSessionId}:`,
          err,
        ),
    );
    const notice = resumeFailureNotice(
      refused,
      args.recordOpen.title || `claude ${claudeSessionId.slice(0, 8)}`,
    );
    if (notice) deps.notify(notice);
    return refused;
  }
  deps.updateTab(tabId, {
    claudeSessionId,
    claudeConfigDir: dirs.typedDir,
    claudeRecordConfigDir: dirs.recordDir,
  });
  // `boot: false`: an operator-initiated resume, not a boot restore — the
  // backend sets only the pending marker (never the boot-restore census
  // stamps) and only on an open row.
  await call("terminal_session_mark_restore_pending", { claudeSessionId, boot: false }).catch(
    (err: unknown) =>
      console.warn(`[TerminalSession] mark restore-pending failed for ${claudeSessionId}:`, err),
  );
  const outcome = await (deps.verify ?? runVerifiedResume)({
    terminalRefs: deps.terminalRefs,
    tabId,
    claudeSessionId,
    configDir: dirs.typedDir,
    updateTab: deps.updateTab,
    defaultConfigHome: deps.defaultConfigHome,
    recordOpen: { ...args.recordOpen, configDir: dirs.recordDir },
  });
  return outcome === "verified"
    ? { kind: "verified", tabId }
    : { kind: "failed", failure: "not-verified", tabId };
}

/**
 * The `ResumeFailedBanner` Retry's resume arguments for `tab`: the TYPED dir
 * for the command, and the tab's never-typed `claudeRecordConfigDir` for the
 * open record re-asserted on a verified handshake — so a Retry of a
 * default-home session (typed form `undefined`) still records its account.
 * `null` when the tab carries no session. No `origin` in the record, so the
 * backend preserves the row's existing origin.
 */
export function retryResumeArgs(params: {
  tab: Pick<OpenRecordTab, "id"> & {
    claudeSessionId?: string;
    claudeConfigDir?: string;
    claudeRecordConfigDir?: string;
  };
  assignments: Record<number, string>;
  tabs: OpenRecordTab[];
  pageId: string;
}): { claudeSessionId: string; configDir: string | undefined; recordOpen: SessionOpenArgs } | null {
  const { tab, assignments, tabs, pageId } = params;
  if (!tab.claudeSessionId) return null;
  return {
    claudeSessionId: tab.claudeSessionId,
    configDir: tab.claudeConfigDir,
    recordOpen: buildSessionOpenArgs({
      assignments,
      tabs,
      tabId: tab.id,
      claudeSessionId: tab.claudeSessionId,
      configDir: tab.claudeRecordConfigDir ?? tab.claudeConfigDir,
      pageId,
    }),
  };
}

/**
 * A Previous Sessions card's one-click Resume, through THE resume path:
 * resolves to the operator-facing notice when it did not come back, `null`
 * when it did. Every way it can fail is said — a refusal before any tab (an
 * unknown account, no directory), an attempt that failed, AND a resume that
 * REJECTED (an IPC that threw), which would otherwise be silent. The directory
 * is THE resume-dir rule's (`resumeDir`: the launch dir, else its worktree
 * root), the same one the copy line uses.
 */
export async function pastSessionResumeNotice(
  ps: Pick<
    PastSession,
    "claudeSessionId" | "resumeName" | "resumeAccount" | "resumeDir" | "provider"
  >,
  resume: (target: ResumeTarget) => Promise<ResumeAttempt>,
): Promise<string | null> {
  const displayName = ps.resumeName || `claude ${ps.claudeSessionId.slice(0, 8)}`;
  if (!ps.resumeAccount.known || !ps.resumeDir) {
    return `Could not resume "${displayName}": ${
      ps.resumeAccount.known
        ? "no working directory was recorded"
        : "the account it ran under is unknown"
    }`;
  }
  try {
    const attempt = await resume({
      claudeSessionId: ps.claudeSessionId,
      displayName,
      workingDir: ps.resumeDir,
      configDir: ps.resumeAccount.configDir ?? undefined,
      provider: ps.provider,
    });
    return resumeFailureNotice(attempt, displayName);
  } catch (err: unknown) {
    return `Could not resume "${displayName}": ${describeThrown(err, "resume failed")}`;
  }
}
