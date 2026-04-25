/**
 * Giant-SCC dev fixture.
 *
 * Mounts a synthetic state machine with ONE giant SCC (>150 states) into
 * `ChunkedGraphView`, specifically to exercise the nested-drill render
 * path that is otherwise unreachable through real-world configs — the
 * primary chunker always breaks production state machines below the
 * 150-state giant-SCC ceiling.
 *
 * Available in every build (including the production temp runners the
 * supervisor spawns for /manual-test). The `import.meta.env.DEV` gate
 * was removed because temp runners build with `npm run build` and
 * would otherwise have no way to exercise the fixture. The component
 * is gated by an obscure keystroke (Ctrl+Shift+G) and an explicit
 * UI Bridge action — both unreachable to end users without intent.
 * It renders nothing until `isOpen === true`, so production bundle
 * impact is the component's static cost only.
 *
 * It is NOT wired into the user-visible tab list; instead it
 * registers with the UI Bridge as a first-class component so a dev can
 * reach it via:
 *
 *   GET  /control/component/dev-giant-scc-fixture
 *   POST /control/component/dev-giant-scc-fixture/action/open
 *   POST /control/component/dev-giant-scc-fixture/action/close
 *   POST /control/component/dev-giant-scc-fixture/action/drill-to-giant
 *
 * Keyboard: Ctrl+Shift+G toggles visibility in development mode.
 *
 * Why this exists: `decomposeGiantSCC` has 8/8 unit tests green but the
 * UI plumbing around it — `ChunkedGraphView`'s nested-overview branch,
 * the breadcrumb trail, the auto-drill with a multi-segment path —
 * has no automated coverage. `qontinui-workflow-ui` does not host a
 * test runner (tsup-only library package), and adding jsdom +
 * @testing-library/react purely for one integration test would be
 * heavier than this fixture. A dev can verify the path end-to-end in
 * about 10 seconds by pressing Ctrl+Shift+G and clicking the giant
 * chunk card.
 *
 * See `session-prompts/giant-scc-decomposition.md` for the original
 * feature plan.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as dagre from "@dagrejs/dagre";
import { useUIComponent } from "@qontinui/ui-bridge";
import { ChunkedGraphView, generateGiantSCC } from "@qontinui/workflow-ui/state-machine";
import "@xyflow/react/dist/style.css";

/**
 * Fixture toggle. Keyboard shortcut: Ctrl+Shift+G.
 */
export function GiantSCCFixture() {
  const [isOpen, setIsOpen] = useState(false);
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);

  // Generate once; regeneration on every render would blow the layout
  // cache and show a noticeable hiccup every time the panel opens.
  const { states, transitions, hubStateId } = useMemo(() => {
    return generateGiantSCC({
      branches: 10,
      branchLength: 20,
      idPrefix: "dev-giant",
    });
  }, []);

  const toggleOpen = useCallback(() => setIsOpen((v) => !v), []);
  const close = useCallback(() => setIsOpen(false), []);

  // Keyboard shortcut: Ctrl+Shift+G.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key === "G") {
        e.preventDefault();
        toggleOpen();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [toggleOpen]);

  // UI Bridge registration — lets automated/manual agents drive the
  // fixture without a keystroke. Registered only in DEV, but the hook
  // must be called unconditionally; handlers no-op in production.
  useUIComponent({
    id: "dev-giant-scc-fixture",
    name: "Giant-SCC Dev Fixture",
    description:
      "Synthetic 201-state giant-SCC state machine rendered through ChunkedGraphView. Exists purely to exercise the nested-overview drill-in path.",
    actions: [
      {
        id: "open",
        label: "Open fixture",
        handler: async () => {
          setIsOpen(true);
        },
      },
      {
        id: "close",
        label: "Close fixture",
        handler: async () => {
          setIsOpen(false);
        },
      },
      {
        id: "drill-to-giant",
        label: "Auto-drill into the giant SCC",
        description:
          "Selects a state inside the giant SCC; ChunkedGraphView's auto-drill effect should walk the nested path and render the secondary overview.",
        handler: async () => {
          setIsOpen(true);
          // Pick a state that lives inside the giant SCC — any branch
          // leaf will force auto-drill through the primary chunk and
          // (if SUB_CHUNK_MAX triggers decomposition) into a sub-chunk.
          setSelectedStateId("dev-giant-b1-s10");
        },
      },
      {
        id: "select-hub",
        label: "Select hub state",
        handler: async () => {
          setIsOpen(true);
          setSelectedStateId(hubStateId);
        },
      },
    ],
  });

  if (!isOpen) return null;

  return (
    <div
      className="fixed inset-4 z-[9998] flex flex-col bg-bg-primary border border-border-primary rounded-lg shadow-2xl"
      role="dialog"
      aria-label="Giant-SCC Dev Fixture"
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-border-secondary bg-bg-secondary/60 shrink-0">
        <div className="flex flex-col">
          <span className="text-sm font-semibold text-text-primary">Giant-SCC Dev Fixture</span>
          <span className="text-[11px] text-text-muted">
            {states.length} states · {transitions.length} transitions · synthetic hub + 10 branches
            · DEV ONLY
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => setSelectedStateId("dev-giant-b1-s10")}
            className="text-xs px-2 py-1 bg-bg-tertiary hover:bg-bg-hover border border-border-secondary rounded transition-colors"
            title="Select a branch state to trigger auto-drill"
          >
            Auto-drill test
          </button>
          <button
            type="button"
            onClick={close}
            className="text-xs px-2 py-1 text-text-muted hover:text-text-primary hover:bg-bg-tertiary rounded transition-colors"
            title="Close (Ctrl+Shift+G)"
          >
            Close
          </button>
        </div>
      </div>

      {/* ChunkedGraphView canvas */}
      <div className="flex-1 min-h-0">
        <ChunkedGraphView
          dagre={dagre}
          states={states}
          transitions={transitions}
          selectedStateId={selectedStateId}
          selectedTransitionId={null}
          onSelectState={setSelectedStateId}
          onSelectTransition={() => {}}
          initialStateId={hubStateId}
          emptyMessage="No states in this fixture (should never happen)."
        />
      </div>
    </div>
  );
}

export default GiantSCCFixture;
