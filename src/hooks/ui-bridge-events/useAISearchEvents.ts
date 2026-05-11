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
          const { SearchEngine } = await import("@qontinui/ui-bridge/ai");
          // Source elements from the registry — same rationale as `ai_find`
          // below. `discover()`'s DOM scan misses `useUIElement`-registered
          // elements that don't match its interactive heuristics, producing
          // a smaller element set than `/control/snapshot`. Read directly
          // from `currentBridge.registry` so ai_search matches the snapshot
          // path's element population exactly.
          const registry = (
            currentBridge as unknown as {
              registry?: {
                getAllElements: () => Array<unknown>;
              } | null;
            }
          ).registry;
          // `engine.updateElements` accepts `(DiscoveredElement | RegisteredElement)[]`.
          // The registry returns RegisteredElement[]; cast through the engine's
          // own parameter type to keep the boundary explicit. Falls back to an
          // empty array if the registry isn't yet populated (rare race during
          // very early page-load) — `find()` returns `found:false` rather than
          // throwing, which is the correct behaviour.
          const registryElements = (registry?.getAllElements?.() ?? []) as Parameters<
            InstanceType<typeof SearchEngine>["updateElements"]
          >[0];
          // Honor the request's includeHidden flag (forwarded by the Rust
          // /ai/find handler at mcp/ui_bridge/ai.rs which defaults to true).
          // Pass `includeHidden: false` to opt into the visibility filter.
          const includeHidden = (payload.params?.includeHidden as boolean | undefined) ?? true;
          const engine = new SearchEngine({ includeHidden });
          engine.updateElements(registryElements);
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
          const { SearchEngine, find: aiFindFn } = await import("@qontinui/ui-bridge/ai");
          // Source elements from the registry, not from `currentBridge.discover()`.
          //
          // `discover()` runs `executor.find()` which performs its own DOM scan
          // through the action-executor's discovery cache. That cache is built
          // by walking the live DOM and filtering by interactive selectors,
          // which systematically misses `useUIElement`-registered elements
          // whose host nodes don't match the heuristic (e.g. disclosure
          // toggles, custom interactive divs). On this page, that produced 117
          // elements via `discover()` while `/control/snapshot` reported 193 —
          // the same divergence between snapshot and ai/find that previous
          // commits had been chasing. The registry is the canonical source of
          // truth for snapshots; routing ai/find through the same source
          // unifies the two paths so `useUIElement` registrations are visible
          // to find() too.
          //
          // We bind to `currentBridge.registry` (the React-context registry,
          // returned from `useUIBridge` via `bridge.registry`). After the
          // ui-bridge `globalThis`-keyed singleton fix, `bridge.registry` and
          // `getGlobalRegistry()` are guaranteed to be the same instance, so
          // either accessor would work — we use `currentBridge.registry` here
          // because the surrounding handler already accesses bridge state
          // through the same reference (see `wait_for_element_registered`
          // below) and reading the React-context registry avoids the
          // import-time dependency on the singleton being initialised.
          const registry = (
            currentBridge as unknown as {
              registry?: {
                getAllElements: () => Array<unknown>;
              } | null;
            }
          ).registry;
          // `engine.updateElements` accepts `(DiscoveredElement | RegisteredElement)[]`.
          // The registry returns RegisteredElement[]; cast through the engine's
          // own parameter type to keep the boundary explicit. Falls back to an
          // empty array if the registry isn't yet populated (rare race during
          // very early page-load) — `find()` returns `found:false` rather than
          // throwing, which is the correct behaviour.
          const registryElements = (registry?.getAllElements?.() ?? []) as Parameters<
            InstanceType<typeof SearchEngine>["updateElements"]
          >[0];
          // Honor the request's includeHidden flag (forwarded by the Rust
          // /ai/find handler at mcp/ui_bridge/ai.rs which defaults to true).
          // Pass `includeHidden: false` to opt into the visibility filter.
          const includeHidden = (payload.params?.includeHidden as boolean | undefined) ?? true;
          const engine = new SearchEngine({ includeHidden });
          engine.updateElements(registryElements);

          const query = (payload.params?.query as string) ?? "";
          const context = payload.params?.context as Record<string, unknown> | undefined;
          const confidenceThreshold = payload.params?.confidenceThreshold as number | undefined;
          // B2 — literal-text precedence + strict mode. When the caller
          // passes `strict: true` (forwarded by the Rust /ai/find handler
          // from `?strict=true` / `"strict": true` in the JSON body), the
          // SDK matcher returns ONLY exact-match candidates. Default
          // behaviour (no flag) keeps the fuzzy path but exact matches
          // still outrank alias-only candidates via the SearchEngine's
          // literal-precedence pass.
          const strict = payload.params?.strict === true;

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
          if (strict) {
            // `strict` is a Phase B2 addition on `FindOptions`; cast through
            // an indexed-property type so we don't depend on the deployed
            // @qontinui/ui-bridge typings carrying the new field yet. Once
            // the SDK bump lands, the cast collapses to a no-op.
            (findOpts as Record<string, unknown>).strict = true;
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

        case "rank_elements": {
          const { findElements } = await import("@qontinui/ui-bridge");
          // Source elements from `createSnapshot()` — NOT `registry.getAllElements()`.
          // `findElements()` echoes each input back as `m.element`; if we
          // pass live `RegisteredElement[]` (which carry an
          // `element: HTMLElement` field), the response contains DOM nodes
          // that can't be JSON-serialized through the IPC boundary, the
          // React handler never `sendResponse`s, and the Rust side hits the
          // 10s `ui_bridge_request_sync` timeout on every non-empty result.
          // `BridgeSnapshot.elements` is the same set as the registry but
          // pre-serialized to flat FindableElement-shaped POJOs, matching
          // the SDK relay-handlers.ts:472-491 pattern (which uses
          // `latestControlSnapshot.elements`).
          const snapshot = currentBridge.createSnapshot();
          const elements = snapshot.elements as Parameters<typeof findElements>[0];
          const query = (payload.params || {}) as Parameters<typeof findElements>[1];
          const matches = findElements(elements, query);
          const ranked = matches.map((m) => ({
            id: m.id,
            score: m.score,
            reasons: m.reasons,
            element: m.element,
          }));
          await sendResponse({
            requestId,
            type,
            success: true,
            data: ranked,
            timestamp: Date.now(),
          });
          return true;
        }

        case "wait_for_element_registered": {
          // SDK-shape wait-for-element: body carries a `predicate` object
          // (id, label, testId, selector) plus optional `requirement` and
          // polling knobs. Rust forwards the params; we implement the
          // matching loop inline using the live registry to avoid pulling
          // `@qontinui/ui-bridge/server/handlers` at runtime (the server
          // subpath isn't published in the runtime bundle).
          const p = (payload.params ?? payload.body ?? {}) as {
            predicate?: {
              id?: string;
              label?: string;
              testId?: string;
              selector?: string;
            };
            requirement?: "registered" | "visible" | "has-layout";
            pollMs?: number;
            timeoutMs?: number;
          };
          const predicate = p.predicate ?? {};
          const requirement = p.requirement ?? "registered";
          const pollMs = Math.min(
            Math.max(typeof p.pollMs === "number" ? p.pollMs : 100, 50),
            1000,
          );
          const timeoutMs = Math.min(
            Math.max(typeof p.timeoutMs === "number" ? p.timeoutMs : 5000, 100),
            60_000,
          );
          const labelNeedle =
            typeof predicate.label === "string" && predicate.label.length > 0
              ? predicate.label.toLowerCase()
              : null;

          const predicateMatches = (
            el: Record<string, unknown>,
            domEl: HTMLElement | null,
          ): boolean => {
            if (typeof predicate.id === "string" && predicate.id.length > 0) {
              if (el.id !== predicate.id) return false;
            }
            if (labelNeedle) {
              const label = typeof el.label === "string" ? el.label.toLowerCase() : "";
              const ariaLabel =
                typeof el.ariaLabel === "string" ? (el.ariaLabel as string).toLowerCase() : "";
              if (!label.includes(labelNeedle) && !ariaLabel.includes(labelNeedle)) {
                return false;
              }
            }
            if (typeof predicate.testId === "string" && predicate.testId.length > 0) {
              const testId = domEl?.getAttribute?.("data-testid");
              if (testId !== predicate.testId) return false;
            }
            return true;
          };

          const requirementMet = (
            el: Record<string, unknown>,
            domEl: HTMLElement | null,
          ): boolean => {
            if (requirement === "registered") return true;
            const state = el.state as
              | { visible?: boolean; rect?: { width?: number; height?: number } }
              | undefined;
            if (requirement === "visible") {
              if (state && typeof state.visible === "boolean") return state.visible;
              if (!domEl) return false;
              if (domEl.offsetParent === null) return false;
              const rect = domEl.getBoundingClientRect();
              return rect.width > 0 && rect.height > 0;
            }
            if (requirement === "has-layout") {
              const w = state?.rect?.width ?? 0;
              const h = state?.rect?.height ?? 0;
              if (w > 0 && h > 0) return true;
              if (domEl) {
                const rect = domEl.getBoundingClientRect();
                return rect.width > 0 && rect.height > 0;
              }
              return false;
            }
            return true;
          };

          const started = Date.now();

          const attempt = (): {
            element: Record<string, unknown>;
            domEl: HTMLElement | null;
          } | null => {
            const registry = (
              currentBridge as unknown as {
                registry?: {
                  getAllElements?: () => Array<{
                    id: string;
                    element?: HTMLElement;
                    type?: unknown;
                    label?: string;
                  }>;
                } | null;
              }
            ).registry;
            const rows = registry?.getAllElements?.() ?? [];
            for (const row of rows) {
              const domEl = (row.element as HTMLElement | null) ?? null;
              const materialized: Record<string, unknown> = {
                id: row.id,
                type: row.type,
                label: row.label,
                ariaLabel: domEl?.getAttribute?.("aria-label") ?? undefined,
                state: undefined,
              };
              if (!predicateMatches(materialized, domEl)) continue;
              if (!requirementMet(materialized, domEl)) continue;
              return { element: materialized, domEl };
            }
            // DOM-selector fallback when the registry can't satisfy the predicate.
            if (typeof predicate.selector === "string" && predicate.selector.length > 0) {
              try {
                const domEl = document.querySelector(predicate.selector) as HTMLElement | null;
                if (domEl) {
                  const syntheticEl: Record<string, unknown> = {
                    id: domEl.id || `dom-${predicate.selector}`,
                    label:
                      domEl.getAttribute("aria-label") ?? domEl.textContent?.trim() ?? undefined,
                    type: domEl.tagName?.toLowerCase?.(),
                    ariaLabel: domEl.getAttribute("aria-label") ?? undefined,
                  };
                  if (!requirementMet(syntheticEl, domEl)) return null;
                  return { element: syntheticEl, domEl };
                }
              } catch {
                // Invalid selector — treat as no match.
              }
            }
            return null;
          };

          const fast = attempt();
          if (fast) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                element: fast.element,
                elapsedMs: Date.now() - started,
              },
              timestamp: Date.now(),
            });
            return true;
          }

          const data = await new Promise<Record<string, unknown>>((resolve) => {
            let done = false;
            let lastPartial: Record<string, unknown> | undefined;
            const tick = () => {
              if (done) return;
              const hit = attempt();
              if (hit) {
                done = true;
                resolve({
                  element: hit.element,
                  elapsedMs: Date.now() - started,
                });
                return;
              }
              if (requirement !== "registered") {
                // Track a "closest match" — an element that matched the
                // predicate but failed the requirement (visibility/layout).
                // Helps callers debug why their wait timed out.
                try {
                  const registry = (
                    currentBridge as unknown as {
                      registry?: {
                        getAllElements?: () => Array<{
                          id: string;
                          element?: HTMLElement;
                          type?: unknown;
                          label?: string;
                        }>;
                      } | null;
                    }
                  ).registry;
                  const rows = registry?.getAllElements?.() ?? [];
                  for (const row of rows) {
                    const domEl = (row.element as HTMLElement | null) ?? null;
                    const materialized: Record<string, unknown> = {
                      id: row.id,
                      type: row.type,
                      label: row.label,
                      ariaLabel: domEl?.getAttribute?.("aria-label") ?? undefined,
                    };
                    if (predicateMatches(materialized, domEl)) {
                      lastPartial = materialized;
                      break;
                    }
                  }
                } catch {
                  /* ignore */
                }
              }
              if (Date.now() - started >= timeoutMs) {
                done = true;
                const timeout: Record<string, unknown> = {
                  reason: "timeout",
                  elapsedMs: Date.now() - started,
                };
                if (lastPartial) timeout.closestMatch = lastPartial;
                resolve(timeout);
                return;
              }
              setTimeout(tick, pollMs);
            };
            setTimeout(tick, pollMs);
          });

          await sendResponse({
            requestId,
            type,
            success: true,
            data,
            timestamp: Date.now(),
          });
          return true;
        }

        case "wait_for_element_state_predicate": {
          // M1 — element-level state polling. Shares the same registry as
          // `wait_for_element_registered` but evaluates a richer set of
          // declarative predicates (visible / enabled / value-not-empty / ...)
          // exported from `@qontinui/ui-bridge/ai`.
          //
          // The Rust handler validates + clamps the body before forwarding;
          // we still defensively re-clamp here in case the SDK ever evolves
          // independently. Predicate logic comes from the SDK to keep the
          // SDK's own unit tests authoritative.
          const {
            evaluateElementPredicate: _evaluateElementPredicate,
            pollWaitForElement,
            snapshotFromRegisteredElement,
          } = await import("@qontinui/ui-bridge/ai");

          const p = (payload.params ?? payload.body ?? {}) as {
            elementId?: string;
            selector?: string;
            state?: string;
            timeoutMs?: number;
            pollMs?: number;
          };
          const elementId =
            typeof p.elementId === "string" && p.elementId.length > 0 ? p.elementId : null;
          const selector =
            typeof p.selector === "string" && p.selector.length > 0 ? p.selector : null;
          const predicateState = p.state as Parameters<typeof _evaluateElementPredicate>[1];
          const timeoutMs = Math.min(
            Math.max(typeof p.timeoutMs === "number" ? p.timeoutMs : 5000, 0),
            30_000,
          );
          const pollMs = Math.max(typeof p.pollMs === "number" ? p.pollMs : 50, 10);

          // Project a registry row → snapshot. Re-resolved each poll so
          // mid-wait re-renders / re-registrations are picked up.
          const findRegistered = (): {
            row: {
              id: string;
              type?: unknown;
              label?: string;
              element?: HTMLElement;
              getState?: () => unknown;
            } | null;
          } => {
            const registry = (
              currentBridge as unknown as {
                registry?: {
                  getElement?: (id: string) => unknown;
                  getAllElements?: () => Array<{
                    id: string;
                    type?: unknown;
                    label?: string;
                    element?: HTMLElement;
                    getState?: () => unknown;
                  }>;
                } | null;
              }
            ).registry;
            if (!registry) return { row: null };

            // 1) elementId direct lookup.
            if (elementId && registry.getElement) {
              const direct = registry.getElement(elementId) as {
                id: string;
                type?: unknown;
                label?: string;
                element?: HTMLElement;
                getState?: () => unknown;
              } | null;
              if (direct) return { row: direct };
            }
            // 2) Selector → DOM → reverse-lookup by data-ui-bridge-id, falling
            // back to a synthetic row if nothing in the registry matches.
            if (selector) {
              try {
                const dom = document.querySelector(selector) as HTMLElement | null;
                if (dom) {
                  const bridgeId = dom.getAttribute("data-ui-bridge-id");
                  if (bridgeId && registry.getElement) {
                    const matched = registry.getElement(bridgeId) as {
                      id: string;
                      type?: unknown;
                      label?: string;
                      element?: HTMLElement;
                      getState?: () => unknown;
                    } | null;
                    if (matched) return { row: matched };
                  }
                }
              } catch {
                /* invalid selector — treat as miss */
              }
            }
            return { row: null };
          };

          const takeSnapshot = () => {
            const { row } = findRegistered();
            // `snapshotFromRegisteredElement` tolerates `null` and surfaces
            // `{ registered: false, state: null }` for the absent path.
            return snapshotFromRegisteredElement(
              (row as unknown as Parameters<typeof snapshotFromRegisteredElement>[0]) ?? null,
            );
          };

          const outcome = await pollWaitForElement({
            takeSnapshot,
            predicate: predicateState,
            timeoutMs,
            pollMs,
          });

          // Project the registered element (if any) into the response shape.
          const finalRow = findRegistered().row;
          const serialize = (
            row: typeof finalRow,
            snap: ReturnType<typeof takeSnapshot> | null,
          ) => ({
            id: row?.id ?? elementId ?? "",
            type: row?.type,
            label: row?.label,
            registered: snap?.registered ?? false,
            fromRegistry: snap?.registered ?? false,
            state: snap?.state ?? null,
          });

          if (outcome.found) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                found: true,
                durationMs: outcome.durationMs,
                finalState: serialize(finalRow, outcome.observed),
              },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                found: false,
                durationMs: outcome.durationMs,
                lastObservedState: outcome.observed ? serialize(finalRow, outcome.observed) : null,
              },
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "wait_for_element_by_condition": {
          // Tier 3.1 companion to the Rust forwarder
          // (qontinui-runner/src-tauri/src/mcp/ui_bridge.rs::ui_bridge_wait_for_element_condition_handler).
          // The Rust handler forwards {params: body}; we poll the registry here and report
          // {matched, element, waited_ms} back. Rust then converts no-match to HTTP 408.
          //
          // The matcher is inlined (not imported from ui-bridge/server) because the
          // server subpath isn't published in the runtime bundle.
          const matches = (
            el: {
              id: string;
              type?: unknown;
              label?: unknown;
              ariaLabel?: unknown;
              title?: unknown;
            },
            sel: { id?: string; title?: string; aria_label?: string; text?: string; type?: string },
          ): boolean => {
            if (sel.id && el.id !== sel.id) return false;
            if (sel.title) {
              const needle = sel.title.toLowerCase();
              const t = ((el.title as string | undefined) ?? "").toLowerCase();
              const a = ((el.ariaLabel as string | undefined) ?? "").toLowerCase();
              const l = ((el.label as string | undefined) ?? "").toLowerCase();
              if (!t.includes(needle) && !a.includes(needle) && !l.includes(needle)) return false;
            }
            if (sel.aria_label) {
              const needle = sel.aria_label.toLowerCase();
              const a = ((el.ariaLabel as string | undefined) ?? "").toLowerCase();
              const l = ((el.label as string | undefined) ?? "").toLowerCase();
              if (!a.includes(needle) && !l.includes(needle)) return false;
            }
            if (sel.text) {
              const needle = sel.text.toLowerCase();
              const l = ((el.label as string | undefined) ?? "").toLowerCase();
              const i = el.id.toLowerCase();
              if (!l.includes(needle) && !i.includes(needle)) return false;
            }
            return true;
          };
          const p = (payload.params ?? payload.body ?? {}) as {
            selector?: Record<string, unknown>;
            condition?: string;
            timeout_ms?: number;
            text_match?: string;
          };
          const selector = (p.selector ?? {}) as {
            id?: string;
            title?: string;
            aria_label?: string;
            text?: string;
            type?: string;
          };
          const condition = p.condition ?? "present";
          const timeoutMs = Math.min(
            Math.max(typeof p.timeout_ms === "number" ? p.timeout_ms : 5000, 100),
            60_000,
          );
          const textMatch = p.text_match;
          const start = Date.now();
          const pollMs = 100;

          const checkCondition = (el: Record<string, unknown>, domEl: HTMLElement | null) => {
            switch (condition) {
              case "present":
                return true;
              case "visible":
                if (!domEl) return false;
                if (domEl.offsetParent === null) return false;
                return (
                  domEl.getBoundingClientRect().width > 0 &&
                  domEl.getBoundingClientRect().height > 0
                );
              case "clickable":
                if (!domEl) return false;
                if (domEl.offsetParent === null) return false;
                if (
                  (domEl as HTMLButtonElement | HTMLInputElement).disabled ||
                  domEl.getAttribute("aria-disabled") === "true"
                )
                  return false;
                return (
                  domEl.getBoundingClientRect().width > 0 &&
                  domEl.getBoundingClientRect().height > 0
                );
              case "text-matches": {
                if (!textMatch) return true;
                const needle = textMatch.toLowerCase();
                const label = ((el.label as string | undefined) ?? "").toLowerCase();
                const aria = ((el.ariaLabel as string | undefined) ?? "").toLowerCase();
                const tt = ((el.title as string | undefined) ?? "").toLowerCase();
                const tc = (domEl?.textContent ?? "").toLowerCase();
                return (
                  label.includes(needle) ||
                  aria.includes(needle) ||
                  tt.includes(needle) ||
                  tc.includes(needle)
                );
              }
              default:
                return true;
            }
          };

          // FIX: Poll the registry directly (same source `GET /control/elements` uses
          // via createSnapshotAsync), not `currentBridge.discover()`.
          //
          // `discover()` delegates to `executor.find()` which does a fresh DOM scan +
          // filters + async batching on every call. On a freshly-spawned runner, back-
          // to-back discover() calls can return stale/empty results while mutations
          // settle — the exact flake this handler hit (HTTP 408 while the element was
          // already in the registry one second earlier).
          //
          // `registry.getAllElements()` is a synchronous `Array.from(Map.values())` —
          // always fresh and authoritative. If the live DOM element is still attached,
          // we pull ariaLabel/title off it; otherwise we use the element's cached label.
          //
          // We bind to `currentBridge.registry` (the same registry the bridge is wired
          // to) rather than calling `getGlobalRegistry()` from ui-bridge directly,
          // because the latter has been observed to hand back a different instance in
          // some bundling configurations.
          const pollOnce = () => {
            const registry = (
              currentBridge as unknown as {
                registry?: {
                  getAllElements: () => Array<{
                    id: string;
                    element: HTMLElement;
                    type?: unknown;
                    label?: string;
                  }>;
                } | null;
              }
            ).registry;
            const registryElements = registry?.getAllElements?.() ?? [];
            for (const el of registryElements) {
              const domEl = (el.element as HTMLElement | null) ?? null;
              const materialized = {
                id: el.id,
                type: el.type,
                label: el.label,
                ariaLabel: domEl?.getAttribute("aria-label") ?? undefined,
                title: domEl?.getAttribute("title") ?? undefined,
              };
              if (!matches(materialized, selector)) continue;
              if (
                selector.type &&
                materialized.type !== selector.type &&
                !(domEl?.tagName ?? "").toLowerCase().includes(selector.type.toLowerCase())
              ) {
                continue;
              }
              if (checkCondition(materialized, domEl)) {
                return {
                  matched: true,
                  element: materialized,
                  waited_ms: Date.now() - start,
                };
              }
            }
            return null;
          };

          const result = await new Promise<{
            matched: boolean;
            element?: unknown;
            waited_ms: number;
          }>((resolve) => {
            const tick = () => {
              try {
                // First poll runs immediately (no initial 100ms wait). If the
                // registry is empty right now, we don't give up — we keep
                // polling until timeout_ms (the element may still be
                // registering on a freshly-navigated page).
                const hit = pollOnce();
                if (hit) {
                  resolve(hit);
                  return;
                }
              } catch {
                /* swallow and retry */
              }
              if (Date.now() - start >= timeoutMs) {
                resolve({ matched: false, waited_ms: Date.now() - start });
                return;
              }
              setTimeout(tick, pollMs);
            };
            tick();
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

        case "ai_execute": {
          const { NLActionExecutor } = await import("@qontinui/ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const executor = new NLActionExecutor();
          executor.updateElements(discovered.elements);
          // Delegate DOM actions to the bridge (UseUIBridgeReturn is API-compatible)
          executor.setActionExecutor(currentBridge as never);
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
          const { AssertionExecutor, parseNLAssertion } = await import("@qontinui/ui-bridge/ai");
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
          const { AssertionExecutor, parseNLAssertion } = await import("@qontinui/ui-bridge/ai");
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
          const { SemanticSnapshotManager } = await import("@qontinui/ui-bridge/ai");
          const controlSnapshot = await currentBridge.createSnapshotAsync();
          const maxTokens = typeof payload?.maxTokens === "number" ? payload.maxTokens : 0;
          const manager = new SemanticSnapshotManager({ maxTokens });
          const snapshot = manager.createSnapshot(controlSnapshot as never, {
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
          const { generatePageSummary, SemanticSnapshotManager } =
            await import("@qontinui/ui-bridge/ai");
          const controlSnapshot = await currentBridge.createSnapshotAsync();
          // Convert to AI elements via snapshot manager
          const manager = new SemanticSnapshotManager();
          const semanticSnapshot = manager.createSnapshot(controlSnapshot as never, {
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
          const { SearchEngine } = await import("@qontinui/ui-bridge/ai");
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
          const { SemanticSnapshotManager } = await import("@qontinui/ui-bridge/ai");
          const diffSnapshot = await currentBridge.createSnapshotAsync();
          const diffManager = new SemanticSnapshotManager();
          const currentSemantic = diffManager.createSnapshot(diffSnapshot as never, {
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
            const { NLActionExecutor } = await import("@qontinui/ui-bridge/ai");
            const discovered = await currentBridge.discover({ includeHidden: true });
            const executor = new NLActionExecutor();
            executor.updateElements(discovered.elements);
            // Delegate DOM actions to the bridge (UseUIBridgeReturn is API-compatible)
            executor.setActionExecutor(currentBridge as never);
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

        case "delete_intent": {
          const { name } = (payload.params || {}) as { name?: string };
          if (!name) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "name is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const uiBridgeGlobal = getUIBridgeGlobal();
          const intentRegistry = uiBridgeGlobal?.intentRegistry as
            | { delete?: (name: string) => boolean; remove?: (name: string) => boolean }
            | undefined;
          const removedFromRegistry =
            intentRegistry?.delete?.(name) ?? intentRegistry?.remove?.(name) ?? false;
          const beforeLen = localIntentStore.length;
          for (let i = localIntentStore.length - 1; i >= 0; i--) {
            const entry = localIntentStore[i] as { name?: string; id?: string };
            if (entry.name === name || entry.id === name) localIntentStore.splice(i, 1);
          }
          const removedFromLocal = localIntentStore.length < beforeLen;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { deleted: removedFromRegistry || removedFromLocal },
            timestamp: Date.now(),
          });
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
              const { NLActionExecutor } = await import("@qontinui/ui-bridge/ai");
              const discovered = await currentBridge.discover({ includeHidden: true });
              const executor = new NLActionExecutor();
              executor.updateElements(discovered.elements);
              // Delegate DOM actions to the bridge (UseUIBridgeReturn is API-compatible)
              executor.setActionExecutor(currentBridge as never);
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
