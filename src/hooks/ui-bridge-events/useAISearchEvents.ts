import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

// Module-level intent store for persistence across IPC calls (capped at 1000 entries)
const LOCAL_INTENT_STORE_MAX = 1000;
const localIntentStore: Array<Record<string, unknown>> = [];

/**
 * Handles: ai_search, ai_find, ai_execute, ai_assert, ai_assert_batch, ai_snapshot, ai_summary,
 *          ai_semantic_search, ai_diff, ai_analyze_data, ai_analyze_regions,
 *          ai_analyze_structured_data, ai_analyze_cross_app, ai_recovery_attempt,
 *          get_intents, register_intent, find_intent, execute_intent, execute_intent_from_query
 */
export function useAISearchEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "ai_search": {
          const { SearchEngine } = await import("ui-bridge/ai");
          // Use discover() for fresh elements with up-to-date visibility state
          // (currentBridge.elements is memoized and may have stale DOM refs)
          const discovered = await currentBridge.discover({ includeHidden: true });
          const engine = new SearchEngine({ includeHidden: true });
          engine.updateElements(discovered.elements);
          const criteria = payload.params ?? payload.body ?? {};
          const results = engine.search(criteria as Parameters<typeof engine.search>[0]);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: results,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_find": {
          const { SearchEngine, find: aiFindFn } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const engine = new SearchEngine({ includeHidden: true });
          engine.updateElements(discovered.elements);

          const query = (payload.params?.query as string) ?? "";
          const context = payload.params?.context as Record<string, unknown> | undefined;
          const confidenceThreshold = payload.params?.confidenceThreshold as number | undefined;

          // Auto-detect active modal for context-aware scoring
          const uiBridgeGlobal = getUIBridgeGlobal();
          const modalDet = uiBridgeGlobal?.modalDetector as
            | { detect: () => { modals: Array<{ id: string }> } }
            | undefined;
          const findContext: Record<string, unknown> = { ...context };
          if (!findContext.activeModalId && modalDet) {
            try {
              const modalStack = modalDet.detect();
              if (modalStack.modals.length > 0) {
                findContext.activeModalId = modalStack.modals[modalStack.modals.length - 1].id;
              }
            } catch {
              /* ignore */
            }
          }

          const findOpts: Parameters<typeof aiFindFn>[2] = {
            context: findContext as Parameters<typeof aiFindFn>[2] extends
              | { context?: infer C }
              | undefined
              ? C
              : never,
            pickFirst: true,
          };
          // Only pass confidenceThreshold if explicitly provided (undefined overrides defaults)
          if (typeof confidenceThreshold === "number" && !Number.isNaN(confidenceThreshold)) {
            findOpts.confidenceThreshold = confidenceThreshold;
          }
          const result = aiFindFn(query, engine, findOpts);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: result,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_execute": {
          const { NLActionExecutor } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const executor = new NLActionExecutor();
          executor.updateElements(discovered.elements);
          // The bridge's ActionExecutor handles DOM actions
          if (currentBridge.executor) {
            executor.setActionExecutor(currentBridge.executor);
          }
          const instruction = (payload.params?.instruction as string) ?? "";
          try {
            const result = await executor.execute({ instruction });
            await sendResponse({
              requestId,
              type,
              success: true,
              data: result,
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "ai_assert": {
          const { AssertionExecutor, parseNLAssertion } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const executor = new AssertionExecutor();
          executor.updateElements(discovered.elements);
          const assertion = (payload.params?.assertion as string) ?? "";
          const parsed = parseNLAssertion({ assertion });
          const result = await executor.assert({
            target: parsed.target,
            type: parsed.type as never,
            expected: parsed.expected,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: result,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_assert_batch": {
          const { AssertionExecutor, parseNLAssertion } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const executor = new AssertionExecutor();
          executor.updateElements(discovered.elements);
          const assertions = (payload.params?.assertions as string[]) ?? [];
          const results = await Promise.all(
            assertions.map(async (a) => {
              const parsed = parseNLAssertion({ assertion: a });
              return executor.assert({
                target: parsed.target,
                type: parsed.type as never,
                expected: parsed.expected,
              });
            }),
          );
          const allPassed = results.every((r) => r.passed);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { passed: allPassed, results },
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_snapshot": {
          const { SemanticSnapshotManager } = await import("ui-bridge/ai");
          const controlSnapshot = await currentBridge.createSnapshotAsync();
          const manager = new SemanticSnapshotManager();
          const snapshot = manager.createSnapshot(controlSnapshot, {
            url: window.location.href,
            title: document.title,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: snapshot,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_summary": {
          const { generatePageSummary, SemanticSnapshotManager } = await import("ui-bridge/ai");
          const controlSnapshot = await currentBridge.createSnapshotAsync();
          // Convert to AI elements via snapshot manager
          const manager = new SemanticSnapshotManager();
          const semanticSnapshot = manager.createSnapshot(controlSnapshot, {
            url: window.location.href,
            title: document.title,
          });
          const summary = generatePageSummary(semanticSnapshot.elements, {
            url: window.location.href,
            title: document.title,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { summary },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // AI Semantic Search & Diff
        // ==================================================================

        case "ai_semantic_search": {
          const { SearchEngine } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const engine = new SearchEngine({ includeHidden: true });
          engine.updateElements(discovered.elements);
          const searchParams = payload.params ?? payload.body ?? {};
          const query = (searchParams.query as string) ?? "";
          // Use the search engine with semantic matching
          const results = engine.search({
            text: query,
            ...(searchParams as Record<string, unknown>),
          } as Parameters<typeof engine.search>[0]);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: results,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_diff": {
          const { SemanticSnapshotManager } = await import("ui-bridge/ai");
          const diffSnapshot = await currentBridge.createSnapshotAsync();
          const diffManager = new SemanticSnapshotManager();
          const currentSemantic = diffManager.createSnapshot(diffSnapshot, {
            url: window.location.href,
            title: document.title,
          });
          // Return the current semantic snapshot for the caller to diff against a previous one
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              snapshot: currentSemantic,
              timestamp: Date.now(),
              url: window.location.href,
              title: document.title,
            },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // AI Analysis
        // ==================================================================

        case "ai_analyze_data": {
          const analyzeDataParams = payload.params ?? payload.body ?? {};
          const discovered = await currentBridge.discover({ includeHidden: true });
          // Collect element data for analysis
          const elementData = discovered.elements.map(
            (el: { id: string; type: string; label?: string; getState?: () => unknown }) => ({
              id: el.id,
              type: el.type,
              label: el.label,
              state: el.getState?.(),
            }),
          );
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              elements: elementData,
              query: analyzeDataParams,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_analyze_regions": {
          const regionParams = payload.params ?? payload.body ?? {};
          const discovered = await currentBridge.discover({ includeHidden: true });
          // Group elements by their bounding box regions
          const regions: Record<
            string,
            Array<{ id: string; type: string; label?: string; rect?: unknown }>
          > = {};
          for (const el of discovered.elements) {
            const state = (
              el as {
                getState?: () => { rect?: { x: number; y: number; width: number; height: number } };
              }
            ).getState?.();
            const rect = state?.rect;
            if (rect) {
              // Group by quadrant (top-left, top-right, bottom-left, bottom-right)
              const midX = window.innerWidth / 2;
              const midY = window.innerHeight / 2;
              const regionKey = `${rect.y < midY ? "top" : "bottom"}-${rect.x < midX ? "left" : "right"}`;
              if (!regions[regionKey]) regions[regionKey] = [];
              regions[regionKey].push({
                id: (el as { id: string }).id,
                type: (el as { type: string }).type,
                label: (el as { label?: string }).label,
                rect,
              });
            }
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { regions, query: regionParams, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_analyze_structured_data": {
          const structuredParams = payload.params ?? payload.body ?? {};
          const discovered = await currentBridge.discover({ includeHidden: true });
          // Extract structured data (tables, lists, forms)
          const tables: Array<{ id: string; rows: number; cols: number }> = [];
          const lists: Array<{ id: string; items: number }> = [];
          for (const el of discovered.elements) {
            const domEl = (el as { element?: Element }).element;
            if (domEl instanceof HTMLElement) {
              if (domEl.tagName === "TABLE") {
                tables.push({
                  id: (el as { id: string }).id,
                  rows: domEl.querySelectorAll("tr").length,
                  cols: domEl.querySelector("tr")?.querySelectorAll("td,th").length ?? 0,
                });
              }
              if (domEl.tagName === "UL" || domEl.tagName === "OL") {
                lists.push({
                  id: (el as { id: string }).id,
                  items: domEl.querySelectorAll("li").length,
                });
              }
            }
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { tables, lists, query: structuredParams, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_analyze_cross_app": {
          const crossAppParams = payload.params ?? payload.body ?? {};
          // Cross-app comparison: capture current state for comparison
          const crossSnapshot = await currentBridge.createSnapshotAsync();
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              snapshot: {
                url: window.location.href,
                title: document.title,
                elementCount: crossSnapshot.elements.length,
                elements: crossSnapshot.elements.map((el) => ({
                  id: el.id,
                  type: el.type,
                  label: el.label,
                  state: el.state,
                })),
              },
              query: crossAppParams,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_recovery_attempt": {
          const recoveryParams = payload.params ?? payload.body ?? {};
          const instruction = (recoveryParams.instruction as string) ?? "";
          try {
            const { NLActionExecutor } = await import("ui-bridge/ai");
            const discovered = await currentBridge.discover({ includeHidden: true });
            const executor = new NLActionExecutor();
            executor.updateElements(discovered.elements);
            if (currentBridge.executor) {
              executor.setActionExecutor(currentBridge.executor);
            }
            const result = await executor.execute({
              instruction: instruction || "recover from error state",
            });
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { recovered: true, result, timestamp: Date.now() },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        // ==================================================================
        // Intents
        // ==================================================================

        case "get_intents": {
          const uiBridgeGlobal = getUIBridgeGlobal();
          const intentRegistry = uiBridgeGlobal?.intentRegistry as
            | { getAll?: () => unknown[]; list?: () => unknown[] }
            | undefined;
          const registryIntents = intentRegistry?.getAll?.() ?? intentRegistry?.list?.() ?? [];
          // Merge global registry intents with local store (deduplicated by reference)
          const globalList = Array.isArray(registryIntents) ? registryIntents : [];
          const globalSet = new Set(globalList);
          const localOnly = localIntentStore.filter((intent) => !globalSet.has(intent));
          const allIntents = [...globalList, ...localOnly];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { intents: allIntents },
            timestamp: Date.now(),
          });
          return true;
        }

        case "register_intent": {
          const registerParams = payload.params ?? payload.body ?? {};
          const uiBridgeGlobal2 = getUIBridgeGlobal();
          const intentRegistry2 = uiBridgeGlobal2?.intentRegistry as
            | { register?: (intent: unknown) => unknown; add?: (intent: unknown) => unknown }
            | undefined;
          const registered =
            intentRegistry2?.register?.(registerParams) ?? intentRegistry2?.add?.(registerParams);
          // Persist to local store (cap at max to prevent unbounded growth)
          if (localIntentStore.length >= LOCAL_INTENT_STORE_MAX) {
            localIntentStore.shift();
          }
          localIntentStore.push(registerParams as Record<string, unknown>);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { registered: registered ?? registerParams },
            timestamp: Date.now(),
          });
          return true;
        }

        case "find_intent": {
          const findIntentParams = payload.params ?? payload.body ?? {};
          const findQuery = (findIntentParams.query as string) ?? "";
          const uiBridgeGlobal3 = getUIBridgeGlobal();
          const intentRegistry3 = uiBridgeGlobal3?.intentRegistry as
            | { find?: (query: string) => unknown; search?: (query: string) => unknown }
            | undefined;
          const found =
            intentRegistry3?.find?.(findQuery) ?? intentRegistry3?.search?.(findQuery) ?? null;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { intent: found, query: findQuery },
            timestamp: Date.now(),
          });
          return true;
        }

        case "execute_intent": {
          const execIntentParams = payload.params ?? payload.body ?? {};
          const uiBridgeGlobal4 = getUIBridgeGlobal();
          const intentRegistry4 = uiBridgeGlobal4?.intentRegistry as
            | { execute?: (params: unknown) => Promise<unknown> | unknown }
            | undefined;
          try {
            const result = await Promise.resolve(intentRegistry4?.execute?.(execIntentParams));
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { result: result ?? { note: "Intent registry not available" } },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "execute_intent_from_query": {
          const nlIntentParams = payload.params ?? payload.body ?? {};
          const nlQuery = (nlIntentParams.query as string) ?? "";
          const uiBridgeGlobal5 = getUIBridgeGlobal();
          const intentRegistry5 = uiBridgeGlobal5?.intentRegistry as
            | {
                find?: (query: string) => { id?: string } | null;
                execute?: (params: unknown) => Promise<unknown> | unknown;
              }
            | undefined;
          try {
            const foundIntent = intentRegistry5?.find?.(nlQuery);
            if (foundIntent && intentRegistry5?.execute) {
              const result = await Promise.resolve(
                intentRegistry5.execute({ ...nlIntentParams, intentId: foundIntent.id }),
              );
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { intent: foundIntent, result, query: nlQuery },
                timestamp: Date.now(),
              });
            } else {
              // Fall back to NL action executor
              const { NLActionExecutor } = await import("ui-bridge/ai");
              const discovered = await currentBridge.discover({ includeHidden: true });
              const executor = new NLActionExecutor();
              executor.updateElements(discovered.elements);
              if (currentBridge.executor) {
                executor.setActionExecutor(currentBridge.executor);
              }
              const result = await executor.execute({ instruction: nlQuery });
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { intent: null, result, query: nlQuery, fallback: "nl_executor" },
                timestamp: Date.now(),
              });
            }
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse],
  );
}
