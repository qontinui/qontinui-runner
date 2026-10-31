/**
 * Rendered with `renderToStaticMarkup` (the runner's vitest config is
 * `environment: "node"`, no jsdom), so the section's click paths are not driven
 * here; `resumableIds` and `resumeUnfinished` are covered in
 * `unfinishedSessions.test.ts`.
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import {
  UnfinishedSessionRow,
  UnfinishedSessionsSection,
  unfinishedResumeId,
  unfinishedRowId,
} from "./UnfinishedSessionsSection";
import type { PastSession } from "./usePastSessions";

const session = {
  claudeSessionId: "abc",
  resumeName: "plan-a",
  state: "closed",
  provider: "claude",
  finished: false,
} as PastSession;

const row = (blocker: string | null, busy = false) =>
  renderToStaticMarkup(
    <UnfinishedSessionRow
      session={session}
      blocker={blocker}
      verdict={undefined}
      busy={busy}
      onResume={() => {}}
    />,
  );

/** The opening tag of the row's Resume button. */
const resumeButton = (html: string) => {
  const at = html.indexOf(`data-ui-bridge-id="${unfinishedResumeId("abc")}"`);
  return html.slice(html.lastIndexOf("<button", at), html.indexOf(">", at));
};

describe("UnfinishedSessionRow", () => {
  it("stamps the row and leaves Resume available when nothing blocks it", () => {
    const html = row(null);
    expect(html).toContain(`data-ui-bridge-id="${unfinishedRowId("abc")}"`);
    expect(resumeButton(html)).not.toContain('disabled=""');
    expect(resumeButton(html)).toContain('title="Resume this session"');
  });

  it("disables Resume and names why when the row is blocked", () => {
    const button = resumeButton(row("Account unknown"));
    expect(button).toContain('disabled=""');
    expect(button).toContain('title="Account unknown"');
  });

  it("disables Resume while its own call is in flight", () => {
    expect(resumeButton(row(null, true))).toContain('disabled=""');
  });
});

describe("UnfinishedSessionsSection", () => {
  const label = (loaded: boolean, error: string | null, sessions: PastSession[]) =>
    renderToStaticMarkup(
      <UnfinishedSessionsSection
        sessions={sessions}
        loaded={loaded}
        error={error}
        resumeBlockerOf={() => null}
        onSettled={() => {}}
      />,
    );

  it("says unknown, not zero, until history has been read", () => {
    expect(label(false, null, [])).toContain("Unfinished (unknown)");
    expect(label(true, "boom", [])).toContain("Unfinished (unknown)");
  });

  it("counts once history has been read", () => {
    expect(label(true, null, [])).toContain("Unfinished (0)");
    expect(label(true, null, [session])).toContain("Unfinished (1)");
  });
});
