import { useCallback } from "react";
import { getGlobalSpecStore } from "ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";
import { getAllSpecs } from "../../lib/spec-registry";

// ---------------------------------------------------------------------------
// Browser / Console capture type interfaces
// ---------------------------------------------------------------------------

/** Subset of BrowserCapture methods used by event handlers */
interface BrowserCaptureAPI {
  getByType: (type: string) => unknown[];
  getSince: (ts: number) => unknown[];
  getRecent: (n: number) => unknown[];
  getConsoleRecent: (n: number) => unknown[];
  getConsoleSince: (ts: number) => unknown[];
  getMemoryTrend?: () => unknown;
  getFrameworkOverlays?: () => unknown;
}

interface ConsoleCaptureAPI {
  getSince: (ts: number) => unknown[];
  getRecent: (n?: number) => unknown[];
  clear: () => void;
}

interface UndoTrackerAPI {
  getSnapshotUndoContext: () => unknown;
  executeUndo?: () => boolean;
  executeRedo?: () => boolean;
}

/** Get the browserCapture from the UI Bridge global, cast to the known API. */
function getBrowserCapture(): BrowserCaptureAPI | undefined {
  return getUIBridgeGlobal()?.browserCapture as BrowserCaptureAPI | undefined;
}

/** Get the consoleCapture from the UI Bridge global. */
function getConsoleCapture(): ConsoleCaptureAPI | undefined {
  return getUIBridgeGlobal()?.consoleCapture as ConsoleCaptureAPI | undefined;
}

/** Get the undoTracker from the UI Bridge global. */
function getUndoTracker(): UndoTrackerAPI | undefined {
  return getUIBridgeGlobal()?.undoTracker as UndoTrackerAPI | undefined;
}

/** Filters bridge elements to form-related types and runs discoverForms. */
async function discoverBridgeForms(bridge: {
  elements: Array<{
    id: string;
    element: Element;
    type: string;
    label?: string;
    getState: () => unknown;
  }>;
}) {
  const { discoverForms } = await import("ui-bridge/ai");
  const formElements = bridge.elements
    .filter((el) => ["input", "select", "textarea", "checkbox", "radio"].includes(el.type))
    .map((el) => ({
      id: el.id,
      element: el.element,
      type: el.type,
      label: el.label,
      getState: () => el.getState(),
    }));
  return discoverForms(formElements);
}

// ---------------------------------------------------------------------------
// Module-level state for error sessions and baselines
// (mirrors the pattern in ui-bridge commandHandlers.ts)
// ---------------------------------------------------------------------------

interface ErrorSession {
  id: string;
  label?: string;
  startTime: number;
  endTime?: number;
  errors: unknown[];
}

interface ErrorBaseline {
  label: string;
  timestamp: number;
  errors: unknown[];
}

let currentErrorSession: ErrorSession | null = null;
const errorSessions: ErrorSession[] = [];
const errorBaselines = new Map<string, ErrorBaseline>();

/**
 * Handles: get_console_errors, clear_console_errors, get_specs, get_spec,
 *          get_undo_state, undo, redo, get_element_state, get_forms,
 *          get_browser_events, get_timeline, get_health_report, get_network_chains,
 *          get_error_snapshots, start_error_session, end_error_session,
 *          get_error_sessions, capture_error_baseline, compare_error_baseline,
 *          get_error_report, get_action_history, get_interaction_metrics,
 *          get_performance_entries, clear_performance_entries,
 *          get_element_tree, highlight_element
 */
export function useDebugInspectEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "get_console_errors": {
          // Use browserCapture (not consoleCapture) to access console-specific filters
          const capture = getBrowserCapture();

          if (!capture) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { errors: [], count: 0, note: "BrowserCapture not installed" },
              timestamp: Date.now(),
            });
            return true;
          }

          const since = payload.params?.since as number | undefined;
          const limit = payload.params?.limit as number | undefined;
          // Use console-specific methods to filter out navigation/long-task events
          const errors = since
            ? capture.getConsoleSince(since)
            : capture.getConsoleRecent(limit ?? 50);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { errors, count: errors.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "clear_console_errors": {
          const capture2 = getConsoleCapture();

          if (capture2) {
            capture2.clear();
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { cleared: !!capture2 },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_specs": {
          // Read from the globalThis-backed SpecStore (populated by
          // BundledSpecsLoader). Falls back to the spec-registry module
          // directly if the store is empty (e.g. after HMR invalidation).
          const store = getGlobalSpecStore();
          const allConfigs = store.getAll();
          let specs: Array<{ specId: string; config: unknown }> = [];
          for (const [id, config] of allConfigs) {
            specs.push({ specId: id, config });
          }
          if (specs.length === 0) {
            const bundled = getAllSpecs();
            specs = bundled.map((s) => ({ specId: s.specId, config: s.config }));
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { specs, count: specs.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_spec": {
          const { specId } = payload;
          if (!specId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "specId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const specConfig = getGlobalSpecStore().get(specId);
          if (!specConfig) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Spec not found: ${specId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { specId, config: specConfig },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_undo_state": {
          const undoTracker = getUndoTracker();

          await sendResponse({
            requestId,
            type,
            success: true,
            data: undoTracker?.getSnapshotUndoContext() ?? {
              note: "UndoTracker not installed",
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "undo": {
          const undoTrackerUndo = getUndoTracker();
          const undoState = undoTrackerUndo?.getSnapshotUndoContext() as
            | { undoAvailable?: boolean }
            | undefined;

          if (undoTrackerUndo?.executeUndo) {
            if (undoState && undoState.undoAvailable === false) {
              await sendResponse({
                requestId,
                type,
                success: true,
                data: {
                  success: false,
                  noOp: true,
                  reason: "Nothing to undo",
                  method: "undoTracker",
                },
                timestamp: Date.now(),
              });
            } else {
              const result = undoTrackerUndo.executeUndo();
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { success: result, method: "undoTracker" },
                timestamp: Date.now(),
              });
            }
          } else {
            // Fallback: simulate Ctrl+Z keyboard shortcut
            document.dispatchEvent(
              new KeyboardEvent("keydown", { key: "z", ctrlKey: true, bubbles: true }),
            );
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, method: "keyboard" },
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "redo": {
          const undoTrackerRedo = getUndoTracker();
          const redoState = undoTrackerRedo?.getSnapshotUndoContext() as
            | { redoAvailable?: boolean }
            | undefined;

          if (undoTrackerRedo?.executeRedo) {
            if (redoState && redoState.redoAvailable === false) {
              await sendResponse({
                requestId,
                type,
                success: true,
                data: {
                  success: false,
                  noOp: true,
                  reason: "Nothing to redo",
                  method: "undoTracker",
                },
                timestamp: Date.now(),
              });
            } else {
              const result = undoTrackerRedo.executeRedo();
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { success: result, method: "undoTracker" },
                timestamp: Date.now(),
              });
            }
          } else {
            // Fallback: simulate Ctrl+Shift+Z keyboard shortcut
            document.dispatchEvent(
              new KeyboardEvent("keydown", {
                key: "z",
                ctrlKey: true,
                shiftKey: true,
                bubbles: true,
              }),
            );
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, method: "keyboard" },
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "get_element_state": {
          const { elementId: stateId } = payload;
          if (!stateId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const stateEl = currentBridge.getElement(stateId);
          if (!stateEl) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found: ${stateId}`,
              timestamp: Date.now(),
            });
            return true;
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: stateEl.getState(),
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_forms": {
          const formsResult = await discoverBridgeForms(currentBridge);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: formsResult,
            timestamp: Date.now(),
          });
          return true;
        }

        case "fill_form": {
          const fillParams = payload.params ?? payload.body ?? {};
          try {
            const result = await currentBridge.executor.fillForm(fillParams as never);
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

        case "snapshot_forms": {
          const snapshot = await discoverBridgeForms(currentBridge);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { snapshot, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Browser Events & Timeline
        // ==================================================================

        case "get_browser_events": {
          const cap = getBrowserCapture();

          if (!cap) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { events: [], timestamp: Date.now() },
              timestamp: Date.now(),
            });
            return true;
          }

          const evtType = payload.params?.type as string | undefined;
          const evtSince = payload.params?.since as number | undefined;
          const evtLimit = (payload.params?.limit as number) ?? 100;
          let events: unknown[];
          if (evtType) events = cap.getByType(evtType);
          else if (evtSince) events = cap.getSince(evtSince);
          else events = cap.getRecent(evtLimit);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { events: events.slice(0, evtLimit), timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_timeline": {
          const cap2 = getBrowserCapture();

          if (!cap2) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { events: [], timestamp: Date.now() },
              timestamp: Date.now(),
            });
            return true;
          }

          const tlSince = payload.params?.since as number | undefined;
          const tlLimit = (payload.params?.limit as number) ?? 100;
          const tlEvents = tlSince ? cap2.getSince(tlSince) : cap2.getRecent(tlLimit);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { events: tlEvents.slice(0, tlLimit), timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_health_report": {
          const capHealth = getBrowserCapture();

          const hrErrors = capHealth ? capHealth.getConsoleRecent(50) : [];
          const memTrend = capHealth?.getMemoryTrend?.() ?? null;
          const overlays = capHealth?.getFrameworkOverlays?.() ?? null;

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              healthy: hrErrors.length === 0,
              errorCount: hrErrors.length,
              recentErrors: hrErrors.slice(0, 10),
              memoryTrend: memTrend,
              frameworkOverlays: overlays,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_network_chains": {
          const capChains = getBrowserCapture();

          if (!capChains) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { chains: [], timestamp: Date.now() },
              timestamp: Date.now(),
            });
            return true;
          }

          const ncLimit = (payload.params?.limit as number) ?? 50;
          const networkEvents = capChains.getByType("network");

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { chains: networkEvents.slice(-ncLimit), timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_error_snapshots": {
          const capSnaps = getBrowserCapture();

          const esLimit = (payload.params?.limit as number) ?? 10;
          const snapErrors = capSnaps ? capSnaps.getConsoleRecent(esLimit) : [];

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { snapshots: snapErrors, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Error Sessions
        // ==================================================================

        case "start_error_session": {
          const sessionLabel = payload.params?.label as string | undefined;
          const capSes = getBrowserCapture();

          currentErrorSession = {
            id: `session-${Date.now()}`,
            label: sessionLabel,
            startTime: Date.now(),
            errors: [],
          };
          if (capSes) {
            currentErrorSession.errors = [...capSes.getConsoleRecent(100)];
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              sessionId: currentErrorSession.id,
              startTime: currentErrorSession.startTime,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "end_error_session": {
          if (!currentErrorSession) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "No active error session",
              timestamp: Date.now(),
            });
            return true;
          }

          currentErrorSession.endTime = Date.now();
          const capEnd = getBrowserCapture();
          const currentErrors = capEnd ? capEnd.getConsoleSince(currentErrorSession.startTime) : [];
          const session = {
            ...currentErrorSession,
            newErrors: currentErrors,
            duration: currentErrorSession.endTime - currentErrorSession.startTime,
          };
          errorSessions.push(session);
          currentErrorSession = null;

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { session, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_error_sessions": {
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              sessions: errorSessions,
              activeSession: currentErrorSession,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Error Baselines
        // ==================================================================

        case "capture_error_baseline": {
          const blLabel = payload.params?.label as string;
          if (!blLabel) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "label is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const capBl = getBrowserCapture();
          const blErrors = capBl ? capBl.getConsoleRecent(200) : [];
          errorBaselines.set(blLabel, {
            label: blLabel,
            timestamp: Date.now(),
            errors: blErrors,
          });

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              success: true,
              label: blLabel,
              errorCount: blErrors.length,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "compare_error_baseline": {
          const cmpLabel = payload.params?.label as string;
          if (!cmpLabel) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "label is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const baseline = errorBaselines.get(cmpLabel);
          if (!baseline) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Baseline '${cmpLabel}' not found`,
              timestamp: Date.now(),
            });
            return true;
          }

          const capCmp = getBrowserCapture();
          const cmpCurrent = capCmp ? capCmp.getConsoleRecent(200) : [];

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              baseline: {
                label: cmpLabel,
                errorCount: baseline.errors.length,
                timestamp: baseline.timestamp,
              },
              current: { errorCount: cmpCurrent.length, timestamp: Date.now() },
              newErrors: cmpCurrent.length - baseline.errors.length,
            },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Error Report
        // ==================================================================

        case "get_error_report": {
          const capRpt = getBrowserCapture();

          const rptErrors = capRpt ? capRpt.getConsoleRecent(50) : [];
          const rptOverlays = capRpt?.getFrameworkOverlays?.() ?? null;

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              healthy: rptErrors.length === 0,
              recentErrors: rptErrors.slice(0, 20),
              frameworkOverlays: rptOverlays,
              activeSession: currentErrorSession
                ? {
                    id: currentErrorSession.id,
                    label: currentErrorSession.label,
                    startTime: currentErrorSession.startTime,
                  }
                : null,
              sessionCount: errorSessions.length,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_action_history": {
          const uiBridgeGlobal = getUIBridgeGlobal();
          const renderLog = uiBridgeGlobal?.renderLog as
            | { getEntries?: () => unknown[] }
            | undefined;
          // Fall back to render log entries as action history
          const entries = renderLog?.getEntries?.() ?? [];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: entries,
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_interaction_metrics": {
          const uiBridgeGlobal2 = getUIBridgeGlobal();
          const renderLog2 = uiBridgeGlobal2?.renderLog as
            | { getEntries?: () => Array<{ type?: string }> }
            | undefined;
          const allEntries = renderLog2?.getEntries?.() ?? [];
          const byType: Record<string, number> = {};
          for (const entry of allEntries) {
            const t = entry.type ?? "unknown";
            byType[t] = (byType[t] ?? 0) + 1;
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              totalActions: allEntries.length,
              byType,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Performance Entries
        // ==================================================================

        case "get_performance_entries": {
          const perfParams = payload.params ?? {};
          const perfType = perfParams.type as string | undefined;
          const perfLimit = (perfParams.limit as number) ?? 100;

          let entries: PerformanceEntry[];
          if (perfType) {
            entries = performance.getEntriesByType(perfType);
          } else {
            entries = performance.getEntries();
          }

          // Serialize to plain objects (PerformanceEntry is not directly serializable)
          const serialized = entries.slice(-perfLimit).map((e) => ({
            name: e.name,
            entryType: e.entryType,
            startTime: e.startTime,
            duration: e.duration,
          }));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { entries: serialized, count: serialized.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "clear_performance_entries": {
          performance.clearMarks();
          performance.clearMeasures();
          performance.clearResourceTimings();

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { cleared: true },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // Element Tree & Highlight
        // ==================================================================

        case "get_element_tree": {
          // Build a lightweight DOM tree from the bridge's registered elements
          const treeElements = currentBridge.elements;

          interface TreeNode {
            id: string;
            type: string;
            label?: string;
            tagName: string;
            children: TreeNode[];
            depth: number;
            visible: boolean;
          }

          const buildTree = (el: (typeof treeElements)[0], depth: number): TreeNode => {
            const domEl = el.element;
            const state = el.getState();
            return {
              id: el.id,
              type: el.type,
              label: el.label,
              tagName: domEl instanceof HTMLElement ? domEl.tagName.toLowerCase() : "unknown",
              children: [], // Flat list of registered elements (no parent-child in bridge)
              depth,
              visible: state.visible ?? true,
            };
          };

          const tree = treeElements.map((el) => buildTree(el, 0));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { tree, count: tree.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "highlight_element": {
          const highlightId = (payload.params?.elementId as string) ?? payload.elementId ?? "";
          if (!highlightId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const highlightEl = currentBridge.getElement(highlightId);
          if (!highlightEl) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found: ${highlightId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          const domEl = highlightEl.element;
          if (domEl instanceof HTMLElement) {
            // Add a temporary highlight overlay
            const originalOutline = domEl.style.outline;
            const originalTransition = domEl.style.transition;
            domEl.style.transition = "outline 0.2s ease-in-out";
            domEl.style.outline = "3px solid #ff6b35";
            domEl.scrollIntoView({ behavior: "smooth", block: "center" });

            // Remove highlight after 2 seconds
            setTimeout(() => {
              domEl.style.outline = originalOutline;
              domEl.style.transition = originalTransition;
            }, 2000);

            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                highlighted: true,
                elementId: highlightId,
                rect: domEl.getBoundingClientRect().toJSON(),
              },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element is not an HTMLElement: ${highlightId}`,
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
