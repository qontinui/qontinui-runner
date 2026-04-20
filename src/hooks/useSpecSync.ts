/**
 * useSpecSync — Batch sync all specs with AI merge mode + state machine generation
 *
 * Orchestrates:
 * 1. Analyze phase: identify stale specs (missing stateMachine, or outdated)
 * 2. Sync phase: re-generate each stale spec via AI in merge mode (sequential)
 * 3. Compile phase: compile all specs' stateMachine sections into a runtime state machine
 */

import { useState, useRef, useCallback, useEffect } from "react";
import { useAiSession } from "./useAiSession";
import { buildPageSpecPrompt } from "@/lib/page-analysis-prompt-builder";
import { getApiBase } from "@/lib/runner-api";
import { compileStateMachineFromSpecs } from "@/lib/compile-state-machine";
import type { LoadedSpec } from "@/pages/specs/types";
import type { SpecConfig } from "@/lib/spec-prompt-builder";

// ============================================================================
// Types
// ============================================================================

export type SyncPhase = "idle" | "analyzing" | "syncing" | "compiling" | "done";

export interface SyncProgress {
  current: number;
  total: number;
  currentSpec: string;
}

export interface SyncResults {
  updated: string[];
  skipped: string[];
  failed: string[];
}

export interface SpecSyncState {
  phase: SyncPhase;
  progress: SyncProgress;
  results: SyncResults;
  warnings: string[];
}

interface SpecToSync {
  spec: LoadedSpec;
  reason: "missing-state-machine" | "stale";
}

// ============================================================================
// JSON extraction (mirrors HookGenerationPanel pattern)
// ============================================================================

const MAX_JSON_BLOCK_SIZE = 1024 * 1024;

function extractJsonBlock(content: string): string | null {
  const jsonRegex = /```json\s*\n([\s\S]*?)```/gi;
  let match;
  while ((match = jsonRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue;
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }

  const bareRegex = /```\s*\n(\s*\{[\s\S]*?\})\s*\n```/g;
  while ((match = bareRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue;
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }

  return null;
}

// ============================================================================
// Staleness analysis
// ============================================================================

function analyzeSpecs(specs: LoadedSpec[]): { toSync: SpecToSync[]; toSkip: string[] } {
  const toSync: SpecToSync[] = [];
  const toSkip: string[] = [];

  for (const spec of specs) {
    if (spec.kind !== "page-spec") {
      toSkip.push(spec.specId);
      continue;
    }

    const config = spec.config as SpecConfig;

    // Check if stateMachine section is missing
    if (
      !config.stateMachine ||
      !config.stateMachine.states ||
      config.stateMachine.states.length === 0
    ) {
      toSync.push({ spec, reason: "missing-state-machine" });
      continue;
    }

    // Check if the spec is stale: state machine references groups that no longer
    // exist, or groups exist that have no corresponding state machine state.
    const groupNames = new Set(config.groups.map((g) => g.name));
    const smStateNames = new Set(config.stateMachine.states.map((s: { name: string }) => s.name));
    const hasOrphanedStates = [...smStateNames].some((n) => !groupNames.has(n));
    const hasMissingStates = [...groupNames].some((n) => !smStateNames.has(n));
    if (hasOrphanedStates || hasMissingStates) {
      toSync.push({ spec, reason: "stale" });
      continue;
    }

    // Spec looks fresh — skip
    toSkip.push(spec.specId);
  }

  return { toSync, toSkip };
}

// ============================================================================
// Source reading
// ============================================================================

interface ReadPageSourceResult {
  main_source: string;
  imported_sources: Array<{ path: string; content: string }>;
}

/**
 * Try to read page source for a spec by deriving the component path from metadata.
 * Returns null if source is unavailable.
 */
async function tryReadPageSource(
  spec: LoadedSpec,
  signal: AbortSignal,
): Promise<ReadPageSourceResult | null> {
  const config = spec.config as SpecConfig;
  const component = config.metadata?.component as string | undefined;
  if (!component) return null;

  // Derive project path and component path based on app
  const isRunner = spec.appName === "Qontinui Runner";
  const projectPath = isRunner ? "qontinui-runner" : "qontinui-web/frontend";

  // Convert component name to file path: "ActiveDashboardPage" → "active-dashboard"
  const kebab = component
    .replace(/Page$/, "")
    .replace(/([a-z])([A-Z])/g, "$1-$2")
    .toLowerCase();

  // Try common page path patterns
  const candidates = [
    `src/pages/${kebab}/${component}.tsx`,
    `src/pages/${kebab}/index.tsx`,
    `src/pages/${kebab}.tsx`,
  ];

  for (const componentPath of candidates) {
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-page-source`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          project_path: projectPath,
          component_path: componentPath,
          max_depth: 2,
        }),
        signal,
      });
      if (signal.aborted) return null;
      const data = await resp.json();
      if (data.success && data.data?.main_source) {
        return data.data as ReadPageSourceResult;
      }
    } catch {
      if (signal.aborted) return null;
    }
  }

  return null;
}

// ============================================================================
// Hook
// ============================================================================

export function useSpecSync(specs: LoadedSpec[], onSpecUpdated: (spec: LoadedSpec) => void) {
  const [state, setState] = useState<SpecSyncState>({
    phase: "idle",
    progress: { current: 0, total: 0, currentSpec: "" },
    results: { updated: [], skipped: [], failed: [] },
    warnings: [],
  });

  const ai = useAiSession();
  const abortRef = useRef<AbortController | null>(null);
  const syncQueueRef = useRef<SpecToSync[]>([]);
  const processedCountRef = useRef(0);
  const currentSpecRef = useRef<SpecToSync | null>(null);
  const resultsRef = useRef<SyncResults>({ updated: [], skipped: [], failed: [] });
  const warningsRef = useRef<string[]>([]);
  const totalRef = useRef(0);

  // Keep refs in sync to avoid stale closures in processNextSpec/finishSync chains
  // Update in effects to avoid ref mutation during render
  const specsRef = useRef(specs);
  const aiRef = useRef(ai);
  const onSpecUpdatedRef = useRef(onSpecUpdated);
  useEffect(() => {
    specsRef.current = specs;
    aiRef.current = ai;
    onSpecUpdatedRef.current = onSpecUpdated;
  });

  // Track specs updated during the current sync batch (may not yet be in specsRef)
  const updatedSpecsRef = useRef<Map<string, SpecConfig>>(new Map());

  // Refs for processNextSpec/finishSync to avoid stale closures
  const processNextSpecRef = useRef<() => Promise<void>>(async () => {});
  const finishSyncRef = useRef<() => void>(() => {});

  // Watch for AI session state transitions: processing → ready means a turn finished
  useEffect(() => {
    if (state.phase !== "syncing") return;
    if (ai.sessionState !== "ready") return;
    if (!currentSpecRef.current) return;

    // AI finished its turn — extract the JSON result
    const lastMessage = ai.messages[ai.messages.length - 1];
    if (!lastMessage || lastMessage.role !== "ai") return;

    const jsonBlock = extractJsonBlock(lastMessage.content);
    const currentSpec = currentSpecRef.current;
    currentSpecRef.current = null; // Prevent re-processing

    if (jsonBlock) {
      try {
        const parsed = JSON.parse(jsonBlock) as SpecConfig;
        const updatedSpec: LoadedSpec = {
          ...currentSpec.spec,
          config: parsed,
        } as LoadedSpec;
        onSpecUpdatedRef.current(updatedSpec);
        resultsRef.current.updated.push(currentSpec.spec.specId);
        updatedSpecsRef.current.set(currentSpec.spec.specId, parsed);
      } catch {
        resultsRef.current.failed.push(currentSpec.spec.specId);
        warningsRef.current.push(`Failed to parse JSON for ${currentSpec.spec.specId}`);
      }
    } else {
      resultsRef.current.failed.push(currentSpec.spec.specId);
      warningsRef.current.push(`No JSON block in AI response for ${currentSpec.spec.specId}`);
    }

    processedCountRef.current++;
    setState((prev) => ({
      ...prev,
      progress: { ...prev.progress, current: processedCountRef.current },
      results: {
        updated: [...resultsRef.current.updated],
        skipped: [...resultsRef.current.skipped],
        failed: [...resultsRef.current.failed],
      },
      warnings: [...warningsRef.current],
    }));

    // Process next spec or finish
    processNextSpecRef.current();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ai.sessionState, ai.messages.length, state.phase]);

  const processNextSpec = useCallback(async () => {
    const controller = abortRef.current;
    if (!controller || controller.signal.aborted) return;

    const next = syncQueueRef.current.shift();
    if (!next) {
      // All specs processed — move to compile phase
      finishSyncRef.current();
      return;
    }

    currentSpecRef.current = next;
    const config = next.spec.config as SpecConfig;
    const specId = next.spec.specId;

    setState((prev) => ({
      ...prev,
      progress: { ...prev.progress, currentSpec: specId },
    }));

    // Try to read page source
    const source = await tryReadPageSource(next.spec, controller.signal);
    if (controller.signal.aborted) return;

    let prompt: string;
    if (source) {
      // Full prompt with page source + merge mode
      const existingJson = JSON.stringify(config, null, 2);
      prompt = buildPageSpecPrompt(
        source.main_source,
        source.imported_sources,
        (config.metadata?.component as string) || specId,
        (config.metadata?.pageUrl as string) || "",
        "",
        existingJson,
      );
    } else {
      // Fallback: merge prompt without source (just add stateMachine)
      const existingJson = JSON.stringify(config, null, 2);
      prompt = [
        `Update this existing page spec to add a stateMachine section.`,
        `The page has the following groups: ${config.groups.map((g) => g.name).join(", ")}.`,
        `Analyze the groups and assertions to infer what UI states (tabs, modals, panels) exist`,
        `and what transitions connect them.`,
        ``,
        `Existing spec:`,
        "```json",
        existingJson,
        "```",
        ``,
        `Output the complete updated spec JSON with the stateMachine section added.`,
        `Preserve ALL existing groups, assertions, and IDs exactly as they are.`,
        `Only ADD the stateMachine section. Output as a single JSON code block.`,
      ].join("\n");
    }

    await aiRef.current.sendMessage(
      `Update spec "${specId}" with state machine generation.\n\n${prompt}`,
    );
  }, []);
  useEffect(() => {
    processNextSpecRef.current = processNextSpec;
  });

  const finishSync = useCallback(() => {
    // Compile phase
    setState((prev) => ({
      ...prev,
      phase: "compiling",
      progress: { ...prev.progress, currentSpec: "Compiling state machine..." },
    }));

    // Merge specs from prop with any that were updated during this sync batch.
    // React state may not have flushed yet, so updatedSpecsRef ensures we compile
    // with the freshest versions.
    const allSpecs = specsRef.current.map((s) => {
      const updated = updatedSpecsRef.current.get(s.specId);
      return updated ?? (s.config as SpecConfig);
    });
    try {
      const { stateMachine, stats } = compileStateMachineFromSpecs(allSpecs);

      // Load into engine
      const bridge = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
        | Record<string, unknown>
        | undefined;
      if (bridge?.loadStateMachine) {
        const json = JSON.stringify(stateMachine);
        (bridge.loadStateMachine as (json: string) => unknown)(json);
      }

      if (stats.warnings.length > 0) {
        warningsRef.current.push(...stats.warnings);
      }

      console.warn(
        `[SpecSync] Compiled: ${stats.statesCompiled} states, ${stats.transitionsCompiled} transitions from ${stats.specsProcessed} specs`,
      );
    } catch (err) {
      warningsRef.current.push(`Compilation error: ${err}`);
    }

    setState((prev) => ({
      ...prev,
      phase: "done",
      results: {
        updated: [...resultsRef.current.updated],
        skipped: [...resultsRef.current.skipped],
        failed: [...resultsRef.current.failed],
      },
      warnings: [...warningsRef.current],
    }));

    // Close AI session via ref to avoid stale closure
    aiRef.current.close();
    updatedSpecsRef.current.clear();
  }, []);
  useEffect(() => {
    finishSyncRef.current = finishSync;
  });

  const startSync = useCallback(async () => {
    // Guard against double-start
    if (abortRef.current && !abortRef.current.signal.aborted) return;

    // Reset state
    const controller = new AbortController();
    abortRef.current = controller;
    processedCountRef.current = 0;
    resultsRef.current = { updated: [], skipped: [], failed: [] };
    warningsRef.current = [];

    setState({
      phase: "analyzing",
      progress: { current: 0, total: 0, currentSpec: "Analyzing specs..." },
      results: { updated: [], skipped: [], failed: [] },
      warnings: [],
    });

    // Analyze which specs need updating
    const { toSync, toSkip } = analyzeSpecs(specs);
    resultsRef.current.skipped = toSkip;
    totalRef.current = toSync.length;

    setState((prev) => ({
      ...prev,
      progress: { current: 0, total: toSync.length, currentSpec: "" },
      results: { ...resultsRef.current },
    }));

    if (toSync.length === 0) {
      // Nothing to sync — go straight to compile
      setState((prev) => ({ ...prev, phase: "compiling" }));
      finishSyncRef.current();
      return;
    }

    // Create AI session for sync
    const sessionId = await aiRef.current.createSession("Spec Sync");
    if (!sessionId || controller.signal.aborted) {
      setState((prev) => ({
        ...prev,
        phase: "done",
        warnings: ["Failed to create AI session"],
      }));
      return;
    }

    // Load queue and start processing
    syncQueueRef.current = [...toSync];
    setState((prev) => ({ ...prev, phase: "syncing" }));

    processNextSpecRef.current();
  }, [specs]);

  const cancel = useCallback(() => {
    abortRef.current?.abort();
    currentSpecRef.current = null;
    syncQueueRef.current = [];
    aiRef.current.interrupt();
    setState((prev) => ({
      ...prev,
      phase: "done",
      warnings: [...prev.warnings, "Cancelled by user"],
    }));
  }, []);

  const reset = useCallback(() => {
    setState({
      phase: "idle",
      progress: { current: 0, total: 0, currentSpec: "" },
      results: { updated: [], skipped: [], failed: [] },
      warnings: [],
    });
  }, []);

  return {
    state,
    startSync,
    cancel,
    reset,
    isSyncing: state.phase !== "idle" && state.phase !== "done",
  };
}
