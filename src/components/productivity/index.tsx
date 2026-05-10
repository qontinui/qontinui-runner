/**
 * Productivity Page
 *
 * Top-level entry for the "Productivity" tab. Hosts a tiny local sub-view
 * router (`view: plans | coordinator | knowledge`) — only the `plans` view
 * has real content in Phase 1. The other two are placeholders that a later
 * phase will replace with the real CoordinatorDashboard / KnowledgeBrowser.
 *
 * Sub-view requests come from two places:
 *   - the local top-bar tabs (clicking one of the three view buttons)
 *   - the `productivity-set-view` window event dispatched by useAppNavigation
 *     when an alias like `productivity-coordinator` is requested via
 *     UI Bridge / pageNavigate.
 */

import { useEffect, useState } from "react";
import { ClipboardList, Bot, BookOpen } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { PlanTaskBoard } from "./PlanTaskBoard";
import { KnowledgeBrowser } from "./KnowledgeBrowser";
import { CoordinatorDashboard } from "./CoordinatorDashboard";
import type { ProductivityView } from "./types";

interface ViewTab {
  id: ProductivityView;
  label: string;
  icon: LucideIcon;
  uibridgeId: string;
}

const VIEW_TABS: ViewTab[] = [
  {
    id: "plans",
    label: "Plans & Tasks",
    icon: ClipboardList,
    uibridgeId: "productivity.view-tab-plans",
  },
  {
    id: "coordinator",
    label: "Coordinator",
    icon: Bot,
    uibridgeId: "productivity.view-tab-coordinator",
  },
  {
    id: "knowledge",
    label: "Knowledge",
    icon: BookOpen,
    uibridgeId: "productivity.view-tab-knowledge",
  },
];

export function ProductivityPage() {
  const [view, setView] = useState<ProductivityView>("plans");
  const [knowledgeModalOpen, setKnowledgeModalOpen] = useState(false);

  // Listen for the "productivity-set-view" event dispatched by
  // useAppNavigation when an alias like `productivity-coordinator` is
  // requested via UI Bridge / pageNavigate.
  //
  // Also publish a mount-promise (window flag + one-shot event) so
  // `dispatchProductivitySubView` in useAppNavigation can await mount
  // before firing the event. Without this, dispatching synchronously
  // after a tab switch races the just-mounted listener and the event
  // is dropped, leaving the page on its default "plans" view.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<{ view?: ProductivityView }>).detail;
      if (detail?.view) {
        setView(detail.view);
      }
    };
    window.addEventListener("productivity-set-view", handler);
    (window as unknown as Record<string, unknown>).__qontinuiProductivityReady = true;
    window.dispatchEvent(new Event("productivity-page-mounted"));
    return () => {
      window.removeEventListener("productivity-set-view", handler);
      delete (window as unknown as Record<string, unknown>).__qontinuiProductivityReady;
    };
  }, []);

  return (
    <div className="h-full flex flex-col bg-background" data-page-id="productivity">
      {/* Top bar with sub-view tabs */}
      <div className="flex items-center gap-1 px-3 py-2 border-b border-border bg-card/40">
        <h1 className="text-h2 text-foreground mr-3">Productivity</h1>
        <div className="flex items-center gap-1">
          {VIEW_TABS.map((tab) => {
            const Icon = tab.icon;
            const active = view === tab.id;
            return (
              <button
                key={tab.id}
                onClick={() => setView(tab.id)}
                data-ui-bridge-id={tab.uibridgeId}
                className={`flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium transition-colors ${
                  active
                    ? "bg-primary/15 text-primary"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted/30"
                }`}
                aria-pressed={active}
              >
                <Icon className="w-3.5 h-3.5" />
                {tab.label}
              </button>
            );
          })}
        </div>
        <button
          onClick={() => setKnowledgeModalOpen(true)}
          data-ui-bridge-id="productivity.open-knowledge-modal"
          className="ml-auto flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium border border-border text-foreground hover:bg-muted/30"
          title="Open knowledge browser (Ctrl+Shift+K)"
        >
          <BookOpen className="w-3.5 h-3.5" />
          Knowledge
        </button>
      </div>

      {/* Sub-view content */}
      <div className="flex-1 min-h-0 overflow-hidden">
        {view === "plans" && <PlanTaskBoard />}
        {view === "coordinator" && <CoordinatorDashboard />}
        {view === "knowledge" && <KnowledgeBrowser mode="inline" />}
      </div>

      {/* Top-bar-button-triggered modal. Ctrl+Shift+K is mounted globally
          at the App level so it works from any page; here the button
          provides an in-tab trigger for users who prefer mouse. */}
      <KnowledgeBrowser
        mode="modal"
        open={knowledgeModalOpen}
        onClose={() => setKnowledgeModalOpen(false)}
      />
    </div>
  );
}

export default ProductivityPage;
