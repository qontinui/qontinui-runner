/**
 * Per-tab commit-readiness traffic light + commit-progress button.
 *
 * Pure presentational component — no fetching, no polling. The dot color
 * and button enabled state are derived entirely from `props.state`. The
 * containing `useCommitState` hook is responsible for keeping that state
 * fresh.
 *
 * Click action injects `COMMIT_PROMPT_TEMPLATE` into the session so the
 * AI runs a verified, file-set-scoped commit:
 *   - PTY tabs (every tab routed through `TerminalTabBar`) → typed into
 *     the PTY via `onWriteToTerminal(prompt + "\r")`. We accept the
 *     loss of tool-call-boundary semantics here; per the plan §0.5
 *     resolution, Claude CLI buffers typed input the same way whether
 *     the human or the runner typed it.
 *   - SDK chat tabs (a future surface; not currently routed through
 *     `TerminalTabBar`) → `invoke("send_user_message", { taskRunId,
 *     message })`, which respects the queue-at-tool-call-boundary
 *     semantics built into `ClaudeSession::send_user_message`.
 *
 * The `isPtyTab` prop is the toggle. The caller decides which path
 * applies — see `TerminalTabBar.tsx` for the live wiring.
 */

import { GitCommit } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import type { CommitState } from "./useCommitState";

export type { CommitState } from "./useCommitState";

const logger = createLogger("CommitTrafficLight");

/**
 * Prompt the AI runs when the user clicks the commit button. The
 * runtime-injected text is identical whether the click lands in a PTY
 * tab or an SDK chat — see plan §4 for the full procedure.
 *
 * IMPORTANT: do NOT include "Co-Authored-By: Claude" trailers in any
 * runtime artifact (commit messages, file headers, etc.) — the
 * qontinui-runner pre-commit hook rejects them. The template tells the
 * AI not to add them.
 */
export const COMMIT_PROMPT_TEMPLATE = `[QONTINUI: COMMIT-PROGRESS REQUEST — automated]

Please commit the changes you have made in this session. Do NOT commit edits made by other tools or by the user — only your own writes.

Procedure:
1. List your touched files. They are recorded in the per-session tracker; ask via the Read tool / Bash if you don't already know them. The runner has them in PG \`session_touched_files\` keyed on this session's task_run_id.
2. For each repo containing those files, run \`git status --porcelain -- <paths>\` and \`git diff --stat -- <paths>\` against the touched paths only.
3. Cross-reference the diff against your own prior tool calls in THIS conversation. Any file in the diff that you did not Edit or Write yourself is foreign — do NOT include it in the commit. (External tools like VS Code may have written while you were idle.)
4. If a file you did write is no longer dirty (someone reverted it), skip it.
5. If \`git status\` reports an in-progress merge, rebase, cherry-pick, or revert (MERGE_HEAD / rebase-merge / etc.), STOP and report the situation. Do not commit.
6. Stage only the verified-yours subset: \`git add -- <verified files>\`.
7. Compose a commit message that describes what YOU changed, in your own words, based on your transcript. One short subject line, then a body if more than ~3 files. Do NOT include a "Co-Authored-By: Claude" trailer (the repo pre-commit hook rejects it).
8. Commit, one commit per repo if multiple repos are involved.
9. Print the resulting commit SHA(s) so the user sees them.

If something looks wrong (mismatched diff, unexpected files, merge in progress) — stop, do not commit, and explain what you observed.`;

interface CommitTrafficLightProps {
  /** Latest known commit state for this tab; `undefined` while
   *  the hook is still gathering its first probe. */
  state?: CommitState;
  /** The session id (= task_run_id) passed to `send_user_message`.
   *  Required for SDK chat injection; unused for PTY injection. */
  sessionId?: string;
  /** True for PTY-launched tabs (Claude CLI inside a shell). All tabs
   *  routed through `TerminalTabBar` are PTY tabs today. False would
   *  indicate an SDK chat session — currently no such tab type passes
   *  through this component. */
  isPtyTab: boolean;
  /** Inject text into the PTY for `isPtyTab=true`. Sourced by the
   *  caller from `terminalRefs.current.get(tab.id)?.current?.writeToTerminal`. */
  onWriteToTerminal?: (text: string) => void;
}

// Color palette mirrors `STATE_DOT_COLORS` in `TerminalTabBar.tsx`
// (Tokyo Night) so the dot blends with the existing tab visual language.
const DOT_BG: Record<CommitState["status"], string | null> = {
  clean: "bg-[#9ece6a]", // green
  dirty: "bg-[#e0af68]", // yellow
  empty: "bg-[#565f89]", // gray
  merging: "bg-[#f7768e]", // red
  unknown: null, // hide entirely
};

/** Build the hover-tooltip string per plan §3:
 *  "{dirty}/{touched} dirty in {N} repo(s)[; merging: a, b]". */
function buildTooltip(state: CommitState): string {
  const repos = state.repo_roots.length;
  const repoWord = `${repos} repo${repos === 1 ? "" : "s"}`;
  const base = `${state.dirty_count}/${state.touched_count} dirty in ${repoWord}`;
  if (state.merging_repos.length > 0) {
    return `${base}; merging: ${state.merging_repos.join(", ")}`;
  }
  return base;
}

/** Returns true iff the commit button should be clickable. */
export function isCommitButtonEnabled(state?: CommitState): boolean {
  return state?.status === "dirty";
}

export function CommitTrafficLight({
  state,
  sessionId,
  isPtyTab,
  onWriteToTerminal,
}: CommitTrafficLightProps) {
  // Don't render the indicator when state is undefined (probe never fired
  // / no claudeSessionId yet). The button still slots in disabled so the
  // tab layout doesn't shift.
  const status = state?.status ?? "unknown";
  const dotClass = DOT_BG[status];
  const enabled = isCommitButtonEnabled(state);
  const tooltip = state ? buildTooltip(state) : "Commit state unknown";
  const buttonTitle = (() => {
    if (status === "merging") {
      const repos = state?.merging_repos.join(", ") || "this repo";
      return `In-progress merge/rebase in ${repos} — resolve manually before committing`;
    }
    if (status === "clean") return "All your touched files are clean — nothing to commit";
    if (status === "empty") return "No tracked files yet";
    if (status === "unknown") return "Commit state unknown";
    return `Ask the AI to commit (${tooltip})`;
  })();

  const handleClick = async () => {
    if (!enabled) return;
    try {
      if (isPtyTab) {
        if (!onWriteToTerminal) {
          logger.warn("PTY commit click without onWriteToTerminal callback");
          return;
        }
        onWriteToTerminal(COMMIT_PROMPT_TEMPLATE + "\r");
      } else {
        if (!sessionId) {
          logger.warn("SDK commit click without sessionId");
          return;
        }
        await invoke("send_user_message", {
          taskRunId: sessionId,
          message: COMMIT_PROMPT_TEMPLATE,
        });
      }
    } catch (err) {
      logger.error("Commit prompt injection failed:", err);
    }
  };

  return (
    <span
      className="flex items-center gap-1 shrink-0"
      data-testid="commit-traffic-light"
      data-commit-status={status}
    >
      {dotClass && (
        <span
          aria-label={`Commit state: ${status}`}
          className={`w-2 h-2 rounded-full shrink-0 ${dotClass}`}
          title={tooltip}
        />
      )}
      <button
        type="button"
        disabled={!enabled}
        onClick={(e) => {
          e.stopPropagation();
          void handleClick();
        }}
        title={buttonTitle}
        aria-label="Commit progress"
        className={`flex items-center justify-center w-4 h-4 rounded transition-colors ${
          enabled
            ? "text-[#7aa2f7] hover:text-[#bb9af7] hover:bg-[#1a1b26]/50"
            : "text-[#565f89]/40 cursor-not-allowed"
        }`}
      >
        <GitCommit className="w-3 h-3" />
      </button>
    </span>
  );
}
