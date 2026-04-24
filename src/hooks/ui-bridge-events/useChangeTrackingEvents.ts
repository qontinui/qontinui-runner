import { useCallback } from "react";
import { handleChangeTrackingCommand } from "../changeTrackingHandler";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

// ---------------------------------------------------------------------------
// Tauri event-name registry (module-global).
//
// The runner frontend fetches the list of Tauri event channel names from
// GET /ui-bridge/sdk/tauri-event-names at startup (see App.tsx) and calls
// `setPendingTauriEventNames(...)`. When the ChangeTracker is lazily created
// for the first change-buffer request, the pending names are applied via
// `ChangeTracker.setTauriEventNames(...)`, so the buffer picks up backend
// events on the next `enable_change_buffer`.
//
// If the ChangeTracker already exists when the frontend receives updated
// names (e.g., on hot-reload), the setter also propagates to it directly.
// ---------------------------------------------------------------------------

let pendingTauriEventNames: string[] = [];
// Weak reference to the live ChangeTracker so updates after creation propagate.
// We keep a function-typed setter rather than a direct ref to avoid adding
// a second source of truth for the tracker instance.
type TrackerSetter = (names: string[]) => void | Promise<void>;
let liveTrackerSetter: TrackerSetter | null = null;

/**
 * Record the list of Tauri event names to subscribe to when the change
 * buffer is enabled. Called once at app startup after discovering names
 * from the runner backend. Idempotent and safe to call multiple times.
 */
export function setPendingTauriEventNames(names: string[]): void {
  // Dedupe: skip when the list hasn't changed. StrictMode double-invokes
  // the TauriEventNamesLoader effect in dev, so this would otherwise call
  // the live setter twice on startup and race the initial subscribe.
  if (
    pendingTauriEventNames.length === names.length &&
    pendingTauriEventNames.every((n, i) => n === names[i])
  ) {
    return;
  }
  pendingTauriEventNames = [...names];
  if (liveTrackerSetter) {
    // Fire-and-forget — the ChangeTracker resubscribes asynchronously.
    void liveTrackerSetter(pendingTauriEventNames);
  }
}

/**
 * Handles: save_bookmark, get_bookmark, delete_bookmark, list_bookmarks,
 *          diff_from_bookmark, execute_with_diff, execute_batch_with_diff, wait_for_change, categorize_last_diff,
 *          scoped_diff, summarize_diff, structured_changes, enable_change_buffer,
 *          disable_change_buffer, drain_change_buffer, get_change_buffer_size,
 *          wait_for_route_change
 */
export function useChangeTrackingEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse" | "changeTrackerRef">,
) {
  const { bridgeRef, sendResponse, changeTrackerRef } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "wait_for_route_change": {
          // Rust forwarder: ui_bridge_wait_for_route_change_handler forwards
          // `{params: body}`. We subscribe to the ChangeTracker's route-change
          // stream and resolve on the first entry matching fromRoute / toRoute
          // (with matchMode = exact/prefix/regex). Falls back to a recent-
          // buffer scan so callers that triggered the navigation *before*
          // calling us still see it.
          //
          // The ChangeTracker is lazy-initialized on first change-tracking
          // command, so `changeTrackerRef.current` may be null here. We
          // synthesize a minimal tracker bootstrap identical to the one in
          // the change-tracking case below but scoped to this request.
          if (!changeTrackerRef.current) {
            // Lazy-init to the same shape as the other cases. We reuse the
            // bootstrap below by falling through once — but because the
            // dispatcher is a switch, easier to just do a minimal init.
            const { ChangeTracker, createSnapshotManager } = await import("@qontinui/ui-bridge/ai");
            const manager = createSnapshotManager({});
            changeTrackerRef.current = new ChangeTracker(
              {
                idleDetector: null,
                createControlSnapshot: () => {
                  const snap = currentBridge.createSnapshot();
                  return {
                    timestamp: Date.now(),
                    elements: snap.elements.map((e) => ({
                      id: e.id,
                      type: e.type,
                      label: e.label ?? "",
                      actions: e.actions,
                      state: e.state,
                    })),
                    components: [],
                    workflows: [],
                    activeRuns: [],
                  };
                },
                refreshElements: () => {},
                snapshotManager: manager,
                executeElementAction: async (id, request) => {
                  const result = await currentBridge.executeAction(id, {
                    action: request.action,
                    params: request.params,
                  });
                  return result;
                },
                resolveScope: (scope: string) => {
                  const container = document.querySelector(scope);
                  if (!container) return null;
                  const ids = new Set<string>();
                  for (const el of currentBridge.elements) {
                    if (el.element && container.contains(el.element as Node)) {
                      ids.add(el.id);
                    }
                  }
                  return ids;
                },
              },
              {
                defaultSettleTimeout: 3000,
                defaultSettleMinStable: 300,
                defaultPollInterval: 200,
              },
            );

            // Subscribe to navigation events so the tracker's route-change
            // stream is populated. (In the main change-tracking branch this
            // is guarded by __routeSubscribed; we use the same guard here so
            // both entry points coexist without double-wiring.)
            const ct0 = changeTrackerRef.current;
            // eslint-disable-next-line @typescript-eslint/no-explicit-any -- introspection on tracker for idempotent init
            const ct0Any = ct0 as any;
            if (!ct0Any.__routeSubscribed) {
              ct0Any.__routeSubscribed = true;
              const bridgeWithRegistry = currentBridge as {
                registry?: {
                  on?: (
                    t: string,
                    cb: (e: { data?: { from?: { url?: string }; to?: { url?: string } } }) => void,
                  ) => () => void;
                };
              };
              const reg = bridgeWithRegistry.registry;
              if (reg?.on) {
                reg.on("navigation:change", (event) => {
                  if (typeof ct0.pushRouteChange !== "function") return;
                  const fromUrl = event?.data?.from?.url ?? "";
                  const toUrl = event?.data?.to?.url ?? "";
                  ct0.pushRouteChange(fromUrl, toUrl, Date.now());
                });
              }
            }
          }

          const ct = changeTrackerRef.current;
          const p = (payload.params ?? payload.body ?? {}) as {
            fromRoute?: string;
            toRoute?: string;
            matchMode?: "exact" | "prefix" | "regex";
            timeoutMs?: number;
          };
          const matchMode = p.matchMode ?? "exact";
          const timeoutMs = Math.min(
            Math.max(typeof p.timeoutMs === "number" ? p.timeoutMs : 5000, 100),
            60_000,
          );

          // Build `toRoute` matcher up front — invalid regex returns an error.
          let toMatcher: ((candidate: string) => boolean) | null = null;
          if (typeof p.toRoute === "string" && p.toRoute.length > 0) {
            const needle = p.toRoute;
            if (matchMode === "exact") {
              toMatcher = (c) => c === needle;
            } else if (matchMode === "prefix") {
              toMatcher = (c) => c.startsWith(needle);
            } else if (matchMode === "regex") {
              try {
                const re = new RegExp(needle);
                toMatcher = (c) => re.test(c);
              } catch (err) {
                await sendResponse({
                  requestId,
                  type,
                  success: false,
                  error: `Invalid regex toRoute: ${
                    err instanceof Error ? err.message : String(err)
                  }`,
                  timestamp: Date.now(),
                });
                return true;
              }
            }
          }
          const fromMatcher =
            typeof p.fromRoute === "string" && p.fromRoute.length > 0
              ? (candidate: string) => candidate === p.fromRoute
              : null;

          const matchEntry = (entry: { from: string; to: string }): boolean => {
            if (fromMatcher && !fromMatcher(entry.from)) return false;
            if (toMatcher && !toMatcher(entry.to)) return false;
            return true;
          };

          const started = Date.now();

          // Fast-path: scan recent buffer.
          if (typeof ct.getRecentRouteChanges === "function") {
            const recent = ct.getRecentRouteChanges(started - timeoutMs);
            for (const entry of recent) {
              if (matchEntry(entry)) {
                await sendResponse({
                  requestId,
                  type,
                  success: true,
                  data: {
                    from: entry.from,
                    to: entry.to,
                    elapsedMs: 0,
                  },
                  timestamp: Date.now(),
                });
                return true;
              }
            }
          }

          const data = await new Promise<Record<string, unknown>>((resolve) => {
            let settled = false;
            let unsubscribe: (() => void) | null = null;
            let timer: ReturnType<typeof setTimeout> | null = null;

            const done = (value: Record<string, unknown>) => {
              if (settled) return;
              settled = true;
              if (timer) clearTimeout(timer);
              unsubscribe?.();
              resolve(value);
            };

            if (typeof ct.subscribeRouteChange === "function") {
              unsubscribe = ct.subscribeRouteChange((evt) => {
                if (!matchEntry(evt)) return;
                done({
                  from: evt.from,
                  to: evt.to,
                  elapsedMs: Date.now() - started,
                });
              });
            }

            timer = setTimeout(() => {
              const history =
                typeof ct.getRecentRouteChanges === "function" ? ct.getRecentRouteChanges() : [];
              const last = history.length > 0 ? history[history.length - 1] : undefined;
              done({
                reason: "timeout",
                lastKnownRoute: last?.to,
                elapsedMs: Date.now() - started,
              });
            }, timeoutMs);
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

        case "save_bookmark":
        case "get_bookmark":
        case "delete_bookmark":
        case "list_bookmarks":
        case "diff_from_bookmark":
        case "execute_with_diff":
        case "execute_batch_with_diff":
        case "wait_for_change":
        case "categorize_last_diff":
        case "scoped_diff":
        case "summarize_diff":
        case "structured_changes":
        case "enable_change_buffer":
        case "disable_change_buffer":
        case "drain_change_buffer":
        case "get_change_buffer_size": {
          const {
            ChangeTracker,
            createSnapshotManager,
            analyzeStructuredChanges: analyzeStructured,
          } = await import("@qontinui/ui-bridge/ai");

          // Lazy-init ChangeTracker singleton
          if (!changeTrackerRef.current) {
            const manager = createSnapshotManager({});
            changeTrackerRef.current = new ChangeTracker(
              {
                idleDetector: null,
                createControlSnapshot: () => {
                  const snap = currentBridge.createSnapshot();
                  return {
                    timestamp: Date.now(),
                    elements: snap.elements.map((e) => ({
                      id: e.id,
                      type: e.type,
                      label: e.label ?? "",
                      actions: e.actions,
                      state: e.state,
                    })),
                    components: [],
                    workflows: [],
                    activeRuns: [],
                  };
                },
                refreshElements: () => {},
                snapshotManager: manager,
                executeElementAction: async (
                  id: string,
                  request: { action: string; params?: Record<string, unknown> },
                ) => {
                  const result = await currentBridge.executeAction(id, {
                    action: request.action,
                    params: request.params,
                  });
                  return result;
                },
                resolveScope: (scope: string) => {
                  const container = document.querySelector(scope);
                  if (!container) return null;
                  const ids = new Set<string>();
                  for (const el of currentBridge.elements) {
                    if (el.element && container.contains(el.element as Node)) {
                      ids.add(el.id);
                    }
                  }
                  return ids;
                },
                // Push-based change observation (allio-inspired):
                // Subscribe to registry element events for event-driven waitForChange.
                // Access via .registry (added to UseUIBridgeReturn) with fallback for older builds.
                subscribeChanges: (() => {
                  const bridge = currentBridge as {
                    registry?: { on?: (type: string, cb: () => void) => () => void };
                  };
                  const reg = bridge.registry;
                  if (!reg?.on) return undefined;
                  const onEvent = reg.on.bind(reg);
                  return (callback: (event: { type: string; timestamp: number }) => void) => {
                    const unsubs = [
                      onEvent("element:registered", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                      onEvent("element:unregistered", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                      onEvent("element:stateChanged", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                    ];
                    return () => unsubs.forEach((u) => u());
                  };
                })(),
              },
              {
                defaultSettleTimeout: 3000,
                defaultSettleMinStable: 300,
                defaultPollInterval: 200,
              },
            );

            // Wire the tauri-event-names registry: seed with any names
            // already discovered, and register a live setter so future
            // updates propagate to this tracker instance.
            const tracker = changeTrackerRef.current as unknown as {
              setTauriEventNames?: (names: string[]) => Promise<void> | void;
            };
            if (typeof tracker.setTauriEventNames === "function") {
              if (pendingTauriEventNames.length > 0) {
                void tracker.setTauriEventNames(pendingTauriEventNames);
              }
              liveTrackerSetter = (names) => tracker.setTauriEventNames!(names);
            }
          }

          const ct = changeTrackerRef.current;

          // P1.3 — Wire SPA route changes into the change buffer. The
          // NavigationTracker dispatches `navigation:change` on the
          // registry; we relay each one as a route-change buffer entry so
          // `/ai/change-buffer/drain` returns SPA navigations interleaved
          // with DOM mutations. Subscription is idempotent: we attach once
          // per ChangeTracker instance and store the unsubscribe on the
          // tracker so a future `disable_change_buffer` doesn't leak it.
          // eslint-disable-next-line @typescript-eslint/no-explicit-any -- introspection on bridge for backward compat
          const ctAny = ct as any;
          if (!ctAny.__routeSubscribed) {
            ctAny.__routeSubscribed = true;
            const bridgeWithRegistry = currentBridge as {
              registry?: {
                on?: (
                  type: string,
                  cb: (event: {
                    data?: { from?: { url?: string }; to?: { url?: string } };
                  }) => void,
                ) => () => void;
              };
            };
            const reg = bridgeWithRegistry.registry;
            if (reg?.on) {
              reg.on("navigation:change", (event) => {
                if (typeof ct.pushRouteChange !== "function") return;
                const fromUrl = event?.data?.from?.url ?? "";
                const toUrl = event?.data?.to?.url ?? "";
                ct.pushRouteChange(fromUrl, toUrl, Date.now());
              });
            }
          }

          const ctResult = await handleChangeTrackingCommand(
            ct,
            type,
            payload as unknown as Record<string, unknown>,
            {
              createSnapshot: () => currentBridge.createSnapshot(),
              createSnapshotManager,
              analyzeStructuredChanges: analyzeStructured,
            },
          );

          await sendResponse({
            requestId,
            type,
            success: true,
            data: ctResult,
            timestamp: Date.now(),
          });
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse, changeTrackerRef],
  );
}
