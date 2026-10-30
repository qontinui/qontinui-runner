import { useCallback, useEffect } from "react";

import { usePageSessionGetter, useTerminalSession, useTransitionEffects } from "./contexts";
import { TERMINAL_OPEN_PAGE_EVENT } from "./commands/orchestrateCommand";
import { conversationLabel, id8 } from "./paneLabel";
import { describeHolder, goToHolderPane } from "./resumeGate";
import { ResumeLiveDialog } from "./ResumeLiveDialog";
import type { LiveClaudeSession } from "./liveClaudeSessions";

/**
 * Mounts the resume-live dialog for the page the operator is viewing, and
 * carries out "Go to the existing pane" — the one choice that acts on the page
 * rather than on the resume.
 *
 * The prompt lives in the page's `useShellIntegration` (the click gate awaits
 * its answer there). Navigation is the existing doors: the
 * `terminal-open-page` event activates the holder's page (`useTerminalPages`),
 * the page's own zone layout reveals and focuses the pane
 * (`revealTabInLayout`), and `flashTab` flashes it so the move is visible. A
 * pane that is not in this window is said, never silently not shown.
 *
 * A prompt this host stops showing is answered "cancel": the host shows only
 * the ACTIVE page's prompt, so a page switch hides the old page's dialog, and
 * its click would otherwise wait — holding the id's in-flight claim — until
 * the operator happened back to that page.
 */
export function ResumeGateHost() {
  const { shellIntegration, pageId, workflowGen } = useTerminalSession();
  const getPageSession = usePageSessionGetter();
  const { flashTab } = useTransitionEffects();
  const { resumeGatePrompt: prompt, answerResumeGate } = shellIntegration;
  const { setNotification } = workflowGen;

  // `answerResumeGate` is one stable callback per page, so this cleanup runs
  // exactly when the host stops showing that page: on a page switch, and when
  // the host unmounts. With no dialog open it does nothing.
  useEffect(() => () => answerResumeGate("cancel"), [answerResumeGate]);

  const onGoToPane = useCallback(
    (holder: LiveClaudeSession) => {
      // Answer first, so the page switch below cannot land the cleanup's
      // "cancel" on this dialog before its own answer does.
      answerResumeGate("go-to-pane");
      const result = goToHolderPane(
        {
          currentPageId: pageId,
          getPage: (id) => getPageSession(id),
          openPage: (id) =>
            window.dispatchEvent(
              new CustomEvent(TERMINAL_OPEN_PAGE_EVENT, {
                detail: { pageId: id, name: `Page ${id8(id)}` },
              }),
            ),
          flash: flashTab,
        },
        holder,
      );
      if (result === "not-found" && prompt) {
        setNotification({
          message:
            `"${conversationLabel(prompt.conversationName, prompt.claudeSessionId)}" is live in a ` +
            `pane this window does not show — ${describeHolder(holder)}`,
          type: "error",
        });
      }
    },
    [pageId, getPageSession, flashTab, prompt, setNotification, answerResumeGate],
  );

  if (!prompt) return null;
  return (
    <ResumeLiveDialog
      prompt={prompt}
      onGoToPane={onGoToPane}
      onFork={() => answerResumeGate("fork")}
      onResumeAnyway={() => answerResumeGate("resume-anyway")}
      onCancel={() => answerResumeGate("cancel")}
    />
  );
}
