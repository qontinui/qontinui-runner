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
 *   **C — the claims that read NO field at all** (`findNonDomChordClaims`).
 *   A and B both START from a `KeyboardEvent` field read, so a chord handed
 *   to a library as text (`register("CommandOrControl+J")`,
 *   `useHotkeys("ctrl+j")`), claimed with Monaco's numeric constants
 *   (`addCommand(KeyMod.CtrlCmd | KeyCode.KeyJ, …)`), or claimed by the
 *   platform itself (`<button accessKey="j">`) is invisible to BOTH — and to
 *   the global-listener grade as well, since there is no registration to
 *   grade. Iteration 12 planted ELEVEN live app-wide `Ctrl+J` claimants at
 *   once and this suite stayed 33/33 green, exit 0. C1 is spelling-
 *   independent across libraries in the same way A is across comparisons:
 *   whatever the API, the chord has to be NAMED.
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
 *   G.  Every RULE ENTRY of both mechanisms is falsified by a corpus row,
 *       and deleting one reds the suite. That is not asserted here — it is
 *       `keyRules.mutation.test.ts`, which discovers the rule tables by
 *       parsing the two modules and mutates each entry out. 61 of 126 rule
 *       entries used to be deletable with this file 33/33 green.
 *
 * ## What the walk covers, and the residuals it does not
 *
 * The walk is every file Vite bundles — `.ts`, `.tsx`, `.js`, `.jsx`,
 * `.mjs`, `.cjs` under `src/`, PLUS each inline `<script>` of every
 * root-level HTML entry point as its own unit. `*.test.ts` is no longer
 * skipped. All of that was invisible before, and `index.html` — which ships
 * a live `window.addEventListener("unhandledrejection", …)` — was outside
 * the walk entirely.
 *
 * Three residuals are DECLARED rather than left to a thirteenth iteration,
 * each with a probe that asserts it still escapes:
 *
 *   R1. **A listener target this pass cannot resolve to a name** —
 *       `const t = getTarget(); t.addEventListener("keydown", h)`. Deciding
 *       it needs a call graph, and grading it global would flag every
 *       `el.addEventListener` in the tree. Bounded: mechanism A still
 *       rosters the file and B still inventories its claims; only the
 *       app-wide GRADE is lost. Probed by
 *       "declares the listener target it cannot resolve".
 *   R2. **A chord string assembled at runtime** — `register("Ctrl" + "+J")`.
 *       No literal is a whole chord spelling, so C1 cannot fire. Same class
 *       as mechanism A's declared escape 1.
 *   R3. **A numeric keybinding API under an unrostered name.** C3 is
 *       enumerative and says so; C1 catches any library that takes its chord
 *       as TEXT, which is what bounds R3 to numeric-constant APIs alone.
 *       Every C3 entry is falsified by `keyRules.mutation.test.ts`, so the
 *       list cannot rot — it just cannot anticipate.
 *
 * R2 and R3 are probed by "escapes a chord assembled at runtime, and an
 * unrostered numeric API".
 *
 * `environment: "node"` vitest, so `fs` is available; same precedent as
 * `terminal/useKeyboardShortcuts.chords.test.ts`.
 */

import {
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "fs";
import { tmpdir } from "os";
import { join, relative, resolve } from "path";

import * as ts from "typescript";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  GLOBAL_CHORDS,
  GLOBAL_DIGIT_CHORDS,
  type GlobalChord,
  type GlobalDigitChord,
} from "./globalChords";
import { CONTROL_TAGS, scanKeyClaims, scanKeyClaimsIn, type FileScan } from "./keyClaimScan";
import {
  COULD_CLAIM_A_CHORD,
  findKeyFieldReads,
  findNonDomChordClaims,
  hasGlobalKeyListener,
  type KeyFieldReads,
  type NonDomChordClaims,
  parseSource,
} from "./keyFieldReads";

const SRC = resolve(__dirname, "..");

/**
 * Where the DISK arm of the mutation matrix writes its probe files.
 *
 * A TEMP root with its own `src/` inside it, and NOT the live `src/` tree.
 *
 * The arm used to write into the real `src/` — "so the probe is rediscovered
 * by the same walk the roster properties use". The walk is the same; the
 * ROOT does not have to be, and writing into a live tree made this suite a
 * flake generator for every sibling suite that also walks `src/`. Under
 * `vitest run` with parallel workers a neighbouring scanner hit
 * `ENOENT: statSync 'src\__chord_enforcement_probe__'` — a directory that
 * existed when `readdirSync` listed it and was gone by the time `statSync`
 * asked about it. An intermittently-red enforcement suite is how a REAL red
 * gets ignored.
 *
 * What the arm still proves is what it was always for: the same
 * `discoverUnits` code path, the same extension set, the same prefilter, the
 * same parse and the same two mechanisms, over a file that was WRITTEN and
 * then READ BACK rather than concatenated in memory. What it no longer proves
 * on its own is that the walk is rooted at the real `src/` — and that is
 * proven directly, and better, by the roster properties, which walk the real
 * tree and pin every file in it by name.
 */
const PROBE_ROOT = mkdtempSync(join(tmpdir(), "chord-enforcement-probe-"));
const PROBE_SRC = join(PROBE_ROOT, "src");
const PROBE_DIR_REL = "__chord_enforcement_probe__";
const PROBE_DIR = join(PROBE_SRC, PROBE_DIR_REL);

/** Remove the probe directory. Idempotent, and safe when it never existed. */
function sweepProbeDir(): void {
  rmSync(PROBE_DIR, { recursive: true, force: true });
}

sweepProbeDir();

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
  "lib/globalChords.enforcement.test.ts":
    "THIS FILE. Its mutation corpus names every modifier field in string " +
    "literals, so R4/R5 roster it — deliberately. The walk stopped skipping " +
    "`*.test.ts` because skipping them is what hid a whole file class; the " +
    "honest consequence is that the enforcement corpus appears on its own " +
    "roster rather than being exempted by a filename pattern.",
  "lib/globalChords.test.ts":
    "the chord table's own unit test: it constructs `KeyboardEvent`-shaped " +
    "fixtures (`{ ctrlKey, shiftKey, metaKey, key }`) to exercise " +
    "`matchesChord`. Rostered as a claimant of nothing — its predicate calls " +
    "are pinned separately in CHORD_PREDICATE_HARNESSES.",
  "lib/keyRules.mutation.test.ts":
    "the rule-falsification harness. Its corpus names every modifier field in " +
    "string literals, for the same reason this file does.",
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
 *
 * Seventeen `*.test.ts` files are on this tier since the walk stopped
 * skipping them. That is not noise to be filtered back out: a test file reads
 * `key` for the same reasons app code does, and one that grows a chord claim
 * is a claimant like any other. The class was skipped, and skipping it is
 * what let `index.html` and every `.js` file be skipped too.
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
  "components/settings/performanceCapsConfig.test.ts",
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
  "components/terminal/approveAll.test.ts",
  "components/terminal/approveAll.ts",
  "components/terminal/backends/webglContextLru.ts",
  "components/terminal/commands/bind.ts",
  "components/terminal/commands/corpus.test.ts",
  "components/terminal/commands/corpus.testkit.ts",
  "components/terminal/commands/differential.testkit.ts",
  "components/terminal/commands/handlers.test.ts",
  "components/terminal/commands/interpret.test.ts",
  "components/terminal/commands/parse.ts",
  "components/terminal/commands/pipeline.testkit.ts",
  "components/terminal/commands/registeredActions.test.ts",
  "components/terminal/commands/spawnVerdict.test.ts",
  "components/terminal/commands/uibridge.test.ts",
  "components/terminal/commands/uibridge.ts",
  "components/terminal/commands/usePromptLibraryCommands.test.ts",
  "components/terminal/commands/useTerminalCommands.ts",
  "components/terminal/commands/verdict.test.ts",
  "components/terminal/commands/verdict.ts",
  "components/terminal/result-card/ResultCardMount.tsx",
  "components/terminal/resumeVerification.ts",
  "components/terminal/suggestions/useSuggestions.tsx",
  "components/terminal/terminalKeySequence.test.ts",
  "components/terminal/terminalKeySequence.ts",
  "components/terminal/terminalWriteResult.test.ts",
  "components/terminal/terminalWriteResult.ts",
  "components/terminal/useKeyboardShortcuts.ts",
  "components/terminal/useMidSessionProbe.ts",
  "components/terminal/useSessionInfo.test.ts",
  "components/terminal/writePtyById.test.ts",
  "components/terminal/writeWhenReady.test.ts",
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
  "hooks/ui-bridge-events/recoveryScope.test.ts",
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
  "lib/ui-bridge/actionSurfaces.ts",
  "lib/workflow-builder/buildSpecWorkflow.ts",
  "pages/specs/ApiOverview.tsx",
  "pages/specs/ConnectionBar.tsx",
  "pages/ui-bridge-integration/DiscoveryPanel.tsx",
  "pages/ui-bridge-integration/ProjectCoordinator.tsx",
  "specs/specs.compile.test.ts",
  "utils/ExecutionTreeManager.ts",
];

/**
 * TIER C — every file that claims a chord WITHOUT reading a keyboard-event
 * field: a chord spelling handed to a library, Monaco's numeric keybinding
 * constants, or the platform's own `accessKey`.
 *
 * Mechanisms A and B both start from a `KeyboardEvent` field read, so this
 * whole class was invisible to them AND to the global-listener grade (there
 * is no `addEventListener` to grade). Iteration 12 planted ELEVEN live
 * app-wide `Ctrl+J` claimants at once — a Monaco `addCommand`, a hotkeys
 * library, a Tauri `register`, a `<button accessKey>` — and the suite stayed
 * 33/33 green, exit 0.
 *
 * Every entry today is DOCUMENTATION: a shortcut label rendered in an
 * overlay, a palette row, a legend. That is the expected shape of this
 * roster in a repo whose real chords go through `GLOBAL_CHORDS`, and it is
 * exactly why the roster is worth having — the first entry that is a
 * BINDING rather than a label is a one-line diff a reviewer cannot miss.
 */
const NON_DOM_CHORD_ROSTER: Record<string, string> = {
  "components/terminal/CommandPalette.tsx":
    'the palette\'s own rows carry a `shortcut: "Ctrl+Shift+P"` LABEL for display. ' +
    "The binding itself lives in useKeyboardShortcuts.ts and goes through the table.",
  "components/terminal/KeyboardShortcutsOverlay.tsx":
    "the shortcut cheat-sheet — 37 chord spellings rendered as help text. Documentation " +
    "of the table, not a second claimant of it.",
  "lib/globalChords.enforcement.test.ts":
    "THIS FILE. Its allowlists spell chords (`ctrl+shift+g`, `shift+pageup`) as the " +
    "inventory it pins. Scanned rather than exempted, for the same reason it is on the " +
    "tier-1 roster.",
  "lib/ui-bridge/UIBridgeHooks.tsx":
    "the UI Bridge's shortcut inventory, exposed to the bridge as `combo` strings so an " +
    "external driver can name a chord. Description of the table, not a binding.",
  "pages/state-machine/UIBridgeStateGraph.tsx":
    '`["Move element", "Alt+Drag"]` in the graph\'s on-screen legend. A pointer ' +
    "modifier described in prose, and not a key chord at all.",
};

/**
 * Files whose calls to the chord PREDICATES exercise the predicate rather
 * than claim the chord.
 *
 * Since the walk stopped skipping `*.test.ts`, `globalChords.test.ts` and
 * `useKeyboardShortcuts.chords.test.ts` are scanned like any other source —
 * and they call `matchesChord`, `matchesDigitChord` and `isCtrlShiftChord`
 * for real, against constructed fixtures. Those calls are not surfaces: no
 * listener routes to them and no press reaches them.
 *
 * This is an exemption, and it is ENUMERATED rather than inferred from a
 * filename pattern — which is the difference between "the class is scanned
 * and two files are named" and "the class is skipped". A NEW test file that
 * calls a predicate is not on this roster, so its claims land in `strays`
 * and go red. Each entry is checked below to still produce at least one
 * claim, so a stale one cannot sit here silently.
 */
const CHORD_PREDICATE_HARNESSES: Record<string, string> = {
  "components/terminal/useKeyboardShortcuts.chords.test.ts":
    'asserts `isCtrlShiftChord` returns true for a constructed `{ key: "b", ctrlKey, ' +
    "shiftKey }` fixture. Exercises the predicate; claims nothing.",
  "lib/globalChords.test.ts":
    "the chord table's unit test — calls all three predicates against constructed " +
    "events to pin their semantics. Exercises the predicates; claims nothing.",
};

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

/**
 * Every extension Vite actually bundles into the app.
 *
 * The old walk was `/\.tsx?$/` and additionally skipped `*.test.tsx?`, so
 * `.js`, `.jsx`, `.mjs` and `.cjs` were invisible to the whole mechanism
 * though Vite bundles all four, and every test file was invisible too.
 * Iteration 12 measured a real keydown listener added in an invisible class
 * leaving the suite green.
 *
 * Test files are no longer skipped, and that is the deliberate half. The
 * enforcement corpus is scanned like any other source — the alternative,
 * re-excluding the class because its own fixtures are noisy, is the move that
 * produced this blind spot. What makes re-including them SAFE is a change of
 * mechanism, not a filter: {@link claimsIn} now reads the AST instead of the
 * text, so a `matchesChord(…)` inside a string FIXTURE is a string and not a
 * call. String-literal fixtures are exempt BY CONSTRUCTION. The two files
 * that call the predicates for real from a test are rostered, with notes, in
 * {@link CHORD_PREDICATE_HARNESSES}.
 */
const BUNDLED_SOURCE = /\.(?:[cm]?[jt]sx?)$/;

/**
 * Vite's HTML entry points live at the REPO ROOT, outside `src/`.
 *
 * `index.html` was outside the walk entirely, and it is not an empty shell:
 * it ships two inline `<script>` blocks, one of which already registers
 * `window.addEventListener("unhandledrejection", …)`. A real keydown listener
 * added beside it left the suite green — an app-wide claimant in the one file
 * that runs before any bundle does.
 */
const REPO_ROOT = resolve(SRC, "..");

/** One scannable unit: a source file, or ONE inline `<script>` of an HTML entry. */
interface SourceUnit {
  /** `components/x.tsx`, or `index.html#script2`. */
  rel: string;
  /** The name handed to the parser — it, and only it, selects the script kind. */
  parseName: string;
  source: string;
}

/**
 * True for the one error a walk over a LIVE tree must survive, and no other.
 *
 * A sibling suite writing and deleting its own scratch files under `src/`
 * makes a directory exist for `readdirSync` and vanish before `statSync`.
 * Dying on that turns every concurrent run into a coin flip. Anything else —
 * a permission fault above all — means the walk saw LESS than the tree holds,
 * and a coverage mechanism that quietly saw less is the exact failure this
 * whole file exists to prevent, so it is rethrown.
 */
function isMissing(err: unknown): boolean {
  return (err as NodeJS.ErrnoException | undefined)?.code === "ENOENT";
}

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  let entries: string[];
  try {
    entries = readdirSync(dir);
  } catch (err) {
    if (isMissing(err)) return out;
    throw err;
  }
  for (const entry of entries) {
    const full = join(dir, entry);
    let isDir: boolean;
    try {
      isDir = statSync(full).isDirectory();
    } catch (err) {
      if (isMissing(err)) continue;
      throw err;
    }
    if (isDir) {
      if (entry === "node_modules") continue;
      out.push(...sourceFiles(full));
      continue;
    }
    if (BUNDLED_SOURCE.test(entry) || /\.html$/.test(entry)) out.push(full);
  }
  return out;
}

/** The HTML files Vite treats as entry points — root-level, not recursive. */
function htmlEntryPoints(root: string): string[] {
  return readdirSync(root)
    .map((entry) => join(root, entry))
    .filter((path) => {
      if (!/\.html$/.test(path)) return false;
      try {
        return statSync(path).isFile();
      } catch (err) {
        if (isMissing(err)) return false;
        throw err;
      }
    });
}

/**
 * Every inline `<script>` body of an HTML file, in document order.
 *
 * A `<script src=…>` carries no inline body and is skipped — its content is
 * a source file the walk already found. The INDEX is kept from the document
 * rather than from the kept list, so `#script2` names the second `<script>`
 * tag whether or not the first one had a body.
 */
const SCRIPT_BLOCK = /<script\b[^>]*>([\s\S]*?)<\/script>/gi;

function inlineScripts(rel: string, text: string): SourceUnit[] {
  const out: SourceUnit[] = [];
  SCRIPT_BLOCK.lastIndex = 0;
  let m: RegExpExecArray | null;
  let index = 0;
  while ((m = SCRIPT_BLOCK.exec(text)) !== null) {
    index++;
    if (m[1].trim().length === 0) continue;
    // `.ts`, never `.tsx`: an inline block is plain JavaScript, and a TSX
    // parse reads `a < b` as the start of a JSX element.
    out.push({ rel: `${rel}#script${index}`, parseName: `${rel}.script${index}.ts`, source: m[1] });
  }
  return out;
}

function toUnits(path: string, srcRoot: string, htmlRoot: string): SourceUnit[] {
  const abs = resolve(path);
  const rel = (abs.startsWith(resolve(srcRoot)) ? relative(srcRoot, abs) : relative(htmlRoot, abs))
    .split("\\")
    .join("/");
  let source: string;
  try {
    source = readFileSync(abs, "utf8");
  } catch (err) {
    // Same rule as the walk: a file that vanished between the listing and the
    // read belongs to a concurrent writer, not to this tree. Anything else is
    // rethrown, because a file this walk cannot read is coverage it does not
    // have and must not claim.
    if (isMissing(err)) return [];
    throw err;
  }
  if (/\.html$/.test(rel)) return inlineScripts(rel, source);
  // `.jsx` is JSX and must parse as such; `.js`, `.mjs` and `.cjs` are not.
  return [{ rel, parseName: rel.replace(/\.jsx$/, ".tsx"), source }];
}

/**
 * The walk, as ONE function — the roster pass and the disk arm share it, and
 * differ only in the ROOT they are pointed at.
 */
function discoverUnits(srcRoot: string = SRC, htmlRoot: string = REPO_ROOT): SourceUnit[] {
  return [...sourceFiles(srcRoot), ...htmlEntryPoints(htmlRoot)].flatMap((p) =>
    toUnits(p, srcRoot, htmlRoot),
  );
}

const FILES: SourceUnit[] = discoverUnits();

/**
 * Cheap gate on which files are PARSED — now DERIVED from the rule tables
 * rather than written out beside them.
 *
 * The hand-written predecessor was
 * `/\b(?:…|keydown|keyup|keypress)\b/`, and `\bkeydown\b` does not match
 * inside `onkeydown`, nor `onKeyDown` in any case. So
 * `window.onkeydown = (ev) => { if (isChord(ev as KeyboardEvent, "Ctrl+J")) act(); }`
 * — an app-wide `Ctrl+J` claimant — was NEVER PARSED AT ALL, while a passing
 * unit test in this very file asserted `listens("window.onkeydown = h;")`
 * returns true. A rule the pipeline can never feed is a FAKE falsification:
 * the rule is right, the test is right, and the two never meet. The fix
 * belongs in the pipeline, so the pattern moved into `keyFieldReads.ts` where
 * it is built from `MODIFIER_FIELDS`, `AMBIGUOUS_KEY_FIELDS`, the event names
 * and mechanism C's own tables — a new entry in any of them widens the gate
 * with no edit here, which is the only way this drift does not recur.
 */
const COULD_READ_A_KEY = COULD_CLAIM_A_CHORD;

/**
 * Parse each candidate ONCE and hand the tree to both mechanisms.
 *
 * A second parse per mechanism would have doubled the suite's dominant
 * cost — parsing is ~90% of its runtime, not the walking.
 */
const PARSED = FILES.filter(
  (f) => !MECHANISM_FILES.has(f.rel) && COULD_READ_A_KEY.test(f.source),
).map((f) => {
  const sf = parseSource(f.source, f.parseName);
  return { rel: f.rel, sf, reads: findKeyFieldReads(sf), nonDom: findNonDomChordClaims(sf) };
});

/** Files mechanism C detects claiming a chord with NO keyboard field read. */
const DETECTED_NON_DOM = PARSED.filter(
  (p) =>
    p.nonDom.chordStrings.length + p.nonDom.accessKeys.length + p.nonDom.keybindingApis.length > 0,
).map((p) => p.rel);

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

const HOST_REL = TERMINAL_REGISTRY;

/**
 * The host file for the injected arm, resolved FAIL-CLOSED.
 *
 * This used to be `FILES.find(…)?.source ?? ""`, and that `?? ""` is
 * iteration 11's D3. Renaming or moving the host silently produced an EMPTY
 * host, which made "injected into a real file" a byte-for-byte copy of the
 * bare-snippet arm — and the anti-vacuity guard that was supposed to notice
 * (`expect(scanKeyClaims(HOST_SOURCE, HOST_REL).claims).toEqual([])`) passes
 * on `""`, so the suite stayed green while one of its two arms was gone.
 *
 * A missing host is now a collection error: the file goes red, loudly, with
 * the path to fix.
 */
function resolveHostSource(): string {
  const entry = FILES.find((f) => f.rel === HOST_REL);
  if (!entry || entry.source.trim().length === 0) {
    throw new Error(
      `the mutation matrix cannot run: host file "${HOST_REL}" was not found by the ` +
        `src/ walk (renamed, moved, or emptied). Do NOT let this degrade to an empty ` +
        `host — that turns the injected arm into a second copy of the snippet arm and ` +
        `leaves the suite green with one arm missing. Point HOST_REL at the new path.`,
    );
  }
  return entry.source;
}

const HOST_SOURCE = resolveHostSource();

/* ── the DISK arm ────────────────────────────────────────────────────── */

/**
 * What the REAL pipeline says about one probe, after it has been written to
 * disk and rediscovered by the walk.
 *
 * Every field here is produced by the same code path the roster properties
 * run: `sourceFiles` found the file, `readFileSync` read it back, the
 * `COULD_READ_A_KEY` prefilter graded it, `parseSource` parsed it, and both
 * mechanisms ran on the resulting tree. Nothing is concatenated in memory.
 */
interface DiskVerdict {
  /** The walk found the probe. False is a bug in the harness, not a verdict. */
  discovered: boolean;
  /**
   * The name the walk gave the unit it graded.
   *
   * For an HTML probe this is `…/probe3.html#script1`, which is the evidence
   * that an inline `<script>` block is a scannable unit and not a file the
   * walk merely opened.
   */
  rel: string;
  /** The prefilter that guards `PARSED` admitted the BARE probe file. */
  prefiltered: boolean;
  /** Mechanism A on the BARE probe file — the coverage half. */
  reads: KeyFieldReads;
  /** Mechanism A's listener detector on the BARE probe file. */
  globalListener: boolean;
  /**
   * Mechanism B on the BARE probe file, gated EXACTLY as `SCANS` gates it:
   * `null` when the file reads no key field, because that is precisely the
   * case in which the real pipeline never scans it. D2 is that `null`.
   */
  bareScan: FileScan | null;
  /** Mechanism B on the probe INJECTED INTO THE REAL HOST, off disk. */
  claims: string[];
  /** Whether the probe imports the chord table — see {@link ListenerGrade}. */
  routesThroughChordTable: boolean;
  /** Mechanism C on the probe — a chord claimed with no key field read. */
  nonDom: NonDomChordClaims;
}

/** One probe: an id to look the verdict up by, and the snippet to write. */
interface DiskProbe {
  id: string;
  /**
   * `"injected"` writes `HOST_SOURCE` + the snippet in a probe function, and
   * is the arm for mechanism B — a claim has to survive a whole real module
   * around it. `"bare"` writes the snippet alone, and is the arm for
   * mechanism A — injecting into a host that reads six modifier fields would
   * swamp the very signal ("this file reads NO key field") being measured.
   * `"raw"` writes the id VERBATIM, which is the only way to probe a file
   * class whose text is not TypeScript at all — an HTML entry point.
   */
  kind: "injected" | "bare" | "raw";
  /**
   * The extension to write the probe under. `.ts` by default; `.js` and
   * `.html` are what prove the widened walk actually reaches those classes
   * rather than merely listing them in a regex.
   */
  ext?: string;
}

/** A probe is addressed by BOTH its snippet and which body was written. */
function probeKey(kind: DiskProbe["kind"], id: string): string {
  return `${kind} :: ${id}`;
}

/* ── the global-listener GRADE (D2) ──────────────────────────────────── */

/**
 * One file, as the global-listener property grades it.
 *
 * `scan` is `null` exactly when the real pipeline never scanned the file —
 * i.e. mechanism A found no key field in it, so it never entered `SCANS`.
 * That `null` is D2: the old property iterated `SCANS`, and so could not
 * see this row at all.
 */
interface ListenerGrade {
  rel: string;
  globalListener: boolean;
  scan: FileScan | null;
  /** The file imports `lib/globalChords`, the one sanctioned hoist target. */
  routesThroughChordTable: boolean;
}

/**
 * True when a file imports the chord table.
 *
 * A textual rule on the IMPORT, which — like a listener registration, and
 * unlike a claim — has a shape fixed by the language rather than by the
 * author's taste. Matches `@/lib/globalChords`, `./globalChords`,
 * `../../lib/globalChords`: the tail is what identifies the module.
 */
function importsChordTable(source: string): boolean {
  return /from\s*["'][^"']*\bglobalChords["']/.test(source);
}

/**
 * Every way a GLOBAL key listener can claim a chord the table should own.
 *
 * Iterates LISTENERS, not scans — that inversion is the D2 fix. A global key
 * listener that reads no key field lands on neither roster and so never
 * reached `SCANS`; the property that was supposed to be the strictest in
 * this file simply did not look at it, and a brand-new app-wide
 * `Ctrl+Shift+J` claimant was invisible to the whole mechanism with the
 * suite 27/27 green.
 *
 * The acquittal for a listener with no key read is DERIVED, not rostered:
 * the file must route its chord through `GLOBAL_CHORDS`, the one hoist
 * target properties C and D already pin. A hand-maintained allowlist would
 * have gone stale in exactly the way every roster before it did.
 */
function globalListenerOffenders(graded: readonly ListenerGrade[]): string[] {
  const out: string[] = [];
  for (const f of graded) {
    if (!f.globalListener) continue;
    if (f.scan === null) {
      if (f.routesThroughChordTable) continue;
      out.push(
        `${f.rel} registers a GLOBAL key listener but reads no key field, so mechanism B ` +
          `never ran on it and its chord claims — if any — are INVISIBLE. Either hoist the ` +
          `key test into GLOBAL_CHORDS (the one sanctioned target, which properties C and D ` +
          `pin), or bring the field read back into this file so the inventory can see it.`,
      );
      continue;
    }
    for (const claim of f.scan.claims) {
      if (!claim.modifiers.some((t) => CONTROL_TAGS.has(t))) continue;
      out.push(`${f.rel}:${claim.line} claims ${claim.spelling} — ${claim.text}`);
    }
  }
  return out;
}

/** Mechanism B's scan per rostered file, for the grade above. */
const SCAN_BY_REL = new Map(SCANS.map((s) => [s.rel, s.scan]));

/** Every parsed file, graded. Built from the SAME pass the rosters use. */
const LISTENER_GRADES: ListenerGrade[] = PARSED.map((p) => ({
  rel: p.rel,
  globalListener: GLOBAL_LISTENER_FILES.has(p.rel),
  scan: SCAN_BY_REL.get(p.rel) ?? null,
  routesThroughChordTable: importsChordTable(FILES.find((f) => f.rel === p.rel)?.source ?? ""),
}));

/** key → verdict. Filled by the file-level `beforeAll`; empty before it runs. */
const DISK = new Map<string, DiskVerdict>();

/** The verdict for a probe, or a failure that names the missing registration. */
function diskVerdict(kind: DiskProbe["kind"], id: string): DiskVerdict {
  const v = DISK.get(probeKey(kind, id));
  if (!v) {
    throw new Error(
      `no ${kind} disk verdict for probe ${JSON.stringify(id)} — every snippet probed ` +
        `through the disk arm must reach DISK_PROBES, or the arm silently does not run ` +
        `for it, which is the whole defect this arm replaced.`,
    );
  }
  if (!v.discovered) {
    throw new Error(`probe ${JSON.stringify(id)} was written to disk but the walk missed it`);
  }
  return v;
}

/** Mechanism B's verdict on a snippet injected into the real host, off disk. */
function claimsOnDisk(snippet: string): string[] {
  return diskVerdict("injected", snippet).claims;
}

/** Mechanism A's verdict on a snippet written to disk as its own file. */
function readsOnDisk(snippet: string): KeyFieldReads {
  return diskVerdict("bare", snippet).reads;
}

/** The probe body for the injected arm. */
function injectedBody(snippet: string): string {
  return (
    `${HOST_SOURCE}\n` +
    `function __mutationProbe(e: KeyboardEvent, act: () => void) {\n  ${snippet}\n  void act;\n}\n`
  );
}

/**
 * The probe body for the bare arm — the snippet at TOP LEVEL.
 *
 * Not wrapped in a function: several spellings are `import` declarations and
 * `export default`, which are illegal inside one, and arm 1 already parses
 * these snippets at top level. Wrapping would have measured a different
 * parse. `e` and `act` are declared so the file reads as plausible source;
 * neither name affects any rule, and neither word reaches the prefilter.
 */
function bareBody(snippet: string): string {
  return (
    "declare const e: KeyboardEvent;\n" +
    "declare function act(...args: unknown[]): void;\n" +
    `${snippet}\n`
  );
}

/**
 * Write every probe to disk, REDISCOVER them through the actual file walk,
 * and record what both mechanisms say about each.
 *
 * One walk for the whole batch rather than one per probe: the walk is the
 * expensive half and the probes do not interact, so batching costs nothing in
 * fidelity. The files exist only for the duration of this call — they are
 * removed in the `finally` before any test runs, so a failure inside the walk
 * cannot leave `src/` dirty.
 */
function runDiskProbes(probes: readonly DiskProbe[]): void {
  mkdirSync(PROBE_DIR, { recursive: true });
  try {
    const byRel = new Map<string, DiskProbe>();
    probes.forEach((probe, i) => {
      // `.ts`, never `.tsx`, unless the probe asks otherwise: `parseSource`
      // picks ScriptKind off the extension, and TSX parses `<T>` as JSX. The
      // host is a `.ts` module, so a `.tsx` probe would measure a DIFFERENT
      // parse of the same text.
      const rel = `${PROBE_DIR_REL}/probe${i}${probe.ext ?? ".ts"}`;
      byRel.set(rel, probe);
      const body =
        probe.kind === "injected"
          ? injectedBody(probe.id)
          : probe.kind === "bare"
            ? bareBody(probe.id)
            : probe.id;
      writeFileSync(join(PROBE_SRC, rel), body, "utf8");
    });

    // THE ACTUAL WALK — the same function, from the same root, that built
    // `FILES`. If a probe is not in its output, the harness is lying. An HTML
    // probe arrives here as one unit per inline `<script>`, named
    // `…/probeN.html#script1`, so the key strips that suffix to find it.
    for (const unit of discoverUnits(PROBE_SRC, PROBE_ROOT)) {
      const probe = byRel.get(unit.rel.replace(/#script\d+$/, ""));
      if (!probe) continue;
      const source = unit.source;
      const prefiltered = !MECHANISM_FILES.has(unit.rel) && COULD_READ_A_KEY.test(source);
      const sf = parseSource(source, unit.parseName);
      const reads = findKeyFieldReads(sf);
      const rostered = prefiltered && (reads.modifier.length > 0 || reads.ambiguous.length > 0);
      const scan = rostered ? scanKeyClaimsIn(sf) : null;
      DISK.set(probeKey(probe.kind, probe.id), {
        discovered: true,
        rel: unit.rel,
        prefiltered,
        reads,
        globalListener: prefiltered && hasGlobalKeyListener(sf),
        bareScan: probe.kind === "bare" ? scan : null,
        claims: probe.kind === "injected" && scan ? scan.claims.map((c) => c.spelling) : [],
        routesThroughChordTable: importsChordTable(source),
        nonDom: findNonDomChordClaims(sf),
      });
    }
  } finally {
    sweepProbeDir();
  }
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

/**
 * Mechanism A's OWN limits, probed rather than asserted.
 *
 * A ban on reading a NAME can only be escaped by never naming the field.
 * Each is written down with spellings, and each spelling is checked to
 * actually escape — a floor that shrinks goes red here, the same as one
 * that grows.
 *
 * Module scope rather than inside the describe, so the disk arm can write
 * these spellings to disk with everything else in one batch.
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
      "No literal equal to the field name exists anywhere, and no reference position " +
      "for R5 to anchor on either, so neither string rule can fire. Closing it needs " +
      "constant folding.",
    seen: "none",
    spellings: [
      'const C = "ctrl" + "Key"; if (e[C]) act();',
      'const K = "ke" + "y"; if (e[K] === "z") act();',
      `eval("e." + "ctrl" + "Key");`,
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
      "tier 2 exists to bound: the file is named, its claim is not inventoried. " +
      "NOTE the bound this class states about itself is false when BOTH halves are " +
      "hoisted — see D2 and `globalListenerOffenders`, which grades that case off the " +
      "listener rather than off the roster.",
    seen: "ambiguous",
    spellings: [
      'import { isMod } from "./m";\nif (isMod(e) && e.key === "z") act();',
      'import { isMod } from "./m";\nif (isMod(e)) { if (e.key === "z") act(); }',
    ],
  },
];

/**
 * Classes iteration 11 measured ESCAPING and this round CLOSED.
 *
 * They are kept as probes rather than deleted with the fix, because a rule
 * nothing exercises is a rule that can vanish silently — the same reason the
 * caught half of mechanism B's matrix exists. Each entry names the field the
 * read must be attributed to.
 */
const MECHANISM_A_CLOSED: Array<{
  name: string;
  why: string;
  spellings: Array<[string, string]>;
}> = [
  {
    name: "field read as a bare identifier inside a `with` body",
    why:
      "`with (e) { if (ctrlKey && key === 'z') … }` puts the field names in the " +
      "source in READ position with no receiver, no access node and no literal, so " +
      "every one of R1–R5 was blind to it. R6 treats every identifier in a `with` " +
      "body as a potential read.",
    spellings: [
      ['with (e) { if (ctrlKey && key === "z") act(); }', "ctrlKey"],
      ['with (e) { if (ctrlKey && key === "z") act(); }', "key"],
      ['with (ev) { if (metaKey) { switch (key) { case "z": act(); } } }', "metaKey"],
      ["with (e) { if (getModifierState('Control')) act(); }", "getModifierState"],
    ],
  },
  {
    name: "field name inside a LARGER string literal",
    why:
      "R4 tests literal EQUALITY, so `eval('e.ctrlKey')` and " +
      "`new Function('e','return e.ctrlKey')(e)` named the field and were missed. " +
      "This is not the assembled-at-runtime class: the name is right there in one " +
      "piece. R5 matches it in a field-reference position only.",
    spellings: [
      [`eval('e.ctrlKey');`, "ctrlKey"],
      [`new Function('e', 'return e.ctrlKey')(e);`, "ctrlKey"],
      [`eval('e["altKey"]');`, "altKey"],
      [`eval('e.which === 90');`, "which"],
      ["eval(`e.shiftKey && x`);", "shiftKey"],
    ],
  },
];

/**
 * The BOUND on R5, as spellings.
 *
 * R5 matches a field REFERENCE inside a string, not a word. Every one of
 * these reaches the parser (the prefilter matches `which` / `key` / `code`)
 * and must still read as nothing, or the chord-relevant tier-1 roster fills
 * with every file that contains ordinary English.
 */
const R5_PROSE: string[] = [
  'const s = "the zone which is focused";',
  'const t = "press the key to continue";',
  'const u = "a code review, which is prose";',
];

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
        for (const [arm, reads] of [
          ["snippet", fieldReadsInSnippet(snippet)],
          ["on disk", readsOnDisk(snippet)],
        ] as const) {
          expect(
            reads.modifier.length + reads.ambiguous.length,
            `B misses and A must not (${arm}): ${c.name}: ${snippet}`,
          ).toBeGreaterThan(0);
        }
      }
    }
  });

  it("escapes on every spelling of every class mechanism A admits escaping", () => {
    for (const c of MECHANISM_A_ESCAPES) {
      expect(c.why.length, `${c.name} must say why`).toBeGreaterThan(20);
      expect(c.spellings.length, `${c.name} needs >1 spelling`).toBeGreaterThan(1);
      for (const snippet of c.spellings) {
        // BOTH ARMS. The second is a file written to disk and rediscovered
        // by the walk, so the prefilter and the real reader participate.
        for (const [arm, reads] of [
          ["snippet", fieldReadsInSnippet(snippet)],
          ["on disk", readsOnDisk(snippet)],
        ] as const) {
          expect(
            reads.modifier,
            `${c.name} must read no modifier field (${arm}): ${snippet}`,
          ).toEqual([]);
          if (c.seen === "none") {
            expect(reads.ambiguous, `${c.name} (${arm}): ${snippet}`).toEqual([]);
          } else {
            expect(
              reads.ambiguous.length,
              `${c.name} must stay tier-2 visible (${arm}): ${snippet}`,
            ).toBeGreaterThan(0);
          }
        }
      }
    }
  });

  it("catches every spelling of the classes iteration 11 CLOSED", () => {
    for (const c of MECHANISM_A_CLOSED) {
      expect(c.why.length, `${c.name} must say why`).toBeGreaterThan(20);
      expect(c.spellings.length, `${c.name} needs >1 spelling`).toBeGreaterThan(1);
      for (const [snippet, field] of c.spellings) {
        // A closed class the PREFILTER skips is not closed — the real
        // pipeline would never parse the file. R5 and R6 both still require
        // the field's name in the text, so `COULD_READ_A_KEY` stays sound;
        // this is where that soundness is checked rather than argued.
        expect(diskVerdict("bare", snippet).prefiltered, `prefiltered: ${snippet}`).toBe(true);
        for (const [arm, reads] of [
          ["snippet", fieldReadsInSnippet(snippet)],
          ["on disk", readsOnDisk(snippet)],
        ] as const) {
          expect(
            [...reads.modifier, ...reads.ambiguous],
            `${c.name} (${arm}): ${snippet}`,
          ).toContain(field);
        }
      }
    }
  });

  it("does not roster prose that merely contains a field name", () => {
    // The bound on R5. It matches a field REFERENCE inside a string, not a
    // word — `which` is ordinary English, and a bare-word rule would put
    // every file with prose in it on the chord-relevant tier-1 roster.
    for (const snippet of R5_PROSE) {
      for (const [arm, reads] of [
        ["snippet", fieldReadsInSnippet(snippet)],
        ["on disk", readsOnDisk(snippet)],
      ] as const) {
        expect([...reads.modifier, ...reads.ambiguous], `${arm}: ${snippet}`).toEqual([]);
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
      "window.onkeypress = h;",
      // Iteration 12. Every one of these is an app-wide registration written
      // in a spelling the detector could not read: a CAST around the target
      // (`chain` unwrapped parentheses and `!` but not `as`), and an event
      // name assembled by concatenation from pieces that are all literals.
      '(window as EventTarget).addEventListener("keydown", h);',
      "(<EventTarget>window).addEventListener('keydown', h);",
      "(window satisfies Window).addEventListener('keydown', h);",
      '(window).addEventListener("keydown", h);',
      'window!.addEventListener("keydown", h);',
      'window.addEventListener!("keydown", h);',
      'window.addEventListener("key" + "down", h);',
      'const PREFIX = "key"; window.addEventListener(PREFIX + "down", h);',
      '(document.body as HTMLElement).addEventListener("keydown", h);',
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

  /**
   * The detector's SECOND declared escape, named here rather than left to be
   * measured by a thirteenth iteration: a target this pass cannot resolve to
   * a name.
   *
   * `const t = getTarget(); t.addEventListener("keydown", h)` may or may not
   * be `window` — deciding it needs a call graph, and the same is true of an
   * element pulled out of a collection. Grading it as global would flag every
   * legitimate `el.addEventListener("keydown", …)` in the tree, which is the
   * over-report that gets a ban switched off. The BOUND is the same one every
   * other escape here has: mechanism A still rosters the file (it reads a key
   * field to do anything with the event) and mechanism B still inventories
   * its claims — only the app-wide GRADE is lost.
   */
  it("escapes a registration whose target it cannot resolve", () => {
    expect(listens('const t = getTarget(); t.addEventListener("keydown", h);')).toBe(false);
    expect(listens('els[i].addEventListener("keydown", h);')).toBe(false);
    expect(listens('this.host.addEventListener("keydown", h);')).toBe(false);
  });

  it("finds the global listeners that are actually in the tree", () => {
    expect(GLOBAL_LISTENER_FILES.size).toBeGreaterThan(10);
  });
});

/* ── C. a chord claimed with no keyboard-event field read ────────────── */

/**
 * Spellings mechanism C must CATCH, one per way a chord is claimed without
 * touching a `KeyboardEvent`.
 *
 * `field` names which half of the verdict has to carry it, so a row cannot
 * pass by being detected as the wrong kind of claim.
 */
const MECHANISM_C_CAUGHT: Array<{
  name: string;
  snippet: string;
  field: keyof NonDomChordClaims;
  value: string;
}> = [
  {
    name: "a Tauri global shortcut",
    snippet: 'register("CommandOrControl+J", act);',
    field: "chordStrings",
    value: "commandorcontrol+j",
  },
  {
    name: "a hotkeys hook",
    snippet: 'useHotkeys("ctrl+j", act);',
    field: "chordStrings",
    value: "ctrl+j",
  },
  {
    name: "Mousetrap",
    snippet: 'Mousetrap.bind("mod+shift+p", act);',
    field: "chordStrings",
    value: "mod+shift+p",
  },
  {
    name: "an Electron-style accelerator",
    snippet: 'globalShortcut.register("Alt+F4", act);',
    field: "chordStrings",
    value: "alt+f4",
  },
  {
    name: "a chord spelled in a template literal",
    snippet: "bind(`Ctrl+Shift+K`, act);",
    field: "chordStrings",
    value: "ctrl+shift+k",
  },
  {
    name: "a chord spelled with spaces around the plus",
    snippet: 'bind("Ctrl + J", act);',
    field: "chordStrings",
    value: "ctrl+j",
  },
  {
    name: "Monaco addCommand",
    snippet: "ed.addCommand(KeyMod.CtrlCmd | KeyCode.KeyJ, act);",
    field: "keybindingApis",
    value: "addCommand",
  },
  {
    name: "Monaco addAction",
    snippet: "ed.addAction({ id: 'x', keybindings: [2048 | 42], run: act });",
    field: "keybindingApis",
    value: "addAction",
  },
  {
    name: "a raw keybinding registration",
    snippet: "svc.addKeybinding(2048 | 42, act);",
    field: "keybindingApis",
    value: "addKeybinding",
  },
  {
    name: "the other raw keybinding registration",
    snippet: "svc.registerKeybinding(2048 | 42, act);",
    field: "keybindingApis",
    value: "registerKeybinding",
  },
  {
    name: "the Monaco constant namespaces on their own",
    snippet: "const b = monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyJ;",
    field: "keybindingApis",
    value: "KeyMod",
  },
  {
    name: "the platform accelerator, assigned as a property",
    snippet: 'el.accessKey = "j";',
    field: "accessKeys",
    value: "j",
  },
  {
    name: "the platform accelerator, via setAttribute",
    snippet: 'el.setAttribute("accesskey", "j");',
    field: "accessKeys",
    value: "?",
  },
  {
    name: "the platform accelerator, as a JSX attribute",
    snippet: '<button accessKey="j">go</button>;',
    field: "accessKeys",
    value: "j",
  },
];

/**
 * Spellings mechanism C must NOT read as a chord.
 *
 * The bound on {@link MECHANISM_C_CAUGHT}. A rule keyed on `<word>+<word>`
 * that fired on arithmetic or on prose would put most of the tree on tier C
 * and get switched off — the same failure mode R5's prose bound exists for.
 */
const MECHANISM_C_CLEAN: string[] = [
  'const sum = "1 + 2";',
  'const label = "a+b";',
  'const prose = "hold ctrl and press j";',
  'const expr = "x + y";',
  'const version = "1.0+build.7";',
  'const word = "altitude+x";',
  'const trailing = "ctrl+";',
];

/** Mechanism C's verdict on a snippet. */
function nonDomInSnippet(snippet: string): NonDomChordClaims {
  return findNonDomChordClaims(parseSource(snippet, "snippet.tsx"));
}

describe("C. a chord claimed with no KeyboardEvent field read is still found", () => {
  it("pins every file that claims a chord outside the DOM keyboard API", () => {
    const declared = Object.keys(NON_DOM_CHORD_ROSTER).sort();
    const detected = [...DETECTED_NON_DOM].sort();
    expect(rosterDiff(detected, declared)).toEqual([]);
    expect(detected).toEqual(declared);
    for (const [rel, why] of Object.entries(NON_DOM_CHORD_ROSTER)) {
      expect(why.length, `${rel} needs a note`).toBeGreaterThan(20);
    }
  });

  it("sees a chord claimed through a library, a constant table, or the platform", () => {
    for (const c of MECHANISM_C_CAUGHT) {
      expect(nonDomInSnippet(c.snippet)[c.field], `${c.name}: ${c.snippet}`).toContain(c.value);
    }
  });

  it("does not read an ordinary sum, a version or a sentence as a chord", () => {
    for (const snippet of MECHANISM_C_CLEAN) {
      const v = nonDomInSnippet(snippet);
      expect([...v.chordStrings, ...v.accessKeys, ...v.keybindingApis], snippet).toEqual([]);
    }
  });

  it("is not silently matching nothing", () => {
    expect(DETECTED_NON_DOM.length).toBeGreaterThan(3);
  });

  /**
   * Mechanism C's own DECLARED ESCAPES, stated here rather than discovered
   * later:
   *
   *   1. **A chord string assembled at runtime** — `register("Ctrl" + "+J")`,
   *      `` register(`${mod}+J`) ``. No literal is a whole chord spelling, so
   *      C1 cannot fire. Same class as mechanism A's escape 1.
   *   2. **A numeric keybinding API under a name not on the roster.** C3 is
   *      enumerative and says so: `KEYBINDING_CALLS` in `keyFieldReads.ts` is
   *      a list. Every entry is falsified by `keyRules.mutation.test.ts`, so
   *      the list cannot rot, but it cannot anticipate a library either. C1
   *      catches any such library that takes its chord as TEXT, which is what
   *      bounds this: only a numeric-constant API escapes.
   */
  it("escapes a chord assembled at runtime, and an unrostered numeric API", () => {
    expect(nonDomInSnippet('register("Ctrl" + "+J", act);').chordStrings).toEqual([]);
    expect(nonDomInSnippet("register(`${mod}+J`, act);").chordStrings).toEqual([]);
    expect(nonDomInSnippet("weirdLib.installBinding(2048 | 42, act);").keybindingApis).toEqual([]);
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

/* ── the disk batch ──────────────────────────────────────────────────── */

/**
 * D2's fixture: an app-wide `Ctrl+Shift+J` claimant with BOTH halves of the
 * chord test hoisted into a sibling module.
 *
 * This is the ordinary way an accelerator table is written, and it is the
 * counterexample to mechanism A's declared escape 3, which asserts its own
 * bound as "the importing file reads `key`, so it IS on the tier-2 roster
 * and is not invisible". With the key comparison hoisted too, it reads
 * nothing, is on neither roster, is not in `SCANS`, and mechanism B never
 * runs on it.
 */
const D2_HOISTED_FIXTURE =
  'import { isChord } from "./chordUtil";\n' +
  '  window.addEventListener("keydown", (ev) => {\n' +
  '    if (isChord(ev, "Ctrl+Shift+J")) act();\n' +
  "  });";

/** The same shape, hoisted into the SANCTIONED target — must be acquitted. */
const D2_TABLE_FIXTURE =
  'import { GLOBAL_CHORDS, matchesChord } from "@/lib/globalChords";\n' +
  '  window.addEventListener("keydown", (ev) => {\n' +
  "    if (matchesChord(ev, GLOBAL_CHORDS.commandBar)) act();\n" +
  "  });";

/* ── D9: eleven claimants the PIPELINE never reached ─────────────────── */

/**
 * The claimant that was never PARSED.
 *
 * `COULD_READ_A_KEY` was `/\b…|keydown|keyup|keypress\b/`, and `\bkeydown\b`
 * does not match inside `onkeydown`. So this file — an app-wide `Ctrl+J`
 * claimant registered with no `addEventListener` at all — never entered
 * `PARSED`, and every mechanism downstream of it was looking at nothing. The
 * unit test asserting `listens("window.onkeydown = h;") === true` passed the
 * whole time: the rule was right and the pipeline could not feed it.
 */
const D9_ONKEYDOWN_FIXTURE =
  'import { isChord } from "./chordUtil";\n' +
  "  window.onkeydown = (ev) => {\n" +
  '    if (isChord(ev as KeyboardEvent, "Ctrl+J")) act();\n' +
  "  };";

/** The listener target behind a CAST — `chain` unwrapped `( )` and `!`, not `as`. */
const D9_CAST_TARGET_FIXTURE =
  '(window as EventTarget).addEventListener("keydown", (ev) => {\n' +
  "    const k = ev as KeyboardEvent;\n" +
  '    if (k.ctrlKey && k.key === "j") act();\n' +
  "  });";

/** The event name folded from a concatenation of literals. */
const D9_CONCAT_EVENT_FIXTURE =
  'window.addEventListener("key" + "down", (ev: KeyboardEvent) => {\n' +
  '    if (ev.ctrlKey && ev.key === "j") act();\n' +
  "  });";

/** Monaco — a chord claimed with a numeric constant and no field read at all. */
const D9_MONACO_FIXTURE =
  'import { KeyCode, KeyMod } from "monaco-editor";\n' +
  "  ed.addCommand(KeyMod.CtrlCmd | KeyCode.KeyJ, act);";

/** A Tauri global shortcut — OS-level, and it outlives the window. */
const D9_TAURI_FIXTURE =
  'import { register } from "@tauri-apps/plugin-global-shortcut";\n' +
  '  register("CommandOrControl+J", act);';

/** A third-party hotkeys library. */
const D9_HOTKEYS_FIXTURE =
  'import { useHotkeys } from "react-hotkeys-hook";\n  useHotkeys("ctrl+j", act);';

/** The platform's own accelerator, with no JavaScript in the loop. */
const D9_ACCESSKEY_FIXTURE = 'const el = document.createElement("button");\n  el.accessKey = "j";';

/**
 * The listener target this mechanism genuinely CANNOT resolve — declared, not
 * closed. See "escapes a registration whose target it cannot resolve".
 */
const D9_OPAQUE_TARGET_FIXTURE =
  "const target = getTarget();\n" +
  '  target.addEventListener("keydown", (ev: KeyboardEvent) => {\n' +
  '    if (ev.ctrlKey && ev.key === "j") act();\n' +
  "  });";

/* ── D10: the file classes the walk could not see ────────────────────── */

/** A plain `.js` module — bundled by Vite, invisible to a `/\.tsx?$/` walk. */
const D10_JS_PROBE =
  '// .js probe\nwindow.addEventListener("keydown", function (ev) {\n' +
  '  if (ev.ctrlKey && ev.key === "j") act();\n});\n';

/** A `.jsx` module, claiming through the platform accelerator. */
const D10_JSX_PROBE = '// .jsx probe\nexport const Go = () => <button accessKey="j">go</button>;\n';

/** An ESM `.mjs` module, registering with no call at all. */
const D10_MJS_PROBE =
  "// .mjs probe\ndocument.onkeydown = (ev) => {\n" +
  '  if (ev.ctrlKey && ev.key === "j") act();\n};\n';

/** A CommonJS `.cjs` module. */
const D10_CJS_PROBE =
  '// .cjs probe\nglobalThis.addEventListener("keyup", function (ev) {\n' +
  '  if (ev.metaKey && ev.key === "j") act();\n});\n';

/**
 * An HTML ENTRY POINT with a keydown listener in an inline `<script>`.
 *
 * `index.html` was outside the walk entirely, and it already ships a live
 * `window.addEventListener("unhandledrejection", …)` in a `<script>` block —
 * so the class was not hypothetical, only unclaimed. A real keydown listener
 * added there left the suite green.
 */
const D10_HTML_PROBE =
  '<!doctype html>\n<html>\n  <body>\n    <div id="root"></div>\n' +
  '    <script src="/src/main.tsx"></script>\n' +
  "    <script>\n" +
  '      window.addEventListener("keydown", function (ev) {\n' +
  '        if (ev.ctrlKey && ev.key === "j") act();\n' +
  "      });\n" +
  "    </script>\n  </body>\n</html>\n";

/** Every raw-file probe, with the extension that makes it that file class. */
const RAW_PROBES: Array<{ label: string; id: string; ext: string }> = [
  { label: ".js", id: D10_JS_PROBE, ext: ".js" },
  { label: ".jsx", id: D10_JSX_PROBE, ext: ".jsx" },
  { label: ".mjs", id: D10_MJS_PROBE, ext: ".mjs" },
  { label: ".cjs", id: D10_CJS_PROBE, ext: ".cjs" },
  { label: ".html", id: D10_HTML_PROBE, ext: ".html" },
];

/** Every D9 fixture graded as a WHOLE FILE, never injected. */
const D9_FIXTURES: Array<{ label: string; id: string }> = [
  { label: "window.onkeydown", id: D9_ONKEYDOWN_FIXTURE },
  { label: "cast listener target", id: D9_CAST_TARGET_FIXTURE },
  { label: "concatenated event name", id: D9_CONCAT_EVENT_FIXTURE },
  { label: "monaco addCommand", id: D9_MONACO_FIXTURE },
  { label: "tauri register", id: D9_TAURI_FIXTURE },
  { label: "hotkeys library", id: D9_HOTKEYS_FIXTURE },
  { label: "accessKey", id: D9_ACCESSKEY_FIXTURE },
  { label: "opaque listener target", id: D9_OPAQUE_TARGET_FIXTURE },
];

/**
 * Every snippet that is probed through the disk arm.
 *
 * `injected` for mechanism B (a claim must survive a real module around it),
 * `bare` for mechanism A (injecting into a host that reads six modifier
 * fields would swamp the "this file reads NO key field" signal). Both are
 * written for every snippet, so a row's two arms are always comparable.
 */
function collectDiskProbes(): DiskProbe[] {
  const ids = new Set<string>([
    ...OFFENDERS.map(([snippet]) => snippet),
    ...CLEAN,
    ...PROBE_CLASSES.flatMap((c) => c.spellings),
    ...MECHANISM_A_ESCAPES.flatMap((c) => c.spellings),
    ...MECHANISM_A_CLOSED.flatMap((c) => c.spellings.map(([snippet]) => snippet)),
    ...R5_PROSE,
  ]);
  const probes: DiskProbe[] = [];
  for (const id of ids) {
    probes.push({ id, kind: "injected" }, { id, kind: "bare" });
  }
  // The D2 and D9 fixtures are graded as WHOLE FILES, never injected: the
  // point is what a file with no key read looks like, and the host has plenty.
  probes.push({ id: D2_HOISTED_FIXTURE, kind: "bare" }, { id: D2_TABLE_FIXTURE, kind: "bare" });
  for (const f of D9_FIXTURES) probes.push({ id: f.id, kind: "bare" });
  // The D10 probes are written VERBATIM under the extension that makes them
  // the file class in question — a `.ts` copy of a `.js` offender would
  // measure the walk this file already had.
  for (const r of RAW_PROBES) probes.push({ id: r.id, kind: "raw", ext: r.ext });
  return probes;
}

// File-level, so every describe below sees a filled `DISK`. The probes exist
// on disk only for the duration of this call.
// The explicit timeout is not decoration. The batch writes ~290 files and
// re-walks `src/`, which takes ~2 s on an idle box and blew through vitest's
// 10 s default when the whole 191-file suite ran in parallel on this one —
// reported as "Hook timed out", i.e. the arm silently not running at all,
// which is the exact failure mode this arm exists to remove.
beforeAll(() => {
  runDiskProbes(collectDiskProbes());
}, 300_000);

// Belt and braces. `runDiskProbes` already sweeps in its own `finally`, so
// the probe FILES are gone by now either way; this removes the temp root the
// suite minted for them, which nothing else would.
afterAll(() => {
  rmSync(PROBE_ROOT, { recursive: true, force: true });
});

describe("F. the scanner can actually fail", () => {
  it("flags every mutation spelling as a snippet", () => {
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsInSnippet(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("flags every mutation spelling INJECTED INTO A REAL FILE ON DISK", () => {
    // FAIL CLOSED FIRST. `expect(scanKeyClaims(HOST_SOURCE, …)).toEqual([])`
    // passes on `""`, so on its own it is not evidence the host exists — it
    // is exactly what let a missing host collapse this arm into a copy of
    // the one above. Assert the host is REAL before asserting it is clean.
    expect(HOST_SOURCE.length, `${HOST_REL} must be a real file`).toBeGreaterThan(2000);
    // The host is clean on its own, so any claim comes from the probe.
    expect(scanKeyClaims(HOST_SOURCE, HOST_REL).claims).toEqual([]);
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsOnDisk(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("runs arm 2 through the real walk, not through a string concat", () => {
    // What D3 was: arm 2 concatenated the host's TEXT with the snippet in
    // memory, never wrote to disk, and never re-invoked `sourceFiles()`. All
    // 159 rows then gave identical verdicts across bare-snippet /
    // empty-host / real-host, because two of the three were the same code.
    // These are the properties that make the arm a second MECHANISM rather
    // than a second call: a file existed, the walk found it, the prefilter
    // graded it, and mechanism A rostered it.
    const sample = OFFENDERS[0][0];
    const injected = diskVerdict("injected", sample);
    expect(injected.discovered, "the walk must have found the probe").toBe(true);
    expect(injected.prefiltered, "the prefilter must have admitted it").toBe(true);
    expect(injected.reads.modifier.length, "mechanism A must roster it").toBeGreaterThan(0);
    // And the bare arm is a DIFFERENT file, or the two arms are one arm.
    const bare = diskVerdict("bare", sample);
    expect(bare.reads.modifier.length).toBeGreaterThan(0);
    expect(bare.bareScan, "the bare probe must reach mechanism B too").not.toBeNull();
  });

  it("agrees between the two GENUINE arms on every row of the matrix", () => {
    // The report iteration 11 asked for. A row RED in one arm and GREEN in
    // the other is a real finding: under the old spelling both arms were the
    // same code, so "RED in both arms" was never evidence of two paths.
    const differing: string[] = [];
    const rows: Array<[string, string]> = [
      ...OFFENDERS.map(([s]) => [s, "OFFENDER"] as [string, string]),
      ...CLEAN.map((s) => [s, "CLEAN"] as [string, string]),
      ...PROBE_CLASSES.flatMap((c) =>
        c.spellings.map((s) => [s, c.caught ? "caught" : "escapes"] as [string, string]),
      ),
    ];
    for (const [snippet, label] of rows) {
      const arm1 = claimsInSnippet(snippet).length > 0;
      const arm2 = claimsOnDisk(snippet).length > 0;
      if (arm1 !== arm2) {
        differing.push(
          `${label}: snippet=${arm1 ? "RED" : "GREEN"} on-disk=${arm2 ? "RED" : "GREEN"} — ${snippet}`,
        );
      }
    }
    expect(differing).toEqual([]);
  });

  it("leaves bare-key and table-routed spellings alone", () => {
    for (const snippet of CLEAN) {
      expect(claimsInSnippet(snippet), snippet).toEqual([]);
      expect(claimsOnDisk(snippet), `on-disk: ${snippet}`).toEqual([]);
    }
  });

  it("catches every spelling of every class it claims to catch", () => {
    for (const c of PROBE_CLASSES.filter((x) => x.caught)) {
      for (const snippet of c.spellings) {
        expect(claimsInSnippet(snippet).length, `${c.name}: ${snippet}`).toBeGreaterThan(0);
        expect(claimsOnDisk(snippet).length, `on-disk — ${c.name}: ${snippet}`).toBeGreaterThan(0);
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
        expect(claimsOnDisk(snippet), `on-disk — ${c.name}: ${snippet}`).toEqual([]);
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
    // An app-wide claim outside the table is the double-fire itself, not a
    // documentation gap: two window listeners on one target both run.
    expect(globalListenerOffenders(LISTENER_GRADES)).toEqual([]);
  });

  it("grades a global key listener that reads NO key field — D2", () => {
    // The bound the old spelling had: it iterated SCANS, and SCANS holds
    // only files that read a key field. A global listener whose key test is
    // hoisted into a sibling module reads none, so it was never graded at
    // all — a brand-new app-wide Ctrl+Shift+J claimant, invisible to the
    // entire mechanism, with the suite 27/27 green. Measured, not argued:
    // the fixture is written to disk and rediscovered by the walk.
    const v = diskVerdict("bare", D2_HOISTED_FIXTURE);
    expect(v.prefiltered, "the fixture must reach the parser at all").toBe(true);
    expect(v.globalListener, "the fixture must register a GLOBAL key listener").toBe(true);
    expect(v.reads.modifier, "both halves are hoisted, so A sees no modifier").toEqual([]);
    expect(v.reads.ambiguous, "both halves are hoisted, so A sees no key field").toEqual([]);
    // …and therefore mechanism B never ran on it. THAT is the invisibility.
    expect(v.bareScan, "a file with no key read is not in SCANS").toBeNull();

    const graded: ListenerGrade[] = [
      {
        rel: "probe/hoisted.ts",
        globalListener: v.globalListener,
        scan: v.bareScan,
        routesThroughChordTable: v.routesThroughChordTable,
      },
    ];
    expect(globalListenerOffenders(graded)).toHaveLength(1);
    expect(globalListenerOffenders(graded)[0]).toContain("reads no key field");
  });

  it("acquits a global key listener that routes its chord through the table", () => {
    // The other arm of the same grade, and the reason it is not a
    // hand-maintained allowlist. `PerformanceOverlay.tsx` and
    // `GiantSCCFixture.tsx` hoist BOTH halves too — into `GLOBAL_CHORDS`,
    // which is the one hoist target properties C and D already pin. The
    // grade is derived from the import, so a file that stops routing
    // through the table reds without anyone remembering to edit a list.
    const v = diskVerdict("bare", D2_TABLE_FIXTURE);
    expect(v.globalListener).toBe(true);
    expect([...v.reads.modifier, ...v.reads.ambiguous]).toEqual([]);
    expect(
      globalListenerOffenders([
        {
          rel: "probe/tabled.ts",
          globalListener: true,
          scan: v.bareScan,
          routesThroughChordTable: v.routesThroughChordTable,
        },
      ]),
    ).toEqual([]);
    // And the two live files this describes are in the tree right now.
    const ungraded = LISTENER_GRADES.filter((g) => g.globalListener && g.scan === null).map(
      (g) => g.rel,
    );
    expect(ungraded.length, "the ungraded-listener case must be LIVE, not hypothetical").toBe(2);
  });

  it("reaches a claimant registered through `onkeydown` — the D9 prefilter", () => {
    // THE PIPELINE PROPERTY. Every mechanism in this file is downstream of
    // `COULD_READ_A_KEY`, and this fixture matched none of its alternatives:
    // `\bkeydown\b` has no boundary inside `onkeydown`, `KeyboardEvent` has
    // none inside it either, and `"Ctrl+J"` names no field. So the file was
    // never parsed, and asserting that the RULE catches `window.onkeydown`
    // was a fake falsification — the rule could not be fed.
    const v = diskVerdict("bare", D9_ONKEYDOWN_FIXTURE);
    expect(v.prefiltered, "the fixture must reach the parser at all").toBe(true);
    expect(v.globalListener, "and be graded as an app-wide registration").toBe(true);
    // Both halves of the chord test are hoisted, so mechanism A sees nothing
    // and mechanism B never runs — which is what makes the listener grade the
    // only thing standing between this file and a silent double-fire.
    expect([...v.reads.modifier, ...v.reads.ambiguous]).toEqual([]);
    expect(v.bareScan).toBeNull();
    expect(
      globalListenerOffenders([
        {
          rel: "probe/onkeydown.ts",
          globalListener: v.globalListener,
          scan: v.bareScan,
          routesThroughChordTable: v.routesThroughChordTable,
        },
      ]),
    ).toHaveLength(1);
    // …and mechanism C names the chord it claims, which mechanism A cannot.
    expect(v.nonDom.chordStrings).toContain("ctrl+j");
  });

  it("grades a listener whose TARGET is behind a cast, and whose EVENT is concatenated", () => {
    for (const id of [D9_CAST_TARGET_FIXTURE, D9_CONCAT_EVENT_FIXTURE]) {
      const v = diskVerdict("bare", id);
      expect(v.prefiltered, id).toBe(true);
      expect(v.globalListener, `must be graded app-wide: ${id}`).toBe(true);
      // These two DO read key fields, so the claim is inventoried as well as
      // graded — and an app-wide Ctrl+J claim is an offender outright.
      expect(v.reads.modifier, id).toContain("ctrlKey");
      expect(
        globalListenerOffenders([
          {
            rel: "probe/cast.ts",
            globalListener: v.globalListener,
            scan: v.bareScan,
            routesThroughChordTable: v.routesThroughChordTable,
          },
        ]).length,
        `must be an offender: ${id}`,
      ).toBeGreaterThan(0);
    }
  });

  it("names the chord in every claimant that reads NO keyboard field — D9", () => {
    // Monaco, a Tauri OS-level shortcut, a hotkeys library and the platform
    // accelerator. Eleven such claimants were planted live at once and the
    // suite stayed 33/33 green: mechanisms A and B both start from a field
    // read, and there is none, so neither could see any of them.
    const expected: Array<[string, keyof NonDomChordClaims, string]> = [
      [D9_MONACO_FIXTURE, "keybindingApis", "addCommand"],
      [D9_TAURI_FIXTURE, "chordStrings", "commandorcontrol+j"],
      [D9_HOTKEYS_FIXTURE, "chordStrings", "ctrl+j"],
      [D9_ACCESSKEY_FIXTURE, "accessKeys", "j"],
    ];
    for (const [id, field, value] of expected) {
      const v = diskVerdict("bare", id);
      expect(v.prefiltered, `must reach the parser: ${id}`).toBe(true);
      expect([...v.reads.modifier, ...v.reads.ambiguous], `reads no key field: ${id}`).toEqual([]);
      expect(v.nonDom[field], `${field} must carry it: ${id}`).toContain(value);
    }
  });

  it("declares the listener target it cannot resolve, rather than pretending to", () => {
    // The residual, measured. `const t = getTarget(); t.addEventListener(…)`
    // is not graded app-wide, and this asserts that it ESCAPES — so a future
    // round that closes it has to come here and say so, and a round that
    // thinks it is already closed is corrected by a red.
    const v = diskVerdict("bare", D9_OPAQUE_TARGET_FIXTURE);
    expect(v.prefiltered).toBe(true);
    expect(v.globalListener, "declared escape: an unresolvable target").toBe(false);
    // The BOUND: it is still rostered and still inventoried, so the file is
    // named and its claim counted — only the app-wide grade is lost.
    expect(v.reads.modifier).toContain("ctrlKey");
    expect(v.bareScan?.claims.map((c) => c.spelling)).toContain("ctrl+j");
  });

  it("walks every file class Vite bundles — .js, .jsx, .mjs, .cjs — D10", () => {
    // The walk was `/\.tsx?$/`. Vite bundles all four of these, so an
    // offender in any of them was invisible to every property in this file.
    for (const r of RAW_PROBES) {
      if (r.ext === ".html") continue;
      const v = diskVerdict("raw", r.id);
      expect(v.discovered, `${r.label} must be found by the walk`).toBe(true);
      expect(v.rel.endsWith(r.ext), `${r.label}: ${v.rel}`).toBe(true);
      expect(v.prefiltered, `${r.label} must reach the parser`).toBe(true);
      const claimed =
        v.reads.modifier.length +
        v.nonDom.chordStrings.length +
        v.nonDom.accessKeys.length +
        v.nonDom.keybindingApis.length;
      expect(claimed, `${r.label}: the planted offender must be SEEN`).toBeGreaterThan(0);
    }
  });

  it("walks an HTML entry point's inline <script> blocks — D10", () => {
    // `index.html` was outside the walk entirely. It is not an empty shell:
    // it ships a live `window.addEventListener("unhandledrejection", …)`, so
    // a keydown listener planted beside it ran in the real app and was
    // invisible here.
    const v = diskVerdict("raw", D10_HTML_PROBE);
    expect(v.rel, "an inline block is a UNIT, not a file the walk merely opened").toMatch(
      /\.html#script\d+$/,
    );
    expect(v.prefiltered).toBe(true);
    expect(v.globalListener, "the planted keydown listener must be graded app-wide").toBe(true);
    expect(v.reads.modifier).toContain("ctrlKey");
    expect(
      globalListenerOffenders([
        {
          rel: v.rel,
          globalListener: v.globalListener,
          scan: v.bareScan,
          routesThroughChordTable: v.routesThroughChordTable,
        },
      ]).length,
      "an app-wide ctrl+j claim in index.html is an offender",
    ).toBeGreaterThan(0);
    // And the REAL entry point is in the walk, not only the probe — otherwise
    // this proves the probe machinery and nothing about the app.
    const real = FILES.filter((f) => /^index\.html#script\d+$/.test(f.rel));
    expect(real.length, "index.html must be walked, as one unit per inline <script>").toBe(2);
    expect(real.some((f) => /addEventListener\(\s*"unhandledrejection"/.test(f.source))).toBe(true);
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

/** A file cannot call a named predicate without naming it. */
const CALLS_A_CHORD_PREDICATE = /matchesChord|matchesDigitChord|isCtrlShiftChord/;

/** `GLOBAL_CHORDS.commandBar` → `"commandBar"`; anything else → null. */
function namespaceMember(node: ts.Expression | undefined, ns: string): string | null {
  if (!node || !ts.isPropertyAccessExpression(node)) return null;
  return ts.isIdentifier(node.expression) && node.expression.text === ns ? node.name.text : null;
}

/** `{ key: "k", shift: false, meta: false }` → the chord it spells. */
function objectChord(node: ts.ObjectLiteralExpression): GlobalChord | null {
  let key: string | null = null;
  let shift = false;
  for (const prop of node.properties) {
    if (!ts.isPropertyAssignment(prop) || !ts.isIdentifier(prop.name)) continue;
    if (prop.name.text === "key" && ts.isStringLiteralLike(prop.initializer)) {
      key = prop.initializer.text;
    }
    if (prop.name.text === "shift") shift = prop.initializer.kind === ts.SyntaxKind.TrueKeyword;
  }
  return key === null ? null : { key, shift, meta: false };
}

/**
 * ROUTED claims — calls to the sanctioned predicates, read off the AST.
 *
 * This used to be four `source.matchAll(…)` passes over the raw text, and
 * that was sound only while the walk skipped test files. It does not any
 * more, and this file's own mutation corpus contains
 * `"isCtrlShiftChord(e, \"t\")"` as a STRING — which a text scan reports as a
 * live claim of `ctrl+shift+t` by the enforcement suite itself.
 *
 * Reading the AST makes the exemption structural instead of a filename rule:
 * a call inside a string literal is a string, and there is no CallExpression
 * to find. That is what "exempt the fixtures BY CONSTRUCTION" means here, and
 * it is why re-including the test-file class costs nothing in precision.
 *
 * Matching a CALL structurally stays fair for the same reason matching a
 * listener registration does: its shape is fixed by the callee's name. The
 * open-ended space that broke five scanners is the HAND-ROLLED side, and that
 * side is mechanism B's job.
 */
function claimsIn(unit: SourceUnit): Claim[] {
  const out: Claim[] = [];
  if (!CALLS_A_CHORD_PREDICATE.test(unit.source)) return out;
  const rel = unit.rel;
  const sf = parseSource(unit.source, unit.parseName);
  const walk = (n: ts.Node): void => {
    if (ts.isCallExpression(n)) {
      const callee = n.expression;
      const name = ts.isPropertyAccessExpression(callee)
        ? callee.name.text
        : ts.isIdentifier(callee)
          ? callee.text
          : null;
      const arg = n.arguments[1];
      if (name === "matchesChord") {
        const member = namespaceMember(arg, "GLOBAL_CHORDS");
        if (member !== null) {
          const chord = TABLE_BY_NAME[member];
          expect(chord, `GLOBAL_CHORDS.${member} is referenced by ${rel} but absent`).toBeDefined();
          out.push({ rel, spelling: spelling(chord), viaTable: true });
        } else if (arg && ts.isObjectLiteralExpression(arg)) {
          const literal = objectChord(arg);
          if (literal !== null) out.push({ rel, spelling: spelling(literal), viaTable: false });
        }
      } else if (name === "matchesDigitChord") {
        const member = namespaceMember(arg, "GLOBAL_DIGIT_CHORDS");
        if (member !== null) {
          const chord = DIGIT_TABLE_BY_NAME[member];
          expect(
            chord,
            `GLOBAL_DIGIT_CHORDS.${member} is referenced by ${rel} but absent`,
          ).toBeDefined();
          for (const sp of digitSpellings(chord)) out.push({ rel, spelling: sp, viaTable: true });
        }
      } else if (name === "isCtrlShiftChord" && arg && ts.isStringLiteralLike(arg)) {
        out.push({
          rel,
          spelling: spelling({ key: arg.text, shift: true, meta: false }),
          viaTable: false,
        });
      }
    }
    ts.forEachChild(n, walk);
  };
  walk(sf);
  return out;
}

// The chord module itself only MENTIONS the call shapes in its docstring;
// it is the table, not a claimant.
const CLAIMS = FILES.filter((f) => !MECHANISM_FILES.has(f.rel)).flatMap(claimsIn);

/**
 * The claims made by SURFACES — everything except the two declared
 * predicate harnesses, whose calls exercise the predicate rather than claim
 * the chord. See {@link CHORD_PREDICATE_HARNESSES}.
 */
const APP_CLAIMS = CLAIMS.filter((c) => !(c.rel in CHORD_PREDICATE_HARNESSES));

describe("chord registries", () => {
  it("finds the claims it is meant to police", () => {
    expect(APP_CLAIMS.length).toBeGreaterThan(20);
    // The digit ranges must actually be reaching the counter — a
    // `matchesDigitChord` call that stopped being recognised would
    // silently restore the exact blind spot this table was added for.
    expect(APP_CLAIMS.filter((c) => /^ctrl\+(shift\+)?\d$/.test(c.spelling).valueOf()).length).toBe(
      8 + 8 + 9,
    );
  });

  it("reads a predicate call in a string FIXTURE as a string, not a claim", () => {
    // The property that makes re-including `*.test.ts` sound. This file's own
    // corpus contains `isCtrlShiftChord(e, "t")` and
    // `matchesChord(e, GLOBAL_CHORDS.commandBar)` as STRING LITERALS; the old
    // text scan reported both as live claims by the enforcement suite.
    for (const rel of ["lib/globalChords.enforcement.test.ts", "lib/keyRules.mutation.test.ts"]) {
      const unit = FILES.find((f) => f.rel === rel);
      expect(unit, `${rel} must be in the walk at all`).toBeDefined();
      expect(CALLS_A_CHORD_PREDICATE.test(unit?.source ?? ""), `${rel} must MENTION one`).toBe(
        true,
      );
      expect(CLAIMS.filter((c) => c.rel === rel)).toEqual([]);
    }
  });

  it("pins the test files that call a chord predicate for real", () => {
    const stale = Object.keys(CHORD_PREDICATE_HARNESSES).filter(
      (rel) => !CLAIMS.some((c) => c.rel === rel),
    );
    expect(stale, "a harness that no longer calls a predicate must be removed").toEqual([]);
    for (const [rel, why] of Object.entries(CHORD_PREDICATE_HARNESSES)) {
      expect(why.length, `${rel} needs a note`).toBeGreaterThan(20);
    }
  });

  it("keeps every non-terminal chord claim in GLOBAL_CHORDS", () => {
    const strays = APP_CLAIMS.filter((c) => !c.viaTable && c.rel !== TERMINAL_REGISTRY).map(
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
    for (const c of APP_CLAIMS) {
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
