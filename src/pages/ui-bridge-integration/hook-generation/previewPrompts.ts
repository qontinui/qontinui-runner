/**
 * window.__qontinuiPreviewPrompts — the registration prompts the pipeline
 * built, most-recent-first and capped at 20, read back by the integration
 * page's UI Bridge registrations
 * (`src/lib/ui-bridge/pages/uibridgeintegrationpage-registrations.tsx`).
 */

const MAX_PREVIEW_PROMPTS = 20;

/** Record one page's registration prompt. Callers own any `typeof window` guard. */
export function recordPreviewPrompt(pageRoute: string, pageName: string, prompt: string): void {
  const w = window as unknown as {
    __qontinuiPreviewPrompts?: Array<{
      pageRoute: string;
      pageName: string;
      prompt: string;
      timestamp: number;
    }>;
  };
  w.__qontinuiPreviewPrompts = w.__qontinuiPreviewPrompts ?? [];
  w.__qontinuiPreviewPrompts.unshift({
    pageRoute,
    pageName,
    prompt,
    timestamp: Date.now(),
  });
  if (w.__qontinuiPreviewPrompts.length > MAX_PREVIEW_PROMPTS) {
    w.__qontinuiPreviewPrompts.length = MAX_PREVIEW_PROMPTS;
  }
}
