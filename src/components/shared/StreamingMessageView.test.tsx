/**
 * `StreamingMessageView` — what the operator actually sees while a response is
 * still arriving, and the invariant that EVERY consumer goes through it.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no
 * `@testing-library/react` — see `StewardControl.test.tsx`), so the render
 * assertions go through `react-dom/server`, which needs no DOM. That covers the
 * initial render, which is exactly what these cases are about.
 *
 * Two things are pinned here:
 *
 *  1. **The retention-cap notice reaches the surface.** When `useAiSession`'s
 *     2M-char cap fires mid-turn, the head of the response is gone; without the
 *     "(N trimmed)" notice the operator is reading a truncated answer with
 *     nothing saying so, and the completion marker only lands once the turn
 *     ends.
 *  2. **All five `useAiSession` consumers render `streamingContent` through
 *     this component.** `ProjectExplainerPage` was the one that shipped
 *     unconverted — a raw `{session.streamingContent}` in a
 *     `whitespace-pre-wrap` div, with no `droppedChars` anywhere near it. That
 *     is a source-level guard because those pages (mermaid, ReactMarkdown,
 *     Tauri `invoke`) cannot be rendered in this environment at all.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { StreamingMessageView } from "./StreamingMessageView";
import { STREAM_TAIL_CHARS } from "../../lib/ai-streaming/streamingBuffer";

describe("StreamingMessageView rendering", () => {
  it("renders a bounded tail, not the whole retained buffer", () => {
    const content = "x".repeat(STREAM_TAIL_CHARS * 3);
    const html = renderToStaticMarkup(<StreamingMessageView content={content} />);
    // The <pre> holds the tail window, not all 12k characters.
    const pre = html.match(/<pre[^>]*>([\s\S]*?)<\/pre>/)?.[1] ?? "";
    expect(pre).toHaveLength(STREAM_TAIL_CHARS);
    // ...and the rest is reachable rather than lost.
    expect(html).toContain("Show earlier");
    expect(html).toContain("Show all");
  });

  it("renders the whole buffer when it fits inside the window", () => {
    const html = renderToStaticMarkup(<StreamingMessageView content="short answer" />);
    expect(html).toContain("short answer");
    expect(html).not.toContain("Show earlier");
  });

  it("surfaces the retention-cap trim, in the operator's units", () => {
    const html = renderToStaticMarkup(
      <StreamingMessageView content="the surviving tail" droppedChars={500_001} />,
    );
    expect(html).toContain("trimmed");
    expect(html).toContain("488 KB"); // 500,001 chars, the cap's own trim slab
    expect(html).toContain("the surviving tail");
  });

  it("says nothing about trimming when nothing was trimmed", () => {
    const html = renderToStaticMarkup(
      <StreamingMessageView content="complete so far" droppedChars={0} />,
    );
    expect(html).not.toContain("trimmed");
  });
});

/**
 * Every file that renders `useAiSession().streamingContent`. Adding a consumer
 * without converting it is the defect this list exists to catch.
 */
const STREAMING_CONSUMERS = [
  "../process-manager/AiFixPanel.tsx",
  "../../pages/specs/SpecChatPanel.tsx",
  "../../pages/ui-bridge-integration/HookGenerationPanel.tsx",
  "../../pages/project-explainer/ProjectExplainerPage.tsx",
] as const;

describe("streaming consumers all render through StreamingMessageView", () => {
  for (const relative of STREAMING_CONSUMERS) {
    it(`${relative} uses StreamingMessageView`, () => {
      const source = readFileSync(fileURLToPath(new URL(relative, import.meta.url)), "utf8");
      expect(source).toContain("StreamingMessageView");
    });
  }

  it("ProjectExplainerPage renders the streaming buffer with its dropped count", () => {
    // The specific regression: `<div ...>{session.streamingContent}</div>` read
    // the raw retained buffer — up to 2MB into one DOM text node per batched
    // flush, and a silent head-truncation when the cap fired.
    const source = readFileSync(
      fileURLToPath(
        new URL("../../pages/project-explainer/ProjectExplainerPage.tsx", import.meta.url),
      ),
      "utf8",
    );
    expect(source).toContain("droppedChars={session.streamingDroppedChars}");
    expect(source).not.toMatch(/>\s*\{session\.streamingContent\}\s*</);
  });
});
