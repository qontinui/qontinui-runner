/**
 * Makes `GLOBAL_CHORDS` ENFORCEABLE rather than documentary.
 *
 * The same defect has now landed six times: a surface claims a chord with
 * its own hand-rolled key test, the chord table never hears about it, and
 * two handlers fire on one press. Six occurrences is a missing mechanism,
 * not six mistakes.
 *
 * ## Why there are now TWO mechanisms
 *
 * Every previous fix — five regexes and then one AST scanner — was the same
 * bet: that a claim can be recognised by the SHAPE of its comparison. Each
 * widening produced a fresh escape set, because the space of shapes is
 * open-ended. Iteration 9 of the manual-test loop measured the state after
 * the sixth and found at least six escaping classes beyond the four this
 * file declared — while the file asserted `ESCAPING_CLASS_COUNT = 4` and
 * said the number existed "so neither direction can pass silently".
 *
 * **It passed silently.** A floor that is wrong is worse than no floor: it
 * converts an unknown into a false assurance. So the number is gone, and
 * the problem is split into two mechanisms with different jobs.
 *
 *   **A — COVERAGE, by ban** (`./keyFieldReads.ts`). No file outside an
 *   explicit roster may READ a keyboard-event field at all: `.key`,
 *   `.code`, `.keyCode`, `.which`, `.ctrlKey`, `.metaKey`, `.altKey`,
 *   `.shiftKey`, `getModifierState`. A field read is a fact about a NAME,
 *   not about a comparison, so parenthesising, destructuring, aliasing,
 *   Yoda order, `switch`, a regex, `Reflect.get` and polarity tricks are
 *   all irrelevant to it BY CONSTRUCTION. It answers "has a new file
 *   started claiming chords?" with no false negatives short of three
 *   declared escapes, each of which is probed below.
 *
 *   **B — INVENTORY, by AST** (`./keyClaimScan.ts`). For rostered files,
 *   WHICH chords each claims, so collisions can be counted and
 *   {@link KNOWN_SHARED_CHORDS} / {@link KNOWN_KEY_CLAIMS} stay meaningful.
 *   This is the existing scanner, kept — and with six defects iteration 9
 *   found now fixed.
 *
 * The win is in the blast radius. Every escape in B degrades from *"a claim
 * is invisible"* to *"the inventory may be imprecise for a file we already
 * know about"* — and an escape can no longer hide a collision, because the
 * file holding it cannot be off the roster.
 *
 * ## The roster is the point
 *
 * A file that legitimately handles a bare `Escape` or an arrow key is ON
 * the roster, with that noted, rather than invisible. The roster is
 * therefore large — measured, not guessed: 20 files read a MODIFIER or an
 * unambiguous key field, and 184 more read only `key`/`code`. Adding a file
 * to either is a one-line, reviewable diff; the failure message prints the
 * exact lines to add or remove.
 *
 * ## The properties
 *
 *   A1. Every file reading a modifier field is on {@link MODIFIER_FIELD_ROSTER}.
 *   A2. Every file reading `key`/`code` is on {@link KEY_FIELD_ROSTER}.
 *   A3. Every spelling mechanism B admits missing is nonetheless a field
 *       read mechanism A sees — the bound on B's blast radius, asserted per
 *       spelling rather than remembered as a number.
 *   B.  Every modifier-qualified key claim in the rostered files is exactly
 *       the allowlisted set, per file, spelled as a full chord. A claim
 *       inside a GLOBAL key listener is additionally forbidden outright.
 *   C.  There are exactly TWO global chord registries — this table, and the
 *       inline `isCtrlShiftChord(e, "<letter>")` calls in
 *       `terminal/useKeyboardShortcuts.ts`.
 *   D.  The set of chords claimed from more than one file is exactly the
 *       documented one, with digit RANGES expanded first.
 *   E.  `switch` registries on a key value are inventoried by name.
 *   F.  Both mechanisms can actually fail — the mutation matrix, run as
 *       snippets AND injected into a real file, plus a per-class probe of
 *       each mechanism's own limits.
 *
 * `environment: "node"` vitest, so `fs` is available; same precedent as
 * `terminal/useKeyboardShortcuts.chords.test.ts`.
 */

import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";

import { describe, expect, it } from "vitest";

import {
  GLOBAL_CHORDS,
  GLOBAL_DIGIT_CHORDS,
  type GlobalChord,
  type GlobalDigitChord,
} from "./globalChords";
import { CONTROL_TAGS, scanKeyClaims, scanKeyClaimsIn, type FileScan } from "./keyClaimScan";
import { findKeyFieldReads, hasGlobalKeyListener, parseSource } from "./keyFieldReads";

const SRC = resolve(__dirname, "..");

/** The terminal's own inline registry — the one sanctioned second home. */
const TERMINAL_REGISTRY = "components/terminal/useKeyboardShortcuts.ts";

/**
 * Files that are the MECHANISM rather than a claimant: the chord table
 * whose predicates do the reading, and the two scanners that read them.
 */
const MECHANISM_FILES = new Set([
  "lib/globalChords.ts",
  "lib/keyClaimScan.ts",
  "lib/keyFieldReads.ts",
]);

/* ── A. the roster ───────────────────────────────────────────────────── */

/**
 * TIER 1 — every file that reads a field only a keyboard (or pointer)
 * event has: `ctrlKey`, `metaKey`, `altKey`, `shiftKey`, `getModifierState`,
 * `keyCode`, `which`.
 *
 * This is the CHORD-RELEVANT tier. A chord is a modifier plus a key, so a
 * file absent from here cannot hand-roll one — that is the whole coverage
 * argument, and it holds regardless of how the comparison is spelled.
 *
 * Every entry carries WHY it reads a modifier, because that note is what a
 * reviewer grades a new entry against. Most are the same benign idiom:
 * `Enter` submits, `Shift+Enter` inserts a newline.
 */
const MODIFIER_FIELD_ROSTER: Record<string, string> = {
  "components/AiOutputTab.tsx": "textarea: Enter submits, Shift+Enter newlines",
  "components/active-dashboard/ApprovalDialog.tsx":
    "approval textarea: Enter responds, Shift+Enter newlines",
  "components/active-dashboard/DashboardPage.tsx":
    "`?` opens the shortcut overlay only when NO modifier is held — the negative " +
    "test that keeps it off Ctrl+? and friends. Also holds the widget-by-position " +
    "digit claims, routed through `matchesDigitChord`.",
  "components/knowledge-acquisition/KnowledgeExplorerPage.tsx":
    "textarea: Enter submits, Shift+Enter newlines",
  "components/process-manager/AiFixPanel.tsx": "textarea: Enter submits, Shift+Enter newlines",
  "components/scheduler/AiScheduleBuilder.tsx":
    "Ctrl/Cmd+Enter submits the prompt from the focused textarea — an ELEMENT-scoped " +
    "chord claim, inventoried in KNOWN_KEY_CLAIMS",
  "components/terminal/CommandBar.tsx":
    "`!e.altKey` refines the table-routed commandBar chord so Alt+<key> falls through",
  "components/terminal/DocFinderModal.tsx": "modal input: Enter confirms, Shift+Enter newlines",
  "components/terminal/FilePathLinkProvider.ts":
    "xterm link provider: Ctrl/Cmd must be held for a path to be clickable. Not a " +
    "chord — a hover/click modifier, with no key field read at all.",
  "components/terminal/TerminalFindBar.tsx":
    "F3 / Shift+F3 pick the find direction. Shift-only, and `matchesChord` cannot " +
    "express a shift-only chord — see keyClaimScan.ts::CONTROL_TAGS.",
  "components/terminal/TerminalInstance.tsx":
    "xterm `attachCustomKeyEventHandler` — clipboard + find, PTY-scoped. Inventoried " +
    "in KNOWN_KEY_CLAIMS.",
  "components/terminal/ZoneControlPanel.tsx": "textarea: Enter submits, Shift+Enter newlines",
  "components/terminal/ZoneGrid.tsx":
    "Ctrl/Cmd+CLICK multi-selects a zone. A pointer modifier, not a key chord.",
  "components/terminal/scrollKeys.ts":
    "VS Code-parity scrollback navigation, consumed by TerminalInstance's xterm " +
    "handler. Eight ELEMENT-scoped claims, inventoried in KNOWN_KEY_CLAIMS.",
  "components/ui-bridge/NaturalLanguagePanel.tsx": "textarea: Enter submits, Shift+Enter newlines",
  "components/widgets/ai-conversation/MessageInput.tsx":
    "textarea: Enter sends, Shift+Enter newlines",
  "hooks/useElementDrag.ts":
    "Alt held during a DRAG selects move-vs-link. A pointer modifier, not a key chord.",
  "pages/project-explainer/ProjectExplainerPage.tsx":
    "textarea: Enter submits, Shift+Enter newlines",
  "pages/specs/PageSpecComponents.tsx": "textarea: Enter submits, Shift+Enter newlines",
  "pages/specs/SpecChatPanel.tsx": "textarea: Enter submits, Shift+Enter newlines",
};

/**
 * TIER 2 — every file that reads `key` or `code` and NO modifier field.
 *
 * `key` and `code` are not keyboard-exclusive names (`item.key`,
 * `response.code`, `localStorage.key(i)`), so this tier is large and mixed:
 * bare-key handlers (Escape, Enter, arrows) sit beside list rendering and
 * API payloads. That is the honest shape of the question "who reads a field
 * called `key`", and the alternative — guessing which receiver is an event
 * — is exactly the shape-recognition problem mechanism A exists to avoid.
 *
 * What this tier buys: a file holding the key half of an interprocedural
 * chord test, or a bare-key handler that might grow a modifier, is NAMED
 * rather than invisible. A new entry is a one-line diff a reviewer reads
 * against "is this a keyboard surface, and if so is it claiming a chord?".
 *
 * No per-file note is required here — a note on `item.key` in a list would
 * be noise, and the tier that demands justification is tier 1.
 */
const KEY_FIELD_ROSTER: readonly string[] = [
  "components/ActionDetailModal.tsx",
  "components/AiTab.tsx",
  "components/HistoryTab.tsx",
  "components/ImageDetailModal.tsx",
  "components/PromptSnippetSelector.tsx",
  "components/TreeNode.tsx",
  "components/accessibility-explorer/AccessibilityExplorer.tsx",
  "components/accessibility-explorer/DetailsPanel.tsx",
  "components/accessibility-explorer/TreeNodeView.tsx",
  "components/active-dashboard/ActiveRunsBar.tsx",
  "components/active-dashboard/BreakpointInspector.tsx",
  "components/active-dashboard/CompletionSummary.tsx",
  "components/active-dashboard/DashboardLayout.tsx",
  "components/active-dashboard/NewRunDialog.tsx",
  "components/active-dashboard/ShortcutsModal.tsx",
  "components/activity-timeline/ActivityTimelinePanel.tsx",
  "components/api-request-builder/AiApiRequestGenerator.tsx",
  "components/architecture-view/ArchitectureView.tsx",
  "components/contexts/ContextCard.tsx",
  "components/contexts/ContextEditor.tsx",
  "components/contexts/ContextList.tsx",
  "components/contexts/ContextSelector.tsx",
  "components/doctor/DoctorHealthBadge.tsx",
  "components/dom-captures/DomSnapshotsPanel.tsx",
  "components/dom-captures/HtmlViewerModal.tsx",
  "components/error-monitor/BrowserErrorsPanel.tsx",
  "components/error-monitor/ErrorMonitorTab.tsx",
  "components/error-monitor/FixErrorsButton.tsx",
  "components/evaluation/DatasetList.tsx",
  "components/evaluation/EvaluationDashboard.tsx",
  "components/evaluation/ExperimentList.tsx",
  "components/findings/UserInputPanel.tsx",
  "components/gui-automation/AutomationToolkitSidebar.tsx",
  "components/hooks/HookActionConfig.tsx",
  "components/library/CheckGroupsPage.tsx",
  "components/library/ChecksPage.tsx",
  "components/library/ContextsPage.tsx",
  "components/library/PlaywrightTestsPage.tsx",
  "components/library/ShellCommandsPage.tsx",
  "components/library/TasksPage.tsx",
  "components/markdown/shared-components.tsx",
  "components/meta-optimizer/BeamRunsTab.tsx",
  "components/meta-optimizer/DuelPoolsTab.tsx",
  "components/meta-optimizer/EvalSpecsTab.tsx",
  "components/meta-optimizer/PromptRegistryTab.tsx",
  "components/meta-optimizer/RecommendationsTab.tsx",
  "components/meta-optimizer/RegressionAlertBanner.tsx",
  "components/meta-optimizer/RobustnessTab.tsx",
  "components/meta-optimizer/SpanEventsTab.tsx",
  "components/navigation/Sidebar.tsx",
  "components/observations/ObservationBrowser.tsx",
  "components/orchestration-loop/OrchestrationLoopPanel.tsx",
  "components/pipeline-events/PipelineEventsTimeline.tsx",
  "components/process-manager/ProcessManagerTab.tsx",
  "components/productivity/CoordinatorDashboard.tsx",
  "components/productivity/KnowledgeBrowser.tsx",
  "components/productivity/SpawnFromPlanModal.tsx",
  "components/productivity/coordinatorApi.ts",
  "components/projects/FrontPageAddress.tsx",
  "components/projects/FrontPageSetup.tsx",
  "components/prompt-versions/PromptVersionHistory.tsx",
  "components/reflection-dashboard/StepProvenanceTimeline.tsx",
  "components/run-recap/AutomationTab.tsx",
  "components/run-recap/StagedTimeline.tsx",
  "components/run-recap/TestsTab.tsx",
  "components/scheduler/SchedulerTaskList.tsx",
  "components/settings/AdvancedSettings.tsx",
  "components/settings/BackupSettings.tsx",
  "components/settings/DevenvEnrollSettings.tsx",
  "components/settings/DiscoverySettings.tsx",
  "components/settings/McpSettings.tsx",
  "components/settings/NotificationSettings.tsx",
  "components/settings/RunnerInstancesSettings.tsx",
  "components/settings/WsvCalibrationSection.tsx",
  "components/settings/performanceCapsConfig.ts",
  "components/spec-workflow-builder/SpecFileLoader.tsx",
  "components/spec-workflow-builder/WorkflowPreview.tsx",
  "components/specs/SpecExperimentationDashboard.tsx",
  "components/terminal/BatchActions.tsx",
  "components/terminal/CommandPalette.tsx",
  "components/terminal/KeyboardShortcutsOverlay.tsx",
  "components/terminal/LaunchMenu.tsx",
  "components/terminal/OutputSearchBar.tsx",
  "components/terminal/PromptModal.tsx",
  "components/terminal/SessionCard.tsx",
  "components/terminal/SessionInfoDropdown.tsx",
  "components/terminal/TerminalFindingsPanel.tsx",
  "components/terminal/TerminalPageTabBar.tsx",
  "components/terminal/ZoneDiffOverlay.tsx",
  "components/terminal/ZoneHoverActions.tsx",
  "components/terminal/ZoneProfilePicker.tsx",
  "components/terminal/approveAll.ts",
  "components/terminal/backends/webglContextLru.ts",
  "components/terminal/commands/bind.ts",
  "components/terminal/commands/corpus.testkit.ts",
  "components/terminal/commands/differential.testkit.ts",
  "components/terminal/commands/parse.ts",
  "components/terminal/commands/pipeline.testkit.ts",
  "components/terminal/commands/uibridge.ts",
  "components/terminal/commands/useTerminalCommands.ts",
  "components/terminal/commands/verdict.ts",
  "components/terminal/result-card/ResultCardMount.tsx",
  "components/terminal/resumeVerification.ts",
  "components/terminal/suggestions/useSuggestions.tsx",
  "components/terminal/terminalKeySequence.ts",
  "components/terminal/terminalWriteResult.ts",
  "components/terminal/useKeyboardShortcuts.ts",
  "components/terminal/useMidSessionProbe.ts",
  "components/terminal/zone-grid/CompactZoneCard.tsx",
  "components/terminal/zone-grid/ZoneContextMenu.tsx",
  "components/terminal/zone-grid/ZoneLabel.tsx",
  "components/triggers/TriggerHistory.tsx",
  "components/tutorial/SpotlightOverlay.tsx",
  "components/ui-bridge/ElementDescriptionPanel.tsx",
  "components/ui-bridge/ElementOverlay.tsx",
  "components/ui-bridge/ElementPicker.tsx",
  "components/ui-bridge/ElementTreeView.tsx",
  "components/ui-bridge/EventTimelineView.tsx",
  "components/ui-bridge/FailureChainViewer.tsx",
  "components/ui-bridge/ImageViewerModal.tsx",
  "components/ui-bridge/StateDiscoveryCards.tsx",
  "components/ui/BatchDeleteDialog.tsx",
  "components/ui/BuilderToolbar.tsx",
  "components/ui/ConfirmDialog.tsx",
  "components/unified-search/CommandPalette.tsx",
  "components/widgets/ai-conversation/AiConversationSummary.tsx",
  "components/widgets/ai-conversation/AiConversationWidget.tsx",
  "components/widgets/api-request/ApiRequestWidget.tsx",
  "components/widgets/canvas/components/ChecklistPanel.tsx",
  "components/widgets/canvas/components/KeyValuePanel.tsx",
  "components/widgets/canvas/components/PanelCard.tsx",
  "components/widgets/command/CommandWidget.tsx",
  "components/widgets/execution-timeline/ExecutionTimelineWidget.tsx",
  "components/widgets/execution-timeline/useExecutionTimelineData.ts",
  "components/widgets/findings/FindingsSummary.tsx",
  "components/widgets/flow-execution/useFlowExecutionData.ts",
  "components/widgets/gui-automation/GuiAutomationSummary.tsx",
  "components/widgets/mcp-call/McpCallWidget.tsx",
  "components/widgets/playwright-test/PlaywrightTestWidget.tsx",
  "components/widgets/shared/StepOutputPanel.tsx",
  "components/widgets/shell-command/ShellCommandWidget.tsx",
  "components/widgets/trace-viewer/ReplayController.tsx",
  "components/widgets/trace-viewer/SpanDetailPanel.tsx",
  "components/widgets/trace-viewer/SpanRow.tsx",
  "components/widgets/trace-viewer/TraceComparison.tsx",
  "components/widgets/ui-bridge/UiBridgeWidget.tsx",
  "components/widgets/verification/VerificationWidget.tsx",
  "components/widgets/workflow-ref/WorkflowRefWidget.tsx",
  "components/workflow-builder/AddStateStepsModal.tsx",
  "components/workflow-builder/AiGeneratePanel.tsx",
  "components/workflow-builder/AiGenerateWorkflowModal.tsx",
  "components/workflow-builder/CurlImportDialog.tsx",
  "components/workflow-builder/GenerateFromStatesModal.tsx",
  "components/workflow-builder/PipelineConfigPanel.tsx",
  "components/workflow-builder/PromptLibraryPicker.tsx",
  "components/workflow-builder/RunOptionsDialog.tsx",
  "components/workflow-builder/SettingsPanel.tsx",
  "components/workflow-builder/ShellCommandLibraryPicker.tsx",
  "components/workflow-builder/StageSelector.tsx",
  "components/workflow-builder/StepItem.tsx",
  "components/workflow-builder/WorkflowBuilderTab.tsx",
  "components/workflow-builder/step-config/CommandConfig.tsx",
  "components/workflow-builder/step-config/DataFlowSection.tsx",
  "components/workflow-versions/WorkflowVersionLineage.tsx",
  "contexts/SessionContext.tsx",
  "contexts/WorkflowExecutionContext.tsx",
  "hooks/dashboard/useDashboardLayout.ts",
  "hooks/ui-bridge-events/recoveryScope.ts",
  "hooks/ui-bridge-events/useAISearchEvents.ts",
  "hooks/ui-bridge-events/useControlEvents.ts",
  "hooks/useArchitecture.ts",
  "hooks/useTutorialKeyboard.ts",
  "hooks/useUIBridgeDiscovery.ts",
  "hooks/useWebSocketEvents.ts",
  "lib/compile-state-machine.ts",
  "lib/step-output-handlers/code-execution-handler.ts",
  "lib/step-output-handlers/command-handler.ts",
  "lib/step-output-handlers/prompt-handler.ts",
  "lib/workflow-builder/buildSpecWorkflow.ts",
  "pages/specs/ApiOverview.tsx",
  "pages/specs/ConnectionBar.tsx",
  "pages/ui-bridge-integration/DiscoveryPanel.tsx",
  "pages/ui-bridge-integration/ProjectCoordinator.tsx",
  "utils/ExecutionTreeManager.ts",
];

/* ── the inventory allowlists (mechanism B) ──────────────────────────── */

/**
 * Chords claimed by more than one FILE, and why that is tolerated.
 *
 * "Claimed by two files" is a STATIC property. Two of these are live
 * simultaneous double-fires; the digit range is not, and the difference
 * is recorded here rather than smoothed over, because a reader who
 * cannot tell them apart cannot prioritise them.
 */
const KNOWN_SHARED_CHORDS: Record<string, string> = {
  "ctrl+shift+g":
    "terminal cycle-tag-filter vs. dev/GiantSCCFixture. LIVE double-fire: the fixture " +
    "is deliberately shipped in every build (see its header) and mounted app-wide from " +
    "App.tsx, so on the terminal page one press does both. Reassigning a documented " +
    "letter is a product call.",
  "ctrl+shift+p":
    "terminal TOGGLE_CONTROL_PANEL vs. dev/PerformanceOverlay. The overlay's LISTENER is " +
    "now gated on `import.meta.env.DEV` (it used to be gated only at render, so the " +
    "production bundle kept a dead surface's chord claim and its 1 Hz interval alive), " +
    "so production has exactly one claimant. The static two-file claim remains.",
  "ctrl+shift+tab":
    "terminal focus-prev-zone vs. active-dashboard/ActiveRunsBar prev-run (live while >=2 runs)",
  "ctrl+tab":
    "terminal focus-next-zone vs. active-dashboard/ActiveRunsBar next-run (live while >=2 runs)",
  // Ctrl+1..8 — DashboardPage's widget-by-position vs. the terminal's
  // focus-zone-by-number. This WAS a live double-fire (one Ctrl+3 on the
  // Active dashboard moved the terminal's focused zone), and it was
  // invisible to this suite twice over: the range spelling produced no
  // countable claim, and a fixture explicitly whitelisted it.
  // It is no longer live in either direction — `TabContent` mounts
  // `DashboardPage` only on the Active tab, and the terminal's listener
  // is now inert while its surface is hidden (`isSurfaceVisible`) — but
  // both claims still exist in source, so they are pinned here.
  ...Object.fromEntries(
    [1, 2, 3, 4, 5, 6, 7, 8].map((d) => [
      `ctrl+${d}`,
      "active-dashboard/DashboardPage widget-by-position vs. terminal focus-zone-by-number. " +
        "Not simultaneously live: DashboardPage mounts only on the Active tab and the " +
        "terminal's window listener is surface-visibility gated.",
    ]),
  ),
};

/**
 * Every `switch` on a key value in the tree.
 *
 * All four dispatch on BARE keys today, so none is a chord claim and none
 * can collide with `GLOBAL_CHORDS`.
 *
 * This roster used to carry the claim that "a `case` arm that started
 * testing `event.ctrlKey` WOULD be a claim, and property A catches that
 * directly". **That was false**: `guardModifiers` read only the switch
 * DISCRIMINANT, so `switch (e.key) { case "k": if (e.ctrlKey) act(); }`
 * scanned green — in exactly the four files most likely to grow such an
 * arm. The scanner now emits a claim PER CASE CLAUSE, and the mutation
 * matrix pins the spelling.
 */
const KNOWN_SWITCH_KEY_REGISTRIES: string[] = [
  "components/PromptSnippetSelector.tsx",
  "components/active-dashboard/ActiveRunsBar.tsx",
  "components/navigation/Sidebar.tsx",
  "hooks/useTutorialKeyboard.ts",
];

/**
 * EVERY modifier-qualified key claim in the rostered files, spelled as a
 * full chord — the allowlist that makes the inventory an inversion rather
 * than a search.
 *
 * A `shift+…` entry is a shift-ONLY claim: `matchesChord` cannot express
 * one (every table entry requires Ctrl), so it is inventoried rather than
 * demanded into the table. See `keyClaimScan.ts::CONTROL_TAGS`.
 *
 * All three files below are ELEMENT-scoped — a focused textarea, and
 * xterm's `attachCustomKeyEventHandler`. That is why they are tolerated
 * outside the table and not merely tolerated silently: their scope is
 * different in kind from a `window` listener's. They fire only while one
 * element has focus, and their claims are passthrough semantics (Ctrl+C is
 * copy-or-SIGINT, Ctrl+V is paste-into-PTY, Ctrl+Home scrolls the
 * scrollback) rather than app chords.
 *
 * What IS enforceable, and is enforced here, is that the set does not grow
 * silently. It had already grown by eight before this list could see them.
 */
const KNOWN_KEY_CLAIMS: Record<string, string[]> = {
  // Ctrl/Cmd+Enter submits the prompt from the focused textarea.
  "components/scheduler/AiScheduleBuilder.tsx": ["ctrl+enter"],
  // xterm `attachCustomKeyEventHandler` — clipboard + find, PTY-scoped.
  // Bare F3 / Shift+F3 also handled there; a bare key is not a chord claim.
  "components/terminal/TerminalInstance.tsx": ["ctrl+c", "ctrl+f", "ctrl+shift+c", "ctrl+v"],
  // VS Code-parity scrollback navigation, consumed by `TerminalInstance`'s
  // xterm handler. THE EIGHT CLAIMS THE OLD SCANNER COULD NOT SEE: the file
  // holds no listener of its own, so no selection rule reached it, and its
  // `const { key, shiftKey, ctrlKey, altKey, metaKey } = e` destructure hid
  // every key test from a `.key`-anchored regex.
  "components/terminal/scrollKeys.ts": [
    "ctrl+alt+pagedown",
    "ctrl+alt+pageup",
    "ctrl+arrowdown",
    "ctrl+arrowup",
    "ctrl+end",
    "ctrl+home",
    "shift+pagedown",
    "shift+pageup",
  ],
};

/* ── source walk ─────────────────────────────────────────────────────── */

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      if (entry === "node_modules") continue;
      out.push(...sourceFiles(full));
      continue;
    }
    if (!/\.tsx?$/.test(entry)) continue;
    if (/\.(test|spec)\.tsx?$/.test(entry)) continue;
    out.push(full);
  }
  return out;
}

const FILES = sourceFiles(SRC).map((path) => ({
  rel: relative(SRC, path).split("\\").join("/"),
  source: readFileSync(path, "utf8"),
}));

/**
 * Cheap gate on which files are PARSED, and why it is sound rather than a
 * second (regex) scanner smuggled back in.
 *
 * Mechanism A recognises a field READ, which requires the field's NAME to
 * appear literally in the text — as a property access (`.key`), an element
 * access (`["key"]`), a destructured binding (`{ key }`) or a string
 * literal. A file containing none of these words anywhere cannot contain a
 * key read under any spelling, so skipping it removes no coverage. The
 * listener event names are here for the same reason: a registration cannot
 * be spelled without naming its event.
 *
 * It is a substring test on FIELD NAMES, never on the shape of a
 * comparison, which is the thing that kept failing.
 */
const COULD_READ_A_KEY =
  /\b(?:key|code|keyCode|which|ctrlKey|metaKey|altKey|shiftKey|getModifierState|keydown|keyup|keypress)\b/;

/**
 * Parse each candidate ONCE and hand the tree to both mechanisms.
 *
 * A second parse per mechanism would have doubled the suite's dominant
 * cost — parsing is ~90% of its runtime, not the walking.
 */
const PARSED = FILES.filter(
  (f) => !MECHANISM_FILES.has(f.rel) && COULD_READ_A_KEY.test(f.source),
).map((f) => {
  const sf = parseSource(f.source, f.rel);
  return { rel: f.rel, sf, reads: findKeyFieldReads(sf) };
});

/** Files mechanism A DETECTS reading a modifier / unambiguous key field. */
const DETECTED_MODIFIER_READERS = PARSED.filter((p) => p.reads.modifier.length > 0).map(
  (p) => p.rel,
);

/** Files mechanism A detects reading ONLY `key` / `code`. */
const DETECTED_KEY_READERS = PARSED.filter(
  (p) => p.reads.modifier.length === 0 && p.reads.ambiguous.length > 0,
).map((p) => p.rel);

/**
 * Mechanism B runs on the DETECTED field readers, not on the declared
 * roster — so a brand-new claimant reds property B as well as property A1,
 * instead of hiding behind a roster it is not yet on.
 */
const SCANS: Array<{ rel: string; scan: FileScan }> = PARSED.filter(
  (p) => p.reads.modifier.length > 0 || p.reads.ambiguous.length > 0,
).map((p) => ({ rel: p.rel, scan: scanKeyClaimsIn(p.sf) }));

/**
 * Files that attach a key listener to a GLOBAL target.
 *
 * Structural, not textual. The regex this replaced hard-coded
 * `\b(window|document)\.addEventListener\(\s*"key…`, so it graded none of
 * `globalThis.addEventListener("keydown", …)`, a bare
 * `addEventListener("keydown", …)`, an aliased `const w = window;
 * w.addEventListener(…)`, or a single-quoted `'keydown'` — four app-wide
 * claimants the strictest property in this file simply did not look at.
 *
 * Matching a REGISTRATION structurally is fair: its shape is fixed by the
 * DOM API. What must never be matched structurally is the CLAIM.
 */
const GLOBAL_LISTENER_FILES = new Set(
  PARSED.filter((p) => hasGlobalKeyListener(p.sf)).map((p) => p.rel),
);

/* ── mutation harness ────────────────────────────────────────────────── */

/** Scan a snippet as if it were a file, keeping only the claims. */
function claimsInSnippet(snippet: string): string[] {
  return scanKeyClaims(snippet, "snippet.ts").claims.map((c) => c.spelling);
}

/**
 * Scan a snippet INJECTED INTO A REAL FILE.
 *
 * The previous rewrite's mutation matrix caught a spelling that passed as
 * a standalone snippet and scanned GREEN inside a real module, because the
 * scanner's alias pass had a whole-file artefact. A snippet fixture alone
 * is therefore not evidence; this is the arm that is.
 */
const HOST_REL = TERMINAL_REGISTRY;
const HOST_SOURCE = FILES.find((f) => f.rel === HOST_REL)?.source ?? "";

function claimsInRealFile(snippet: string): string[] {
  const injected = `${HOST_SOURCE}\nfunction __mutationProbe(e: KeyboardEvent, act: () => void) {\n  ${snippet}\n  void act;\n}\n`;
  return scanKeyClaims(injected, HOST_REL).claims.map((c) => c.spelling);
}

/** What mechanism A sees in a snippet — the coverage half of the pair. */
function fieldReadsInSnippet(snippet: string): { modifier: string[]; ambiguous: string[] } {
  return findKeyFieldReads(parseSource(snippet, "snippet.ts"));
}

/* ── A. coverage by ban ──────────────────────────────────────────────── */

/** `add`/`remove` lines a reviewer can paste straight into the roster. */
function rosterDiff(detected: readonly string[], declared: readonly string[]): string[] {
  const d = new Set(declared);
  const f = new Set(detected);
  return [
    ...detected.filter((r) => !d.has(r)).map((r) => `ADD    "${r}",`),
    ...declared.filter((r) => !f.has(r)).map((r) => `REMOVE "${r}",`),
  ];
}

describe("A. no file reads a keyboard field unless it is on the roster", () => {
  it("pins every file that reads a MODIFIER or unambiguous key field", () => {
    const declared = Object.keys(MODIFIER_FIELD_ROSTER).sort();
    const detected = [...DETECTED_MODIFIER_READERS].sort();
    expect(rosterDiff(detected, declared)).toEqual([]);
    expect(detected).toEqual(declared);
  });

  it("pins every file that reads `key` or `code`", () => {
    const declared = [...KEY_FIELD_ROSTER].sort();
    const detected = [...DETECTED_KEY_READERS].sort();
    expect(rosterDiff(detected, declared)).toEqual([]);
    expect(detected).toEqual(declared);
  });

  it("keeps the two tiers disjoint and each sorted", () => {
    const tier1 = Object.keys(MODIFIER_FIELD_ROSTER);
    const tier2 = [...KEY_FIELD_ROSTER];
    expect(tier1.filter((r) => tier2.includes(r))).toEqual([]);
    // Sorted, so a new entry lands next to its neighbours in the diff
    // rather than wherever it was appended.
    expect(tier1).toEqual([...tier1].sort());
    expect(tier2).toEqual([...tier2].sort());
    // Every tier-1 entry justifies itself; that is what a reviewer grades
    // a new one against.
    for (const [rel, why] of Object.entries(MODIFIER_FIELD_ROSTER)) {
      expect(why.length, `${rel} needs a note`).toBeGreaterThan(20);
    }
  });

  it("sees the read under every spelling of the READ", () => {
    // The rules mechanism A is made of, pinned individually. Falsification
    // found that two of them — element access and the string-literal rule
    // — could be DELETED with the whole suite still green, which is the
    // same defect class as the count this rework removed: a rule nothing
    // exercises is a rule that can vanish silently.
    const modifierSpellings = [
      ["property access", "if (e.ctrlKey) act();", "ctrlKey"],
      ["element access", 'if (e["ctrlKey"]) act();', "ctrlKey"],
      ["binding element", "const { ctrlKey } = e;", "ctrlKey"],
      ["renamed binding", "const { ctrlKey: c } = e;", "ctrlKey"],
      ["computed binding", 'const { ["ctrlKey"]: c } = e;', "ctrlKey"],
      ["destructured parameter", "const f = ({ ctrlKey }) => ctrlKey;", "ctrlKey"],
      ["string literal in a table", 'const MODS = ["ctrlKey"];', "ctrlKey"],
      ["string literal via Reflect", 'Reflect.get(e, "metaKey");', "metaKey"],
      ["legacy keyCode", "if (e.keyCode === 90) act();", "keyCode"],
      ["legacy which", "if (e.which === 90) act();", "which"],
      ["getModifierState call", 'e.getModifierState("Control");', "getModifierState"],
      ["cast receiver", "if ((e as KeyboardEvent).altKey) act();", "altKey"],
      ["chained receiver", "if (e.nativeEvent.shiftKey) act();", "shiftKey"],
    ] as const;
    for (const [label, snippet, field] of modifierSpellings) {
      expect(fieldReadsInSnippet(snippet).modifier, `${label}: ${snippet}`).toContain(field);
    }

    const ambiguousSpellings = [
      ["property access", 'if (e.key === "z") act();', "key"],
      ["element access", 'if (e["key"] === "z") act();', "key"],
      ["binding element", "const { key } = e;", "key"],
      ["destructured parameter", "const f = ({ key }) => key;", "key"],
      [".code", 'if (e.code === "KeyZ") act();', "code"],
    ] as const;
    for (const [label, snippet, field] of ambiguousSpellings) {
      expect(fieldReadsInSnippet(snippet).ambiguous, `${label}: ${snippet}`).toContain(field);
    }
  });

  it("does not cry wolf on things that are not field reads", () => {
    // A ban that fires on a property WRITE, a JSX attribute or a method
    // named `key` would put most of the tree on the roster and get
    // switched off. These are the exclusions, pinned.
    const clean = [
      "const o = { key: 1, code: 2 };", // an object literal WRITES the name
      "localStorage.key(0);", // `KeyboardEvent.key` is a string, never called
      "type T = { key: string };", // a type member
      "import { key } from './k';", // an import specifier
      "obj.keys(); obj.keyboard; obj.codeName;", // near-miss names
    ];
    for (const snippet of clean) {
      const r = fieldReadsInSnippet(snippet);
      expect([...r.modifier, ...r.ambiguous], snippet).toEqual([]);
    }
  });

  it("is not silently matching nothing", () => {
    // A ban that detects zero reads passes vacuously. These numbers are
    // measured floors, not targets.
    expect(PARSED.length).toBeGreaterThan(500);
    expect(DETECTED_MODIFIER_READERS.length).toBeGreaterThan(10);
    expect(DETECTED_KEY_READERS.length).toBeGreaterThan(100);
  });

  it("sees a field read in every spelling mechanism B is blind to", () => {
    // THE BOUND ON B'S BLAST RADIUS. Asserted per spelling against the
    // mechanism itself, which is precisely what `ESCAPING_CLASS_COUNT = 4`
    // was not: a remembered number instead of a probe.
    for (const c of PROBE_CLASSES.filter((x) => !x.caught)) {
      for (const snippet of c.spellings) {
        const reads = fieldReadsInSnippet(snippet);
        expect(
          reads.modifier.length + reads.ambiguous.length,
          `B misses and A must not: ${c.name}: ${snippet}`,
        ).toBeGreaterThan(0);
      }
    }
  });

  /**
   * Mechanism A's OWN limits, probed rather than asserted.
   *
   * A ban on reading a NAME can only be escaped by never naming the field.
   * These are the three ways to do that. Each is written down with
   * spellings, and each spelling is checked to actually escape — a floor
   * that shrinks goes red here, the same as one that grows.
   */
  const MECHANISM_A_ESCAPES: Array<{
    name: string;
    why: string;
    spellings: string[];
    /** `"none"` = no field seen at all. `"ambiguous"` = tier 2 only. */
    seen: "none" | "ambiguous";
  }> = [
    {
      name: "field name assembled at runtime",
      why:
        "No literal equal to the field name exists anywhere, so the string-literal " +
        "rule cannot fire either. Closing it needs constant folding.",
      seen: "none",
      spellings: [
        'const C = "ctrl" + "Key"; if (e[C]) act();',
        'const K = "ke" + "y"; if (e[K] === "z") act();',
      ],
    },
    {
      name: "positional read with no field name anywhere",
      why:
        "The field is addressed by POSITION in a derived collection, so there is no " +
        "name for a name-based rule to see. Also mechanism B's fourth escape.",
      seen: "none",
      spellings: ["if (Object.values(e)[2]) act();", 'if (Object.entries(e)[3][1] === "z") act();'],
    },
    {
      name: "modifier asserted in another FILE",
      why:
        "The importing file reads `key` — so it IS on the tier-2 roster and is not " +
        "invisible — but reads no modifier field, so it is not on the chord-relevant " +
        "tier-1 roster. Closing it needs a cross-file call graph. This is the escape " +
        "tier 2 exists to bound: the file is named, its claim is not inventoried.",
      seen: "ambiguous",
      spellings: [
        'import { isMod } from "./m";\nif (isMod(e) && e.key === "z") act();',
        'import { isMod } from "./m";\nif (isMod(e)) { if (e.key === "z") act(); }',
      ],
    },
  ];

  it("escapes on every spelling of every class mechanism A admits escaping", () => {
    for (const c of MECHANISM_A_ESCAPES) {
      expect(c.why.length, `${c.name} must say why`).toBeGreaterThan(20);
      expect(c.spellings.length, `${c.name} needs >1 spelling`).toBeGreaterThan(1);
      for (const snippet of c.spellings) {
        const reads = fieldReadsInSnippet(snippet);
        expect(reads.modifier, `${c.name} must read no modifier field: ${snippet}`).toEqual([]);
        if (c.seen === "none") {
          expect(reads.ambiguous, `${c.name}: ${snippet}`).toEqual([]);
        } else {
          expect(
            reads.ambiguous.length,
            `${c.name} must stay tier-2 visible: ${snippet}`,
          ).toBeGreaterThan(0);
        }
      }
    }
  });
});

/* ── A. the global-listener detector ─────────────────────────────────── */

describe("A. global key listeners are found structurally", () => {
  const listens = (src: string): boolean => hasGlobalKeyListener(parseSource(src, "probe.ts"));

  it("finds every spelling of a global registration", () => {
    const spellings = [
      'window.addEventListener("keydown", h);',
      "window.addEventListener('keydown', h);",
      'document.addEventListener("keyup", h);',
      'globalThis.addEventListener("keydown", h);',
      'self.addEventListener("keypress", h);',
      'addEventListener("keydown", h);',
      'const w = window; w.addEventListener("keydown", h);',
      'const d = document; const d2 = d; d2.addEventListener("keydown", h);',
      'const EVT = "keydown"; window.addEventListener(EVT, h);',
      'window.document.addEventListener("keydown", h);',
      // Found by probing this rework, not by iteration 9.
      'window?.addEventListener("keydown", h);',
      "window.addEventListener(`keydown`, h);",
      'document.body.addEventListener("keydown", h);',
      'document.documentElement.addEventListener("keydown", h);',
      "window.onkeydown = h;",
      "document.onkeyup = (e) => act(e);",
    ];
    for (const s of spellings) expect(listens(s), s).toBe(true);
  });

  it("leaves element-scoped and non-key registrations alone", () => {
    const clean = [
      'el.addEventListener("keydown", h);',
      'ref.current.addEventListener("keydown", h);',
      'window.addEventListener("resize", h);',
      'window.addEventListener("click", h);',
      "el.onkeydown = h;",
    ];
    for (const s of clean) expect(listens(s), s).toBe(false);
  });

  /**
   * The detector's own declared escape. A registration hidden behind a
   * helper has no `addEventListener` at the call site to match, and the
   * helper's own file IS matched — so the effect is that the CALLER is
   * graded as element-scoped rather than app-wide, not that it disappears:
   * mechanism A still rosters it and the claim inventory still pins it.
   */
  it("escapes a registration hidden behind a helper", () => {
    expect(listens('onKey(window, "keydown", h);')).toBe(false);
    expect(listens('registerGlobalKey("keydown", h);')).toBe(false);
  });

  it("finds the global listeners that are actually in the tree", () => {
    expect(GLOBAL_LISTENER_FILES.size).toBeGreaterThan(10);
  });
});

/* ── F. the scanner can actually fail ────────────────────────────────── */

/**
 * The mutation matrix. Every spelling iterations 7, 8 and 9 tested, with
 * the verdict the mechanism gave BEFORE the round that closed it. All of
 * them must be red now, as snippets and injected into a real file.
 */
const OFFENDERS: Array<[string, string]> = [
  // ── caught by the original regex scanner (13) ──
  ['e.ctrlKey && e.key === "Z";', "RED — plain equality"],
  ['e.ctrlKey && !e.shiftKey && e.key === "/";', "RED — negated shift term"],
  ['e.ctrlKey && e.key.toLowerCase() === "z";', "RED — case-folded"],
  ['e.ctrlKey && e.code === "KeyZ";', "RED — .code"],
  ['const hit = e.ctrlKey && e.key === "Z"; if (hit) { act(); }', "RED — hoisted key half"],
  ['if (e.ctrlKey) switch (e.key) { case "Z": act(); }', "RED — unbraced guarded switch"],
  ['if (e.ctrlKey) { switch (e.key) { case "Z": act(); } }', "RED — braced guarded switch"],
  ['if (e.metaKey) { switch (e.code) { case "KeyZ": act(); } }', "RED — guarded switch on .code"],
  ['if (e.ctrlKey) { if (e.key === "Z") act(); }', "RED — guarded nested equality"],
  ['e.ctrlKey && ["Z"].includes(e.key);', "RED — array membership"],
  ['e.ctrlKey && "Z" === e.key;', "RED — Yoda"],
  ['e.altKey && e.key === "Z";', "RED — Alt as the modifier"],
  ['e.ctrlKey && e.key >= "1" && e.key <= "8";', "RED — digit range"],

  // ── ALSO caught by the regex scanner, from earlier rounds ──
  ['(e.metaKey || e.ctrlKey) && e.key === "k";', "RED — Cmd-alias idiom"],
  ['e.ctrlKey && e.shiftKey && e.key === "Tab";', "RED — named key"],
  ['e.ctrlKey && (e.key === "Tab" || e.key === "`");', "RED — disjunction of keys"],
  ['const mod = e.ctrlKey || e.metaKey; if (mod && e.key === "z") { act(); }', "RED — alias hoist"],
  ['e.ctrlKey && e.key.toUpperCase() === "Z";', "RED — .toUpperCase()"],
  ["e.ctrlKey && KEYS.has(e.key);", "RED — Set membership"],

  // ── the escape set: GREEN against the regex scanner (14) ──
  ['const { key, ctrlKey } = e; if (ctrlKey && key === "z") { act(); }', "GREEN — destructured"],
  ['const k = e.key; if (e.ctrlKey && k === "z") { act(); }', "GREEN — aliased key"],
  ["e.ctrlKey && /^[1-8]$/.test(e.key);", "GREEN — regex range"],
  ["e.ctrlKey && e.keyCode === 90;", "GREEN — legacy .keyCode"],
  ["e.ctrlKey && e.which === 90;", "GREEN — legacy .which"],
  ['if (isMod(e)) { if (e.key === "z") { act(); } }', "GREEN — modifier behind a helper"],
  ['e.ctrlKey && e["key"] === "z";', "GREEN — bracket access"],
  ['e.getModifierState("Control") && e.key === "z";', "GREEN — getModifierState"],
  ["e.ctrlKey && KEYMAP[e.key];", "GREEN — lookup table, no comparison at all"],
  ['e.ctrlKey && e.key.startsWith("z");', "GREEN — startsWith"],
  ["e.ctrlKey && e.key.match(/^z$/);", "GREEN — String.match"],
  ['e.ctrlKey && e.key.localeCompare("z") === 0;', "GREEN — localeCompare"],
  [
    "if (!(e.ctrlKey || e.metaKey)) return;\n  if (!/^[0-9]$/.test(e.key)) return;\n  act();",
    "GREEN — `matchesDigitChord`'s OWN body: early-return guards, regex range",
  ],
  [
    'const { key, shiftKey, ctrlKey } = e;\n  if (ctrlKey && !shiftKey) { if (key === "Home") act(); }',
    "GREEN — `scrollKeys.ts`'s own live spelling",
  ],

  // ── the AST scanner's escape set, found by iteration 8 (D6). Every one
  //    is a RECEIVER spelled a way `ts.isIdentifier` could not see.
  ['(e).ctrlKey && (e).key === "z";', "GREEN — parenthesised receiver"],
  ['e!.ctrlKey && e!.key === "z";', "GREEN — non-null-asserted receiver"],
  ['(e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "z";', "GREEN — cast receiver"],
  ['e.ctrlKey && e.nativeEvent.key === "z";', "GREEN — React's `nativeEvent`, one hop"],
  ['e.nativeEvent.ctrlKey && e.nativeEvent.key === "z";', "GREEN — `nativeEvent` on both halves"],
  ['this.ev.ctrlKey && this.ev.key === "z";', "GREEN — member receiver"],
  ['evs[0].ctrlKey && evs[0].key === "z";', "GREEN — indexed receiver"],
  [
    'const isUndo = (ev: KeyboardEvent) => ev.ctrlKey && ev.key === "z";',
    "GREEN — an arrow's CONCISE body: the modifier and the key read are three " +
      "tokens apart in ONE expression, and it was the only statement-shaped " +
      "position with no arm in `guardModifiers`.",
  ],
  [
    'switch (true) { case e.ctrlKey: if (e.key === "z") act(); }',
    "GREEN — assertion in the CASE CLAUSE, not the discriminant",
  ],

  // ── iteration 9's findings against the AST scanner (D3–D9, D14) ──
  [
    'if (!e.ctrlKey) return;\n  if (e.key === "z") act();',
    "GREEN — D3, NEGATION POLARITY DOUBLE-COUNTED. `assertedBy` had already " +
      "stripped the `!` and called the modifier walk with positive polarity; the " +
      "walk then re-applied the same `!` and dropped the modifier. The most " +
      "idiomatic guard spelling there is. The pinned `!(e.ctrlKey||e.metaKey)` row " +
      "was red only by the accident of being a parenthesised BINARY.",
  ],
  [
    'if (!e.ctrlKey) { return; }\n  if (e.key === "z") act();',
    "GREEN — D3, braced exit arm of the same defect",
  ],
  ['!!e.ctrlKey && e.key === "z";', "GREEN — D3, `!!` was not folded: a two-character mutation"],
  ['!!!!e.ctrlKey && e.key === "z";', "GREEN — D3, parity rather than a boolean test"],
  [
    'switch (e.key) { case "k": if (e.ctrlKey) act(); break; }',
    "GREEN — D4, CASE ARM BODIES WERE NEVER CONSULTED, while this file's own " +
      "comment asserted they were. `KNOWN_SWITCH_KEY_REGISTRIES` allowlists four " +
      "files that could grow exactly this.",
  ],
  [
    'switch (e.key) { case "k": { if (e.metaKey) act(); break; } }',
    "GREEN — D4, braced arm of the same defect",
  ],
  [
    'switch (true) { case e.ctrlKey && e.key === "z": act(); }',
    "GREEN — D5, both halves in the CLAUSE: the `p.expression !== cur` exclusion " +
      "skipped the very expression carrying the claim, while the two-statement " +
      "spelling of the same thing was red.",
  ],
  ['getEv().ctrlKey && getEv().key === "z";', "GREEN — D6, a CALL as the receiver"],
  [
    'const probeAwait = async (p: Promise<KeyboardEvent>) => (await p).ctrlKey && (await p).key === "z";',
    "GREEN — D6, an AWAIT as the receiver",
  ],
  [
    '(Reflect).get(e, "key") === "z" && e.ctrlKey;',
    "GREEN — D6, `Reflect` behind a parenthesis: the receiver test was hard-coded " +
      'to `ts.isIdentifier(…) && text === "Reflect"`',
  ],
  [
    'window.addEventListener("keydown", ({ key, ctrlKey }) => { if (ctrlKey && key === "z") act(); });',
    "GREEN — D7, a DESTRUCTURED LISTENER PARAMETER, on `window` itself. The " +
      "identifier spelling was red; two rules each assumed the other covered this one.",
  ],
  [
    'const onKey = ({ key, ctrlKey }: KeyboardEvent) => { if (ctrlKey && key === "z") act(); };',
    "GREEN — D7, the same via a type annotation rather than a registration",
  ],
  ['if (this.isMod(e)) { if (e.key === "z") act(); }', "GREEN — D9, method predicate on `this`"],
  ['if (mods.isMod(e)) { if (e.key === "z") act(); }', "GREEN — D9, method predicate on an object"],
  ['if ((isMod)(e)) { if (e.key === "z") act(); }', "GREEN — D9, parenthesised callee"],
  ['if (isMod.call(null, e)) { if (e.key === "z") act(); }', "GREEN — D9/D14, `.call`"],
  ['if (isMod.apply(null, [e])) { if (e.key === "z") act(); }', "GREEN — D9/D14, `.apply`"],
  [
    'const [c1] = [e.ctrlKey];\n  if (c1 && e.key === "z") act();',
    "GREEN — D14, the ARRAY spelling of an alias hoist whose object spelling was red",
  ],
  [
    'if ((() => e.ctrlKey)()) { if (e.key === "z") act(); }',
    "GREEN — D14, an IIFE: its body runs right here, but the walk refused to " +
      "descend into anything function-shaped",
  ],

  // ── found by probing THIS rework, not listed by iteration 9 ──
  [
    'if (e.key === "z") { if (e.ctrlKey) act(); }',
    "GREEN — D15, THE MIRROR OF A PINNED RED ROW. `if (e.ctrlKey) { if (e.key === " +
      '"z") act(); }` was red; swapping the two lines made it green, because the ' +
      "guard walk only ever looked UP. Iteration 9 did not list this one; it was " +
      "found by probing the tier-1 roster for interprocedural escapes and noticing " +
      "that `TerminalFindBar.interpretFindKey` reads its modifier downstream.",
  ],
  [
    'if (e.key === "z") { while (e.metaKey) act(); }',
    "GREEN — D15, the `while` spelling of the same ordering defect",
  ],
  [
    'if (!e.ctrlKey) { return; } else { void 0; }\n  if (e.key === "z") act();',
    "GREEN — D16, an early-return guard DISQUALIFIED BY ITS `else`. When the THEN " +
      "arm exits, the only way past the `if` is the false branch, so the assertion " +
      "holds exactly as it does without an `else` — the arm was refused for no reason.",
  ],
  [
    'let m = false; m ||= e.ctrlKey;\n  if (m && e.key === "z") act();',
    "GREEN — D17, a modifier hoisted by ASSIGNMENT rather than by declaration. The " +
      "alias pass only ever looked at declarations, so the idiom for building a flag " +
      "across several lines dropped it.",
  ],
  [
    'let m2 = false; m2 = e.metaKey;\n  if (m2 && e.key === "z") act();',
    "GREEN — D17, plain `=` spelling of the same",
  ],
  [
    'class Probe { hit = e.ctrlKey && e.key === "z"; }',
    "GREEN — D18, a CLASS FIELD initializer: a variable statement wearing a " +
      "different node kind, with no arm in `guardModifiers` at all",
  ],
  [
    'export default e.ctrlKey && e.key === "z";',
    "GREEN — D18, `export default` — the same hole at the other end of the file",
  ],
];

/**
 * Spellings that must stay CLEAN. Over-reporting is the safe direction for
 * a scanner, but not without limit: a rule that flags a bare-key handler
 * demands a rewrite into a predicate (`matchesChord`) that cannot express
 * it, so these are load-bearing. A ban that cries wolf gets disabled.
 */
const CLEAN: string[] = [
  // A bare key claimed while DELIBERATELY excluding the modifiers.
  'e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey;',
  'e.key === "Escape";',
  'switch (e.key) { case "ArrowRight": next(); }',
  // A `switch` whose arms do real work but assert no modifier — the shape
  // the new per-clause rule must not over-report on.
  'switch (e.key) { case "ArrowRight": next(); break; case "Escape": close(); break; }',
  // A guard clause that excludes the modifier — the polarity mirror of the
  // `matchesDigitChord` offender above, and the one that would break if
  // guard inheritance were written without tracking polarity.
  'if (e.ctrlKey) return;\n  if (e.key === "Escape") { act(); }',
  // The `!!` fold must not invert an ordinary single negation.
  'if (!e.ctrlKey) { act(); }\n  if (e.key === "Escape") { act(); }',
  // The sanctioned spellings.
  "matchesChord(e, GLOBAL_CHORDS.commandBar);",
  'isCtrlShiftChord(e, "t");',
  "matchesDigitChord(e, GLOBAL_DIGIT_CHORDS.terminalFocusZone);",
];

/**
 * The limits of MECHANISM B, probed and pinned AS CLASSES.
 *
 * ## Why classes and not strings
 *
 * An earlier version pinned TEN LITERAL SNIPPETS, which cannot go red when
 * the floor MOVES — only when one of those exact strings does. What is
 * pinned here is a CLASS plus several spellings OF that class, and the
 * assertion is that every spelling agrees with its class's verdict. A
 * variant that disagrees is either a new escape or a floor that has shrunk,
 * and both have to be written down before this file goes green again.
 *
 * ## What replaced the count
 *
 * There is no `ESCAPING_CLASS_COUNT` any more. It was a remembered number,
 * it was wrong by at least six, and it passed silently. In its place:
 *
 *   1. the escaping classes are asserted BY NAME, and any count is derived
 *      from that list rather than written beside it, so the two cannot
 *      disagree;
 *   2. every spelling of every class is executed in both arms, so the floor
 *      moving in either direction is loud;
 *   3. every escaping spelling is separately asserted to be CAUGHT BY
 *      MECHANISM A (see "sees a field read in every spelling mechanism B is
 *      blind to"), which is the property that actually matters — B's
 *      escapes bound the INVENTORY, never the COVERAGE.
 */
interface ProbeClass {
  /** What the class IS — the property, not one of its spellings. */
  name: string;
  /** True when the scanner catches EVERY spelling below. */
  caught: boolean;
  /** Why it escapes. Required when `caught` is false. */
  why?: string;
  /** Several spellings of the SAME class. All must agree with `caught`. */
  spellings: string[];
}

const PROBE_CLASSES: ProbeClass[] = [
  {
    name: "event alias chain",
    caught: true,
    spellings: [
      'const t = e; const u = t; if (u.ctrlKey && u.key === "z") act();',
      'const t = e; const u = t; const v = u; if (v.ctrlKey && v.key === "z") act();',
    ],
  },
  {
    name: "key alias chain",
    caught: true,
    spellings: [
      'const k2 = e.key; const k3 = k2; if (e.ctrlKey && k3 === "z") act();',
      'const k2 = e.key; if (e.ctrlKey && k2 === "z") act();',
      'const [k4] = [e.key]; if (e.ctrlKey && k4 === "z") act();',
    ],
  },
  {
    name: "key test in a closure under a modifier guard",
    caught: true,
    spellings: [
      'if (e.ctrlKey) { const f = () => { if (e.key === "z") act(); }; f(); }',
      'if (e.ctrlKey) { [1].forEach(() => { if (e.key === "z") act(); }); }',
    ],
  },
  {
    name: "dynamic field name",
    caught: true,
    spellings: [
      'const F = "key"; e.ctrlKey && e[F] === "z";',
      'const FIELDS = ["key"]; e.ctrlKey && e[FIELDS[0]] === "z";',
    ],
  },
  {
    name: "Reflect.get",
    caught: true,
    spellings: [
      'e.ctrlKey && Reflect.get(e, "key") === "z";',
      'const F2 = "key"; e.ctrlKey && Reflect.get(e, F2) === "z";',
      '(Reflect).get(e, "key") === "z" && e.ctrlKey;',
    ],
  },
  {
    name: "computed / renamed binding property",
    caught: true,
    spellings: [
      'const { ["key"]: k } = e; e.ctrlKey && k === "z";',
      'const { key: k, ctrlKey: c } = e; if (c && k === "z") act();',
      'const onK = ({ key: k, ctrlKey: c }: KeyboardEvent) => { if (c && k === "z") act(); };',
    ],
  },
  {
    // Closed by D6 of iteration 8, extended by D6 of iteration 9.
    name: "receiver spelled around the identifier test",
    caught: true,
    spellings: [
      '(e).ctrlKey && (e).key === "z";',
      'e!.ctrlKey && e!.key === "z";',
      '(e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "z";',
      '((e))!.ctrlKey && ((e as KeyboardEvent)).key === "z";',
      'e.ctrlKey && e.nativeEvent.key === "z";',
      'e.nativeEvent.ctrlKey && e.nativeEvent.key === "z";',
      'this.ev.ctrlKey && this.ev.key === "z";',
      'evs[0].ctrlKey && evs[0].key === "z";',
      'const g = { e }; g.e.ctrlKey && g.e.key === "z";',
      'e?.ctrlKey && e?.key === "z";',
      'getEv().ctrlKey && getEv().key === "z";',
      'getEv().ctrlKey && getEv()!.key === "z";',
    ],
  },
  {
    // Also closed by iteration 8 — found by probing for a SIXTH class
    // rather than by re-reading the five already written down.
    name: "modifier and key read in an expression-bodied position",
    caught: true,
    spellings: [
      'const isUndo = (ev: KeyboardEvent) => ev.ctrlKey && ev.key === "z";',
      "const isUndo2 = (ev: KeyboardEvent) => ev.ctrlKey && /^[a-z]$/.test(ev.key);",
      'switch (true) { case e.ctrlKey: if (e.key === "z") act(); }',
      'switch (true) { case e.ctrlKey && !e.shiftKey: if (e.key === "z") act(); }',
      'switch (true) { case e.ctrlKey && e.key === "z": act(); }',
    ],
  },
  {
    name: "modifier asserted through a data table inside a nested callback",
    caught: true,
    spellings: [
      'const MODS = ["ctrlKey"]; if (MODS.every((m) => e[m])) { if (e.key === "z") act(); }',
      'const MODS = ["ctrlKey"]; if (MODS.some((m) => e[m])) { if (e.key === "z") act(); }',
      'const MODS = ["ctrlKey"]; if (MODS.filter((m) => e[m]).length > 0) { if (e.key === "z") act(); }',
    ],
  },
  {
    // D3 of iteration 9.
    name: "modifier established by a NEGATED early-return guard",
    caught: true,
    spellings: [
      'if (!e.ctrlKey) return;\n  if (e.key === "z") act();',
      'if (!e.ctrlKey) { return; }\n  if (e.key === "z") act();',
      'if (!e.metaKey) throw new Error("x");\n  if (e.key === "z") act();',
      '!!e.ctrlKey && e.key === "z";',
      '!!!!e.ctrlKey && e.key === "z";',
    ],
  },
  {
    // D4 of iteration 9.
    name: "modifier asserted inside a `case` ARM BODY",
    caught: true,
    spellings: [
      'switch (e.key) { case "k": if (e.ctrlKey) act(); break; }',
      'switch (e.key) { case "k": { if (e.metaKey) act(); break; } }',
      'switch (e.key) { case "k": if (!e.shiftKey && e.altKey) act(); break; }',
    ],
  },
  {
    // D7 of iteration 9.
    name: "destructured listener PARAMETER",
    caught: true,
    spellings: [
      'window.addEventListener("keydown", ({ key, ctrlKey }) => { if (ctrlKey && key === "z") act(); });',
      'document.addEventListener("keydown", ({ key, metaKey }) => { if (metaKey && key === "z") act(); });',
      'const onKey = ({ key, ctrlKey }: KeyboardEvent) => { if (ctrlKey && key === "z") act(); };',
      'el.addEventListener("keydown", ({ key, altKey }) => { if (altKey && key === "z") act(); });',
    ],
  },
  {
    // D9 / D14 of iteration 9.
    name: "modifier behind a predicate that is not a bare identifier",
    caught: true,
    spellings: [
      'if (this.isMod(e)) { if (e.key === "z") act(); }',
      'if (mods.isMod(e)) { if (e.key === "z") act(); }',
      'if ((isMod)(e)) { if (e.key === "z") act(); }',
      'if (isMod.call(null, e)) { if (e.key === "z") act(); }',
      'if (isMod.apply(null, [e])) { if (e.key === "z") act(); }',
    ],
  },
  {
    // D14 of iteration 9.
    name: "modifier hoisted through an array destructure or an IIFE",
    caught: true,
    spellings: [
      'const [c1] = [e.ctrlKey];\n  if (c1 && e.key === "z") act();',
      'const [, c2] = [0, e.metaKey];\n  if (c2 && e.key === "z") act();',
      'if ((() => e.ctrlKey)()) { if (e.key === "z") act(); }',
      'if ((function () { return e.altKey; })()) { if (e.key === "z") act(); }',
    ],
  },
  {
    // D15 — found by probing this rework. Iteration 9 did not list it.
    name: "modifier GATING a branch the key test already selected",
    caught: true,
    spellings: [
      'if (e.key === "z") { if (e.ctrlKey) act(); }',
      'if (e.key === "z") { if (e.metaKey && !e.shiftKey) act(); }',
      'if (e.key === "z") { while (e.altKey) act(); }',
      'if (e.key === "z") { doThing(); if (e.ctrlKey) act(); }',
    ],
  },
  {
    // D16 — found by probing this rework.
    name: "early-return guard that also has an `else` arm",
    caught: true,
    spellings: [
      'if (!e.ctrlKey) { return; } else { void 0; }\n  if (e.key === "z") act();',
      'if (!e.metaKey) { throw new Error("x"); } else { void 0; }\n  if (e.key === "z") act();',
    ],
  },
  {
    // D17 — found by probing this rework.
    name: "modifier hoisted by ASSIGNMENT rather than declaration",
    caught: true,
    spellings: [
      'let m = false; m ||= e.ctrlKey;\n  if (m && e.key === "z") act();',
      'let m2 = false; m2 = e.metaKey;\n  if (m2 && e.key === "z") act();',
      'let m3 = true; m3 &&= e.altKey;\n  if (m3 && e.key === "z") act();',
      'let k5; k5 = e.key;\n  if (e.ctrlKey && k5 === "z") act();',
    ],
  },
  {
    // D18 — found by probing this rework.
    name: "expression-container position with no statement arm",
    caught: true,
    spellings: [
      'class Probe { hit = e.ctrlKey && e.key === "z"; }',
      'class Probe2 { static hit = e.ctrlKey && e.key === "z"; }',
      'export default e.ctrlKey && e.key === "z";',
    ],
  },

  /* ── the floor ── */

  {
    name: "key test in a helper whose parameter is neither event-named nor typed",
    caught: false,
    why:
      "ESCAPES, interprocedural: the modifier is in the CALLER and the key test in " +
      "the callee, and the callee's receiver carries no evidence at all — no typed " +
      "parameter, no conventional name, no unambiguous field read. Closing it needs " +
      "a call graph. Mechanism A still sees the `key` read.",
    spellings: [
      'function h(x) { if (x.key === "z") act(); } if (e.ctrlKey) h(e);',
      'const h2 = (x) => { if (x.key === "z") act(); }; if (e.ctrlKey) h2(e);',
    ],
  },
  {
    name: "key test in a helper whose parameter IS a recognised event",
    caught: false,
    why:
      "ESCAPES, interprocedural, and STRICTLY WEAKER than the class above — the " +
      "receiver is recognised (`x: KeyboardEvent`), the key READ is counted, and only " +
      "the modifier is missing because it is asserted at the call site. This is the " +
      "refactor a reader reaches for when hoisting a chord test out of a handler, so " +
      "it is the escape most likely to be written by accident. Closing it needs the " +
      "same call graph: the guard walk would have to follow `h(e)` into `h`.",
    spellings: [
      'function h3(x: KeyboardEvent) { if (x.key === "z") act(); } if (e.ctrlKey) h3(e);',
      'const h4 = (x: KeyboardEvent) => { if (x.key === "z") act(); }; if (e.ctrlKey) h4(e);',
      'function h5(ev) { if (ev.key === "z") act(); } if (e.ctrlKey) h5(e);',
    ],
  },
  {
    name: "the key value is extracted by a helper and returned",
    caught: false,
    why:
      "ESCAPES, interprocedural: the field name is read inside the helper and only " +
      "its VALUE crosses back, so nothing at the comparison site names a key field.",
    spellings: [
      'const g = (o) => o.key; e.ctrlKey && g(e) === "z";',
      'function g2(o) { return o.key; } e.ctrlKey && g2(e) === "z";',
    ],
  },
  {
    name: "positional read with no field name anywhere",
    caught: false,
    why:
      "ESCAPES, fully dynamic: the key is addressed by POSITION in a derived " +
      "collection, so there is no field name, no receiver, and no literal for any " +
      "syntactic rule to key on. This one escapes MECHANISM A as well, and is " +
      "declared there too.",
    spellings: [
      'e.ctrlKey && Object.values(e)[3] === "z";',
      'e.ctrlKey && Object.values(e).at(3) === "z";',
      'e.ctrlKey && Object.entries(e)[3][1] === "z";',
    ],
  },
  {
    // Found by probing this rework — the residual half of D15, and NOT a
    // limit of the technique. It is a scoping decision, recorded here so
    // the next round can take it deliberately.
    name: "modifier REFINING an outcome downstream of the key test",
    caught: false,
    why:
      "ESCAPES BY CHOICE, not by limitation. The modifier is read as a VALUE inside " +
      "a branch the key test already selected, so it picks between outcomes rather " +
      "than gating one. Counting it was implemented and MEASURED rather than " +
      "assumed: it adds `shift+enter` and `shift+f3` to TerminalFindBar.tsx and " +
      "`shift+f3` to TerminalInstance.tsx — three shift-only spellings on two live " +
      "files, all arguably real claims. Widening the pinned claim set of live files " +
      "is a change to make on its own, not as a side effect of a mechanism rework, " +
      "so it is declared rather than shipped. The GATING half of the same ordering " +
      "defect IS closed — see the `modifier GATING a branch` class above. Mechanism " +
      "A rosters every file in this class anyway: they all read a modifier field.",
    spellings: [
      'if (e.key === "F3") return e.shiftKey ? "prev" : "next";',
      'if (e.key === "z") { act(e.ctrlKey); }',
      'if (e.key === "z") { const dir = e.shiftKey ? -1 : 1; act(dir); }',
    ],
  },
];

/** The escaping classes, BY NAME. Any count is derived from this. */
const DECLARED_B_ESCAPES = [
  "key test in a helper whose parameter is neither event-named nor typed",
  "key test in a helper whose parameter IS a recognised event",
  "the key value is extracted by a helper and returned",
  "positional read with no field name anywhere",
  "modifier REFINING an outcome downstream of the key test",
];

describe("F. the scanner can actually fail", () => {
  it("flags every mutation spelling as a snippet", () => {
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsInSnippet(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("flags every mutation spelling INJECTED INTO A REAL FILE", () => {
    // The host is clean on its own, so any claim comes from the probe.
    expect(scanKeyClaims(HOST_SOURCE, HOST_REL).claims).toEqual([]);
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsInRealFile(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("leaves bare-key and table-routed spellings alone", () => {
    for (const snippet of CLEAN) {
      expect(claimsInSnippet(snippet), snippet).toEqual([]);
      expect(claimsInRealFile(snippet), `in-file: ${snippet}`).toEqual([]);
    }
  });

  it("catches every spelling of every class it claims to catch", () => {
    for (const c of PROBE_CLASSES.filter((x) => x.caught)) {
      for (const snippet of c.spellings) {
        expect(claimsInSnippet(snippet).length, `${c.name}: ${snippet}`).toBeGreaterThan(0);
        expect(claimsInRealFile(snippet).length, `in-file — ${c.name}: ${snippet}`).toBeGreaterThan(
          0,
        );
      }
    }
  });

  it("escapes on every spelling of every class it admits escaping", () => {
    // The direction that matters most: a variant of a declared escape that
    // is quietly CAUGHT means the floor moved and this file did not say so.
    for (const c of PROBE_CLASSES.filter((x) => !x.caught)) {
      expect(c.why, `${c.name} must say why it escapes`).toBeTruthy();
      for (const snippet of c.spellings) {
        expect(claimsInSnippet(snippet), `${c.name}: ${snippet}`).toEqual([]);
        expect(claimsInRealFile(snippet), `in-file — ${c.name}: ${snippet}`).toEqual([]);
      }
    }
  });

  it("declares its escaping classes by NAME, with no separate count to drift", () => {
    const escaping = PROBE_CLASSES.filter((c) => !c.caught).map((c) => c.name);
    expect(escaping).toEqual(DECLARED_B_ESCAPES);
    // Derived, never written down beside the list — that is the whole
    // difference from `ESCAPING_CLASS_COUNT = 4`, which was a memory of a
    // probe rather than a probe, and was wrong by at least six.
    expect(PROBE_CLASSES.filter((c) => !c.caught)).toHaveLength(DECLARED_B_ESCAPES.length);
    // Every class carries more than one spelling, or it is a pinned string
    // wearing a class's name — the exact defect this rework replaced.
    for (const c of PROBE_CLASSES) {
      expect(c.spellings.length, `${c.name} needs >1 spelling`).toBeGreaterThan(1);
    }
  });

  it("finds key reads at all", () => {
    // Guards against the whole pass silently matching nothing — a green
    // scan of an empty set is the failure mode this file exists to avoid.
    const reads = SCANS.reduce((n, s) => n + s.scan.keyReads, 0);
    expect(SCANS.length).toBeGreaterThan(50);
    expect(reads).toBeGreaterThan(100);
  });
});

/* ── B. every claim in the tree is allowlisted, spelled out ──────────── */

describe("hand-rolled chord claims", () => {
  it("pins every modifier-qualified key claim in src/, wherever it lives", () => {
    const found: Record<string, string[]> = {};
    for (const { rel, scan } of SCANS) {
      if (scan.claims.length === 0) continue;
      found[rel] = scan.claims.map((c) => c.spelling).sort();
    }
    expect(found).toEqual(KNOWN_KEY_CLAIMS);
  });

  it("lets NO global key listener hand-roll a chord the table could own", () => {
    const offenders: string[] = [];
    for (const { rel, scan } of SCANS) {
      if (!GLOBAL_LISTENER_FILES.has(rel)) continue;
      for (const claim of scan.claims) {
        if (!claim.modifiers.some((t) => CONTROL_TAGS.has(t))) continue;
        offenders.push(`${rel}:${claim.line} claims ${claim.spelling} — ${claim.text}`);
      }
    }
    // An app-wide claim outside the table is the double-fire itself, not a
    // documentation gap: two window listeners on one target both run.
    expect(offenders).toEqual([]);
  });

  it("still selects the files whose claims the old scanner could not reach", () => {
    // scrollKeys.ts registers no listener of its own. Losing it again — by
    // a selection rule, a prefilter, or a walk that stops early — would
    // restore the exact blind spot, so its presence is asserted directly
    // rather than inferred from the equality above.
    const scrollKeys = SCANS.find((s) => s.rel === "components/terminal/scrollKeys.ts");
    expect(scrollKeys, "scrollKeys.ts must be scanned").toBeDefined();
    expect(scrollKeys?.scan.claims).toHaveLength(8);
  });
});

/* ── chord claims routed through the table, extracted from source ────── */

const spelling = (c: GlobalChord) => `ctrl+${c.shift ? "shift+" : ""}${c.key.toLowerCase()}`;

/** A digit range expands to one spelling per digit it covers. */
function digitSpellings(c: GlobalDigitChord): string[] {
  const out: string[] = [];
  for (let d = c.from; d <= c.to; d++) out.push(`ctrl+${c.shift ? "shift+" : ""}${d}`);
  return out;
}

interface Claim {
  rel: string;
  spelling: string;
  viaTable: boolean;
}

const TABLE_BY_NAME: Record<string, GlobalChord> = GLOBAL_CHORDS;
const DIGIT_TABLE_BY_NAME: Record<string, GlobalDigitChord> = GLOBAL_DIGIT_CHORDS;

/**
 * ROUTED claims — calls to the sanctioned predicates.
 *
 * Text-matched on purpose, and safely: a call to a named function has one
 * spelling, fixed by the function's name. The open-ended space that broke
 * five scanners is the HAND-ROLLED side, and that side is now the AST's
 * job. Silence here is caught by "finds the claims it is meant to police".
 */
function claimsIn(rel: string, source: string): Claim[] {
  const out: Claim[] = [];
  for (const m of source.matchAll(/matchesChord\(\s*\w+\s*,\s*GLOBAL_CHORDS\.(\w+)\s*\)/g)) {
    const chord = TABLE_BY_NAME[m[1]];
    expect(chord, `GLOBAL_CHORDS.${m[1]} is referenced by ${rel} but absent`).toBeDefined();
    out.push({ rel, spelling: spelling(chord), viaTable: true });
  }
  for (const m of source.matchAll(
    /matchesDigitChord\(\s*\w+\s*,\s*GLOBAL_DIGIT_CHORDS\.(\w+)\s*\)/g,
  )) {
    const chord = DIGIT_TABLE_BY_NAME[m[1]];
    expect(chord, `GLOBAL_DIGIT_CHORDS.${m[1]} is referenced by ${rel} but absent`).toBeDefined();
    for (const s of digitSpellings(chord)) out.push({ rel, spelling: s, viaTable: true });
  }
  for (const m of source.matchAll(
    /matchesChord\(\s*\w+\s*,\s*\{\s*key:\s*"([^"]+)"\s*,\s*shift:\s*(true|false)/g,
  )) {
    out.push({
      rel,
      spelling: spelling({ key: m[1], shift: m[2] === "true", meta: false }),
      viaTable: false,
    });
  }
  for (const m of source.matchAll(/isCtrlShiftChord\(\s*\w+\s*,\s*"([^"]+)"\s*\)/g)) {
    out.push({
      rel,
      spelling: spelling({ key: m[1], shift: true, meta: false }),
      viaTable: false,
    });
  }
  return out;
}

// The chord module itself only MENTIONS the call shapes in its docstring;
// it is the table, not a claimant.
const CLAIMS = FILES.filter((f) => !MECHANISM_FILES.has(f.rel)).flatMap((f) =>
  claimsIn(f.rel, f.source),
);

describe("chord registries", () => {
  it("finds the claims it is meant to police", () => {
    expect(CLAIMS.length).toBeGreaterThan(20);
    // The digit ranges must actually be reaching the counter — a
    // `matchesDigitChord` call that stopped being recognised would
    // silently restore the exact blind spot this table was added for.
    expect(CLAIMS.filter((c) => /^ctrl\+(shift\+)?\d$/.test(c.spelling).valueOf()).length).toBe(
      8 + 8 + 9,
    );
  });

  it("keeps every non-terminal chord claim in GLOBAL_CHORDS", () => {
    const strays = CLAIMS.filter((c) => !c.viaTable && c.rel !== TERMINAL_REGISTRY).map(
      (c) => `${c.rel} claims ${c.spelling} outside the table`,
    );
    expect(strays).toEqual([]);
  });

  it("assigns a distinct spelling to every table entry", () => {
    const spellings = Object.values(GLOBAL_CHORDS).map(spelling);
    expect(new Set(spellings).size).toBe(spellings.length);
  });

  it("has exactly the documented set of chords claimed by two files", () => {
    const byChord = new Map<string, Set<string>>();
    for (const c of CLAIMS) {
      const files = byChord.get(c.spelling) ?? new Set<string>();
      files.add(c.rel);
      byChord.set(c.spelling, files);
    }
    const shared = [...byChord.entries()]
      .filter(([, files]) => files.size > 1)
      .map(([chord]) => chord)
      .sort();
    expect(shared).toEqual(Object.keys(KNOWN_SHARED_CHORDS).sort());
  });
});

/* ── E. key-dispatch registries ──────────────────────────────────────── */

describe("key-dispatch registries", () => {
  it("has exactly the documented `switch (e.key)` registries", () => {
    const found = SCANS.filter((s) => s.scan.switchRegistry).map((s) => s.rel);
    expect(found.sort()).toEqual([...KNOWN_SWITCH_KEY_REGISTRIES].sort());
  });
});
