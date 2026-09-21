/**
 * Productivity Page
 *
 * Top-level entry for the "Productivity" tab. Interim shape after Phase 4 of
 * plan `2026-09-12-consolidate-local-orchestration-onto-conductor` deleted
 * the plan/task board and the coordinator dashboard: one heading and three
 * panels, stacked and scrollable.
 *
 *   - Knowledge — the browser rendered inline (`src/components/knowledge/`).
 *     The Ctrl+Shift+E modal is mounted once at the App level
 *     (`GlobalKnowledgeBrowser` in `App.tsx`), so this page mounts no second
 *     copy.
 *   - File activity — live dirty-worktree heatmap, held locks, hot files.
 *   - Overlapping intents — coord's L2 view of agent pairs whose declared
 *     paths intersect; UNKNOWN when the read fails.
 *
 * There is no sub-view router: every panel is on screen at once, so nothing
 * needs to be navigated to and no page-mounted handshake exists.
 */

import { KnowledgeBrowser } from "@/components/knowledge/KnowledgeBrowser";
import { FileActivityPanel } from "./FileActivityPanel";
import { OverlappingIntentsPanel } from "./OverlappingIntentsPanel";

export function ProductivityPage() {
  return (
    <div className="h-full flex flex-col bg-background" data-page-id="productivity">
      <div className="flex items-center px-3 py-2 border-b border-border bg-card/40">
        <h1 className="text-h2 text-foreground">Productivity</h1>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="flex flex-col gap-4 p-4">
          <section
            role="region"
            aria-labelledby="productivity-knowledge-heading"
            className="flex flex-col rounded-lg border border-border bg-card/30 overflow-hidden"
            data-ui-bridge-id="productivity.knowledge-section"
          >
            <h2 id="productivity-knowledge-heading" className="sr-only">
              Knowledge
            </h2>
            <div className="h-[28rem]">
              <KnowledgeBrowser mode="inline" />
            </div>
          </section>

          <FileActivityPanel />

          <OverlappingIntentsPanel />
        </div>
      </div>
    </div>
  );
}

export default ProductivityPage;
