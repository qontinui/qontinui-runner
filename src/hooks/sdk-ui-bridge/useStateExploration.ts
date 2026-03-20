/**
 * useStateExploration — Automated state exploration sub-hook.
 *
 * Contains the breadth-first exploration loop that systematically clicks
 * interactive elements and captures UI state snapshots for state discovery.
 */

import { useState, useCallback, useRef } from "react";
import type {
  ExternalElement,
  ExplorationProgress,
  CaptureRecord,
  CaptureSessionStatus,
  CooccurrenceExport,
  SdkAppInfo,
  CaptureSessionRef,
} from "./types";
import { mapSdkElement } from "./utils";
import {
  generateFingerprints,
  extractFingerprintHashes,
} from "../../lib/ui-bridge/fingerprintGenerator";
import { cropThumbnails } from "@/lib/thumbnail-cropper";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { setPendingCaptureScreenshots } from "./useCaptureSession";

/** Maximum number of interactions during automated state exploration */
const MAX_EXPLORATION_INTERACTIONS = 50;

/** Delay in ms after each action to let the UI settle */
const EXPLORATION_SETTLE_DELAY = 500;

/**
 * Position zones considered global (header/footer/nav).
 * Elements in these zones are skipped during exploration to avoid
 * navigating away or triggering global actions.
 */
const GLOBAL_POSITION_ZONES = new Set(["header", "footer", "fixed-top", "fixed-bottom"]);

export interface UseStateExplorationReturn {
  isExploring: boolean;
  explorationProgress: ExplorationProgress | null;
  exploreStates: () => Promise<CooccurrenceExport | null>;
  cancelExploration: () => void;
}

export function useStateExploration(
  connectedApp: SdkAppInfo | null,
  fetchElements: () => Promise<void>,
  generateCooccurrenceExport: () => Promise<CooccurrenceExport | null>,
  captureSessionRef: React.MutableRefObject<CaptureSessionRef | null>,
  setCaptureSession: React.Dispatch<React.SetStateAction<CaptureSessionStatus>>,
  setElements: React.Dispatch<React.SetStateAction<ExternalElement[]>>,
  setCooccurrenceData: (data: CooccurrenceExport | null) => void,
): UseStateExplorationReturn {
  const [isExploring, setIsExploring] = useState(false);
  const [explorationProgress, setExplorationProgress] = useState<ExplorationProgress | null>(null);
  const explorationCancelledRef = useRef(false);

  const cancelExploration = useCallback(() => {
    explorationCancelledRef.current = true;
    console.log("[useStateExploration] Exploration cancellation requested");
  }, []);

  /**
   * Automated state exploration.
   *
   * Systematically clicks every interactive element in the main content area,
   * capturing UI state snapshots before and after each interaction. When new
   * elements appear, they are queued for exploration too (breadth-first).
   *
   * Returns the co-occurrence export when done, which can be fed into the
   * state discovery algorithm.
   */
  const exploreStates = useCallback(async (): Promise<CooccurrenceExport | null> => {
    if (isExploring) {
      console.warn("[useStateExploration] Exploration already in progress");
      return null;
    }

    console.log("[useStateExploration] Starting automated state exploration");
    setIsExploring(true);
    explorationCancelledRef.current = false;
    setExplorationProgress({ current: 0, total: 0 });

    // Step 1: Start a capture session if not already active
    const sessionWasActive = captureSessionRef.current !== null;
    if (!sessionWasActive) {
      const sessionId = `explore-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      captureSessionRef.current = {
        sessionId,
        startedAt: Date.now(),
        captures: [],
        fingerprintCatalog: {},
        lastCaptureId: null,
      };
      setCaptureSession({
        active: true,
        sessionId,
        startedAt: Date.now(),
        captureCount: 0,
        uniqueFingerprints: 0,
      });
      setCooccurrenceData(null);
      console.log(`[useStateExploration] Exploration capture session started: ${sessionId}`);
    }

    try {
      // Step 2: Capture initial state via snapshot (triggers DOM scan, unlike
      // /elements which only returns pre-registered SDK elements)
      await fetchElements();
      await new Promise((r) => setTimeout(r, EXPLORATION_SETTLE_DELAY));

      if (explorationCancelledRef.current) {
        console.log("[useStateExploration] Exploration cancelled before starting interactions");
        return null;
      }

      // Track which element fingerprints we've already interacted with
      // to avoid clicking the same logical element twice (even if its id changes)
      const interactedFingerprints = new Set<string>();
      // Track element ids we've interacted with (fallback when no fingerprint)
      const interactedIds = new Set<string>();
      let interactionCount = 0;

      // We need a way to get the latest elements. Since fetchElements updates
      // the state, and we are inside an async function, we can capture elements
      // from the fetch response directly. Let's create a helper.
      const fetchLatestElements = async (): Promise<ExternalElement[]> => {
        try {
          // Use snapshot endpoint which triggers a full DOM scan, unlike /elements
          // which only returns pre-registered SDK elements (may be 0 for self-connections)
          let resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot`);
          let json = await resp.json();

          if (json.success === false) {
            // Fall back to elements endpoint
            resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/elements`);
            json = await resp.json();
            if (json.success === false) return [];
          }

          const rawElements: unknown[] = json.data?.elements || json.elements || json.data || [];
          const mapped = (rawElements as Record<string, unknown>[]).map(mapSdkElement);
          const { catalog } = generateFingerprints(mapped);

          // Update hook state
          setElements(mapped);

          // Record capture in session (MUST succeed for transitions to work)
          const session = captureSessionRef.current;
          if (session) {
            const hashes = extractFingerprintHashes(mapped);
            const captureId = `cap-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
            const capture: CaptureRecord = {
              captureId,
              timestamp: Date.now(),
              url: connectedApp?.port ? `http://localhost:${connectedApp.port}` : "unknown",
              title: connectedApp?.appName || "SDK App",
              elementFingerprints: hashes,
              elementCount: mapped.length,
            };
            session.captures.push(capture);
            session.lastCaptureId = captureId;
            Object.assign(session.fingerprintCatalog, catalog);

            // Track element bounds for thumbnail cropping
            if (!session.elementBoundsMap) session.elementBoundsMap = {};
            for (const el of mapped) {
              if (
                el.fingerprint?.hash &&
                el.bounds &&
                el.bounds.width > 0 &&
                el.bounds.height > 0
              ) {
                session.elementBoundsMap[el.fingerprint.hash] = {
                  x: el.bounds.x,
                  y: el.bounds.y,
                  width: el.bounds.width,
                  height: el.bounds.height,
                };
              }
            }

            setCaptureSession({
              active: true,
              sessionId: session.sessionId,
              startedAt: session.startedAt,
              captureCount: session.captures.length,
              uniqueFingerprints: Object.keys(session.fingerprintCatalog).length,
            });

            // Capture thumbnails (non-fatal, in separate try/catch)
            try {
              if (Object.keys(session.elementBoundsMap).length > 0) {
                // Capture the connected app's window so element bounds align with the screenshot.
                // For self-connection (same port), use runner=true (Tauri window capture).
                // For cross-runner connections, target the other window by title.
                const ssParams = new URLSearchParams();
                const isSelfConnection = connectedApp?.port === Number(new URL(getApiBase()).port);
                if (isSelfConnection) {
                  ssParams.set("runner", "true");
                } else if (connectedApp?.appName) {
                  // Try window title match for the other runner instance
                  ssParams.set("window_title", connectedApp.appName);
                }
                const ssUrl = `${getApiBase()}/ui-bridge/sdk/screenshot${ssParams.toString() ? `?${ssParams}` : ""}`;
                const ssResp = await tracedFetch(ssUrl);
                const ssJson = await ssResp.json();
                if (ssJson.success && ssJson.data?.screenshot) {
                  const dpr = ssJson.data.scaleFactor || 1;
                  const existingThumbs = session.elementThumbnails || {};
                  const newElements = Object.entries(session.elementBoundsMap)
                    .filter(([hash]) => !existingThumbs[hash])
                    .map(([hash, bounds]) => ({
                      id: hash,
                      bounds: {
                        x: (bounds as { x: number }).x * dpr,
                        y: (bounds as { y: number }).y * dpr,
                        width: (bounds as { width: number }).width * dpr,
                        height: (bounds as { height: number }).height * dpr,
                      },
                    }));
                  if (newElements.length > 0) {
                    const thumbs = await cropThumbnails(ssJson.data.screenshot, newElements, {
                      maxSize: 48,
                    });
                    if (!session.elementThumbnails) session.elementThumbnails = {};
                    for (const [id, data] of thumbs) {
                      session.elementThumbnails[id] = data;
                    }
                  }

                  // Store full screenshot for screenshot state view (deduplicated)
                  // Use only elements from the CURRENT snapshot (mapped), not the
                  // accumulated session.elementBoundsMap which spans all pages.
                  const currentPageHashes = new Set<string>();
                  const currentPageBounds: Record<string, { x: number; y: number; width: number; height: number }> = {};
                  for (const el of mapped) {
                    if (
                      el.fingerprint?.hash &&
                      el.bounds &&
                      el.bounds.width > 0 &&
                      el.bounds.height > 0
                    ) {
                      currentPageHashes.add(el.fingerprint.hash);
                      currentPageBounds[el.fingerprint.hash] = {
                        x: el.bounds.x * dpr,
                        y: el.bounds.y * dpr,
                        width: el.bounds.width * dpr,
                        height: el.bounds.height * dpr,
                      };
                    }
                  }
                  const prevHashes = session.prevCaptureHashes;
                  const hashesChanged = !prevHashes ||
                    currentPageHashes.size !== prevHashes.size ||
                    [...currentPageHashes].some(h => !prevHashes.has(h));
                  if (hashesChanged && currentPageHashes.size > 0) {
                    if (!session.captureScreenshots) session.captureScreenshots = [];
                    const screenshotEntry = {
                      captureIndex: session.captureScreenshots.length,
                      screenshotBase64: ssJson.data.screenshot,
                      width: Math.round((ssJson.data.width || 1920) * dpr),
                      height: Math.round((ssJson.data.height || 1080) * dpr),
                      elementBoundsJson: JSON.stringify(currentPageBounds),
                      fingerprintHashesJson: JSON.stringify([...currentPageHashes]),
                      capturedAt: new Date().toISOString(),
                    };
                    session.captureScreenshots.push(screenshotEntry);
                    session.prevCaptureHashes = currentPageHashes;
                    // Also persist to module-level variable so screenshots survive
                    // page navigation during self-connect exploration
                    setPendingCaptureScreenshots([...session.captureScreenshots]);
                    // Save screenshot to DB immediately with a pending config ID
                    // so it survives process restarts and page navigation
                    import("@tauri-apps/api/core").then(({ invoke }) => {
                      invoke("sm_save_capture_screenshots", {
                        configId: `pending-${session.sessionId}`,
                        screenshots: [screenshotEntry],
                      }).catch(() => { /* non-fatal */ });
                    }).catch(() => { /* non-fatal */ });
                  }
                }
              }
            } catch {
              // Thumbnails are optional — don't fail the capture
            }
          }

          return mapped;
        } catch {
          return [];
        }
      };

      const executeExplorationAction = async (
        elementId: string,
        action: string,
      ): Promise<ExternalElement[]> => {
        const session = captureSessionRef.current;
        const beforeCaptureId = session?.lastCaptureId || null;

        try {
          await tracedFetch(
            `${getApiBase()}/ui-bridge/sdk/element/${encodeURIComponent(elementId)}/action`,
            {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ action, params: {} }),
            },
          );
        } catch {
          // Action failed — still capture current state for co-occurrence
        }

        // Wait for UI to settle, then capture
        await new Promise((r) => setTimeout(r, EXPLORATION_SETTLE_DELAY));

        const afterElements = await fetchLatestElements();

        // Record transition if we have before/after captures
        if (session && beforeCaptureId) {
          const afterCaptureId = session.lastCaptureId;
          if (afterCaptureId && afterCaptureId !== beforeCaptureId) {
            const afterCapture = session.captures.find((c) => c.captureId === afterCaptureId);
            const targetEl = afterElements.find((e) => e.id === elementId) || undefined;
            const targetFingerprint = targetEl?.fingerprint?.hash || "";

            if (afterCapture) {
              afterCapture.triggeredBy = {
                actionType: action,
                targetFingerprint,
                previousCaptureId: beforeCaptureId,
              };
            }
          }
        }

        return afterElements;
      };

      /**
       * Filter elements to only those eligible for exploration.
       */
      const filterExplorable = (els: ExternalElement[]): ExternalElement[] => {
        return els.filter((el) => {
          // Must be interactive
          if (!el.actions || el.actions.length === 0) return false;
          // Must be visible and enabled
          if (!el.visible || !el.enabled) return false;
          // Skip global zones (header, footer, fixed areas)
          const zone = el.fingerprint?.positionZone;
          if (zone && GLOBAL_POSITION_ZONES.has(zone)) return false;
          // Skip already interacted (by fingerprint)
          if (el.fingerprint?.hash && interactedFingerprints.has(el.fingerprint.hash)) return false;
          // Skip already interacted (by id, fallback)
          if (!el.fingerprint?.hash && interactedIds.has(el.id)) return false;
          return true;
        });
      };

      // Get initial explorable elements
      let currentElements = await fetchLatestElements();
      let queue = filterExplorable(currentElements);

      setExplorationProgress({
        current: 0,
        total: queue.length,
      });

      console.log(
        `[useStateExploration] Initial exploration queue: ${queue.length} interactive elements (filtered from ${currentElements.length} total)`,
      );

      // Snapshot the initial fingerprint set to detect "page changes"
      let baselineFingerprints = new Set(extractFingerprintHashes(currentElements));

      // Breadth-first exploration loop
      while (queue.length > 0 && interactionCount < MAX_EXPLORATION_INTERACTIONS) {
        if (explorationCancelledRef.current) {
          console.log(
            `[useStateExploration] Exploration cancelled after ${interactionCount} interactions`,
          );
          break;
        }

        const element = queue.shift()!;
        const elementLabel = element.accessibleName || element.label || element.text || element.id;

        // Mark as interacted
        if (element.fingerprint?.hash) {
          interactedFingerprints.add(element.fingerprint.hash);
        }
        interactedIds.add(element.id);

        interactionCount++;
        setExplorationProgress({
          current: interactionCount,
          total: interactionCount + queue.length,
          currentElement: elementLabel,
        });

        console.log(
          `[useStateExploration] Exploring [${interactionCount}/${interactionCount + queue.length}]: click "${elementLabel}" (${element.id})`,
        );

        // Capture the fingerprint set before the action
        const beforeFingerprints = new Set(extractFingerprintHashes(currentElements));

        // Execute click — creates a capture with triggeredBy for transition tracking
        // and returns the post-action elements (avoids duplicate fetchLatestElements call)
        currentElements = await executeExplorationAction(element.id, "click");
        const afterFingerprints = new Set(extractFingerprintHashes(currentElements));

        // Detect significant change: more than 30% of fingerprints changed
        const appeared = [...afterFingerprints].filter((h) => !beforeFingerprints.has(h));
        const disappeared = [...beforeFingerprints].filter((h) => !afterFingerprints.has(h));
        const totalBefore = beforeFingerprints.size;
        const changeRatio =
          totalBefore > 0 ? (appeared.length + disappeared.length) / totalBefore : 0;

        if (appeared.length > 0 || disappeared.length > 0) {
          console.log(
            `[useStateExploration] State change: +${appeared.length} -${disappeared.length} fingerprints (${(changeRatio * 100).toFixed(1)}% change)`,
          );
        }

        // If significant change (likely navigated to a new "page"), discover new elements
        if (changeRatio > 0.3) {
          console.log(
            "[useStateExploration] Significant state change detected — exploring new elements",
          );

          // Add newly visible interactive elements to the queue
          const newExplorable = filterExplorable(currentElements);
          for (const newEl of newExplorable) {
            // Only add if not already in queue (by id)
            if (!queue.some((q) => q.id === newEl.id)) {
              queue.push(newEl);
            }
          }

          // Update total in progress
          setExplorationProgress({
            current: interactionCount,
            total: interactionCount + queue.length,
            currentElement: elementLabel,
          });

          // Try to navigate back to restore baseline state
          // Attempt: look for a "back" button or similar navigation element
          const backElement = currentElements.find((el) => {
            const name = (el.accessibleName || el.label || el.text || "").toLowerCase();
            return (
              (name.includes("back") ||
                name.includes("close") ||
                name.includes("cancel") ||
                name.includes("return")) &&
              el.actions.length > 0 &&
              el.visible &&
              el.enabled
            );
          });

          if (backElement) {
            console.log(
              `[useStateExploration] Attempting to navigate back via "${backElement.accessibleName || backElement.label || backElement.text || backElement.id}"`,
            );
            currentElements = await executeExplorationAction(backElement.id, "click");

            // Check if we returned to baseline
            const returnedFingerprints = new Set(extractFingerprintHashes(currentElements));
            const returnOverlap = [...baselineFingerprints].filter((h) =>
              returnedFingerprints.has(h),
            ).length;
            const returnRatio =
              baselineFingerprints.size > 0 ? returnOverlap / baselineFingerprints.size : 0;

            if (returnRatio > 0.7) {
              console.log(
                `[useStateExploration] Successfully returned to baseline (${(returnRatio * 100).toFixed(1)}% match)`,
              );
            } else {
              console.log(
                `[useStateExploration] Navigation back resulted in different state (${(returnRatio * 100).toFixed(1)}% baseline match)`,
              );
              // Update baseline since we're in a new state
              baselineFingerprints = new Set(extractFingerprintHashes(currentElements));
            }
          } else {
            // No obvious back button — update baseline to current state
            console.log(
              "[useStateExploration] No back navigation found — updating baseline to current state",
            );
            baselineFingerprints = new Set(extractFingerprintHashes(currentElements));
          }

          // Refresh the queue with current elements
          queue = filterExplorable(currentElements);
        }
      }

      if (interactionCount >= MAX_EXPLORATION_INTERACTIONS) {
        console.log(
          `[useStateExploration] Exploration reached maximum interaction limit (${MAX_EXPLORATION_INTERACTIONS})`,
        );
      }

      console.log(
        `[useStateExploration] Exploration complete: ${interactionCount} interactions, ${captureSessionRef.current?.captures.length ?? 0} captures`,
      );

      // Step 5: Persist thumbnails to dedicated localStorage key
      const thumbs = captureSessionRef.current?.elementThumbnails;
      const thumbCount = Object.keys(thumbs || {}).length;
      if (thumbCount > 0) {
        console.log(
          `[useStateExploration] Captured ${thumbCount} element thumbnails during exploration`,
        );
        try {
          localStorage.setItem("qontinui-runner-sm-thumbnails", JSON.stringify(thumbs));
        } catch {
          /* */
        }
      }

      // Step 5b: Persist capture screenshots to module-level variable
      // (survives route changes unlike React state; too large for localStorage)
      const captureScreenshotsData = captureSessionRef.current?.captureScreenshots;
      if (captureScreenshotsData && captureScreenshotsData.length > 0) {
        setPendingCaptureScreenshots(captureScreenshotsData);
        console.log(
          `[useStateExploration] Stored ${captureScreenshotsData.length} capture screenshots for later save`,
        );
      }

      // Step 6: Generate co-occurrence export
      const result = await generateCooccurrenceExport();

      // Step 7: Persist the result to localStorage so it survives page navigation
      // (self-connection exploration causes the runner to navigate away from the
      // State Machine page, unmounting the discovery component)
      if (result) {
        try {
          const { instanceStorage } = await import("@/lib/instance-storage");
          // Strip thumbnails and capture screenshots from the persisted discovery data
          // (they're saved separately to the DB and would make this JSON too large for localStorage)
          const { elementThumbnails: _thumbs, captureScreenshots: _screenshots, ...resultWithoutThumbs } = result;
          instanceStorage.setJSON("qontinui-runner-sm-discovery", {
            cooccurrenceData: resultWithoutThumbs,
            dataSource: "explore",
            discoveryResult: null,
            configName: "",
            pendingScreenshotSessionId: captureSessionRef.current?.sessionId,
          });
          console.log("[useStateExploration] Persisted exploration result to localStorage");
        } catch {
          console.warn("[useStateExploration] Failed to persist exploration result");
        }
      }

      // Step 8: Stop capture session if we started it
      if (!sessionWasActive) {
        captureSessionRef.current = null;
        setCaptureSession({ active: false });
        console.log("[useStateExploration] Exploration capture session stopped");
      }

      return result;
    } catch (err) {
      console.error("[useStateExploration] State exploration failed:", err);
      return null;
    } finally {
      setIsExploring(false);
      setExplorationProgress(null);
    }
  }, [isExploring, connectedApp, fetchElements, generateCooccurrenceExport, captureSessionRef, setCaptureSession, setElements, setCooccurrenceData]);

  return {
    isExploring,
    explorationProgress,
    exploreStates,
    cancelExploration,
  };
}
