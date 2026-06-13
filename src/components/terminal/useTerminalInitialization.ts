import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LAYOUT_PRESETS } from "./useZoneLayout";
import type { TerminalRefsMap } from "./writeWhenReady";
import {
  typeResumeAndVerify,
  getResumeSummaryPolicy,
  buildPickerAnswer,
  type ResumeSummaryPolicy,
  type TypeAndVerifyOptions,
} from "./resumeVerification";
import type { TerminalTab } from "./useTerminalManager";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { CommandResponse, TerminalSessionRecord } from "./types";
import type { SaveSessionLayoutParams } from "./useSessionPersistence";
import { rememberSessionId } from "./lastKnownSessionIds";

/**
 * Fetch the durable RESTORABLE session records for `pageId` from the backend
 * registry, filtered to this page and deduped by `claudeSessionId` (defensive
 * — the registry should already enforce one row per id, but a duplicate would
 * otherwise spawn two tabs for one session). This is the SOURCE OF TRUTH for
 * the zone↔session binding on restore, replacing the ephemeral-tabId
 * creation-order mapping the localStorage snapshot used to drive.
 *
 * `terminal_session_list_open` returns the restorable superset: `open` records
 * (hard-crash case) PLUS in-grace `closed`/`pty-exit` records (graceful-restart
 * case, where `handleExit` flipped every live PTY to `closed`). The backend
 * owns the state/reason/grace gating, so we deliberately do NOT re-filter on
 * `state === "open"` here — that would drop the pty-exit records the backend
 * just decided are restorable.
 *
 * Exported for unit testing the restore-binding logic without booting React.
 */
export async function fetchOpenRecords(pageId: string): Promise<TerminalSessionRecord[]> {
  let resp: CommandResponse | null;
  try {
    resp = await invoke<CommandResponse>("terminal_session_list_open");
  } catch (err) {
    console.warn("[TerminalPage] terminal_session_list_open failed:", err);
    return [];
  }
  const sessions = (resp?.data as { sessions?: TerminalSessionRecord[] } | undefined)?.sessions;
  if (!Array.isArray(sessions)) return [];
  const byId = new Map<string, TerminalSessionRecord>();
  for (const rec of sessions) {
    if (!rec || typeof rec.claudeSessionId !== "string") continue;
    if ((rec.pageId ?? "default") !== pageId) continue;
    if (!byId.has(rec.claudeSessionId)) byId.set(rec.claudeSessionId, rec);
  }
  return [...byId.values()];
}

/**
 * Build a `claude --resume <id>` command, optionally prefixed with
 * CLAUDE_CONFIG_DIR and (under the default full-resume policy) with the CLI's
 * resume-size thresholds raised so the interactive "Resume from summary?"
 * picker never shows — an unattended restore can't answer it, and a wedged
 * picker reads as a failed resume (#548 item 3). No suppression flag exists;
 * the CLI consults `CLAUDE_CODE_RESUME_TOKEN_THRESHOLD` (default 100000
 * estimated tokens) and `CLAUDE_CODE_RESUME_THRESHOLD_MINUTES` (default 70)
 * before showing it, and skipping it resumes the full session as-is. Scoped
 * to the typed command (per-process) deliberately: seeding the CLI's global
 * "Don't ask again" key (`resumeReturnDismissed` in `.claude.json`) would
 * also kill the picker for the operator's own interactive resumes.
 *
 * Under the opt-in `"summary"` policy the thresholds are left alone so the
 * picker appears and the verification loop answers it
 * (`buildPickerAnswer`). Exported for unit tests.
 *
 * Autonomous resume (`--permission-mode bypassPermissions`, matching the
 * zone-profile + shell-integration resume builders): a boot-restored session
 * is unattended, so a bare `claude --resume` would stall forever on its first
 * permission prompt. Aligned with PR #547 — whichever lands second resolves
 * the textual conflict by keeping this union form.
 */
export function buildResumeCmd(
  sessionId: string,
  configDir: string | undefined,
  policy: ResumeSummaryPolicy = getResumeSummaryPolicy(),
): string {
  const base = `claude --permission-mode bypassPermissions --resume ${sessionId}`;
  const env: Array<[string, string]> = [];
  if (configDir) env.push(["CLAUDE_CONFIG_DIR", configDir]);
  if (policy === "full") {
    env.push(["CLAUDE_CODE_RESUME_TOKEN_THRESHOLD", "999999999"]);
    env.push(["CLAUDE_CODE_RESUME_THRESHOLD_MINUTES", "999999999"]);
  }
  if (env.length === 0) return `${base}\r`;
  const isWindows =
    typeof navigator !== "undefined" && (navigator.platform ?? "").startsWith("Win");
  return isWindows
    ? `${env.map(([k, v]) => `$env:${k}="${v}"; `).join("")}${base}\r`
    : `${env.map(([k, v]) => `${k}="${v}" `).join("")}${base}\r`;
}

/**
 * Re-assert payload for [`runVerifiedResume`]'s verified branch: the durable
 * registry record is re-marked `open` under the NEW terminal id only AFTER
 * the resume handshake verifies. Re-asserting at tab creation (the old
 * behavior) refreshed `lastSeenAt` on every cold-restored record — including
 * ghosts whose resume then failed — resetting both the prune clock and the
 * restore recency cap, so a ghost restored forever ("ghost immortality").
 */
export interface ReassertRecordPayload {
  pageId: string;
  zoneIndex: number;
  title?: string;
  workingDir?: string;
  configDir?: string;
}

/**
 * Type the resume command for a restored tab and VERIFY the Claude UI
 * handshake actually appeared (Phase 3, issue #548): on first failure the
 * same command is retyped once; on persistent failure the tab is parked in an
 * explicit `resumeFailed` state (operator-clickable retry via
 * `ResumeFailedBanner`) and the durable restore-pending marker is left SET so
 * the backend liveness poll keeps protecting the `open` record. Only a
 * verified handshake clears the marker and the reconnecting affordance — and
 * only a verified handshake re-asserts the registry record under the new
 * terminal id (`reassert`); on failure the original row is left untouched
 * (original `lastSeenAt`, old `terminalId`), so it ages and prunes normally.
 *
 * Used by the boot-restore drain below AND by the operator retry/confirm
 * paths in `TerminalPage`. Exported (with injectable verify options) so the
 * verified/failed state machine is unit-testable without booting React.
 */
export async function runVerifiedResume(params: {
  terminalRefs: TerminalRefsMap;
  tabId: string;
  claudeSessionId: string;
  configDir?: string;
  updateTab: (
    id: string,
    updates: Partial<{ isReconnecting?: boolean; resumeFailed?: boolean }>,
  ) => void;
  /** When set, the verified branch re-asserts the record under `tabId`. */
  reassert?: ReassertRecordPayload;
  verifyOptions?: TypeAndVerifyOptions;
}): Promise<"verified" | "failed"> {
  const { terminalRefs, tabId, claudeSessionId, configDir, updateTab, reassert, verifyOptions } =
    params;
  const policy = getResumeSummaryPolicy();
  const resumeCmd = buildResumeCmd(claudeSessionId, configDir, policy);
  const outcome = await typeResumeAndVerify(terminalRefs, tabId, resumeCmd, {
    // Fallback picker answerer (#548 item 3): under the default "full" policy
    // the env thresholds in `buildResumeCmd` already suppress the picker;
    // this catches CLI version drift and the opt-in "summary" policy.
    pickerAnswer: buildPickerAnswer(policy),
    ...verifyOptions,
  });
  if (outcome === "verified") {
    updateTab(tabId, { isReconnecting: false, resumeFailed: false });
    // Verified handshake — NOW re-assert the open record under the freshly
    // created terminal id so the registry's `terminalId` tracks the live tab
    // (the next restart reconnect-matches on this id). Deliberately deferred
    // past verification: a failed resume must leave the original row's
    // `lastSeenAt` untouched so a ghost ages and prunes instead of restoring
    // forever.
    if (reassert) {
      invoke("terminal_session_record_open", {
        claudeSessionId,
        configDir: reassert.configDir,
        workingDir: reassert.workingDir,
        pageId: reassert.pageId,
        zoneIndex: reassert.zoneIndex,
        title: reassert.title,
        terminalId: tabId,
      }).catch((err) => {
        console.warn(`[TerminalPage] re-record open failed for ${claudeSessionId}:`, err);
      });
    }
    // Release the liveness-poll restore guard so normal classification
    // resumes for this session.
    invoke("terminal_session_clear_restore_pending", { claudeSessionId }).catch((err) => {
      // Best-effort: the poll self-heals a stale marker on the next
      // confident-alive tick.
      console.warn(`[TerminalPage] clear restore-pending failed for ${claudeSessionId}:`, err);
    });
  } else {
    // Resume never landed. Surface an explicit retry affordance and KEEP the
    // restore-pending marker — the open record must survive for the retry.
    console.warn(
      `[TerminalPage] resume verification failed for ${tabId} (session ${claudeSessionId})`,
    );
    updateTab(tabId, { isReconnecting: false, resumeFailed: true });
  }
  return outcome;
}

/**
 * Pure guard for the once-per-pageId init backstop (Phase 3 mount-hydration
 * lift). With the session provider lifted above the page, the single
 * `useTerminalInitialization` instance persists across terminal-page switches,
 * so the convergence backstop (reconnect + durable-record restore + resume
 * drain) must run exactly ONCE per pageId — not once per mount. Returns `true`
 * (and records `pageId` as seen) the FIRST time a page is initialized;
 * `false` on every subsequent call for that page.
 *
 * Mutating the passed Set keeps the call-site a one-liner and lets the hook
 * hold the Set in a ref. Exported so the per-pageId semantics can be unit-
 * tested without booting React (vitest `environment: "node"`).
 */
export function claimInitForPage(seen: Set<string>, pageId: string): boolean {
  if (seen.has(pageId)) return false;
  seen.add(pageId);
  return true;
}

/** Validate session IDs before interpolating into shell commands. */
const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
function isValidSessionId(id: string): boolean {
  return SESSION_ID_RE.test(id);
}

/** Validate config dir paths — reject shell metacharacters. */
const SAFE_PATH_RE = /^[a-zA-Z0-9_\-./\\: ]+$/;
function sanitizeConfigDir(dir: string | undefined): string | undefined {
  if (!dir) return undefined;
  return SAFE_PATH_RE.test(dir) ? dir : undefined;
}

interface UseTerminalInitializationParams {
  /** Which terminal page this restore runs for ("default" when unset). */
  pageId: string;
  tabs: TerminalTab[];
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  reconnectToExistingSessions: () => Promise<string[] | null>;
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  createPlanTab: (filePath: string) => string | null;
  setInitialized: (v: boolean) => void;
  updateTab: (
    id: string,
    updates: Partial<{
      claudeSessionId?: string;
      claudeConfigDir?: string;
      isReconnecting?: boolean;
      resumeFailed?: boolean;
      resumeNeedsConfirm?: boolean;
    }>,
  ) => void;
  zoneLayout: {
    layoutId: string;
    setLayoutId: (id: string) => void;
    assignTabToZone: (zoneIdx: number, tabId: string) => void;
    setFocusedZone: (zoneIdx: number) => void;
    assignments: Record<number, string>;
  };
  labelsAndTags: {
    setZoneLabel: (zoneIdx: number, label: string) => void;
    setZoneNote: (zoneIdx: number, note: string) => void;
    setPinnedZones: React.Dispatch<React.SetStateAction<Set<number>>>;
  };
  sessionPersistence: {
    saveSessionLayout: (params: SaveSessionLayoutParams) => void;
    saveScrollbackBuffers: (tabs: Array<{ id: string }>) => Promise<Record<string, string>>;
    updateScrollbackPaths: (
      pathMap: Record<string, string>,
      tabIdToSessionIndex: Record<string, number>,
    ) => void;
    getSavedLayout: () => {
      layoutId: string;
      focusedZone: number;
      sessions: Array<{
        zoneIndex: number;
        title: string;
        workingDir?: string;
        type?: "terminal" | "plan";
        planFilePath?: string;
        scrollbackPath?: string;
        isClaudeSession?: boolean;
        claudeSessionId?: string;
        claudeConfigDir?: string;
        label?: string;
        notes?: string;
        pinned?: boolean;
      }>;
    } | null;
    clearSavedLayout: () => void;
    hasSavedLayout: () => boolean;
  };
  layoutState: {
    layoutId: string;
    zoneLabels: Record<number, string>;
    zoneNotes: Record<number, string>;
    pinnedZones: Set<number>;
    focusedZone: number;
  };
}

export function useTerminalInitialization({
  pageId,
  tabs,
  terminalRefs,
  reconnectToExistingSessions,
  createTerminal,
  createPlanTab,
  setInitialized,
  updateTab,
  zoneLayout,
  labelsAndTags,
  sessionPersistence,
  layoutState,
}: UseTerminalInitializationParams) {
  // Phase 3 (mount-hydration lift): with the session provider lifted above the
  // page and per-page scopes always mounted, a terminal-PAGE switch no longer
  // unmounts/remounts this hook — so the old "REMOUNTED … tabs were lost" /
  // "UNMOUNTED … will be destroyed" warnings (which fired on every page
  // switch) are gone. A genuine unmount of the whole tree (auth change) is
  // still surfaced once.
  const mountCountRef = useRef(0);
  useEffect(() => {
    mountCountRef.current += 1;
    const mountNum = mountCountRef.current;
    if (mountNum > 1) {
      console.warn(
        `[TerminalPage] REMOUNTED (mount #${mountNum}) — the terminal page tree remounted. ` +
          `Page state is preserved by the lifted session provider; this usually means the ` +
          `parent app tree unmounted (e.g., auth state change).`,
      );
    }
  }, []);

  // Init / restore guards are keyed by pageId, NOT per hook mount. The single
  // TerminalPage instance now persists across terminal-page switches (no
  // remount), so the convergence backstop (reconnect + durable-record restore
  // + resume-drain) must run exactly ONCE per pageId — the first time each page
  // becomes active — rather than once per mount.
  const didInitPages = useRef<Set<string>>(new Set());

  // Gate for the debounced auto-save effect. The restore path recreates
  // plain shells and only *asynchronously* types `claude --resume <id>` /
  // re-attaches `claudeSessionId`. If the debounced auto-save fires while
  // those tabs are still plain (no claudeSessionId yet), it persists the
  // degraded layout and clobbers the good saved Claude layout — leaving
  // nothing to resume on the next reopen. We keep auto-save suppressed
  // until restore has fully drained (resume commands issued / ids merged),
  // then open the gate exactly once. The flag is opened in a `finally` so
  // it ALWAYS flips — even when there were no saved sessions or restore
  // threw — so brand-new sessions still persist normally.
  //
  // Keyed per pageId (Phase 3): each page's restore drains independently, and
  // auto-save runs in the single page instance for whichever page is active.
  const restoreCompletePages = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (!claimInitForPage(didInitPages.current, pageId)) return;
    const initPageId = pageId;

    // Per-page restore queue (Phase 3): local to this page's init run so a
    // second page initializing before this one's drain timer fires can't see
    // (or clear) the other page's pending restores.
    const pendingRestores: Array<{
      tabId: string;
      scrollbackPath?: string;
      isClaudeSession?: boolean;
      claudeSessionId?: string;
      claudeConfigDir?: string;
      reassert?: ReassertRecordPayload;
    }> = [];

    (async () => {
      // True once a deferred resume/scrollback drain timer is scheduled. When
      // set, that timer owns flipping the per-page restore-complete gate (after
      // it issues the resume commands); the `finally` below only opens the gate
      // when NO drain was scheduled, so the flag always flips exactly once.
      let drainScheduled = false;
      try {
        // 1) Reconnect to live PTYs that survived a React remount. These tabs
        //    are plain (no claudeSessionId on the wire) but their ids are the
        //    SAME stable terminal ids the registry recorded under `terminalId`.
        const reconnectedTabIds = await reconnectToExistingSessions();
        const reconnectedSet = new Set(reconnectedTabIds ?? []);

        // 2) The durable backend session registry is the SOURCE OF TRUTH for
        //    which Claude sessions exist and their zones. The localStorage
        //    snapshot is demoted to cosmetics only (layout / labels / notes /
        //    pins / focusedZone / scrollback) — matched by zoneIndex.
        const openRecords = await fetchOpenRecords(pageId);

        // Cosmetic snapshot — never the resumable Claude set / zone binding.
        const saved = sessionPersistence.hasSavedLayout()
          ? sessionPersistence.getSavedLayout()
          : null;

        // Per-zone cosmetics lookups from the snapshot (matched by zoneIndex).
        const cosmeticsByZone = new Map<
          number,
          { label?: string; notes?: string; pinned?: boolean; scrollbackPath?: string }
        >();
        if (saved) {
          for (const s of saved.sessions) {
            if (s.zoneIndex < 0) continue;
            cosmeticsByZone.set(s.zoneIndex, {
              label: s.label,
              notes: s.notes,
              pinned: s.pinned,
              scrollbackPath: s.scrollbackPath,
            });
          }
        }

        const applyZoneCosmetics = (zoneIndex: number) => {
          if (zoneIndex < 0) return;
          const c = cosmeticsByZone.get(zoneIndex);
          if (!c) return;
          if (c.label) labelsAndTags.setZoneLabel(zoneIndex, c.label);
          if (c.notes) labelsAndTags.setZoneNote(zoneIndex, c.notes);
          if (c.pinned) labelsAndTags.setPinnedZones((prev) => new Set([...prev, zoneIndex]));
        };

        // Restore the layout preset from the cosmetic snapshot if it differs.
        // The restored layout is a starting point only — auto-grow may expand
        // it later so every live session keeps a visible zone.
        if (saved && saved.layoutId !== zoneLayout.layoutId) {
          const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
          if (preset) zoneLayout.setLayoutId(preset.id);
        }

        // 3) Bind every open Claude record to its RECORDED zone — Claude zones
        //    are claimed from records FIRST so the creation-order auto-fill in
        //    `useZoneLayout` can never steal a zone a record owns (it only fills
        //    zones still empty after this loop runs).
        for (const rec of openRecords) {
          const safeConfigDir = sanitizeConfigDir(rec.configDir);
          const validSessionId = isValidSessionId(rec.claudeSessionId);

          // a) A live reconnected PTY is already running this session (React
          //    remount): match by the record's stable terminalId. Just rebind
          //    the zone + re-attach the claudeSessionId; no resume needed.
          if (reconnectedSet.has(rec.terminalId)) {
            const tabId = rec.terminalId;
            if (rec.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(rec.zoneIndex, tabId);
              applyZoneCosmetics(rec.zoneIndex);
            }
            updateTab(tabId, {
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: safeConfigDir,
            });
            rememberSessionId(tabId, rec.claudeSessionId, safeConfigDir);
            continue;
          }

          // b) Cold restart (no live pty): recreate the tab, bind its recorded
          //    zone, attach the session id, and queue a `claude --resume` via
          //    the existing drain loop. The OPEN record is re-asserted under
          //    the new terminal id only AFTER the resume handshake verifies
          //    (`runVerifiedResume`) — re-asserting here refreshed ghosts'
          //    `lastSeenAt` before anything was proven, making them immortal.
          const tabId = await createTerminal(rec.title, rec.workingDir);
          if (!tabId) continue;
          if (rec.zoneIndex >= 0) {
            zoneLayout.assignTabToZone(rec.zoneIndex, tabId);
            applyZoneCosmetics(rec.zoneIndex);
          }

          // bindOrigin quarantine: only a `pinned` binding (`--session-id` /
          // `--resume` — exact) may auto-type `claude --resume`. A `guessed`
          // (or pre-field absent) binding came from the freshest-transcript
          // mtime race and may belong to a FOREIGN session (e.g. a VS Code
          // CLI session in the same project) — surface a one-click operator
          // confirm instead of blindly resuming wrong content. The tab is
          // still created so the screen layout is preserved.
          const pinned = rec.bindOrigin === "pinned";

          updateTab(tabId, {
            claudeSessionId: rec.claudeSessionId,
            claudeConfigDir: safeConfigDir,
            // Pinned: show the "resuming" affordance until the verified
            // resume drains. Guessed: park awaiting operator confirm.
            ...(pinned && validSessionId
              ? { isReconnecting: true }
              : { resumeNeedsConfirm: validSessionId }),
          });
          rememberSessionId(tabId, rec.claudeSessionId, safeConfigDir);

          if (validSessionId) {
            pendingRestores.push({
              tabId,
              scrollbackPath:
                rec.zoneIndex >= 0 ? cosmeticsByZone.get(rec.zoneIndex)?.scrollbackPath : undefined,
              // Auto-resume only proven (pinned) bindings; quarantined tabs
              // still get their scrollback restored by the drain.
              isClaudeSession: pinned,
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: safeConfigDir,
              reassert: {
                pageId,
                zoneIndex: rec.zoneIndex,
                title: rec.title,
                workingDir: rec.workingDir,
                configDir: rec.configDir,
              },
            });
            // Durable, backend-owned restore-pending marker (Phase 3, #548):
            // from here until the resume handshake is VERIFIED (or the
            // operator confirms/declines a quarantined binding) the liveness
            // poll must never flip this record `poll-dead`/`no-terminal` — a
            // failed restore leaves the `open` record intact for the next
            // attempt. Cleared in `runVerifiedResume` on verified handshake
            // (and self-healed by the poll once it sees the session
            // confidently alive).
            invoke("terminal_session_mark_restore_pending", {
              claudeSessionId: rec.claudeSessionId,
            }).catch((err) => {
              console.warn(
                `[TerminalPage] mark restore-pending failed for ${rec.claudeSessionId}:`,
                err,
              );
            });
          }
        }

        // 4) Plan tabs are cosmetic-only state held in the snapshot (no PTY, no
        //    registry record) — recreate them so the markdown viewers survive a
        //    cold restart. Skip on a React remount: live plan tabs are gone with
        //    the unmounted tree but their snapshot entry still re-creates them
        //    only when we cold-started (no reconnected pty tabs).
        if (saved && !reconnectedTabIds) {
          for (const session of saved.sessions) {
            if (session.type !== "plan" || !session.planFilePath) continue;
            const tabId = createPlanTab(session.planFilePath);
            if (tabId && session.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(session.zoneIndex, tabId);
              applyZoneCosmetics(session.zoneIndex);
            }
          }
        }

        // 5) Focused zone from the cosmetic snapshot.
        if (saved && saved.focusedZone >= 0) {
          zoneLayout.setFocusedZone(saved.focusedZone);
        }

        // 6) Drain: restore scrollback then issue `claude --resume` for every
        //    cold-created Claude tab. Identical mechanism to the prior restore;
        //    the per-page gate flips in the drain's finally.
        if (pendingRestores.length > 0) {
          drainScheduled = true;
          setTimeout(async () => {
            try {
              for (const restore of pendingRestores) {
                const ref = terminalRefs.current.get(restore.tabId);
                const handle = ref?.current;

                if (restore.scrollbackPath && handle) {
                  try {
                    const result = await invoke<CommandResponse>("terminal_get_saved_scrollback", {
                      filePath: restore.scrollbackPath,
                    });
                    if (result.success && result.data) {
                      const encoded = (result.data as { data: string }).data;
                      if (encoded) {
                        const raw = atob(encoded);
                        const decoded = new TextDecoder().decode(
                          Uint8Array.from(raw, (c) => c.charCodeAt(0)),
                        );
                        handle.writeToDisplay(decoded);
                      }
                    }
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to restore scrollback for ${restore.tabId}:`,
                      err,
                    );
                  }
                }

                if (
                  restore.isClaudeSession &&
                  restore.claudeSessionId &&
                  isValidSessionId(restore.claudeSessionId)
                ) {
                  await new Promise((r) => setTimeout(r, 500));
                  // Type the resume and VERIFY the handshake (retry once on
                  // failure). Fire-and-forget per tab so one slow/failed
                  // verification (up to ~30s) doesn't serialize the drain;
                  // `runVerifiedResume` owns clearing `isReconnecting` /
                  // setting `resumeFailed` and never throws.
                  void runVerifiedResume({
                    terminalRefs: terminalRefs.current,
                    tabId: restore.tabId,
                    claudeSessionId: restore.claudeSessionId,
                    configDir: restore.claudeConfigDir,
                    updateTab,
                    reassert: restore.reassert,
                  });
                }
              }

              try {
                await invoke("terminal_cleanup_scrollback");
              } catch (err) {
                console.warn("[TerminalPage] Failed to cleanup scrollback files:", err);
              }

              pendingRestores.length = 0;
            } finally {
              // Restored tabs now carry their claudeSessionId / scrollback —
              // safe to let the debounced auto-save persist this page's layout.
              restoreCompletePages.current.add(initPageId);
            }
          }, 1500);
        }

        // NOTE: deliberately do NOT clearSavedLayout(). The cosmetic snapshot
        // must survive until the debounced auto-save overwrites it (after the
        // drain has run and tabs hold their ids). The resumable Claude set now
        // comes from the registry, so a failed/never-firing resume no longer
        // risks losing sessions — the registry record persists regardless.
        // No default terminal — start empty so users can launch AI sessions via the Launch Menu.
        setInitialized(true);
      } finally {
        // Always open the auto-save gate. If a drain timer was scheduled it
        // owns the flip (after resume commands are issued); otherwise — no
        // saved sessions, nothing to restore, or restore threw — open it now
        // so brand-new sessions still persist. Never leave it permanently
        // closed (that would silently disable persistence).
        if (!drainScheduled) {
          restoreCompletePages.current.add(initPageId);
        }
      }
    })();
  }, [
    pageId,
    reconnectToExistingSessions,
    createTerminal,
    createPlanTab,
    setInitialized,
    sessionPersistence,
    zoneLayout,
    labelsAndTags,
    updateTab,
    terminalRefs,
  ]);

  // Auto-save session layout for persistence across app restarts
  useEffect(() => {
    if (tabs.length === 0) return;
    // Suppress auto-save until THIS page's restore has fully completed.
    // Otherwise the debounced save can fire while the restore path still holds
    // plain shells (no claudeSessionId yet) and clobber the good saved Claude
    // layout. Once the gate opens, the `updateTab` calls that attach
    // claudeSessionId mutate `tabs`, re-running this effect — so no save is
    // permanently lost.
    if (!restoreCompletePages.current.has(pageId)) return;
    sessionPersistence.saveSessionLayout({
      layoutId: layoutState.layoutId,
      tabs,
      assignments: zoneLayout.assignments,
      zoneLabels: layoutState.zoneLabels,
      zoneNotes: layoutState.zoneNotes,
      pinnedZones: layoutState.pinnedZones,
      focusedZone: layoutState.focusedZone,
    });
  }, [
    pageId,
    tabs,
    zoneLayout.assignments,
    layoutState.layoutId,
    layoutState.focusedZone,
    layoutState.zoneLabels,
    layoutState.zoneNotes,
    layoutState.pinnedZones,
    sessionPersistence,
  ]);

  // Refs for unmount/close handlers that need latest values
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const zoneLayoutRef = useRef(zoneLayout);
  useEffect(() => {
    zoneLayoutRef.current = zoneLayout;
  }, [zoneLayout]);
  const layoutStateRef = useRef(layoutState);
  useEffect(() => {
    layoutStateRef.current = layoutState;
  }, [layoutState]);

  // Immediate save on unmount (page switch) — the debounced auto-save may not
  // have flushed, so we save synchronously to avoid losing state.
  useEffect(() => {
    return () => {
      const ls = layoutStateRef.current;
      const currentTabs = tabsRef.current;
      if (currentTabs.length > 0) {
        sessionPersistence.saveSessionLayout({
          layoutId: ls.layoutId,
          tabs: currentTabs,
          assignments: zoneLayoutRef.current.assignments,
          zoneLabels: ls.zoneLabels,
          zoneNotes: ls.zoneNotes,
          pinnedZones: ls.pinnedZones,
          focusedZone: ls.focusedZone,
        });
      }
    };
  }, [sessionPersistence]);

  // Save scrollback buffers to disk when the window is about to close.
  //
  // NOTE: we deliberately do NOT record session CLOSEs to the durable registry
  // here. Window teardown is ambiguous with a supervisor restart — the same
  // `onCloseRequested` fires whether the user is quitting for good or the
  // supervisor is bouncing the app. Recording closes on window teardown would
  // mark still-live Claude sessions as closed and drop them from the next
  // restore. Closes are recorded only on EXPLICIT tab close (useTerminalManager
  // `closeTerminal`) and on pty-exit (TerminalPage `handleExit`).
  const handleWindowClose = useCallback(async () => {
    const currentTabs = tabsRef.current;
    if (currentTabs.length === 0) return;

    try {
      const terminalTabs = currentTabs.filter((t) => t.type !== "plan");
      const pathMap = await sessionPersistence.saveScrollbackBuffers(terminalTabs);
      const tabIdToSessionIndex: Record<string, number> = {};
      const currentAssignments = zoneLayoutRef.current.assignments;
      const assignedTabIds = new Set(Object.values(currentAssignments));
      let idx = 0;
      for (const [, tabId] of Object.entries(currentAssignments)) {
        if (currentTabs.some((t) => t.id === tabId)) {
          tabIdToSessionIndex[tabId] = idx++;
        }
      }
      for (const tab of currentTabs) {
        if (!assignedTabIds.has(tab.id)) {
          tabIdToSessionIndex[tab.id] = idx++;
        }
      }
      sessionPersistence.updateScrollbackPaths(pathMap, tabIdToSessionIndex);
    } catch (err) {
      console.warn("[TerminalPage] Failed to save scrollback on close:", err);
    }
  }, [sessionPersistence]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    getCurrentWindow()
      .onCloseRequested(async () => {
        await handleWindowClose();
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [handleWindowClose]);
}
